//! dss-llm: LLMClient trait + OpenAI 兼容客户端（Deepseek 特化）+ 消息适配。
//!
//! P1：纯文本对话（system/user/assistant 字符串消息）、流式 SSE 解析、
//! `reasoning_content` → Thinking 增量。
//! P2：function-calling（ChatRequest.tools / tool_calls 流式增量）。

pub mod client;
pub mod error;
pub mod types;

pub use client::OpenAICompatClient;
pub use error::LlmError;
pub use types::{
    BoxedEventStream, ChatMessage, ChatRequest, FunctionCall, LlmClient, LlmResponse, StreamEvent,
    ToolCall, ToolCallDelta, ToolDef, ToolFunction, Usage,
};
