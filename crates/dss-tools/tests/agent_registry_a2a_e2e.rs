//! Hermetic cross-protocol coverage for Registry Resource discovery and A2A invocation.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use dss_a2a::{
    A2aClient, REGISTRY_API_KEY_WARNING, REGISTRY_DIRECT_TASK_WARNING,
    REGISTRY_ENDPOINT_OVERRIDE_WARNING,
};
use dss_core::DEFAULT_AGENT_REGISTRY_NAME;
use dss_mcp::MCPServerManager;
use dss_tools::{
    builtin::{
        self,
        agent_registry::{RegistryA2aTool, CALL_AGENT_TOOL_NAME},
        mcp::{MCP_LIST_RESOURCES_TOOL_NAME, MCP_READ_RESOURCE_TOOL_NAME},
    },
    PendingToolCall, ToolContext, ToolRegistry, ToolRouter,
};
use serde_json::{json, Value};
use tokio::task::JoinHandle;

const MCP_SESSION_ID: &str = "registry-e2e-session";
const RESOURCE_URI: &str = "agent://registry-e2e-agent";
const RESOURCE_NAME: &str = "registry-e2e-agent";
const MARKER: &str = "DSS_A2A_E2E_OK";

#[derive(Default)]
struct FixtureCounts {
    initialize: AtomicUsize,
    initialized: AtomicUsize,
    tools_list: AtomicUsize,
    resources_list: AtomicUsize,
    resources_read: AtomicUsize,
    card: AtomicUsize,
    send: AtomicUsize,
    a2a_version_1: AtomicUsize,
    credential: AtomicUsize,
}

#[derive(Clone)]
struct FixtureState {
    counts: Arc<FixtureCounts>,
    a2a_endpoint: String,
    wildcard_interface: String,
    credential_endpoint: String,
    failures: Arc<Mutex<Vec<String>>>,
}

impl FixtureState {
    fn fail(&self, message: impl Into<String>) {
        self.failures.lock().unwrap().push(message.into());
    }

    fn require_mcp_session(&self, headers: &HeaderMap, method: &str) {
        if headers
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            != Some(MCP_SESSION_ID)
        {
            self.fail(format!("{method} omitted the negotiated MCP session id"));
        }
    }
}

async fn mcp_handler(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    if method != "initialize" {
        state.require_mcp_session(&headers, method);
    }

    match method {
        "initialize" => {
            state.counts.initialize.fetch_add(1, Ordering::SeqCst);
            (
                [("mcp-session-id", MCP_SESSION_ID)],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id(&request),
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}, "resources": {"listChanged": false}},
                        "serverInfo": {"name": "registry-e2e", "version": "1.0.0"}
                    }
                })),
            )
                .into_response()
        }
        "notifications/initialized" => {
            state.counts.initialized.fetch_add(1, Ordering::SeqCst);
            StatusCode::ACCEPTED.into_response()
        }
        "tools/list" => {
            state.counts.tools_list.fetch_add(1, Ordering::SeqCst);
            rpc_result(
                &request,
                json!({
                    "tools": [{
                        "name": "evil_registry_tool",
                        "description": "Must never be exposed by the Resources-only Registry",
                        "inputSchema": {"type": "object"}
                    }]
                }),
            )
        }
        "resources/list" => {
            state.counts.resources_list.fetch_add(1, Ordering::SeqCst);
            rpc_result(
                &request,
                json!({
                    "resources": [{
                        "uri": RESOURCE_URI,
                        "name": RESOURCE_NAME,
                        "title": "Hermetic Registry Agent",
                        "description": "Untrusted fixture descriptor",
                        "mimeType": "application/json",
                        "size": 512
                    }]
                }),
            )
        }
        "resources/read" => {
            state.counts.resources_read.fetch_add(1, Ordering::SeqCst);
            let requested_uri = request
                .pointer("/params/uri")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if requested_uri != RESOURCE_URI {
                state.fail(format!(
                    "resources/read requested {requested_uri:?}, expected {RESOURCE_URI:?}"
                ));
            }
            let descriptor = json!({
                "kind": "a2a",
                "uri": RESOURCE_URI,
                "name": RESOURCE_NAME,
                "endpoint_url": state.a2a_endpoint,
                "auth_scheme_type": "none",
                "probe_status": "ok",
                "version": "1.0.0",
                "credential_endpoint": state.credential_endpoint
            });
            rpc_result(
                &request,
                json!({
                    "contents": [{
                        "uri": RESOURCE_URI,
                        "mimeType": "application/json",
                        "text": descriptor.to_string()
                    }]
                }),
            )
        }
        _ => Json(json!({
            "jsonrpc": "2.0",
            "id": rpc_id(&request),
            "error": {"code": -32601, "message": "method not found"}
        }))
        .into_response(),
    }
}

async fn agent_card(State(state): State<FixtureState>) -> Json<Value> {
    state.counts.card.fetch_add(1, Ordering::SeqCst);
    Json(json!({
        "name": "Hermetic Registry Agent",
        "description": "Returns the exact E2E marker",
        "version": "1.0.0",
        "supportedInterfaces": [{
            "url": state.wildcard_interface,
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }],
        "capabilities": {"streaming": false},
        "securitySchemes": {
            "apiKey": {"apiKeySecurityScheme": {"in": "header", "name": "X-API-Key"}}
        },
        "securityRequirements": [{"schemes": {"apiKey": {}}}],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{
            "id": "marker",
            "name": "Marker",
            "description": "Return the hermetic marker",
            "tags": ["test"]
        }]
    }))
}

async fn a2a_send(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Json<Value> {
    state.counts.send.fetch_add(1, Ordering::SeqCst);
    if headers
        .get("a2a-version")
        .and_then(|value| value.to_str().ok())
        == Some("1.0")
    {
        state.counts.a2a_version_1.fetch_add(1, Ordering::SeqCst);
    } else {
        state.fail("SendMessage did not carry A2A-Version: 1.0");
    }
    if headers.contains_key("authorization") || headers.contains_key("x-api-key") {
        state.fail("anonymous Registry A2A invocation sent credentials");
    }
    if request.get("method").and_then(Value::as_str) != Some("SendMessage") {
        state.fail("A2A request did not use SendMessage");
    }

    Json(json!({
        "jsonrpc": "2.0",
        "id": rpc_id(&request),
        "result": {
            "id": "registry-e2e-task",
            "contextId": "registry-e2e-context",
            "status": {"state": "TASK_STATE_INPUT_REQUIRED"},
            "artifacts": [{
                "artifactId": "marker",
                "name": "E2E marker",
                "parts": [{"text": MARKER}]
            }]
        }
    }))
}

async fn credential_route(State(state): State<FixtureState>) -> StatusCode {
    state.counts.credential.fetch_add(1, Ordering::SeqCst);
    StatusCode::INTERNAL_SERVER_ERROR
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

async fn spawn_fixture() -> (
    String,
    Arc<FixtureCounts>,
    Arc<Mutex<Vec<String>>>,
    JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind hermetic fixture");
    let address = listener.local_addr().expect("fixture address");
    let origin = format!("http://{address}");
    let counts = Arc::new(FixtureCounts::default());
    let failures = Arc::new(Mutex::new(Vec::new()));
    let state = FixtureState {
        counts: counts.clone(),
        a2a_endpoint: format!("{origin}/a2a"),
        wildcard_interface: format!("http://0.0.0.0:{}/a2a", address.port()),
        credential_endpoint: format!("{origin}/credential"),
        failures: failures.clone(),
    };
    let app = Router::new()
        .route("/mcp", post(mcp_handler))
        .route("/.well-known/agent-card.json", get(agent_card))
        .route("/a2a", post(a2a_send))
        .route("/credential", any(credential_route))
        .with_state(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve hermetic fixture");
    });
    (format!("{origin}/mcp"), counts, failures, server)
}

async fn execute_one(
    registry: &ToolRegistry,
    context: &ToolContext,
    id: &str,
    name: &str,
    input: Value,
) -> dss_tools::ToolResult {
    ToolRouter::execute_tool_calls(
        registry,
        context,
        vec![PendingToolCall {
            id: id.into(),
            name: name.into(),
            input,
        }],
    )
    .await
    .pop()
    .expect("one routed result")
}

#[tokio::test]
async fn registry_resource_can_drive_one_anonymous_a2a_call_end_to_end() {
    let (mcp_url, counts, failures, server) = spawn_fixture().await;
    let manager = Arc::new(MCPServerManager::new());
    manager
        .try_add_server_resources_only(DEFAULT_AGENT_REGISTRY_NAME, &mcp_url)
        .await
        .expect("connect the real MCP manager to the Registry fixture");

    let mut registry = ToolRegistry::new();
    builtin::register_all(&mut registry);
    assert_eq!(
        builtin::mcp::register_resource_tools(&mut registry, &[DEFAULT_AGENT_REGISTRY_NAME.into()],),
        2
    );
    assert!(registry
        .get(&dss_mcp::mcp_tool_name(
            DEFAULT_AGENT_REGISTRY_NAME,
            "evil_registry_tool"
        ))
        .is_none());
    assert!(builtin::agent_registry::register_tool_if_available(
        &mut registry,
        manager.as_ref(),
        &A2aClient::new().expect("A2A client"),
    )
    .await
    .expect("available Registry registration"));
    // Replace the production-HTTPS instance only inside this feature-gated
    // fixture. The test constructor itself accepts literal loopback HTTP only.
    registry.register(Arc::new(RegistryA2aTool::new_loopback_for_testing(
        A2aClient::new().expect("A2A client"),
    )));
    let context = ToolContext::new(std::env::temp_dir()).with_mcp_arc(manager);

    let listed = execute_one(
        &registry,
        &context,
        "list-resources",
        MCP_LIST_RESOURCES_TOOL_NAME,
        json!({"server": DEFAULT_AGENT_REGISTRY_NAME}),
    )
    .await;
    assert!(!listed.is_error, "list failed: {}", listed.content);
    let listed: Value = serde_json::from_str(&listed.content).expect("list output JSON");
    assert_eq!(listed["schema"], "dss.mcp.resources.v1");
    assert_eq!(listed["server"], DEFAULT_AGENT_REGISTRY_NAME);
    assert_eq!(listed["resources"].as_array().map(Vec::len), Some(1));
    let resource_uri = listed["resources"][0]["uri"]
        .as_str()
        .expect("listed Resource URI")
        .to_owned();
    assert_eq!(resource_uri, RESOURCE_URI);

    let read = execute_one(
        &registry,
        &context,
        "read-resource",
        MCP_READ_RESOURCE_TOOL_NAME,
        json!({"server": DEFAULT_AGENT_REGISTRY_NAME, "uri": resource_uri}),
    )
    .await;
    assert!(!read.is_error, "read failed: {}", read.content);
    let read: Value = serde_json::from_str(&read.content).expect("read output JSON");
    assert_eq!(read["schema"], "dss.mcp.resource-read.v1");
    assert_eq!(read["server"], DEFAULT_AGENT_REGISTRY_NAME);
    assert_eq!(read["uri"], RESOURCE_URI);
    assert_eq!(read["contents"].as_array().map(Vec::len), Some(1));
    let descriptor: Value = serde_json::from_str(
        read["contents"][0]["text"]
            .as_str()
            .expect("descriptor text"),
    )
    .expect("descriptor JSON");
    assert_eq!(descriptor["kind"], "a2a");
    assert_eq!(descriptor["uri"], RESOURCE_URI);
    assert_eq!(descriptor["auth_scheme_type"], "none");

    // Router does not enforce JSON Schema. A locally invalid Message must fail
    // before discovery/network and must not consume the run's one side-effect slot.
    let invalid = execute_one(
        &registry,
        &context,
        "invalid-call-agent",
        CALL_AGENT_TOOL_NAME,
        json!({
            "resource_uri": RESOURCE_URI,
            "action": "send",
            "task": "",
            "timeout_seconds": 5
        }),
    )
    .await;
    assert!(invalid.is_error);
    assert!(invalid.content.contains("task must not be empty"));
    assert_eq!(counts.resources_list.load(Ordering::SeqCst), 1);
    assert_eq!(counts.resources_read.load(Ordering::SeqCst), 1);
    assert_eq!(counts.card.load(Ordering::SeqCst), 0);
    assert_eq!(counts.send.load(Ordering::SeqCst), 0);

    let called = execute_one(
        &registry,
        &context,
        "call-agent",
        CALL_AGENT_TOOL_NAME,
        json!({
            "resource_uri": RESOURCE_URI,
            "action": "send",
            "task": format!("Return exactly {MARKER}"),
            "skill_id": "marker",
            "timeout_seconds": 5
        }),
    )
    .await;
    assert!(!called.is_error, "call_agent failed: {}", called.content);
    let called: Value = serde_json::from_str(&called.content).expect("call_agent output JSON");
    assert_eq!(called["schema"], "dss.a2a.tool-result.v1");
    assert_eq!(called["registry"]["server"], DEFAULT_AGENT_REGISTRY_NAME);
    assert_eq!(called["registry"]["resource_uri"], RESOURCE_URI);
    assert_eq!(called["registry"]["resource_name"], RESOURCE_NAME);
    assert_eq!(called["registry"]["probe_status"], "ok");
    assert_eq!(called["registry"]["version"], "1.0.0");
    assert_eq!(called["terminal"]["kind"], "task_interrupted");
    assert_eq!(called["terminal"]["state"], "TASK_STATE_INPUT_REQUIRED");
    assert_eq!(called["terminal"]["success"], true);
    assert_eq!(called["responses"].as_array().map(Vec::len), Some(1));
    assert_eq!(called["responses"][0]["operation"], "SendMessage");
    assert_eq!(called["responses"][0]["protocol_version"], "v1");
    assert_eq!(
        called["responses"][0]["payload"]["result"]["artifacts"][0]["parts"][0]["text"],
        MARKER
    );
    assert_eq!(
        called["warnings"],
        json!([
            REGISTRY_ENDPOINT_OVERRIDE_WARNING,
            REGISTRY_API_KEY_WARNING,
            REGISTRY_DIRECT_TASK_WARNING
        ])
    );

    assert_eq!(counts.initialize.load(Ordering::SeqCst), 1);
    assert_eq!(counts.initialized.load(Ordering::SeqCst), 1);
    assert_eq!(counts.tools_list.load(Ordering::SeqCst), 0);
    assert_eq!(counts.resources_list.load(Ordering::SeqCst), 2);
    assert_eq!(counts.resources_read.load(Ordering::SeqCst), 2);
    assert_eq!(counts.card.load(Ordering::SeqCst), 1);
    assert_eq!(counts.send.load(Ordering::SeqCst), 1);
    assert_eq!(counts.a2a_version_1.load(Ordering::SeqCst), 1);
    assert_eq!(counts.credential.load(Ordering::SeqCst), 0);

    let duplicate = execute_one(
        &registry,
        &context,
        "duplicate-send",
        CALL_AGENT_TOOL_NAME,
        json!({
            "resource_uri": RESOURCE_URI,
            "action": "send",
            "task": "Do not send this duplicate",
            "timeout_seconds": 5
        }),
    )
    .await;
    assert!(duplicate.is_error, "duplicate send unexpectedly succeeded");
    assert!(
        duplicate.content.contains("already attempted"),
        "unexpected guard error: {}",
        duplicate.content
    );
    assert_eq!(counts.resources_list.load(Ordering::SeqCst), 3);
    assert_eq!(counts.resources_read.load(Ordering::SeqCst), 3);
    assert_eq!(counts.card.load(Ordering::SeqCst), 1);
    assert_eq!(counts.send.load(Ordering::SeqCst), 1);
    assert_eq!(counts.a2a_version_1.load(Ordering::SeqCst), 1);
    assert_eq!(counts.credential.load(Ordering::SeqCst), 0);
    assert!(
        failures.lock().unwrap().is_empty(),
        "fixture protocol failures: {:?}",
        failures.lock().unwrap()
    );

    server.abort();
}
