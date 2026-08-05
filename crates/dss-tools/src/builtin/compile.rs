//! compile_pdf 工具：Tectonic 子进程编译 .tex → PDF。
//!
//! P5a 基础编译；浮动环境 \iffalse 容错重编译留 P5b。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::ffi::OsString;
use std::path::PathBuf;

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::sandbox::run_workspace_tectonic;
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
            description: "Compile a workspace LaTeX (.tex) file with Tectonic in a fail-closed macOS sandbox. Compilation is untrusted, offline (`--only-cached`), and can read/write only the workspace plus the exact Tectonic cache. Returns success, pdf_path, and message (with log excerpt on failure).".into(),
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

    fn timeout(&self, _args: &Value) -> std::time::Duration {
        // Compilation can wait for already-running Bash/Python calls (up to 300s), then has its
        // own 180s process bound and teardown margin.
        std::time::Duration::from_secs(490)
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: CompileArgs = parse_args(&args)?;
        // Tectonic necessarily re-opens its input and output by pathname. Hold the exclusive
        // workspace guard from validation through output commit so concurrently dispatched
        // Bash/Python/file calls cannot swap a checked component for a cache-pointing symlink.
        let _workspace_guard = ctx.lock_workspace_write().await;
        let workspace = ctx.secure_workspace()?;
        let source_rel = a.path.trim();
        let _source_handle = match workspace.open_file(source_rel) {
            Ok(handle) => handle,
            Err(ToolError::NotFound(_)) => {
                return Ok(ToolOutput::err(format!("tex file not found: {}", a.path)))
            }
            Err(error) => return Err(error),
        };
        let tex_abs = ctx.resolve_in_workspace(&a.path)?;
        let stem = tex_abs
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "output".into());
        let out_name = normalize_output_name(a.out_name.as_deref(), &stem)?;
        let generated_rel = format!("{stem}.pdf");
        let pdf_rel = format!("{out_name}.pdf");

        let runtime = resolve_tectonic_runtime()?;
        let command_args = vec![
            OsString::from("-X"),
            OsString::from("compile"),
            OsString::from("--untrusted"),
            OsString::from("--only-cached"),
            OsString::from("--outdir"),
            ctx.workspace.as_os_str().to_os_string(),
            tex_abs.as_os_str().to_os_string(),
        ];
        // Remove a stale regular file or symlink before launching. Since all model-controlled
        // workspace mutators use the same write guard, Tectonic now creates this output itself
        // rather than following a pre-positioned symlink into its writable cache.
        workspace.clear_output_file(&generated_rel)?;
        let output = run_workspace_tectonic(
            &ctx.workspace,
            &runtime.executable,
            &runtime.cache,
            &runtime.home,
            &command_args,
            std::time::Duration::from_secs(180),
        )
        .await?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let code = output
            .status
            .as_ref()
            .and_then(|status| status.code())
            .unwrap_or(-1);
        if code == 0 && generated_rel != pdf_rel {
            workspace.rename_file(&generated_rel, &pdf_rel)?;
        }

        if code == 0 {
            let pdf = workspace.open_file(&pdf_rel)?;
            let size_kb = pdf.metadata().map(|m| m.len() / 1024).unwrap_or(0);
            let pdf_path = ctx.workspace.join(&pdf_rel);
            Ok(ToolOutput::ok(format!(
                "compiled {out_name}.pdf ({size_kb} KB) at {}",
                pdf_path.display()
            )))
        } else {
            let output_limit = if output.output_limit_exceeded {
                "\n[output limit exceeded; process tree terminated]"
            } else {
                ""
            };
            let log_excerpt: String = format!("{stdout}\n--- stderr ---\n{stderr}{output_limit}")
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

fn normalize_output_name(requested: Option<&str>, fallback: &str) -> Result<String, ToolError> {
    let name = requested.unwrap_or(fallback).trim();
    let name = name.strip_suffix(".pdf").unwrap_or(name);
    let path = std::path::Path::new(name);
    if name.is_empty()
        || path.is_absolute()
        || path.components().count() != 1
        || matches!(name, "." | "..")
    {
        return Err(ToolError::InvalidArgs(
            "out_name must be a plain file name without directories".into(),
        ));
    }
    Ok(name.to_string())
}

struct TectonicRuntime {
    executable: PathBuf,
    cache: PathBuf,
    home: PathBuf,
}

fn resolve_tectonic_runtime() -> Result<TectonicRuntime, ToolError> {
    let executable = resolve_tectonic_executable()?;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| ToolError::other("Tectonic sandbox unavailable: HOME is not set"))?
        .canonicalize()
        .map_err(|error| ToolError::other(format!("could not resolve Tectonic HOME: {error}")))?;
    let cache_root = home
        .join("Library/Caches/Tectonic")
        .canonicalize()
        .map_err(|error| {
            ToolError::other(format!(
                "Tectonic cache unavailable at ~/Library/Caches/Tectonic: {error}"
            ))
        })?;
    if !cache_root.starts_with(&home) {
        return Err(ToolError::other(
            "Tectonic cache unavailable: cache root escapes HOME",
        ));
    }
    if !cache_root.is_dir() {
        return Err(ToolError::other(
            "Tectonic cache unavailable: cache root is not a directory",
        ));
    }
    Ok(TectonicRuntime {
        executable,
        cache: cache_root,
        home,
    })
}

fn resolve_tectonic_executable() -> Result<PathBuf, ToolError> {
    let requested = std::env::var_os("DSS_TECTONIC_PATH").filter(|value| !value.is_empty());
    let candidate = if let Some(requested) = requested {
        resolve_application_path(&requested).ok_or_else(|| {
            ToolError::other(format!(
                "DSS_TECTONIC_PATH could not be resolved by the application: {}",
                PathBuf::from(requested).display()
            ))
        })?
    } else {
        [
            PathBuf::from("/opt/homebrew/bin/tectonic"),
            PathBuf::from("/usr/local/bin/tectonic"),
            PathBuf::from("/usr/bin/tectonic"),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| ToolError::other("Tectonic executable is not installed"))?
    };
    let executable = candidate.canonicalize().map_err(|error| {
        ToolError::other(format!(
            "could not resolve Tectonic executable {}: {error}",
            candidate.display()
        ))
    })?;
    if !executable.is_file() {
        return Err(ToolError::other(format!(
            "Tectonic executable is not a file: {}",
            executable.display()
        )));
    }
    Ok(executable)
}

fn resolve_application_path(requested: &std::ffi::OsStr) -> Option<PathBuf> {
    let requested = PathBuf::from(requested);
    if requested.is_absolute() || requested.components().count() > 1 {
        return requested.is_file().then_some(requested);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|directory| directory.join(&requested))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::{normalize_output_name, CompilePdfTool};
    use crate::{Tool, ToolContext};
    use serde_json::json;

    #[test]
    fn output_name_accepts_plain_name_and_pdf_suffix() {
        assert_eq!(
            normalize_output_name(Some("paper"), "main").unwrap(),
            "paper"
        );
        assert_eq!(
            normalize_output_name(Some("paper.pdf"), "main").unwrap(),
            "paper"
        );
        assert_eq!(normalize_output_name(None, "main").unwrap(), "main");
    }

    #[test]
    fn output_name_rejects_paths() {
        assert!(normalize_output_name(Some("../paper"), "main").is_err());
        assert!(normalize_output_name(Some("sub/paper"), "main").is_err());
        assert!(normalize_output_name(Some(""), "main").is_err());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn installed_tectonic_compiles_from_cache_inside_sandbox() {
        if super::resolve_tectonic_runtime().is_err() {
            return;
        }
        let root = std::env::temp_dir().join(format!(
            "dss-tools-real-tectonic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock before Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("create Tectonic test workspace");
        let outside = root.with_extension("outside");
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(&outside).expect("create outside test directory");
        std::fs::write(outside.join("generated-target"), "keep-generated")
            .expect("write generated target");
        std::fs::write(outside.join("final-target"), "keep-final").expect("write final target");
        std::fs::write(
            root.join("main.tex"),
            "\\documentclass{article}\\begin{document}SANDBOX OK\\end{document}",
        )
        .expect("write Tectonic fixture");
        std::os::unix::fs::symlink(outside.join("generated-target"), root.join("main.pdf"))
            .expect("pre-position generated output symlink");
        std::os::unix::fs::symlink(outside.join("final-target"), root.join("result.pdf"))
            .expect("pre-position final output symlink");
        let ctx = ToolContext::new(root.clone());

        let output = CompilePdfTool
            .call(&ctx, json!({"path": "main.tex", "out_name": "result.pdf"}))
            .await
            .expect("run compile tool");

        assert!(!output.is_error, "{}", output.content);
        assert!(root.join("result.pdf").is_file(), "{}", output.content);
        assert!(!root.join("result.pdf").is_symlink());
        assert_eq!(
            std::fs::read_to_string(outside.join("generated-target")).unwrap(),
            "keep-generated"
        );
        assert_eq!(
            std::fs::read_to_string(outside.join("final-target")).unwrap(),
            "keep-final"
        );
        std::fs::remove_dir_all(root).expect("remove Tectonic test workspace");
        std::fs::remove_dir_all(outside).expect("remove outside test directory");
    }
}
