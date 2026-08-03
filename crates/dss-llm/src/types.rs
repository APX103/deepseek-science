use std::pin::Pin;

use futures::future::BoxFuture;
use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;

/// 流式事件流（`Stream<Item = Result<StreamEvent, LlmError>>`）。
pub type BoxedEventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>;

/// OpenAI 风格消息。
///
/// P1 为纯文本（`role`+`content`）；P2 扩展支持 function-calling：
/// assistant 携带 `tool_calls`、role=tool 携带 `tool_call_id`。
/// 完整 content blocks（Anthropic 风格）适配留待 P3 落库时。
///
/// 序列化策略：空字段用 `skip_serializing_if` 抑制，保证发给 DeepSeek 的
/// payload 与 OpenAI 协议一致（assistant 带 tool_calls 时 content 为 null）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    /// 文本内容；assistant 带 tool_calls 时可为 None（序列化为 null）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// assistant 的工具调用（role=assistant 才有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// role=tool 时配对的上一次 assistant tool_call 的 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// role=tool 时可选的工具名（OpenAI 协议非必需，但便于日志）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }
    /// assistant 携带工具调用（content 为空）。
    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
        }
    }
    /// role=tool 的工具结果消息。
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>, name: Option<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name,
        }
    }
}

/// OpenAI function-calling 的 `tool_calls` 单项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    /// 固定 `"function"`。
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

impl ToolCall {
    pub fn function(id: impl Into<String>, name: impl Into<String>, arguments: String) -> Self {
        Self {
            id: id.into(),
            kind: "function".to_string(),
            function: FunctionCall {
                name: name.into(),
                arguments,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// arguments 是 JSON 字符串（OpenAI 协议要求字符串）。
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// 可用工具定义（function-calling）。
    pub tools: Option<Vec<ToolDef>>,
    /// `"auto"` / `"none"` / 指定；None 时不发该字段。
    pub tool_choice: Option<String>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        }
    }
}

/// 传给 LLM 的工具定义（OpenAI tools 数组单项）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    /// 固定 `"function"`。
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

impl ToolDef {
    pub fn function(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            kind: "function".to_string(),
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    /// JSON Schema 对象。
    pub parameters: serde_json::Value,
}

/// token 用量（OpenAI `usage.prompt_tokens` / `completion_tokens`）。
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// 流式增量事件。
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// DeepSeek `reasoning_content` 增量。
    Thinking(String),
    /// 正文 `content` 增量。
    Text(String),
    /// 工具调用增量（OpenAI 流式 `delta.tool_calls[]`，按 index 累积）。
    ToolCallDelta(ToolCallDelta),
    /// 用量（`stream_options.include_usage` 的末包）。
    Usage(Usage),
    /// 结束（finish_reason；`[DONE]` 前最后一个 chunk 给出）。
    Finish { reason: Option<String> },
}

/// OpenAI 流式 tool_calls 的单个增量（每个 index 一路流）。
#[derive(Debug, Clone, Default)]
pub struct ToolCallDelta {
    /// 在最终 ToolCall 数组里的位置（Runner 按 index 累积）。
    pub index: u32,
    /// 首包带 id。
    pub id: Option<String>,
    /// 首包带 function.name。
    pub name: Option<String>,
    /// arguments 是分片字符串，逐片拼接。
    pub arguments: Option<String>,
}

/// 非流式响应。
#[derive(Debug, Clone, Default)]
pub struct LlmResponse {
    pub text: String,
    pub thinking: Option<String>,
    pub usage: Usage,
    pub finish_reason: Option<String>,
    /// 非流式响应里的工具调用（`choices[0].message.tool_calls`）。
    pub tool_calls: Vec<ToolCall>,
}

#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> Result<LlmResponse, LlmError>;

    /// 流式对话。返回事件流；`[DONE]` 或流结束后自然终止。
    /// 注意：**已 yield 内容后不重试**（调用方负责错误处理）。
    fn chat_stream(
        &self,
        req: ChatRequest,
    ) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>>;

    fn model(&self) -> &str;
}
