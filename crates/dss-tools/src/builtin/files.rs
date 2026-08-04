//! 文件工具：read_file / write_file / edit_file / list_files。
//!
//! 全部基于 workspace 做路径穿越防护（ToolContext::resolve_in_workspace）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::fs;

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

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
    fn spec(&self) -> ToolSpec {
        read_spec()
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: ReadArgs = parse_args(&args)?;
        let abs = ctx.resolve_in_workspace(&a.path)?;
        let meta = fs::metadata(&abs).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ToolError::NotFound(format!("{} (relative to workspace)", a.path))
            } else {
                ToolError::Io(e)
            }
        })?;
        if meta.is_dir() {
            return Ok(ToolOutput::err(format!(
                "{} is a directory, not a file",
                a.path
            )));
        }
        let content = fs::read_to_string(&abs).await?;
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

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: WriteArgs = parse_args(&args)?;
        let abs = ctx.resolve_in_workspace(&a.path)?;
        // 原子写：先写 .tmp 再 rename。
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).await?;
        }
        let tmp = abs.with_extension("dsswtmp");
        fs::write(&tmp, a.content.as_bytes()).await?;
        fs::rename(&tmp, &abs).await?;
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
        description: "Write text content to a file in the workspace (creates or overwrites). Paths are relative to the workspace root. Write is atomic.".into(),
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

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: EditArgs = parse_args(&args)?;
        let abs = ctx.resolve_in_workspace(&a.path)?;
        let content = fs::read_to_string(&abs).await?;
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
        // 原子写。
        let tmp = abs.with_extension("dssetmp");
        fs::write(&tmp, new.as_bytes()).await?;
        fs::rename(&tmp, &abs).await?;
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
        description: "Replace a unique string in a workspace file. By default old_string must appear exactly once (error on 0 or >1). Set replace_all=true to replace all occurrences.".into(),
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
    fn spec(&self) -> ToolSpec {
        list_spec()
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: ListArgs = parse_args(&args)?;
        let root = match a.path.as_deref() {
            None => ctx.workspace.clone(),
            Some(p) => ctx.resolve_in_workspace(p)?,
        };
        if !root.exists() {
            return Ok(ToolOutput::err(format!(
                "path not found: {}",
                a.path.unwrap()
            )));
        }
        // 递归列相对路径，最多 3 层深，排除常见噪声目录。
        // 用同步 std::fs 放进 spawn_blocking，避免 async 递归要 Box::pin。
        let root_cloned = root.clone();
        let entries = tokio::task::spawn_blocking(move || -> Result<Vec<String>, ToolError> {
            let mut entries: Vec<String> = Vec::new();
            collect_files(&root_cloned, &root_cloned, &mut entries, 0, 3)?;
            entries.sort();
            Ok(entries)
        })
        .await
        .map_err(|e| ToolError::Other(format!("list task join failed: {e}")))??;

        if entries.is_empty() {
            return Ok(ToolOutput::ok("(empty)".to_string()));
        }
        Ok(ToolOutput::ok(entries.join("\n")))
    }
}

fn collect_files(
    base: &std::path::Path,
    cur: &std::path::Path,
    out: &mut Vec<String>,
    depth: usize,
    max_depth: usize,
) -> Result<(), ToolError> {
    // 排除噪声目录。
    const SKIP: &[&str] = &[".git", "__pycache__", ".venv", "node_modules", "target"];
    if depth > max_depth {
        return Ok(());
    }
    let rd = std::fs::read_dir(cur)?;
    let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    for entry in rd {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        let ft = entry.file_type()?;
        let rel = cur
            .strip_prefix(base)
            .ok()
            .map(|p| p.to_path_buf())
            .unwrap_or_default()
            .join(&name)
            .to_string_lossy()
            .to_string();
        if ft.is_dir() {
            if SKIP.contains(&name.as_str()) {
                continue;
            }
            files.push(format!("{rel}/"));
            subdirs.push(entry.path());
        } else if ft.is_file() {
            files.push(rel);
        }
    }
    files.sort();
    for f in files {
        out.push(f);
    }
    for d in subdirs {
        collect_files(base, &d, out, depth + 1, max_depth)?;
    }
    Ok(())
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
