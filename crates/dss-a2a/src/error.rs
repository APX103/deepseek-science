use thiserror::Error;

/// Errors which occur before a complete A2A result envelope can be produced.
///
/// Display messages intentionally contain neither request headers nor credentials.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum A2aError {
    #[error("invalid A2A configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid A2A endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("Agent Card refresh failed: {0}")]
    CardRefresh(String),
    #[error("Agent Card is larger than the {limit}-byte limit")]
    CardTooLarge { limit: usize },
    #[error("invalid Agent Card: {0}")]
    InvalidCard(String),
    #[error("Agent Card has no supported A2A interface: {0}")]
    UnsupportedCard(String),
    #[error("Agent Card interface origin differs from the configured origin")]
    CrossOrigin,
    #[error("A2A response for {operation} is larger than the {limit}-byte limit")]
    ResponseTooLarge { operation: String, limit: usize },
    #[error("accepted A2A responses would exceed the {limit}-byte invocation limit")]
    TotalResponseTooLarge { limit: usize },
    #[error("A2A protocol error: {0}")]
    Protocol(String),
    #[error("A2A request timed out")]
    Timeout,
    #[error("A2A transport failed: {0}")]
    Transport(String),
}

impl A2aError {
    pub(crate) fn transport(error: impl std::fmt::Display) -> Self {
        // reqwest errors may contain a URL, which is fine after endpoint validation, but never
        // contain headers or request bodies. Keep the message short and deterministic for UI use.
        let text = error.to_string();
        Self::Transport(truncate(&text, 512))
    }

    pub(crate) fn card_refresh(error: impl std::fmt::Display) -> Self {
        Self::CardRefresh(truncate(&error.to_string(), 512))
    }
}

pub(crate) fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}
