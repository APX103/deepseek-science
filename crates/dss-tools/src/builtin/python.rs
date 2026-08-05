//! python 工具：解析可信 Python 3 后，在 macOS Seatbelt 工作区沙箱内执行。

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::sandbox::run_workspace_python;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

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
            description: "Run Python 3 code in a fail-closed macOS workspace sandbox and return stdout+stderr and exit code. The workspace is the only writable area; user files outside it and TCP/loopback network access are denied. HOME/TMPDIR are temporary workspace directories and inherited credentials/PYTHONPATH are cleared. State does NOT persist between calls. Non-zero exit is an error. Default timeout 30s. Start with a small deterministic probe; after an error, diagnose from the actual output and change the method instead of repeating the same code.".into(),
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

    fn timeout(&self, args: &Value) -> std::time::Duration {
        let requested = args
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(30)
            .clamp(1, 300);
        // Preserve the requested execution budget even when a compile currently owns the
        // exclusive workspace guard.
        std::time::Duration::from_secs(requested.saturating_add(190))
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: PythonArgs = parse_args(&args)?;
        let timeout_secs = a.timeout.unwrap_or(30).clamp(1, 300);
        // Python has the same workspace mutation authority as Bash. The shared process guard lets
        // independent code calls stay parallel while compile/edit/write take exclusive access.
        let _workspace_guard = ctx.lock_workspace_read().await;

        let output = run_workspace_python(
            &ctx.workspace,
            &a.code,
            std::time::Duration::from_secs(timeout_secs),
        )
        .await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.and_then(|status| status.code()).unwrap_or(-1);

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

        if code == 0 && !output.output_limit_exceeded {
            Ok(ToolOutput::ok(text))
        } else {
            Ok(ToolOutput::err(text))
        }
    }
}
