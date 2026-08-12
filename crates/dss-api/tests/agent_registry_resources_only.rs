use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use dss_a2a::A2aClient;
use dss_api::state::build_mcp_runtime;
use dss_core::{McpServerConfig, DEFAULT_AGENT_REGISTRY_NAME};
use dss_tools::{builtin, ToolRegistry};
use serde_json::{json, Value};

const SESSION_ID: &str = "resources-only-registry-session";
const EVIL_TOOL: &str = "evil_registry_tool";
const RESOURCE_URI: &str = "agent://resources-only-fixture";

#[derive(Default)]
struct FixtureCounts {
    tools_list: AtomicUsize,
    tools_call: AtomicUsize,
    resources_list: AtomicUsize,
    resources_read: AtomicUsize,
}

#[derive(Clone, Copy)]
struct FixtureCapabilities {
    tools: bool,
    resources: bool,
}

struct FixtureState {
    counts: Arc<FixtureCounts>,
    capabilities: FixtureCapabilities,
}

async fn mcp_fixture(
    State(state): State<Arc<FixtureState>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    if method != "initialize"
        && headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            != Some(SESSION_ID)
    {
        return (StatusCode::UNAUTHORIZED, "missing MCP session").into_response();
    }

    match method {
        "initialize" => (
            [("mcp-session-id", SESSION_ID)],
            Json(json!({
                "jsonrpc": "2.0",
                "id": rpc_id(&request),
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": fixture_capabilities(state.capabilities),
                    "serverInfo": {"name": "mixed-registry-fixture", "version": "1.0.0"}
                }
            })),
        )
            .into_response(),
        "notifications/initialized" => StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            state.counts.tools_list.fetch_add(1, Ordering::SeqCst);
            rpc_result(
                &request,
                json!({
                    "tools": [{
                        "name": EVIL_TOOL,
                        "description": "must not be mounted from the canonical Registry",
                        "inputSchema": {"type": "object"}
                    }]
                }),
            )
        }
        "tools/call" => {
            state.counts.tools_call.fetch_add(1, Ordering::SeqCst);
            rpc_result(
                &request,
                json!({"content": [{"type": "text", "text": "ordinary tool result"}]}),
            )
        }
        "resources/list" => {
            state.counts.resources_list.fetch_add(1, Ordering::SeqCst);
            rpc_result(
                &request,
                json!({
                    "resources": [{
                        "uri": RESOURCE_URI,
                        "name": "resources-only-fixture",
                        "mimeType": "application/json"
                    }]
                }),
            )
        }
        "resources/read" => {
            state.counts.resources_read.fetch_add(1, Ordering::SeqCst);
            rpc_result(
                &request,
                json!({
                    "contents": [{
                        "uri": RESOURCE_URI,
                        "mimeType": "application/json",
                        "text": json!({
                            "kind": "a2a",
                            "uri": RESOURCE_URI,
                            "name": "resources-only-fixture",
                            "endpoint_url": "https://agent.example.com/a2a",
                            "auth_scheme_type": "none"
                        }).to_string()
                    }]
                }),
            )
        }
        _ => (StatusCode::NOT_FOUND, "unknown fixture method").into_response(),
    }
}

fn fixture_capabilities(capabilities: FixtureCapabilities) -> Value {
    let mut value = serde_json::Map::new();
    if capabilities.tools {
        value.insert("tools".into(), json!({}));
    }
    if capabilities.resources {
        value.insert("resources".into(), json!({}));
    }
    Value::Object(value)
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

async fn spawn_fixture() -> (String, Arc<FixtureCounts>) {
    spawn_fixture_with_capabilities(FixtureCapabilities {
        tools: true,
        resources: true,
    })
    .await
}

async fn spawn_fixture_with_capabilities(
    capabilities: FixtureCapabilities,
) -> (String, Arc<FixtureCounts>) {
    let counts = Arc::new(FixtureCounts::default());
    let state = Arc::new(FixtureState {
        counts: counts.clone(),
        capabilities,
    });
    let app = Router::new()
        .route("/mcp", post(mcp_fixture))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mixed MCP fixture");
    let address = listener.local_addr().expect("fixture address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve MCP fixture");
    });
    (format!("http://{address}/mcp"), counts)
}

async fn unavailable_loopback_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve offline fixture address");
    let address = listener.local_addr().expect("offline fixture address");
    drop(listener);
    format!("http://{address}/mcp")
}

#[tokio::test]
async fn canonical_registry_is_resources_only_while_other_servers_keep_tools() {
    let (url, counts) = spawn_fixture().await;
    let mut base = ToolRegistry::new();
    builtin::register_all(&mut base);
    let runtime = build_mcp_runtime(
        Arc::new(base),
        &[
            McpServerConfig {
                name: DEFAULT_AGENT_REGISTRY_NAME.into(),
                url: url.clone(),
                enabled: true,
            },
            McpServerConfig {
                name: "ordinary-mcp".into(),
                url,
                enabled: true,
            },
        ],
    )
    .await;

    let registry_evil = dss_mcp::mcp_tool_name(DEFAULT_AGENT_REGISTRY_NAME, EVIL_TOOL);
    let ordinary_evil = dss_mcp::mcp_tool_name("ordinary-mcp", EVIL_TOOL);
    assert!(runtime.tools.get(&registry_evil).is_none());
    assert!(runtime.tools.get(&ordinary_evil).is_some());
    assert!(runtime
        .tools
        .get(builtin::mcp::MCP_LIST_RESOURCES_TOOL_NAME)
        .is_some());
    assert!(runtime
        .tools
        .get(builtin::mcp::MCP_READ_RESOURCE_TOOL_NAME)
        .is_some());
    assert_eq!(
        runtime
            .tools
            .get(builtin::mcp::MCP_LIST_RESOURCES_TOOL_NAME)
            .expect("list resources tool")
            .spec()
            .parameters["properties"]["server"]["enum"],
        json!([DEFAULT_AGENT_REGISTRY_NAME, "ordinary-mcp"])
    );
    assert_eq!(counts.tools_list.load(Ordering::SeqCst), 1);

    let resources = runtime
        .manager
        .list_resources(DEFAULT_AGENT_REGISTRY_NAME)
        .await
        .expect("Registry Resources remain listable");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].uri, RESOURCE_URI);
    let contents = runtime
        .manager
        .read_resource(DEFAULT_AGENT_REGISTRY_NAME, RESOURCE_URI)
        .await
        .expect("Registry Resource remains readable");
    assert_eq!(contents.len(), 1);
    assert_eq!(counts.resources_list.load(Ordering::SeqCst), 1);
    assert_eq!(counts.resources_read.load(Ordering::SeqCst), 1);

    let registry_call = runtime
        .manager
        .call_tool(DEFAULT_AGENT_REGISTRY_NAME, EVIL_TOOL, json!({}))
        .await
        .expect_err("Resources-only manager entry must reject tools/call");
    assert!(registry_call.to_string().contains("Resources only"));
    assert_eq!(counts.tools_call.load(Ordering::SeqCst), 0);

    let ordinary_call = runtime
        .manager
        .call_tool("ordinary-mcp", EVIL_TOOL, json!({}))
        .await
        .expect("ordinary configured MCP tools remain callable");
    assert_eq!(ordinary_call, "ordinary tool result");
    assert_eq!(counts.tools_call.load(Ordering::SeqCst), 1);

    let mut run_tools = runtime.tools.snapshot();
    assert!(builtin::agent_registry::register_tool_if_available(
        &mut run_tools,
        runtime.manager.as_ref(),
        &A2aClient::new().expect("A2A client"),
    )
    .await
    .expect("register call_agent"));
    assert!(run_tools
        .get(builtin::agent_registry::CALL_AGENT_TOOL_NAME)
        .is_some());
}

#[tokio::test]
async fn resource_tools_are_absent_for_opt_out_offline_and_tools_only_servers() {
    let mut base = ToolRegistry::new();
    builtin::register_all(&mut base);
    let base = Arc::new(base);
    let client = A2aClient::new().expect("A2A client");

    let opted_out = build_mcp_runtime(base.clone(), &[]).await;
    assert_resource_tools(&opted_out, false);
    assert!(!builtin::agent_registry::register_tool_if_available(
        &mut opted_out.tools.snapshot(),
        opted_out.manager.as_ref(),
        &client,
    )
    .await
    .expect("opt-out Registry availability"));

    let offline_url = unavailable_loopback_url().await;
    let offline = build_mcp_runtime(
        base.clone(),
        &[McpServerConfig {
            name: DEFAULT_AGENT_REGISTRY_NAME.into(),
            url: offline_url,
            enabled: true,
        }],
    )
    .await;
    assert_resource_tools(&offline, false);
    assert!(!builtin::agent_registry::register_tool_if_available(
        &mut offline.tools.snapshot(),
        offline.manager.as_ref(),
        &client,
    )
    .await
    .expect("offline Registry availability"));

    let (tools_only_url, _) = spawn_fixture_with_capabilities(FixtureCapabilities {
        tools: true,
        resources: false,
    })
    .await;
    let tools_only = build_mcp_runtime(
        base,
        &[McpServerConfig {
            name: "tools-only".into(),
            url: tools_only_url,
            enabled: true,
        }],
    )
    .await;
    assert_resource_tools(&tools_only, false);
    assert!(tools_only
        .tools
        .get(&dss_mcp::mcp_tool_name("tools-only", EVIL_TOOL))
        .is_some());
    assert!(!builtin::agent_registry::register_tool_if_available(
        &mut tools_only.tools.snapshot(),
        tools_only.manager.as_ref(),
        &client,
    )
    .await
    .expect("ordinary MCP availability"));
}

#[tokio::test]
async fn ordinary_resources_server_exposes_only_the_resource_discovery_pair() {
    let (url, _) = spawn_fixture().await;
    let mut base = ToolRegistry::new();
    builtin::register_all(&mut base);
    let runtime = build_mcp_runtime(
        Arc::new(base),
        &[McpServerConfig {
            name: "ordinary-resources".into(),
            url,
            enabled: true,
        }],
    )
    .await;

    assert_resource_tools(&runtime, true);
    assert_eq!(
        runtime
            .tools
            .get(builtin::mcp::MCP_LIST_RESOURCES_TOOL_NAME)
            .expect("list resources tool")
            .spec()
            .parameters["properties"]["server"]["enum"],
        json!(["ordinary-resources"])
    );
    let mut run_tools = runtime.tools.snapshot();
    assert!(!builtin::agent_registry::register_tool_if_available(
        &mut run_tools,
        runtime.manager.as_ref(),
        &A2aClient::new().expect("A2A client"),
    )
    .await
    .expect("ordinary Resources availability"));
    assert!(run_tools
        .get(builtin::agent_registry::CALL_AGENT_TOOL_NAME)
        .is_none());
}

fn assert_resource_tools(runtime: &dss_api::state::McpRuntime, expected: bool) {
    assert_eq!(
        runtime
            .tools
            .get(builtin::mcp::MCP_LIST_RESOURCES_TOOL_NAME)
            .is_some(),
        expected
    );
    assert_eq!(
        runtime
            .tools
            .get(builtin::mcp::MCP_READ_RESOURCE_TOOL_NAME)
            .is_some(),
        expected
    );
}
