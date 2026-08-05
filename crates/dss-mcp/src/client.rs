//! MCPClient：streamable HTTP + SSE 的 JSON-RPC 客户端（modules.md §6）。
//!
//! 协议：initialize（protocolVersion 2024-11-05）→ 捕获 Mcp-Session-Id →
//! notifications/initialized → tools/list / tools/call。
//! 响应解析兼容 text/event-stream（聚合 data: 取最后 result/error 对象）与纯 JSON。

use std::time::Duration;

use serde_json::{json, Value};
use thiserror::Error;

const PROTOCOL_VERSION: &str = "2024-11-05";

#[derive(Debug, Error)]
pub enum McpError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("transport: {0}")]
    Transport(String),
    #[error("rpc error ({code}): {message}")]
    Rpc { code: i64, message: String },
    #[error("invalid response: {0}")]
    Invalid(String),
}

/// 一个 MCP 工具定义。
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    /// inputSchema（JSON Schema 对象）。
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// MCP 客户端。connect 后持有 base_url + Mcp-Session-Id（Mutex，便于 &self 复用）。
pub struct MCPClient {
    http: reqwest::Client,
    base_url: String,
    session_id: std::sync::Mutex<Option<String>>,
    /// connect() 成功即视为已连接（session_id 可选：部分 server 不下发 Mcp-Session-Id）。
    connected: std::sync::atomic::AtomicBool,
    next_id: std::sync::atomic::AtomicU64,
}

impl MCPClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            http,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            session_id: std::sync::Mutex::new(None),
            connected: std::sync::atomic::AtomicBool::new(false),
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn req_builder(&self) -> reqwest::RequestBuilder {
        let mut r = self
            .http
            .post(&self.base_url)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Ok(sid) = self.session_id.lock() {
            if let Some(s) = sid.as_ref() {
                r = r.header("Mcp-Session-Id", s);
            }
        }
        r
    }

    /// 发一个 JSON-RPC 请求（带 id），解析 result。
    async fn rpc(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id();
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let resp = self.req_builder().json(&body).send().await?;
        // 捕获 session id（initialize 响应带；其它幂等设）。
        if let Some(sid) = resp
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
        {
            if let Ok(mut guard) = self.session_id.lock() {
                *guard = Some(sid.to_string());
            }
        }
        let ct = resp.headers().get(reqwest::header::CONTENT_TYPE).cloned();
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| McpError::Transport(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(McpError::Transport(format!(
                "HTTP {status}: {}",
                truncate(&text, 300)
            )));
        }
        let value = parse_response(&text, ct.as_ref().and_then(|v| v.to_str().ok()))?;
        let ResponsePayload::Result(r) = value;
        Ok(r)
    }

    /// 发一个 notification（无 id，无返回值）。
    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let _ = self.req_builder().json(&body).send().await?;
        Ok(())
    }

    /// initialize → 捕获 session id → notifications/initialized。
    pub async fn connect(&self) -> Result<(), McpError> {
        self.rpc(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "deepseek-science", "version": "0.1.0" },
            }),
        )
        .await?;
        self.notify("notifications/initialized", json!({})).await?;
        self.connected
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// tools/list → Vec<McpTool>。
    pub async fn list_tools(&self) -> Result<Vec<McpTool>, McpError> {
        let result = self.rpc("tools/list", json!({})).await?;
        let tools_val = result.get("tools").cloned().unwrap_or(Value::Array(vec![]));
        let tools = match tools_val {
            Value::Array(arr) => arr,
            _ => {
                return Err(McpError::Invalid(
                    "tools/list result.tools not array".into(),
                ))
            }
        };
        let mut out = Vec::new();
        for t in tools {
            let name = t
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = t.get("inputSchema").cloned().unwrap_or(json!({}));
            out.push(McpTool {
                name,
                description,
                input_schema,
            });
        }
        Ok(out)
    }

    /// tools/call → result.content（简化：聚合 text）。
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<String, McpError> {
        let result = self
            .rpc(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let content = result
            .get("content")
            .cloned()
            .unwrap_or(Value::Array(vec![]));
        // content 是 [{type:"text", text:"..."}, ...]，聚合 text。
        let mut text_parts = Vec::new();
        if let Value::Array(arr) = content {
            for item in arr {
                if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                    if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                        text_parts.push(t.to_string());
                    }
                }
            }
        }
        Ok(text_parts.join("\n"))
    }
}

/// 响应负载：result（错误已转 McpError::Rpc 返回）。
pub enum ResponsePayload {
    Result(Value),
}

/// 解析响应文本：兼容纯 JSON 与 text/event-stream（聚合 data: 行，取最后一个 JSON-RPC result/error 对象）。
pub fn parse_response(text: &str, content_type: Option<&str>) -> Result<ResponsePayload, McpError> {
    let is_sse = content_type
        .map(|c| c.contains("text/event-stream"))
        .unwrap_or(false);
    let candidate = if is_sse {
        // 聚合 data: 行，取最后一个能解析为 JSON-RPC 的对象。
        let mut last: Option<Value> = None;
        for line in text.lines() {
            if let Some(payload) = line.strip_prefix("data:") {
                let payload = payload.trim();
                if payload.is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    last = Some(v);
                }
            }
        }
        last.ok_or_else(|| McpError::Invalid("SSE response had no parseable data".into()))?
    } else {
        serde_json::from_str::<Value>(text)
            .map_err(|e| McpError::Invalid(format!("non-JSON body: {e}")))?
    };

    let obj = candidate
        .as_object()
        .ok_or_else(|| McpError::Invalid("response not an object".into()))?;
    if let Some(err) = obj.get("error") {
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        let message = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        return Err(McpError::Rpc { code, message });
    }
    if let Some(result) = obj.get("result") {
        return Ok(ResponsePayload::Result(result.clone()));
    }
    // 无 result 无 error（如 notification ack）→ 视为空 result。
    Ok(ResponsePayload::Result(Value::Null))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_json_result() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        let p = parse_response(body, Some("application/json")).unwrap();
        let ResponsePayload::Result(v) = p;
        assert_eq!(v["tools"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn parse_sse_takes_last_data() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"a\":1}}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"a\":2}}\n\n";
        let p = parse_response(body, Some("text/event-stream")).unwrap();
        let ResponsePayload::Result(v) = p;
        assert_eq!(v["a"], 2);
    }

    #[test]
    fn parse_error() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"bad"}}"#;
        match parse_response(body, Some("application/json")) {
            Err(McpError::Rpc { code, message }) => {
                assert_eq!(code, -32600);
                assert_eq!(message, "bad");
            }
            _ => panic!("expected rpc error"),
        }
    }
}
