//! Bounded, client-only A2A protocol support.
//!
//! This crate deliberately exposes no inbound A2A server. A configured endpoint is an explicit
//! user grant to contact that origin; each invocation first revalidates its Agent Card and then
//! performs one non-streaming send, submit, GetTask resume, or CancelTask operation. Task polling
//! remains bounded and every accepted response frame is retained.

mod card;
mod client;
mod error;
mod protocol;
mod result;
mod runtime;
mod types;

#[cfg(test)]
mod interop_tests;

pub use card::{
    parse_agent_card, resolve_agent_card_url, same_origin, CardRefreshKind, CardSkillSummary,
    CardSnapshot, CardSummary, ParsedAgentCard, SelectedInterface,
};
pub use client::{A2aClient, A2aClientOptions};
pub use error::A2aError;
pub use result::{
    A2aAgentRef, A2aRequestRecord, A2aTerminal, A2aToolResult, ResponseFrame, TerminalKind,
    A2A_RESULT_SCHEMA,
};
pub use runtime::{A2aRuntimeSnapshot, AgentRuntime, AgentRuntimeStatus};
pub use types::{
    stable_tool_name, validate_config, validate_configs, InvokeAction, InvokeRequest,
    ProtocolBinding, ProtocolVersion, MAX_AGENT_COUNT, MAX_CARD_BYTES, MAX_RESPONSE_BYTES,
    MAX_TOTAL_RESPONSE_BYTES,
};
