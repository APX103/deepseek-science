//! Integration coverage for the bounded MCP Streamable-HTTP client.

use axum::{
    body::Body,
    extract::State,
    http::{header::CONTENT_TYPE, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use dss_mcp::{MCPClient, MCPServerManager, McpError};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const SESSION_ID: &str = "fixture-session";
const MULTILINE_RESOURCE_DESCRIPTION: &str =
    "# Registry fixture\r\n\n- JSON descriptor\n\t- preserves Markdown whitespace";
const MULTILINE_RESOURCE_TEXT: &str =
    "{\n  \"kind\": \"a2a\",\n  \"notes\": \"# Fixture\\n\\n- Markdown\"\n}\r\n\t";

#[derive(Debug, Clone, Copy, Default)]
enum FixtureMode {
    #[default]
    Standard,
    ToolsOnly,
    TooManyTools,
    OversizedToolCatalog,
    MixedInvalidToolSchemas,
    ToolReportedError,
    UnsupportedToolContent,
    EmptyToolContent,
    CursorCycle,
    PageLimit,
    AggregateResourceLimit,
    SlowResourceRead,
    MismatchedReadUri,
    MalformedResponse,
    MalformedJsonRpc,
    MalformedContent,
    OversizedResponse,
    OversizedContent,
    InvalidBlob,
    RpcError,
    HttpError,
    InitializedFailure,
    InitializedBody,
}

#[derive(Default)]
struct ServerState {
    mode: FixtureMode,
    init_count: AtomicUsize,
    initialized_count: AtomicUsize,
    session_request_count: AtomicUsize,
    resource_list_count: AtomicUsize,
    resource_read_count: AtomicUsize,
}

impl ServerState {
    fn new(mode: FixtureMode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }
}

async fn handle_jsonrpc(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");

    if method != "initialize" {
        if headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            != Some(SESSION_ID)
        {
            return (StatusCode::UNAUTHORIZED, "missing MCP session").into_response();
        }
        state.session_request_count.fetch_add(1, Ordering::Relaxed);
    }

    match method {
        "initialize" => {
            state.init_count.fetch_add(1, Ordering::Relaxed);
            let capabilities = if matches!(state.mode, FixtureMode::ToolsOnly) {
                json!({"tools": {}})
            } else {
                json!({"tools": {}, "resources": {"listChanged": true}})
            };
            (
                [("mcp-session-id", SESSION_ID)],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id(&request),
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": capabilities,
                        "serverInfo": {"name": "echo-mcp", "version": "0.0.1"}
                    }
                })),
            )
                .into_response()
        }
        "notifications/initialized" => {
            state.initialized_count.fetch_add(1, Ordering::Relaxed);
            if matches!(state.mode, FixtureMode::InitializedFailure) {
                (StatusCode::INTERNAL_SERVER_ERROR, "initialization rejected").into_response()
            } else if matches!(state.mode, FixtureMode::InitializedBody) {
                (StatusCode::ACCEPTED, "{}").into_response()
            } else {
                StatusCode::ACCEPTED.into_response()
            }
        }
        "tools/list" => {
            let tools = match state.mode {
                FixtureMode::TooManyTools => (0..31)
                    .map(|index| {
                        json!({
                            "name": format!("tool-{index}"),
                            "description": "fixture",
                            "inputSchema": {"type": "object"}
                        })
                    })
                    .collect(),
                FixtureMode::OversizedToolCatalog => vec![json!({
                    "name": "oversized-schema",
                    "description": "fixture",
                    "inputSchema": {
                        "type": "object",
                        "description": "x".repeat(256 * 1024)
                    }
                })],
                FixtureMode::MixedInvalidToolSchemas => vec![
                    json!({
                        "name": "safe-read",
                        "description": "# Safe read\r\n\n- valid tool survives invalid neighbors\n\t- tabbed",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"query": {"type": "string"}},
                            "required": ["query"],
                            "additionalProperties": false
                        },
                        "annotations": {
                            "readOnlyHint": true,
                            "destructiveHint": false
                        }
                    }),
                    json!({
                        "name": "ref-schema",
                        "inputSchema": {"$ref": "#/$defs/Args"}
                    }),
                    json!({
                        "name": "wrong-top-level",
                        "inputSchema": {"type": "array", "items": {"type": "string"}}
                    }),
                    json!({
                        "name": "bad-required",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"known": {"type": "string"}},
                            "required": ["missing"]
                        }
                    }),
                    json!({
                        "name": "bad-properties",
                        "inputSchema": {"type": "object", "properties": []}
                    }),
                ],
                _ => vec![json!({
                    "name": "echo",
                    "description": "Echo back the input text",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"text": {"type": "string"}},
                        "required": ["text"]
                    }
                })],
            };
            rpc_result(&request, json!({"tools": tools}))
        }
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let text = request
                .pointer("/params/arguments/text")
                .and_then(Value::as_str)
                .unwrap_or("(empty)");
            let result = match state.mode {
                FixtureMode::ToolReportedError => json!({
                    "content": [{"type": "text", "text": "remote failure"}],
                    "isError": true
                }),
                FixtureMode::UnsupportedToolContent => json!({
                    "content": [{"type": "image", "data": "aW1hZ2U="}],
                    "isError": false
                }),
                FixtureMode::EmptyToolContent => json!({
                    "content": [],
                    "isError": false
                }),
                _ => json!({
                    "content": [{"type": "text", "text": format!("{name}: {text}")}],
                    "isError": false
                }),
            };
            rpc_result(&request, result)
        }
        "resources/list" => handle_resources_list(&state, &request),
        "resources/read" => {
            state.resource_read_count.fetch_add(1, Ordering::Relaxed);
            if matches!(state.mode, FixtureMode::SlowResourceRead) {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            handle_resource_read(&state, &request)
        }
        _ => rpc_error(&request, -32601, "method not found"),
    }
}

fn handle_resources_list(state: &ServerState, request: &Value) -> Response {
    let page = state.resource_list_count.fetch_add(1, Ordering::Relaxed) + 1;
    match state.mode {
        FixtureMode::CursorCycle => {
            rpc_result(request, json!({"resources": [], "nextCursor": "repeat"}))
        }
        FixtureMode::PageLimit => rpc_result(
            request,
            json!({"resources": [], "nextCursor": format!("page-{page}")}),
        ),
        FixtureMode::AggregateResourceLimit => {
            let resources: Vec<_> = (0..43)
                .map(|index| {
                    json!({
                        "uri": format!("agent://large-{page}-{index}"),
                        "name": format!("large-{page}-{index}"),
                        "description": "x".repeat(16 * 1024),
                        "mimeType": "application/json"
                    })
                })
                .collect();
            if page < 3 {
                rpc_result(
                    request,
                    json!({
                        "resources": resources,
                        "nextCursor": format!("aggregate-page-{}", page + 1)
                    }),
                )
            } else {
                rpc_result(request, json!({"resources": resources}))
            }
        }
        FixtureMode::MalformedResponse => (
            StatusCode::OK,
            [(CONTENT_TYPE, "application/json")],
            "{not-json",
        )
            .into_response(),
        FixtureMode::MalformedJsonRpc => Json(json!({
            "jsonrpc": "1.0",
            "id": rpc_id(request),
            "result": {"resources": []}
        }))
        .into_response(),
        FixtureMode::OversizedResponse => Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from("x".repeat(2 * 1024 * 1024 + 1)))
            .expect("oversized fixture response"),
        FixtureMode::RpcError => rpc_error(request, -32001, "registry unavailable"),
        FixtureMode::HttpError => (StatusCode::FORBIDDEN, "RBAC: access denied").into_response(),
        _ => {
            let cursor = request.pointer("/params/cursor").and_then(Value::as_str);
            let result = if cursor == Some("page-2") {
                json!({
                    "resources": [{
                        "uri": "agent://second",
                        "name": "second",
                        "mimeType": "application/json"
                    }]
                })
            } else {
                json!({
                    "resources": [{
                        "uri": "agent://first",
                        "name": "first",
                        "title": "First Agent",
                        "description": MULTILINE_RESOURCE_DESCRIPTION,
                        "mimeType": "application/json",
                        "size": 42
                    }],
                    "nextCursor": "page-2"
                })
            };
            rpc_result(request, result)
        }
    }
}

fn handle_resource_read(state: &ServerState, request: &Value) -> Response {
    let requested_uri = request
        .pointer("/params/uri")
        .and_then(Value::as_str)
        .unwrap_or("");
    let contents = match state.mode {
        FixtureMode::MismatchedReadUri => json!([{
            "uri": "agent://different",
            "mimeType": "application/json",
            "text": "{}"
        }]),
        FixtureMode::MalformedContent => json!([{
            "uri": requested_uri,
            "text": "text",
            "blob": "dGV4dA=="
        }]),
        FixtureMode::OversizedContent => json!([{
            "uri": requested_uri,
            "text": "x".repeat(1024 * 1024 + 1)
        }]),
        FixtureMode::InvalidBlob => json!([{
            "uri": requested_uri,
            "blob": "***not-base64***"
        }]),
        _ => json!([
            {
                "uri": requested_uri,
                "mimeType": "application/json",
                "text": MULTILINE_RESOURCE_TEXT
            },
            {
                "uri": requested_uri,
                "mimeType": "application/octet-stream",
                "blob": "Zml4dHVyZQ=="
            }
        ]),
    };
    rpc_result(request, json!({"contents": contents}))
}

fn rpc_id(request: &Value) -> Value {
    request.get("id").cloned().unwrap_or(Value::Null)
}

fn rpc_result(request: &Value, result: Value) -> Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": rpc_id(request),
        "result": result
    }))
    .into_response()
}

fn rpc_error(request: &Value, code: i64, message: &str) -> Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": rpc_id(request),
        "error": {"code": code, "message": message}
    }))
    .into_response()
}

async fn spawn_server(mode: FixtureMode) -> (String, Arc<ServerState>) {
    let state = Arc::new(ServerState::new(mode));
    let app = Router::new()
        .route("/", post(handle_jsonrpc))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fixture");
    let address = listener.local_addr().expect("fixture address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve fixture");
    });
    (format!("http://{address}"), state)
}

#[tokio::test]
async fn client_connect_list_and_call_propagates_session() {
    let (url, state) = spawn_server(FixtureMode::Standard).await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("connect");
    assert!(client.is_connected());
    assert_eq!(state.initialized_count.load(Ordering::Relaxed), 1);

    let tools = client.list_tools().await.expect("list_tools");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");

    let result = client
        .call_tool("echo", json!({"text": "hello"}))
        .await
        .expect("call_tool");
    assert!(result.contains("hello"));
    assert_eq!(state.session_request_count.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn tools_list_rejects_excessive_count_before_mounting() {
    let (url, _state) = spawn_server(FixtureMode::TooManyTools).await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("connect");
    assert!(matches!(
        client.list_tools().await,
        Err(McpError::ToolCountExceeded { limit: 30 })
    ));
}

#[tokio::test]
async fn tools_list_rejects_excessive_aggregate_schema_bytes() {
    let (url, _state) = spawn_server(FixtureMode::OversizedToolCatalog).await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("connect");
    assert!(matches!(
        client.list_tools().await,
        Err(McpError::ToolListTooLarge { limit }) if limit == 256 * 1024
    ));
}

#[tokio::test]
async fn tools_list_skips_invalid_provider_schemas_without_poisoning_valid_tools() {
    let (url, _state) = spawn_server(FixtureMode::MixedInvalidToolSchemas).await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("connect");
    let tools = client.list_tools().await.expect("bounded catalog");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "safe-read");
    assert_eq!(
        tools[0].description,
        "# Safe read\r\n\n- valid tool survives invalid neighbors\n\t- tabbed"
    );
    assert_eq!(tools[0].annotations.read_only_hint, Some(true));
    assert!(tools[0].annotations.is_retry_safe());
}

#[tokio::test]
async fn tool_call_preserves_remote_is_error_as_an_error_path() {
    let (url, _state) = spawn_server(FixtureMode::ToolReportedError).await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("connect");
    assert!(matches!(
        client.call_tool("echo", json!({})).await,
        Err(McpError::ToolReported(message)) if message == "remote failure"
    ));
}

#[tokio::test]
async fn tool_call_rejects_unsupported_and_empty_content_instead_of_empty_success() {
    let (url, _state) = spawn_server(FixtureMode::UnsupportedToolContent).await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("connect");
    assert!(matches!(
        client.call_tool("echo", json!({})).await,
        Err(McpError::UnsupportedToolContent(kind)) if kind == "image"
    ));

    let (url, _state) = spawn_server(FixtureMode::EmptyToolContent).await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("connect");
    assert!(matches!(
        client.call_tool("echo", json!({})).await,
        Err(McpError::Invalid(message)) if message.contains("no supported text content")
    ));
}

#[tokio::test]
async fn manager_add_and_call_preserves_legacy_api() {
    let (url, _state) = spawn_server(FixtureMode::Standard).await;
    let manager = MCPServerManager::new();
    assert!(manager.add_server("echo", &url).await);
    assert!(manager.add_server("echo", &url).await);

    let all = manager.list_all_tools().await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, "echo");
    let output = manager
        .call_tool("echo", "echo", json!({"text": "world"}))
        .await
        .expect("call managed tool");
    assert!(output.contains("world"));

    let info = manager.server_info("echo").await.expect("server info");
    assert!(info.connected);
    assert_eq!(info.tools.len(), 1);
    assert!(info.resources);
}

#[tokio::test]
async fn slow_resource_read_does_not_block_manager_availability_queries() {
    let (url, state) = spawn_server(FixtureMode::SlowResourceRead).await;
    let manager = Arc::new(MCPServerManager::new());
    manager
        .try_add_server_resources_only("registry", &url)
        .await
        .expect("connect Resources-only fixture");

    let read = tokio::spawn({
        let manager = manager.clone();
        async move { manager.read_resource("registry", "agent://first").await }
    });
    tokio::time::timeout(Duration::from_millis(100), async {
        while state.resource_read_count.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("slow Resource request started");

    tokio::time::timeout(Duration::from_millis(50), async {
        let info = manager.server_info("registry").await.expect("server info");
        assert!(info.connected);
        assert!(info.resources);
        assert!(manager.is_connected("registry").await);
        assert_eq!(manager.list_servers().await, vec!["registry"]);
    })
    .await
    .expect("availability queries must not wait for Resource network I/O");

    read.await
        .expect("Resource task joins")
        .expect("slow Resource read completes");
}

#[tokio::test]
async fn client_and_manager_preserve_paginated_resources_and_contents() {
    let (url, _state) = spawn_server(FixtureMode::Standard).await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("connect");
    let metadata = client.metadata().expect("negotiated metadata");
    assert_eq!(metadata.protocol_version, "2024-11-05");
    assert!(metadata.capabilities.resources);

    let resources = client.list_resources().await.expect("list resources");
    assert_eq!(resources.len(), 2);
    assert_eq!(resources[0].uri, "agent://first");
    assert_eq!(resources[0].title.as_deref(), Some("First Agent"));
    assert_eq!(
        resources[0].description.as_deref(),
        Some(MULTILINE_RESOURCE_DESCRIPTION)
    );
    assert_eq!(resources[0].size, Some(42));
    assert_eq!(resources[1].uri, "agent://second");

    let contents = client
        .read_resource(&resources[0].uri)
        .await
        .expect("read exact listed URI");
    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0].uri, resources[0].uri);
    assert_eq!(contents[0].text.as_deref(), Some(MULTILINE_RESOURCE_TEXT));
    let descriptor: Value =
        serde_json::from_str(contents[0].text.as_deref().unwrap()).expect("pretty JSON content");
    assert_eq!(descriptor["kind"], "a2a");
    assert_eq!(descriptor["notes"], "# Fixture\n\n- Markdown");
    assert!(contents[0].blob.is_none());
    assert_eq!(contents[1].blob.as_deref(), Some("Zml4dHVyZQ=="));
    assert!(contents[1].text.is_none());

    let manager = MCPServerManager::new();
    manager
        .try_add_server("registry", &url)
        .await
        .expect("manager connect");
    assert_eq!(manager.list_resources("registry").await.unwrap().len(), 2);
    assert_eq!(
        manager
            .read_resource("registry", "agent://first")
            .await
            .unwrap()[0]
            .uri,
        "agent://first"
    );
}

#[tokio::test]
async fn tools_only_server_remains_connected_and_usable() {
    let (url, state) = spawn_server(FixtureMode::ToolsOnly).await;
    let client = MCPClient::new(&url);
    client.connect().await.expect("tools-only connect");
    let metadata = client.metadata().expect("metadata");
    assert!(metadata.capabilities.tools);
    assert!(!metadata.capabilities.resources);
    assert_eq!(client.list_tools().await.unwrap()[0].name, "echo");
    assert!(client
        .call_tool("echo", json!({"text": "usable"}))
        .await
        .unwrap()
        .contains("usable"));
    assert!(matches!(
        client.list_resources().await,
        Err(McpError::ResourcesUnsupported)
    ));
    assert_eq!(state.resource_list_count.load(Ordering::Relaxed), 0);

    let manager = MCPServerManager::new();
    manager
        .try_add_server("tools-only", &url)
        .await
        .expect("manager keeps tools-only server");
    let info = manager.server_info("tools-only").await.unwrap();
    assert!(info.connected);
    assert!(!info.resources);
    assert_eq!(info.tools.len(), 1);
    assert!(manager
        .call_tool("tools-only", "echo", json!({"text": "managed"}))
        .await
        .unwrap()
        .contains("managed"));
    assert!(matches!(
        manager.list_resources("tools-only").await,
        Err(McpError::ResourcesUnsupported)
    ));
}

#[tokio::test]
async fn invalid_initialized_notification_does_not_connect() {
    for mode in [
        FixtureMode::InitializedFailure,
        FixtureMode::InitializedBody,
    ] {
        let (url, state) = spawn_server(mode).await;
        let client = MCPClient::new(&url);
        client.connect().await.expect_err("notification must fail");
        assert!(!client.is_connected());
        assert!(client.metadata().is_none());
        assert_eq!(state.initialized_count.load(Ordering::Relaxed), 1);
    }
}

#[tokio::test]
async fn resources_list_rejects_cursor_cycle() {
    let (url, state) = spawn_server(FixtureMode::CursorCycle).await;
    let client = MCPClient::new(&url);
    client.connect().await.unwrap();
    let error = client.list_resources().await.expect_err("cycle must fail");
    assert!(error.to_string().contains("cursor cycle"));
    assert_eq!(state.resource_list_count.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn resources_list_rejects_page_limit() {
    let (url, state) = spawn_server(FixtureMode::PageLimit).await;
    let client = MCPClient::new(&url);
    client.connect().await.unwrap();
    let error = client
        .list_resources()
        .await
        .expect_err("unbounded pagination must fail");
    assert!(error.to_string().contains("exceeds 32 pages"));
    assert_eq!(state.resource_list_count.load(Ordering::Relaxed), 32);
}

#[tokio::test]
async fn resources_list_rejects_large_descriptions_across_pages() {
    let (url, state) = spawn_server(FixtureMode::AggregateResourceLimit).await;
    let client = MCPClient::new(&url);
    client.connect().await.unwrap();
    let error = client
        .list_resources()
        .await
        .expect_err("cross-page Resource bytes must be bounded");
    assert!(matches!(
        error,
        McpError::ResourceListTooLarge { limit: 2_097_152 }
    ));
    assert_eq!(state.resource_list_count.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn resources_read_rejects_mismatched_uri() {
    let (url, _state) = spawn_server(FixtureMode::MismatchedReadUri).await;
    let client = MCPClient::new(&url);
    client.connect().await.unwrap();
    let error = client
        .read_resource("agent://requested")
        .await
        .expect_err("mismatched URI must fail");
    assert!(error.to_string().contains("does not match"));
}

#[tokio::test]
async fn malformed_json_and_jsonrpc_responses_are_rejected() {
    for mode in [
        FixtureMode::MalformedResponse,
        FixtureMode::MalformedJsonRpc,
    ] {
        let (url, _state) = spawn_server(mode).await;
        let client = MCPClient::new(&url);
        client.connect().await.unwrap();
        assert!(matches!(
            client.list_resources().await,
            Err(McpError::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn malformed_and_invalid_blob_contents_are_rejected() {
    for mode in [FixtureMode::MalformedContent, FixtureMode::InvalidBlob] {
        let (url, _state) = spawn_server(mode).await;
        let client = MCPClient::new(&url);
        client.connect().await.unwrap();
        assert!(matches!(
            client.read_resource("agent://requested").await,
            Err(McpError::Invalid(_))
        ));
    }
}

#[tokio::test]
async fn oversized_transport_and_content_responses_are_rejected() {
    let (url, _state) = spawn_server(FixtureMode::OversizedResponse).await;
    let client = MCPClient::new(&url);
    client.connect().await.unwrap();
    assert!(matches!(
        client.list_resources().await,
        Err(McpError::ResponseTooLarge { .. })
    ));

    let (url, _state) = spawn_server(FixtureMode::OversizedContent).await;
    let client = MCPClient::new(&url);
    client.connect().await.unwrap();
    assert!(matches!(
        client.read_resource("agent://requested").await,
        Err(McpError::Invalid(_))
    ));
}

#[tokio::test]
async fn rpc_and_http_errors_remain_distinguishable() {
    let (url, _state) = spawn_server(FixtureMode::RpcError).await;
    let client = MCPClient::new(&url);
    client.connect().await.unwrap();
    assert!(matches!(
        client.list_resources().await,
        Err(McpError::Rpc { code: -32001, .. })
    ));

    let (url, _state) = spawn_server(FixtureMode::HttpError).await;
    let client = MCPClient::new(&url);
    client.connect().await.unwrap();
    let error = client
        .list_resources()
        .await
        .expect_err("HTTP error must fail");
    assert!(matches!(error, McpError::Transport(_)));
    assert!(error.to_string().contains("403"));
}
