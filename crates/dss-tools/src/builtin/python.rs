//! python 工具：最小子进程方案（非沙箱）。
//!
//! roadmap P2 明确「先用最小子进程方案，沙箱留 P9」。本工具：`python3 -c <code>`，
//! cwd=workspace，捕获 stdout/stderr，超时 kill，kill_on_drop。
//! 无 venv 注入、无变量跨调用持久（沙箱化 + 持久 state 是 P9/方向 2.1）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

#[derive(Deserialize)]
struct PythonArgs {
    code: String,
    #[serde(default)]
    timeout: Option<u64>,
}

pub struct PythonTool;

#[async_trait]
impl Tool for PythonTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "python".into(),
            description: "Run Python 3 code and return stdout+stderr and exit code. CWD is the workspace root. State does NOT persist between calls (fresh process each call). Non-zero exit is an error. Default timeout 30s.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string", "description": "Python 3 source code to run." },
                    "timeout": { "type": "integer", "description": "Timeout in seconds (1-300, default 30)." }
                },
                "required": ["code"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: PythonArgs = parse_args(&args)?;
        let timeout_secs = a.timeout.unwrap_or(30).clamp(1, 300);

        let mut cmd = Command::new("python3");
        cmd.arg("-c").arg(&a.code);
        cmd.current_dir(&ctx.workspace);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            cmd.output(),
        )
        .await
        .map_err(|_| ToolError::Timeout(timeout_secs))??;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code().unwrap_or(-1);

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

        if code == 0 {
            Ok(ToolOutput::ok(text))
        } else {
            Ok(ToolOutput::err(text))
        }
    }
}
