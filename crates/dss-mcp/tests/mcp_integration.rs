//! 集成测试：内嵌一个 MCP-兼容的 axum server，验证 MCPClient 全流程。
//!
//! 该 server 实现 streamable HTTP JSON-RPC：initialize / notifications/initialized /
//! tools/list / tools/call。响应用纯 JSON（也兼容 SSE 形态，由 client 解析）。

use axum::{extract::State, routing::post, Json, Router};
use dss_mcp::{MCPClient, MCPServerManager};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 共享计数：initialize 调用次数（验证 connect）。
#[derive(Default)]
struct ServerState {
    init_count: AtomicUsize,
}

async fn handle_jsonrpc(State(st): State<Arc<ServerState>>, Json(req): Json<Value>) -> Json<Value> {
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
    match method {
        "initialize" => {
            st.init_count.fetch_add(1, Ordering::Relaxed);
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.get("id").cloned().unwrap_or(Value::Null),
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "echo-mcp", "version": "0.0.1" },
                }
            }))
        }
        "notifications/initialized" => Json(json!({})), // notification 无响应
        "tools/list" => Json(json!({
            "jsonrpc": "2.0",
            "id": req.get("id").cloned().unwrap_or(Value::Null),
            "result": {
                "tools": [
                    {
                        "name": "echo",
                        "description": "Echo back the input text",
                        "inputSchema": {
                            "type": "object",
                            "properties": { "text": { "type": "string" } },
                            "required": ["text"]
                        }
                    }
                ]
            }
        })),
        "tools/call" => {
            let name = req
                .pointer("/params/name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let args = req
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or(json!({}));
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("(empty)");
            Json(json!({
                "jsonrpc": "2.0",
                "id": req.get("id").cloned().unwrap_or(Value::Null),
                "result": {
                    "content": [{ "type": "text", "text": format!("{name}: {text}") }],
                    "isError": false,
                }
            }))
        }
        _ => Json(json!({
            "jsonrpc": "2.0",
            "id": req.get("id").cloned().unwrap_or(Value::Null),
            "error": { "code": -32601, "message": "method not found" }
        })),
    }
}

async fn spawn_echo_server() -> (String, Arc<ServerState>) {
    let st = Arc::new(ServerState::default());
    let app = Router::new()
        .route("/", post(handle_jsonrpc))
        .with_state(st.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{addr}"), st)
}

#[tokio::test]
async fn client_connect_list_and_call() {
    let (url, _st) = spawn_echo_server().await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("connect");
    assert!(client.is_connected());

    let tools = client.list_tools().await.expect("list_tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");

    let result = client
        .call_tool("echo", json!({ "text": "hello" }))
        .await
        .expect("call_tool");
    assert!(result.contains("hello"));
}

#[tokio::test]
async fn manager_add_and_call() {
    let (url, _st) = spawn_echo_server().await;
    let mgr = MCPServerManager::new();
    assert!(mgr.add_server("echo", &url).await);
    // idempotent
    assert!(mgr.add_server("echo", &url).await);

    let all = mgr.list_all_tools().await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, "echo");

    let out = mgr
        .call_tool("echo", "echo", json!({ "text": "world" }))
        .await
        .unwrap();
    assert!(out.contains("world"));

    let info = mgr.server_info("echo").await.unwrap();
    assert!(info.connected);
    assert_eq!(info.tools.len(), 1);
}
