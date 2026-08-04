use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use serde_json::json;

use crate::error::LlmError;
use crate::types::{BoxedEventStream, ChatRequest, LlmClient, LlmResponse, StreamEvent, Usage};

/// OpenAI 兼容 chat/completions 客户端（Deepseek 特化：`reasoning_content`）。
pub struct OpenAICompatClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

// api_key 不进入 Debug 输出。
impl fmt::Debug for OpenAICompatClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAICompatClient")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl OpenAICompatClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .expect("failed to build reqwest client");
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn build_body(&self, req: &ChatRequest, stream: bool) -> serde_json::Value {
        let mut body = json!({
            "model": req.model,
            "messages": req.messages,
        });
        if let Some(max_tokens) = req.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = req.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(tools) = &req.tools {
            body["tools"] = serde_json::to_value(tools).unwrap_or(json!([]));
            // 默认 auto：让模型自行决定是否调用工具。
            body["tool_choice"] = json!(req
                .tool_choice
                .clone()
                .unwrap_or_else(|| "auto".to_string()));
        }
        if stream {
            body["stream"] = json!(true);
            body["stream_options"] = json!({"include_usage": true});
        }
        body
    }

    async fn check_status(resp: reqwest::Response) -> Result<reqwest::Response, LlmError> {
        let status = resp.status();
        if !status.is_success() {
            let message = resp
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read error body>".to_string());
            // 上游错误体可能含敏感信息，截断到合理长度。
            let message = message.chars().take(500).collect();
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }
        Ok(resp)
    }
}

#[async_trait::async_trait]
impl LlmClient for OpenAICompatClient {
    async fn chat(&self, req: ChatRequest) -> Result<LlmResponse, LlmError> {
        let resp = self
            .http
            .post(self.chat_completions_url())
            .bearer_auth(&self.api_key)
            .json(&self.build_body(&req, false))
            .send()
            .await?;
        let resp = Self::check_status(resp).await?;
        let body: serde_json::Value = resp.json().await?;

        let choice = &body["choices"][0];
        let message = &choice["message"];
        let text = message["content"].as_str().unwrap_or_default().to_string();
        let thinking = message["reasoning_content"].as_str().map(|s| s.to_string());
        Ok(LlmResponse {
            text,
            thinking,
            usage: parse_usage(&body["usage"]),
            finish_reason: choice["finish_reason"].as_str().map(|s| s.to_string()),
            tool_calls: parse_tool_calls(&message["tool_calls"]),
        })
    }

    fn chat_stream(&self, req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        Box::pin(async move {
            let resp = self
                .http
                .post(self.chat_completions_url())
                .bearer_auth(&self.api_key)
                .json(&self.build_body(&req, true))
                .send()
                .await?;
            let resp = Self::check_status(resp).await?;
            Ok(sse_event_stream(Box::pin(resp.bytes_stream())))
        })
    }

    fn model(&self) -> &str {
        &self.model
    }
}

fn parse_usage(v: &serde_json::Value) -> Usage {
    Usage {
        input_tokens: v["prompt_tokens"].as_u64().unwrap_or(0) as u32,
        output_tokens: v["completion_tokens"].as_u64().unwrap_or(0) as u32,
    }
}

/// 解析非流式响应 message.tool_calls 数组。
fn parse_tool_calls(v: &serde_json::Value) -> Vec<crate::types::ToolCall> {
    let Some(arr) = v.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|tc| {
            let id = tc["id"].as_str()?.to_string();
            let fn_obj = &tc["function"];
            let name = fn_obj["name"].as_str()?.to_string();
            let arguments = fn_obj["arguments"].as_str().unwrap_or("").to_string();
            Some(crate::types::ToolCall {
                id,
                kind: "function".to_string(),
                function: crate::types::FunctionCall { name, arguments },
            })
        })
        .collect()
}

struct SseState {
    chunks: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    /// 原始字节缓冲；按 `\n`（ASCII，不会出现在多字节字符内部）切行，
    /// 避免 UTF-8 字符跨 chunk 被截断。
    buf: Vec<u8>,
    /// 已从完整行解析出、待 yield 的事件。
    pending: VecDeque<StreamEvent>,
    /// 是否已见 `[DONE]`。
    done: bool,
}

/// 把字节流解析为 OpenAI SSE 事件流。
fn sse_event_stream(
    chunks: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
) -> BoxedEventStream {
    let state = SseState {
        chunks,
        buf: Vec::new(),
        pending: VecDeque::new(),
        done: false,
    };

    Box::pin(futures::stream::unfold(state, |mut state| async move {
        loop {
            if let Some(ev) = state.pending.pop_front() {
                return Some((Ok(ev), state));
            }
            if state.done {
                return None;
            }

            // 取一整行（含 \n 前的内容）。
            if let Some(pos) = state.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = state.buf.drain(..=pos).collect();
                match parse_sse_line(&line) {
                    LineOutcome::Skip => continue,
                    LineOutcome::Done => {
                        state.done = true;
                        continue;
                    }
                    LineOutcome::Events(evs) => {
                        state.pending.extend(evs);
                        continue;
                    }
                    LineOutcome::Error(e) => return Some((Err(e), state)),
                }
            }

            match state.chunks.next().await {
                Some(Ok(bytes)) => state.buf.extend_from_slice(&bytes),
                Some(Err(e)) => return Some((Err(LlmError::Transport(e)), state)),
                // 上游 EOF：缓冲里无换行的残余按一行处理一次后终止。
                None => {
                    if state.buf.is_empty() {
                        return None;
                    }
                    let line = std::mem::take(&mut state.buf);
                    state.done = true;
                    match parse_sse_line(&line) {
                        LineOutcome::Events(evs) => state.pending.extend(evs),
                        LineOutcome::Error(e) => return Some((Err(e), state)),
                        _ => {}
                    }
                }
            }
        }
    }))
}

enum LineOutcome {
    Skip,
    Done,
    Events(Vec<StreamEvent>),
    Error(LlmError),
}

/// 解析一行 SSE：`data: {json}` 或 `data: [DONE]`。
fn parse_sse_line(line: &[u8]) -> LineOutcome {
    let line = String::from_utf8_lossy(line);
    let line = line.trim_end_matches(['\n', '\r']);
    if line.is_empty() || line.starts_with(':') {
        return LineOutcome::Skip;
    }
    let Some(payload) = line.strip_prefix("data:") else {
        return LineOutcome::Skip;
    };
    let payload = payload.trim_start();
    if payload == "[DONE]" {
        return LineOutcome::Done;
    }

    let v: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => return LineOutcome::Error(LlmError::Stream(format!("invalid SSE JSON: {e}"))),
    };

    let mut events = Vec::new();

    // usage 末包（choices 为空、usage 非 null）。
    if v["usage"].is_object() {
        events.push(StreamEvent::Usage(parse_usage(&v["usage"])));
    }

    if let Some(choice) = v["choices"].as_array().and_then(|c| c.first()) {
        let delta = &choice["delta"];
        if let Some(t) = delta["reasoning_content"].as_str() {
            if !t.is_empty() {
                events.push(StreamEvent::Thinking(t.to_string()));
            }
        }
        if let Some(t) = delta["content"].as_str() {
            if !t.is_empty() {
                events.push(StreamEvent::Text(t.to_string()));
            }
        }
        // 流式 tool_calls：每个 delta 带一个 tool_calls 数组，按 index 累积。
        if let Some(arr) = delta["tool_calls"].as_array() {
            for tc in arr {
                let index = tc["index"].as_u64().unwrap_or(0) as u32;
                let id = tc["id"].as_str().map(|s| s.to_string());
                let name = tc["function"]["name"].as_str().map(|s| s.to_string());
                let arguments = tc["function"]["arguments"].as_str().map(|s| s.to_string());
                events.push(StreamEvent::ToolCallDelta(crate::types::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments,
                }));
            }
        }
        if let Some(reason) = choice["finish_reason"].as_str() {
            events.push(StreamEvent::Finish {
                reason: Some(reason.to_string()),
            });
        }
    }

    LineOutcome::Events(events)
}
