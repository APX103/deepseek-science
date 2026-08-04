//! bash 工具：tokio::process::Command（sh -c），cwd=workspace，超时 kill。
//!
//! P2a 非沙箱方案：cwd 锁定 workspace、超时 30s、进程组 kill。
//! 沙箱化（Python 子进程 + JSON-RPC）留 P2b/P9（见 decisions.md）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

#[derive(Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default)]
    timeout: Option<u64>,
}

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        bash_spec()
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: BashArgs = parse_args(&args)?;
        let timeout_secs = a.timeout.unwrap_or(30).clamp(1, 300);

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&a.command);
        cmd.current_dir(&ctx.workspace);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let output =
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), cmd.output())
                .await
                .map_err(|_| ToolError::Timeout(timeout_secs))??;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code().unwrap_or(-1);

        // 合并输出，带上退出码与 stderr（便于 LLM 诊断）。
        let mut text = String::new();
        if !stdout.is_empty() {
            text.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !text.is_empty() {
                text.push_str("\n--- stderr ---\n");
            }
            text.push_str(&stderr);
        }
        text.push_str(&format!("\n[exit code: {code}]"));
        let text = text.trim().to_string();

        // 非零退出码视为错误（内容仍返回，便于 LLM 修 bug）。
        if code == 0 {
            Ok(ToolOutput::ok(text))
        } else {
            Ok(ToolOutput::err(text))
        }
    }
}

fn bash_spec() -> ToolSpec {
    ToolSpec {
        name: "bash".into(),
        description: "Run a shell command in the workspace (sh -c). CWD is the workspace root. Returns stdout+stderr and exit code. Non-zero exit is an error. Default timeout 30s.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run." },
                "timeout": { "type": "integer", "description": "Timeout in seconds (1-300, default 30)." }
            },
            "required": ["command"]
        }),
    }
}
