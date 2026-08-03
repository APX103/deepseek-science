use dss_llm::Usage;
use dss_tools::PendingAsk;
use serde::Serialize;
use serde_json::Value;

use crate::frame::FrameStatus;

/// `complete.kind` 取值（api-contract）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompleteKind {
    Natural,
    Awaiting,
    MaxIters,
    Error,
    Cancelled,
}

/// SSE 事件（`data: {json}`，serde tag=type 判别）。
///
/// P2：新增 tool_calls / tool_results，并在 complete 携带 pending_ask。
/// 字段名严格对齐 docs/api-contract.md 的 SSE 事件格式。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Start {
        frame_id: String,
        task_summary: String,
    },
    Iteration {
        n: u32,
    },
    Thinking {
        text: String,
    },
    Text {
        text: String,
    },
    /// 一次 assistant 回复里的工具调用批次（前端按 call.id 去重追加）。
    ToolCalls {
        calls: Vec<ToolCallView>,
    },
    /// 与上一批 tool_calls 配对的执行结果。
    ToolResults {
        results: Vec<ToolResultView>,
    },
    Complete {
        kind: CompleteKind,
        final_text: String,
        awaiting: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        usage: Usage,
        iterations: u32,
        frame_status: FrameStatus,
        /// ask_user 触发的挂起提问（kind=awaiting 时才有）。
        #[serde(skip_serializing_if = "Option::is_none")]
        pending_ask: Option<PendingAsk>,
    },
    Error {
        message: String,
    },
}

/// tool_calls 事件里的单项（对齐 api-contract: {id,name,input}）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallView {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// tool_results 事件里的单项（对齐 api-contract: {tool_use_id,content,is_error}）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolResultView {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
}

impl From<dss_tools::ToolResult> for ToolResultView {
    fn from(r: dss_tools::ToolResult) -> Self {
        ToolResultView {
            tool_use_id: r.tool_use_id,
            content: r.content,
            is_error: r.is_error,
        }
    }
}
