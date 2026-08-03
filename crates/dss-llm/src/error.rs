use thiserror::Error;

#[derive(Error, Debug)]
pub enum LlmError {
    #[error("LLM not configured: {0}")]
    NotConfigured(String),

    #[error("LLM API error (HTTP {status}): {message}")]
    Api { status: u16, message: String },

    #[error("LLM stream error: {0}")]
    Stream(String),

    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),
}
