//! Bounded child-process execution shared by the shell and Python tools.

use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use crate::error::ToolError;

/// Keep command output useful for diagnostics without allowing an untrusted
/// process to grow the backend indefinitely. The two streams therefore have
/// a combined retained ceiling of 1 MiB.
pub(crate) const MAX_STREAM_BYTES: usize = 512 * 1024;

#[derive(Debug)]
pub(crate) struct ProcessOutput {
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub output_limit_exceeded: bool,
}

struct Capture {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Synchronously tears down the command's process group if the async tool
/// future is cancelled. `Child::kill_on_drop` only covers the direct child;
/// a shell can otherwise leave background descendants running after the UI
/// has already reported that the run stopped.
struct ProcessGroupGuard(Option<u32>);

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.0 {
            // The spawned child is the leader of a dedicated process group.
            unsafe {
                let _ = libc::killpg(pid as i32, libc::SIGKILL);
            }
        }
    }
}

/// Run a command in its own process group, drain stdout/stderr concurrently,
/// and terminate the whole group on timeout or retained-output overflow.
pub(crate) async fn run_bounded(
    mut command: Command,
    timeout: Duration,
) -> Result<ProcessOutput, ToolError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    // Fixed-purpose children such as Tectonic keep their normal cache/config environment for
    // compatibility, but never receive common API/cloud credentials or the backend capability.
    // bash/python additionally call `env_clear` before reaching this shared runner.
    for key in [
        "DSS_API_TOKEN",
        "DEEPSEEK_API_KEY",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "AZURE_OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AZURE_CLIENT_ID",
        "AZURE_CLIENT_SECRET",
        "AZURE_TENANT_ID",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "HF_TOKEN",
        "HUGGING_FACE_HUB_TOKEN",
        "KAGGLE_KEY",
        "NPM_TOKEN",
        "WANDB_API_KEY",
        "SSH_AUTH_SOCK",
    ] {
        command.env_remove(key);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }

    let mut child = command.spawn()?;
    let process_group = child.id();
    let _process_group_guard = ProcessGroupGuard(process_group);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::other("child stdout was not piped"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::other("child stderr was not piped"))?;

    let (limit_tx, mut limit_rx) = mpsc::channel::<()>(1);
    let mut stdout_task = tokio::spawn(capture_bounded(stdout, limit_tx.clone()));
    let mut stderr_task = tokio::spawn(capture_bounded(stderr, limit_tx));
    let deadline = tokio::time::Instant::now() + timeout;

    enum Completion {
        Exited(std::io::Result<ExitStatus>),
        OutputLimit,
        Timeout,
    }

    let output_limit = async {
        if limit_rx.recv().await.is_none() {
            std::future::pending::<()>().await;
        }
    };
    tokio::pin!(output_limit);

    let completion = tokio::select! {
        status = child.wait() => Completion::Exited(status),
        _ = &mut output_limit => Completion::OutputLimit,
        _ = tokio::time::sleep_until(deadline) => Completion::Timeout,
    };

    let (status, output_limit_exceeded) = match completion {
        Completion::Exited(status) => (Some(status?), false),
        Completion::OutputLimit => {
            terminate_process_tree(&mut child, process_group).await;
            (None, true)
        }
        Completion::Timeout => {
            terminate_process_tree(&mut child, process_group).await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(ToolError::Timeout(timeout.as_secs()));
        }
    };

    // A shell may exit after spawning a background descendant that inherited
    // its pipes. Bound that drain by the original deadline as well.
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    let captures = tokio::time::timeout(remaining, async {
        let stdout = (&mut stdout_task).await.map_err(ToolError::other)??;
        let stderr = (&mut stderr_task).await.map_err(ToolError::other)??;
        Ok::<_, ToolError>((stdout, stderr))
    })
    .await;

    let (stdout, stderr) = match captures {
        Ok(result) => result?,
        Err(_) => {
            terminate_process_tree(&mut child, process_group).await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(ToolError::Timeout(timeout.as_secs()));
        }
    };

    Ok(ProcessOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        output_limit_exceeded: output_limit_exceeded || stdout.truncated || stderr.truncated,
    })
}

async fn capture_bounded<R>(mut reader: R, limit_tx: mpsc::Sender<()>) -> std::io::Result<Capture>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(MAX_STREAM_BYTES.min(16 * 1024));
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = MAX_STREAM_BYTES.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        if read > remaining && !truncated {
            truncated = true;
            let _ = limit_tx.try_send(());
        }
    }
    Ok(Capture { bytes, truncated })
}

async fn terminate_process_tree(child: &mut Child, process_group: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = process_group {
        // The child was made its own process-group leader before spawn.
        unsafe {
            let _ = libc::killpg(pid as i32, libc::SIGKILL);
        }
    }

    let _ = child.kill().await;
    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminates_on_unbounded_output() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("while :; do printf 1234567890; done");
        let output = run_bounded(command, Duration::from_secs(10))
            .await
            .expect("output limit should be a controlled result");
        assert!(output.output_limit_exceeded);
        assert!(output.status.is_none());
        assert!(output.stdout.len() <= MAX_STREAM_BYTES);
        assert!(output.stderr.len() <= MAX_STREAM_BYTES);
    }

    #[tokio::test]
    async fn strips_common_credentials_from_child_environment() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(
                "printf '%s|%s|%s|%s' \"${DSS_API_TOKEN-unset}\" \"${DEEPSEEK_API_KEY-unset}\" \"${AWS_SECRET_ACCESS_KEY-unset}\" \"${SSH_AUTH_SOCK-unset}\"",
            )
            .env("DSS_API_TOKEN", "fake-test-secret")
            .env("DEEPSEEK_API_KEY", "fake-test-secret")
            .env("AWS_SECRET_ACCESS_KEY", "fake-test-secret")
            .env("SSH_AUTH_SOCK", "/tmp/fake-test-agent.sock");
        let output = run_bounded(command, Duration::from_secs(2)).await.unwrap();

        assert_eq!(output.status.and_then(|status| status.code()), Some(0));
        assert_eq!(output.stdout, b"unset|unset|unset|unset");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_terminates_descendant_process_group() {
        let root =
            std::env::temp_dir().join(format!("dss-tools-process-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 30 & echo $! > child.pid; wait")
            .current_dir(&root);
        let error = run_bounded(command, Duration::from_millis(300))
            .await
            .expect_err("silent process tree should time out");
        assert!(matches!(error, ToolError::Timeout(_)));

        let pid: i32 = std::fs::read_to_string(root.join("child.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut exists = true;
        for _ in 0..20 {
            exists = unsafe { libc::kill(pid, 0) == 0 };
            if !exists {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if exists {
            unsafe {
                let _ = libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        assert!(!exists, "grandchild process {pid} survived timeout");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_terminates_descendant_process_group() {
        let root =
            std::env::temp_dir().join(format!("dss-tools-cancel-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("sleep 30 & echo $! > child.pid; wait")
            .current_dir(&root);
        let task = tokio::spawn(run_bounded(command, Duration::from_secs(30)));

        let pid_path = root.join("child.pid");
        for _ in 0..100 {
            if pid_path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let pid: i32 = std::fs::read_to_string(&pid_path)
            .expect("child pid should be recorded before cancellation")
            .trim()
            .parse()
            .unwrap();

        task.abort();
        let _ = task.await;

        let mut exists = true;
        for _ in 0..20 {
            exists = unsafe { libc::kill(pid, 0) == 0 };
            if !exists {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if exists {
            unsafe {
                let _ = libc::kill(pid, libc::SIGKILL);
            }
        }
        let _ = std::fs::remove_dir_all(&root);
        assert!(!exists, "grandchild process {pid} survived cancellation");
    }
}
