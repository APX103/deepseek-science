//! 文件工具：read_file / write_file / edit_file / list_files。
//!
//! 全部基于 workspace 根目录句柄逐组件访问，不做“校验路径后再打开”的 TOCTOU 操作。

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

// ---- read_file ----

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

pub struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn effect_class(&self, _args: &Value) -> crate::spec::ToolEffectClass {
        crate::spec::ToolEffectClass::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        read_spec()
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: ReadArgs = parse_args(&args)?;
        let workspace = ctx.secure_workspace()?;
        let path = a.path.clone();
        let content = tokio::task::spawn_blocking(move || {
            use std::io::Read;

            let mut file = workspace.open_file(&path)?;
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            Ok::<_, ToolError>(content)
        })
        .await
        .map_err(|error| ToolError::Other(format!("read task join failed: {error}")))??;
        // 按行 offset/limit 截取（1-based offset，与常见 CLI 语义一致）。
        let out = apply_offset_limit(&content, a.offset, a.limit);
        Ok(ToolOutput::ok(out))
    }
}

fn apply_offset_limit(content: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() {
        return content.to_string();
    }
    let offset = offset.unwrap_or(1).saturating_sub(1);
    let lines: Vec<&str> = content.lines().collect();
    let start = offset.min(lines.len());
    let take = limit.unwrap_or(lines.len() - start);
    let end = (start + take).min(lines.len());
    lines[start..end].join("\n")
}

fn read_spec() -> ToolSpec {
    ToolSpec {
        name: "read_file".into(),
        description: "Read a text file from the workspace. Paths are relative to the workspace root. `offset` is 1-based line number, `limit` is max lines to read.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to workspace." },
                "offset": { "type": "integer", "description": "1-based starting line (optional)." },
                "limit": { "type": "integer", "description": "Max lines to read (optional)." }
            },
            "required": ["path"]
        }),
    }
}

// ---- write_file ----

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        write_spec()
    }

    fn timeout(&self, _args: &Value) -> std::time::Duration {
        // A write may wait for one already-running Bash/Python call (up to 300s).
        std::time::Duration::from_secs(310)
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: WriteArgs = parse_args(&args)?;
        let _workspace_guard = ctx.lock_workspace_write().await;
        let workspace = ctx.secure_workspace()?;
        let path = a.path.clone();
        let content = a.content.clone();
        tokio::task::spawn_blocking(move || workspace.atomic_write(&path, content.as_bytes()))
            .await
            .map_err(|error| ToolError::Other(format!("write task join failed: {error}")))??;
        Ok(ToolOutput::ok(format!(
            "wrote {} ({} bytes)",
            a.path,
            a.content.len()
        )))
    }
}

fn write_spec() -> ToolSpec {
    ToolSpec {
        name: "write_file".into(),
        description: "Write text content to a file in the workspace (creates or overwrites). Paths are relative to the workspace root. Write is atomic. Success proves persistence only, not correctness: run a separate focused check before claiming the file is verified, and do not place a dependent run/read in the same tool batch.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to workspace." },
                "content": { "type": "string", "description": "Full file content to write." }
            },
            "required": ["path", "content"]
        }),
    }
}

// ---- edit_file ----

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: Option<bool>,
}

pub struct EditFileTool;

#[async_trait]
impl Tool for EditFileTool {
    fn spec(&self) -> ToolSpec {
        edit_spec()
    }

    fn timeout(&self, _args: &Value) -> std::time::Duration {
        // Includes the maximum workspace process-lock wait; the edit itself is local I/O.
        std::time::Duration::from_secs(310)
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: EditArgs = parse_args(&args)?;
        // Keep the same exclusive guard across read, replacement calculation, and atomic rename.
        // This prevents application-controlled Bash/Python/file calls from turning edit into a
        // lost update or installing a symlink between its two phases.
        let _workspace_guard = ctx.lock_workspace_write().await;
        let workspace = ctx.secure_workspace()?;
        let read_workspace = workspace.clone();
        let read_path = a.path.clone();
        let content = tokio::task::spawn_blocking(move || {
            use std::io::Read;

            let mut file = read_workspace.open_file(&read_path)?;
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            Ok::<_, ToolError>(content)
        })
        .await
        .map_err(|error| ToolError::Other(format!("edit read task join failed: {error}")))??;
        let replace_all = a.replace_all.unwrap_or(false);

        let count = if replace_all {
            content.matches(&a.old_string).count()
        } else {
            // 非 replace_all 时要求唯一。
            content.matches(&a.old_string).count()
        };
        if count == 0 {
            return Ok(ToolOutput::err(format!(
                "old_string not found in {}",
                a.path
            )));
        }
        if !replace_all && count > 1 {
            return Ok(ToolOutput::err(format!(
                "old_string appears {count} times in {}; set replace_all=true or make old_string unique",
                a.path
            )));
        }

        let new = if replace_all {
            content.replace(&a.old_string, &a.new_string)
        } else {
            content.replacen(&a.old_string, &a.new_string, 1)
        };
        let write_path = a.path.clone();
        tokio::task::spawn_blocking(move || workspace.atomic_write(&write_path, new.as_bytes()))
            .await
            .map_err(|error| ToolError::Other(format!("edit write task join failed: {error}")))??;
        Ok(ToolOutput::ok(format!(
            "edited {} ({} replacement{})",
            a.path,
            count,
            if count == 1 { "" } else { "s" }
        )))
    }
}

fn edit_spec() -> ToolSpec {
    ToolSpec {
        name: "edit_file".into(),
        description: "Replace a unique string in a workspace file. By default old_string must appear exactly once (error on 0 or >1). Set replace_all=true to replace all occurrences. After changing executable analysis code, run a separate minimal check before further edits or verification claims; do not batch the dependent run with this edit.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to workspace." },
                "old_string": { "type": "string", "description": "Exact text to find." },
                "new_string": { "type": "string", "description": "Replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)." }
            },
            "required": ["path", "old_string", "new_string"]
        }),
    }
}

// ---- list_files ----

#[derive(Deserialize)]
struct ListArgs {
    #[serde(default)]
    path: Option<String>,
}

pub struct ListFilesTool;

#[async_trait]
impl Tool for ListFilesTool {
    fn effect_class(&self, _args: &Value) -> crate::spec::ToolEffectClass {
        crate::spec::ToolEffectClass::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        list_spec()
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: ListArgs = parse_args(&args)?;
        let workspace = ctx.secure_workspace()?;
        let requested_path = a.path.clone();
        let entries =
            tokio::task::spawn_blocking(move || workspace.list(requested_path.as_deref(), 3))
                .await
                .map_err(|error| ToolError::Other(format!("list task join failed: {error}")))??;

        if entries.is_empty() {
            return Ok(ToolOutput::ok("(empty)".to_string()));
        }
        let lines = entries
            .into_iter()
            .map(|entry| {
                if entry.is_dir {
                    format!("{}/", entry.path)
                } else {
                    entry.path
                }
            })
            .collect::<Vec<_>>();
        Ok(ToolOutput::ok(lines.join("\n")))
    }
}

fn list_spec() -> ToolSpec {
    ToolSpec {
        name: "list_files".into(),
        description: "List files under a directory in the workspace (recursive, up to a few levels). Paths are relative to workspace. Defaults to workspace root.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory relative to workspace (optional, defaults to root)." }
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{EditFileTool, ListFilesTool, ReadFileTool, WriteFileTool};
    use crate::context::ToolContext;
    use crate::error::ToolError;
    use crate::spec::Tool;

    fn test_dir(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "dss-tools-files-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn list_files_treats_explicit_blank_path_as_workspace_root() {
        let root = test_dir("blank-root");
        std::fs::create_dir_all(&root).expect("create workspace");
        std::fs::write(root.join("paper.md"), "research notes").expect("write fixture");
        let ctx = ToolContext::new(root.clone());

        let output = ListFilesTool
            .call(&ctx, json!({"path": "   \t"}))
            .await
            .expect("list files");

        assert!(!output.is_error);
        assert_eq!(output.content, "paper.md");

        let dot_output = ListFilesTool
            .call(&ctx, json!({"path": "."}))
            .await
            .expect("list files from dot");
        assert_eq!(dot_output.content, "paper.md");
        std::fs::remove_dir_all(root).expect("remove workspace");
    }

    #[tokio::test]
    async fn list_files_hides_ephemeral_sandbox_runtime_directories() {
        let root = test_dir("hide-sandbox-runtime");
        std::fs::create_dir_all(root.join(".dss-sandbox-123-456/home"))
            .expect("create internal runtime fixture");
        std::fs::write(root.join("visible.md"), "visible").expect("write visible fixture");
        std::fs::write(
            root.join(".dss-sandbox-123-456/home/internal.txt"),
            "internal",
        )
        .expect("write internal fixture");
        let ctx = ToolContext::new(root.clone());

        let output = ListFilesTool
            .call(&ctx, json!({"path": ""}))
            .await
            .expect("list files");

        assert!(!output.is_error);
        assert_eq!(output.content, "visible.md");
        std::fs::remove_dir_all(root).expect("remove workspace");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_tools_do_not_follow_workspace_symlinks() {
        let root = test_dir("tool-symlink-root");
        let outside = test_dir("tool-symlink-outside");
        std::fs::create_dir_all(&root).expect("create workspace");
        std::fs::create_dir_all(&outside).expect("create outside directory");
        std::fs::write(outside.join("secret.txt"), "outside-secret")
            .expect("write outside fixture");
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("read.txt"))
            .expect("create read symlink");
        std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("edit.txt"))
            .expect("create edit symlink");
        std::os::unix::fs::symlink(&outside, root.join("redirect")).expect("create parent symlink");
        let ctx = ToolContext::new(root.clone());

        assert!(matches!(
            ReadFileTool.call(&ctx, json!({"path": "read.txt"})).await,
            Err(ToolError::PathEscape(_))
        ));
        assert!(matches!(
            EditFileTool
                .call(
                    &ctx,
                    json!({
                        "path": "edit.txt",
                        "old_string": "outside",
                        "new_string": "changed"
                    }),
                )
                .await,
            Err(ToolError::PathEscape(_))
        ));
        assert!(matches!(
            WriteFileTool
                .call(
                    &ctx,
                    json!({"path": "redirect/new.txt", "content": "blocked"}),
                )
                .await,
            Err(ToolError::PathEscape(_))
        ));

        assert_eq!(
            std::fs::read_to_string(outside.join("secret.txt")).unwrap(),
            "outside-secret"
        );
        assert!(!outside.join("new.txt").exists());

        std::fs::remove_dir_all(root).expect("remove workspace");
        std::fs::remove_dir_all(outside).expect("remove outside directory");
    }
}
