use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{CardSnapshot, InvokeAction, ProtocolBinding, ProtocolVersion};

pub const A2A_RESULT_SCHEMA: &str = "dss.a2a.tool-result.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aAgentRef {
    pub config_id: String,
    pub display_name: String,
    pub configured_endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aRequestRecord {
    pub invocation_id: String,
    #[serde(default)]
    pub action: InvokeAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    pub task: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseFrame {
    pub sequence: u32,
    pub operation: String,
    pub received_at: String,
    pub http_status: u16,
    pub protocol_version: ProtocolVersion,
    pub binding: ProtocolBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// The complete bounded response body. Invalid UTF-8/JSON is represented by an explicit
    /// lossless base64 wrapper instead of being silently discarded.
    pub payload: Value,
    pub wire_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalKind {
    Message,
    Task,
    /// The local submit operation succeeded and returned a resumable, non-terminal Task handle.
    TaskPending,
    /// The remote Task paused for user input or authentication and can be continued in place.
    TaskInterrupted,
    CardRefreshError,
    TransportError,
    ProtocolError,
    Timeout,
    SizeLimit,
    OutcomeUnknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aTerminal {
    pub kind: TerminalKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl A2aTerminal {
    pub(crate) fn error(kind: TerminalKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            task_id: None,
            context_id: None,
            state: None,
            success: false,
            error: Some(message.into()),
        }
    }

    /// `INPUT_REQUIRED` and `AUTH_REQUIRED` are resumable Task states, not failed operations.
    /// Recognizing the legacy `kind=task, success=false` representation keeps persisted v1
    /// envelopes created by earlier application builds usable after an upgrade.
    pub fn is_resumable_interruption(&self) -> bool {
        self.kind == TerminalKind::TaskInterrupted
            || (self.kind == TerminalKind::Task
                && matches!(
                    self.state.as_deref(),
                    Some(
                        "TASK_STATE_INPUT_REQUIRED"
                            | "TASK_STATE_AUTH_REQUIRED"
                            | "input-required"
                            | "auth-required"
                    )
                ))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct A2aToolResult {
    pub schema: String,
    pub agent: A2aAgentRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<CardSnapshot>,
    pub request: A2aRequestRecord,
    pub responses: Vec<ResponseFrame>,
    pub terminal: A2aTerminal,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl A2aToolResult {
    pub fn is_error(&self) -> bool {
        !self.terminal.success && !self.terminal.is_resumable_interruption()
    }

    pub fn to_json(&self) -> String {
        // All fields are JSON-safe values. Serialization can only fail on a custom serializer,
        // which this result deliberately does not contain.
        serde_json::to_string(self).expect("A2aToolResult is always serializable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(kind: TerminalKind, state: &str, success: bool) -> A2aTerminal {
        A2aTerminal {
            kind,
            task_id: Some("task-1".into()),
            context_id: Some("context-1".into()),
            state: Some(state.into()),
            success,
            error: None,
        }
    }

    #[test]
    fn current_interruption_is_resumable_and_not_an_error() {
        for state in [
            "TASK_STATE_INPUT_REQUIRED",
            "TASK_STATE_AUTH_REQUIRED",
            "input-required",
            "auth-required",
        ] {
            let terminal = terminal(TerminalKind::TaskInterrupted, state, true);
            assert!(terminal.is_resumable_interruption());
        }
    }

    #[test]
    fn legacy_failed_task_interruption_is_still_recognized() {
        let legacy_terminal = terminal(TerminalKind::Task, "TASK_STATE_INPUT_REQUIRED", false);
        assert!(legacy_terminal.is_resumable_interruption());

        let serialized = serde_json::to_string(&legacy_terminal).unwrap();
        let restored: A2aTerminal = serde_json::from_str(&serialized).unwrap();
        assert!(restored.is_resumable_interruption());

        let protocol_error = terminal(
            TerminalKind::ProtocolError,
            "TASK_STATE_INPUT_REQUIRED",
            false,
        );
        assert!(!protocol_error.is_resumable_interruption());
    }
}
