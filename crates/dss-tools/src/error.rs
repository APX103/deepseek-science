/// 工具错误。Router 会把它转成 `is_error=true` 的 ToolResult。
#[derive(thiserror::Error, Debug)]
pub enum ToolError {
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("path escapes workspace: {0}")]
    PathEscape(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("timed out after {0}s")]
    Timeout(u64),

    #[error("{0}")]
    Other(String),
}

impl ToolError {
    /// 任意 anyhow 错误转 Other。
    pub fn other(e: impl std::fmt::Display) -> Self {
        ToolError::Other(e.to_string())
    }
}
