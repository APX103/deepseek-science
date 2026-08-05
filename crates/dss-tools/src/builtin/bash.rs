//! bash 工具：macOS Seatbelt 工作区沙箱中的 `/bin/sh -c`，超时后终止进程组。

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::sandbox::run_workspace_shell;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

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

    fn timeout(&self, args: &Value) -> std::time::Duration {
        let requested = args
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 300);
        // A process call may wait for a validation-critical compile (up to 180s). Leave that wait
        // outside the process's requested runtime budget, plus teardown margin.
        std::time::Duration::from_secs(requested.saturating_add(190))
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: BashArgs = parse_args(&args)?;
        let timeout_secs = a.timeout.unwrap_or(30).clamp(1, 300);
        // Shell code may rename arbitrary workspace entries. Multiple sandboxed processes may
        // still run together, but the shared side excludes compile/edit/write critical sections.
        let _workspace_guard = ctx.lock_workspace_read().await;

        let output = run_workspace_shell(
            &ctx.workspace,
            &a.command,
            std::time::Duration::from_secs(timeout_secs),
        )
        .await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.and_then(|status| status.code()).unwrap_or(-1);

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
        if output.output_limit_exceeded {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str("[output limit exceeded; process tree terminated]");
        }
        text.push_str(&format!("\n[exit code: {code}]"));
        let text = text.trim().to_string();

        // 非零退出码视为错误（内容仍返回，便于 LLM 修 bug）。
        if code == 0 && !output.output_limit_exceeded {
            Ok(ToolOutput::ok(text))
        } else {
            Ok(ToolOutput::err(text))
        }
    }
}

fn bash_spec() -> ToolSpec {
    ToolSpec {
        name: "bash".into(),
        description: "Run `/bin/sh -c` inside a fail-closed macOS workspace sandbox. The workspace is the only writable area; user files outside it and TCP/loopback network access are denied. HOME/TMPDIR are temporary workspace directories and the inherited environment is cleared. Returns stdout+stderr and exit code. Non-zero exit is an error. Default timeout 30s. Prefer the smallest focused validation; after an error, diagnose and change the command or method rather than repeating it unchanged.".into(),
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
