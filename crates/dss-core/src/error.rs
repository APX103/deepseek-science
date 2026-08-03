use std::path::PathBuf;

/// 领域错误类型。API 层负责将其映射为 HTTP 状态码 + JSON `{error}`。
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("home directory not found (HOME env var unset)")]
    NoHome,

    #[error("failed to parse config file {path}: {message}")]
    ConfigParse { path: PathBuf, message: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
