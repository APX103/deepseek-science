//! compile_pdf 工具：Tectonic 子进程编译 .tex → PDF。
//!
//! P5a 基础编译；浮动环境 \iffalse 容错重编译留 P5b。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

#[derive(Deserialize)]
struct CompileArgs {
    path: String,
    #[serde(default)]
    out_name: Option<String>,
}

pub struct CompilePdfTool;

#[async_trait]
impl Tool for CompilePdfTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "compile_pdf".into(),
            description: "Compile a LaTeX (.tex) file to PDF using Tectonic. `path` is the .tex relative to workspace. Returns success, pdf_path, and message (with log excerpt on failure).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": ".tex file path relative to workspace." },
                    "out_name": { "type": "string", "description": "Output PDF name (optional, defaults to source stem)." }
                },
                "required": ["path"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: CompileArgs = parse_args(&args)?;
        let tex_abs = ctx.resolve_in_workspace(&a.path)?;
        if !tex_abs.exists() {
            return Ok(ToolOutput::err(format!("tex file not found: {}", a.path)));
        }
        let stem = tex_abs
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".into());
        let out_name = a.out_name.unwrap_or(stem);

        // tectonic --outdir <workspace> <tex>
        let mut cmd = Command::new("tectonic");
        cmd.arg("-X").arg("compile");
        cmd.arg("--outdir").arg(ctx.workspace.as_os_str());
        cmd.arg(&tex_abs);
        cmd.current_dir(&ctx.workspace);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let output = tokio::time::timeout(std::time::Duration::from_secs(180), cmd.output())
            .await
            .map_err(|_| ToolError::Timeout(180))??;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output.status.code().unwrap_or(-1);
        let pdf_path = ctx.workspace.join(format!("{out_name}.pdf"));

        if code == 0 && pdf_path.exists() {
            let size_kb = pdf_path.metadata().map(|m| m.len() / 1024).unwrap_or(0);
            Ok(ToolOutput::ok(format!(
                "compiled {out_name}.pdf ({size_kb} KB) at {}",
                pdf_path.display()
            )))
        } else {
            let log_excerpt: String = format!("{stdout}\n--- stderr ---\n{stderr}")
                .chars()
                .rev()
                .take(3000)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            Ok(ToolOutput::err(format!(
                "tectonic failed (exit {code}). Log tail:\n{log_excerpt}"
            )))
        }
    }
}
