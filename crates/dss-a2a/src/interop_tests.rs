use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use dss_core::A2aAgentConfig;
use serde_json::{json, Value};

use crate::{
    A2aClient, A2aClientOptions, A2aRouteOptions, CardRefreshKind, CardSnapshot, InvokeAction,
    InvokeRequest, ProtocolBinding, ProtocolVersion, RegistryInvocationPolicy, TerminalKind,
    A2A_RESULT_SCHEMA, REGISTRY_API_KEY_WARNING, REGISTRY_DIRECT_TASK_WARNING,
    REGISTRY_ENDPOINT_OVERRIDE_WARNING,
};

const TOKEN: &str = "fixture-bearer-secret";

#[derive(Debug, Clone, Copy)]
struct Variant {
    version: ProtocolVersion,
    binding: ProtocolBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixtureMode {
    Normal,
    NotModified,
    ReflectBearerValidators,
    BrokenSend,
    ResumeWorkingOnce,
    Interrupted,
    GetTaskRemoteError,
}

#[derive(Clone)]
struct FixtureState {
    variant: Variant,
    mode: FixtureMode,
    base_url: String,
    card_gets: Arc<AtomicUsize>,
    sends: Arc<AtomicUsize>,
    gets: Arc<AtomicUsize>,
    cancels: Arc<AtomicUsize>,
    failures: Arc<Mutex<Vec<String>>>,
}

impl FixtureState {
    fn verify_headers(&self, headers: &HeaderMap, operation: &str) {
        let actual_version = headers
            .get("a2a-version")
            .and_then(|value| value.to_str().ok());
        if actual_version != Some(self.variant.version.wire()) {
            self.failures.lock().unwrap().push(format!(
                "{operation}: A2A-Version was {actual_version:?}, expected {}",
                self.variant.version.wire()
            ));
        }
        if headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            != Some("Bearer fixture-bearer-secret")
        {
            self.failures
                .lock()
                .unwrap()
                .push(format!("{operation}: Bearer header missing"));
        }
    }
}

async fn card(State(state): State<FixtureState>, headers: HeaderMap) -> Response {
    let get_number = state.card_gets.fetch_add(1, Ordering::SeqCst) + 1;
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer fixture-bearer-secret")
    {
        state
            .failures
            .lock()
            .unwrap()
            .push("Agent Card: Bearer header missing".into());
    }
    if get_number > 1 {
        if state.mode == FixtureMode::ReflectBearerValidators {
            if headers.contains_key("if-none-match") || headers.contains_key("if-modified-since") {
                state
                    .failures
                    .lock()
                    .unwrap()
                    .push("Bearer-reflecting validators were sent conditionally".into());
            }
        } else if !headers.contains_key("if-none-match") {
            state
                .failures
                .lock()
                .unwrap()
                .push("second Agent Card GET was not conditional".into());
        }
    }
    let mut response_headers = HeaderMap::new();
    if state.mode == FixtureMode::ReflectBearerValidators {
        response_headers.insert(
            "etag",
            HeaderValue::from_str(&format!("\"server-reflected-{TOKEN}\"")).unwrap(),
        );
        response_headers.insert(
            "last-modified",
            HeaderValue::from_str(&format!("Wed, 21 Oct 2015 07:28:00 GMT; {TOKEN}")).unwrap(),
        );
    } else {
        response_headers.insert(
            "etag",
            HeaderValue::from_str(&format!("\"card-{get_number}\"")).unwrap(),
        );
    }
    if get_number > 1
        && matches!(
            state.mode,
            FixtureMode::NotModified | FixtureMode::ReflectBearerValidators
        )
    {
        return (StatusCode::NOT_MODIFIED, response_headers).into_response();
    }
    let interface = match state.variant.binding {
        ProtocolBinding::JsonRpc => format!("{}/rpc", state.base_url),
        ProtocolBinding::HttpJson => format!("{}/rest", state.base_url),
    };
    let body = match state.variant.version {
        ProtocolVersion::V1 => json!({
            "name": "Fixture specialist",
            "description": "Returns multiple scientific artifacts",
            "version": format!("revision-{get_number}"),
            "supportedInterfaces": [{
                "url": interface,
                "protocolBinding": match state.variant.binding {
                    ProtocolBinding::JsonRpc => "JSONRPC",
                    ProtocolBinding::HttpJson => "HTTP+JSON",
                },
                "protocolVersion": "1.0",
                "tenant": "lab-a"
            }],
            "capabilities": {"streaming": false},
            "securitySchemes": {"bearer": {"type":"http", "scheme":"bearer"}},
            "securityRequirements": [{"bearer": []}],
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/markdown", "application/json"],
            "skills": [{
                "id":"analysis",
                "name":"Analysis",
                "description":"Analyze a scientific problem",
                "tags":["science"]
            }]
        }),
        ProtocolVersion::V03 => json!({
            "name": "Fixture specialist",
            "description": "Returns multiple scientific artifacts",
            "version": format!("revision-{get_number}"),
            "protocolVersion": "0.3.0",
            "url": interface,
            "preferredTransport": match state.variant.binding {
                ProtocolBinding::JsonRpc => "JSONRPC",
                ProtocolBinding::HttpJson => "HTTP+JSON",
            },
            "capabilities": {"streaming": false},
            "securitySchemes": {"bearer": {"type":"http", "scheme":"bearer"}},
            "security": [{"bearer": []}],
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/markdown", "application/json"],
            "skills": [{"id":"analysis", "name":"Analysis", "tags":["science"]}]
        }),
    };
    (StatusCode::OK, response_headers, Json(body)).into_response()
}

async fn rpc(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    state.verify_headers(&headers, "JSON-RPC");
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let send_method = match state.variant.version {
        ProtocolVersion::V1 => "SendMessage",
        ProtocolVersion::V03 => "message/send",
    };
    let get_method = match state.variant.version {
        ProtocolVersion::V1 => "GetTask",
        ProtocolVersion::V03 => "tasks/get",
    };
    let cancel_method = match state.variant.version {
        ProtocolVersion::V1 => "CancelTask",
        ProtocolVersion::V03 => "tasks/cancel",
    };
    if method == send_method {
        state.sends.fetch_add(1, Ordering::SeqCst);
        verify_send_body(&state, &body["params"]);
        if state.mode == FixtureMode::BrokenSend {
            return broken_response();
        }
        Json(json!({"jsonrpc":"2.0", "id":id, "result":send_task(&state)})).into_response()
    } else if method == get_method {
        let get_number = state.gets.fetch_add(1, Ordering::SeqCst) + 1;
        if body.pointer("/params/id").and_then(Value::as_str) != Some("task-1") {
            state
                .failures
                .lock()
                .unwrap()
                .push("GetTask did not carry task-1".into());
        }
        if state.mode == FixtureMode::GetTaskRemoteError {
            return Json(json!({
                "jsonrpc":"2.0",
                "id":id,
                "error":{"code":-32001, "message":"task store unavailable"}
            }))
            .into_response();
        }
        if state.mode == FixtureMode::Interrupted {
            return Json(json!({
                "jsonrpc":"2.0",
                "id":id,
                "result":interrupted_task(state.variant)
            }))
            .into_response();
        }
        let completed = state.mode != FixtureMode::ResumeWorkingOnce || get_number > 1;
        Json(json!({
            "jsonrpc":"2.0",
            "id":id,
            "result":task_with_state(state.variant, completed)
        }))
        .into_response()
    } else if method == cancel_method {
        state.cancels.fetch_add(1, Ordering::SeqCst);
        if body.pointer("/params/id").and_then(Value::as_str) != Some("task-1") {
            state
                .failures
                .lock()
                .unwrap()
                .push("CancelTask did not carry task-1".into());
        }
        Json(json!({
            "jsonrpc":"2.0",
            "id":id,
            "result":canceled_task(state.variant)
        }))
        .into_response()
    } else {
        state
            .failures
            .lock()
            .unwrap()
            .push(format!("unexpected JSON-RPC method {method}"));
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"unexpected method"})),
        )
            .into_response()
    }
}

async fn rest_send(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    state.verify_headers(&headers, "REST send");
    state.sends.fetch_add(1, Ordering::SeqCst);
    verify_send_body(&state, &body);
    if state.mode == FixtureMode::BrokenSend {
        return broken_response();
    }
    Json(json!({"task":send_task(&state)})).into_response()
}

async fn rest_get(State(state): State<FixtureState>, headers: HeaderMap) -> Response {
    state.verify_headers(&headers, "REST get");
    let get_number = state.gets.fetch_add(1, Ordering::SeqCst) + 1;
    if state.mode == FixtureMode::Interrupted {
        return Json(interrupted_task(state.variant)).into_response();
    }
    let completed = state.mode != FixtureMode::ResumeWorkingOnce || get_number > 1;
    Json(task_with_state(state.variant, completed)).into_response()
}

async fn rest_cancel(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    state.verify_headers(&headers, "REST cancel");
    state.cancels.fetch_add(1, Ordering::SeqCst);
    if !body.is_empty() {
        state
            .failures
            .lock()
            .unwrap()
            .push("REST cancel unexpectedly carried a request body".into());
    }
    Json(canceled_task(state.variant)).into_response()
}

fn verify_send_body(state: &FixtureState, params: &Value) {
    let (expected_role, content_field) = match (state.variant.version, state.variant.binding) {
        (ProtocolVersion::V1, _) => ("ROLE_USER", "parts"),
        (ProtocolVersion::V03, ProtocolBinding::JsonRpc) => ("user", "parts"),
        // a2a-python 1.1.2 REST03Adapter parses the legacy protobuf with ProtoJSON. It does not
        // accept or emit the Pydantic JSON-RPC Message shape used by v0.3's JSON-RPC binding.
        (ProtocolVersion::V03, ProtocolBinding::HttpJson) => ("ROLE_USER", "content"),
    };
    if params.pointer("/message/role").and_then(Value::as_str) != Some(expected_role) {
        state
            .failures
            .lock()
            .unwrap()
            .push("send body used the wrong version-specific role".into());
    }
    if !params
        .pointer(&format!("/message/{content_field}"))
        .and_then(Value::as_array)
        .is_some_and(|content| !content.is_empty())
    {
        state.failures.lock().unwrap().push(format!(
            "send body did not use the required Message.{content_field} field"
        ));
    }
    match (state.variant.version, state.variant.binding) {
        (ProtocolVersion::V1, ProtocolBinding::JsonRpc) => {
            if params.get("tenant").and_then(Value::as_str) != Some("lab-a") {
                state
                    .failures
                    .lock()
                    .unwrap()
                    .push("v1 JSON-RPC request did not carry the interface tenant".into());
            }
        }
        (ProtocolVersion::V1, ProtocolBinding::HttpJson) => {
            if params.get("tenant").is_some() {
                state
                    .failures
                    .lock()
                    .unwrap()
                    .push("v1 REST request incorrectly carried tenant in its body".into());
            }
        }
        (ProtocolVersion::V03, _) => {}
    }
    if params.pointer("/configuration/acceptedOutputModes")
        != Some(&json!(["text/markdown", "application/json"]))
    {
        state.failures.lock().unwrap().push(
            "send body did not intersect acceptedOutputModes with Agent Card defaults".into(),
        );
    }
}

fn send_task(state: &FixtureState) -> Value {
    let task = task_with_state(state.variant, false);
    match (state.variant.version, state.variant.binding) {
        (ProtocolVersion::V1, ProtocolBinding::JsonRpc) => json!({"task": task}),
        (ProtocolVersion::V03, ProtocolBinding::JsonRpc) => task,
        (_, ProtocolBinding::HttpJson) => task,
    }
}

fn task_with_state(variant: Variant, completed: bool) -> Value {
    let (kind, role, working, complete, text_part, data_part, history_field, file_part) = match (
        variant.version,
        variant.binding,
    ) {
        (ProtocolVersion::V1, _) => (
            None,
            "ROLE_AGENT",
            "TASK_STATE_WORKING",
            "TASK_STATE_COMPLETED",
            json!({"text":"## Result\n\n| metric | value |\n|---|---:|\n| k | 1 |"}),
            json!({"data":{"confidence":0.97}}),
            "parts",
            json!({"url":"https://example.invalid/report"}),
        ),
        (ProtocolVersion::V03, ProtocolBinding::JsonRpc) => (
            Some("task"),
            "agent",
            "working",
            "completed",
            json!({"kind":"text", "text":"## Result\n\n| metric | value |\n|---|---:|\n| k | 1 |"}),
            json!({"kind":"data", "data":{"confidence":0.97}}),
            "parts",
            json!({"kind":"file", "file":{"uri":"https://example.invalid/report"}}),
        ),
        (ProtocolVersion::V03, ProtocolBinding::HttpJson) => (
            None,
            "ROLE_AGENT",
            "TASK_STATE_WORKING",
            "TASK_STATE_COMPLETED",
            json!({"text":"## Result\n\n| metric | value |\n|---|---:|\n| k | 1 |"}),
            json!({"data":{"data":{"confidence":0.97}}}),
            "content",
            json!({"file":{"fileWithUri":"https://example.invalid/report"}}),
        ),
    };
    let mut history_message = json!({
        "messageId":"remote-message-1", "role":role
    });
    history_message[history_field] = json!([text_part]);
    let mut task = json!({
        "id":"task-1",
        "contextId":"context-1",
        "status":{"state": if completed { complete } else { working }},
        "history": if completed { json!([history_message]) } else { json!([]) },
        "artifacts": if completed { json!([{
            "artifactId":"artifact-1", "name":"metrics", "parts":[data_part, file_part]
        }]) } else { json!([]) }
    });
    if let Some(kind) = kind {
        task["kind"] = Value::String(kind.into());
    }
    task
}

fn canceled_task(variant: Variant) -> Value {
    let mut task = task_with_state(variant, true);
    task["status"]["state"] = Value::String(
        match (variant.version, variant.binding) {
            (ProtocolVersion::V1, _) => "TASK_STATE_CANCELED",
            (ProtocolVersion::V03, ProtocolBinding::JsonRpc) => "canceled",
            (ProtocolVersion::V03, ProtocolBinding::HttpJson) => "TASK_STATE_CANCELLED",
        }
        .into(),
    );
    task
}

fn interrupted_task(variant: Variant) -> Value {
    let mut task = task_with_state(variant, false);
    task["status"]["state"] = Value::String(
        match (variant.version, variant.binding) {
            (ProtocolVersion::V1, ProtocolBinding::JsonRpc) => "TASK_STATE_INPUT_REQUIRED",
            (ProtocolVersion::V1, ProtocolBinding::HttpJson) => "TASK_STATE_AUTH_REQUIRED",
            (ProtocolVersion::V03, ProtocolBinding::JsonRpc) => "input-required",
            (ProtocolVersion::V03, ProtocolBinding::HttpJson) => "TASK_STATE_AUTH_REQUIRED",
        }
        .into(),
    );
    task
}

fn broken_response() -> Response {
    let stream = futures::stream::once(async {
        Err::<Bytes, std::io::Error>(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "fixture dropped response body",
        ))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from_stream(stream))
        .unwrap()
}

async fn spawn_fixture(
    variant: Variant,
    mode: FixtureMode,
) -> (FixtureState, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = FixtureState {
        variant,
        mode,
        base_url: format!("http://{address}"),
        card_gets: Arc::new(AtomicUsize::new(0)),
        sends: Arc::new(AtomicUsize::new(0)),
        gets: Arc::new(AtomicUsize::new(0)),
        cancels: Arc::new(AtomicUsize::new(0)),
        failures: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(card))
        .route("/rpc", post(rpc))
        .route("/rest/lab-a/message:send", post(rest_send))
        .route("/rest/lab-a/tasks/{id}", get(rest_get))
        .route("/rest/lab-a/tasks/task-1:cancel", post(rest_cancel))
        .route("/rest/v1/message:send", post(rest_send))
        .route("/rest/v1/tasks/{id}", get(rest_get))
        .route("/rest/v1/tasks/task-1:cancel", post(rest_cancel))
        .with_state(state.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (state, handle)
}

fn config(state: &FixtureState) -> A2aAgentConfig {
    A2aAgentConfig {
        id: format!(
            "agent-{:?}-{:?}",
            state.variant.version, state.variant.binding
        ),
        name: "Fixture Agent".into(),
        endpoint: state.base_url.clone(),
        enabled: true,
        bearer_token: Some(TOKEN.into()),
        timeout_seconds: 5,
    }
}

#[tokio::test]
async fn all_supported_version_binding_pairs_refresh_poll_and_preserve_every_response() {
    for version in [ProtocolVersion::V1, ProtocolVersion::V03] {
        for binding in [ProtocolBinding::JsonRpc, ProtocolBinding::HttpJson] {
            let (state, server) =
                spawn_fixture(Variant { version, binding }, FixtureMode::Normal).await;
            let client = A2aClient::with_options(A2aClientOptions {
                connect_timeout: Duration::from_secs(1),
                card_timeout: Duration::from_secs(1),
                poll_initial: Duration::from_millis(1),
                poll_max: Duration::from_millis(2),
                max_polls: 4,
            })
            .unwrap();
            let config = config(&state);
            let first_card: CardSnapshot = client.refresh_card(&config, None).await.unwrap();
            assert_eq!(first_card.summary.agent_version, "revision-1");

            let result = client
                .invoke(
                    &config,
                    Some(&first_card),
                    InvokeRequest::new("analyze the experiment"),
                )
                .await;
            assert_eq!(result.schema, A2A_RESULT_SCHEMA);
            assert_eq!(
                result.card.as_ref().unwrap().summary.agent_version,
                "revision-2"
            );
            assert_eq!(result.terminal.kind, TerminalKind::Task);
            assert!(result.terminal.success, "result was {result:#?}");
            assert_eq!(result.responses.len(), 2, "result was {result:#?}");
            assert_eq!(result.responses[0].sequence, 1);
            assert_eq!(result.responses[1].sequence, 2);
            assert!(result.responses[1]
                .payload
                .to_string()
                .contains("artifact-1"));
            assert!(!result.to_json().contains(TOKEN));
            assert_eq!(state.card_gets.load(Ordering::SeqCst), 2);
            assert_eq!(state.sends.load(Ordering::SeqCst), 1);
            assert_eq!(state.gets.load(Ordering::SeqCst), 1);
            assert!(
                state.failures.lock().unwrap().is_empty(),
                "fixture failures: {:?}",
                state.failures.lock().unwrap()
            );
            server.abort();
        }
    }
}

#[tokio::test]
async fn input_and_auth_required_are_resumable_non_error_task_interruptions() {
    for version in [ProtocolVersion::V1, ProtocolVersion::V03] {
        for binding in [ProtocolBinding::JsonRpc, ProtocolBinding::HttpJson] {
            let (state, server) =
                spawn_fixture(Variant { version, binding }, FixtureMode::Interrupted).await;
            let client = A2aClient::with_options(A2aClientOptions {
                connect_timeout: Duration::from_secs(1),
                card_timeout: Duration::from_secs(1),
                poll_initial: Duration::from_millis(1),
                poll_max: Duration::from_millis(2),
                max_polls: 4,
            })
            .unwrap();

            let result = client
                .invoke(
                    &config(&state),
                    None,
                    InvokeRequest::new("continue until the remote Agent needs help"),
                )
                .await;

            assert_eq!(result.terminal.kind, TerminalKind::TaskInterrupted);
            assert!(result.terminal.success, "result was {result:#?}");
            assert!(!result.is_error(), "result was {result:#?}");
            assert_eq!(result.terminal.task_id.as_deref(), Some("task-1"));
            assert_eq!(result.terminal.context_id.as_deref(), Some("context-1"));
            assert!(result.terminal.is_resumable_interruption());
            assert!(matches!(
                result.terminal.state.as_deref(),
                Some("TASK_STATE_INPUT_REQUIRED" | "TASK_STATE_AUTH_REQUIRED" | "input-required")
            ));
            assert_eq!(result.responses.len(), 2);
            assert_eq!(state.sends.load(Ordering::SeqCst), 1);
            assert_eq!(state.gets.load(Ordering::SeqCst), 1);

            let mut legacy_json = serde_json::to_value(&result).unwrap();
            legacy_json["terminal"]["kind"] = Value::String("task".into());
            legacy_json["terminal"]["success"] = Value::Bool(false);
            legacy_json["terminal"]["error"] =
                Value::String("remote task requires input or authentication".into());
            let legacy: crate::A2aToolResult = serde_json::from_value(legacy_json).unwrap();
            assert!(legacy.terminal.is_resumable_interruption());
            assert!(!legacy.is_error(), "legacy result was {legacy:#?}");

            assert!(
                state.failures.lock().unwrap().is_empty(),
                "fixture failures: {:?}",
                state.failures.lock().unwrap()
            );
            server.abort();
        }
    }
}

#[tokio::test]
async fn conditional_304_reuses_only_the_validated_cached_card() {
    let (state, server) = spawn_fixture(
        Variant {
            version: ProtocolVersion::V1,
            binding: ProtocolBinding::JsonRpc,
        },
        FixtureMode::NotModified,
    )
    .await;
    let client = A2aClient::new().unwrap();
    let config = config(&state);
    let first = client.refresh_card(&config, None).await.unwrap();
    let second = client.refresh_card(&config, Some(&first)).await.unwrap();
    assert_eq!(second.refresh_kind, CardRefreshKind::NotModified);
    assert_eq!(second.sha256, first.sha256);
    assert_eq!(second.raw, first.raw);

    // A cache accepted by an older application version must not bypass current validation when
    // the remote answers 304.
    let mut legacy_cache = first.clone();
    legacy_cache
        .raw
        .as_object_mut()
        .unwrap()
        .remove("capabilities");
    assert!(matches!(
        client.refresh_card(&config, Some(&legacy_cache)).await,
        Err(crate::A2aError::InvalidCard(_))
    ));

    assert_eq!(state.card_gets.load(Ordering::SeqCst), 3);
    assert!(state.failures.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn bearer_reflecting_card_validators_are_neither_cached_nor_replayed() {
    let (state, server) = spawn_fixture(
        Variant {
            version: ProtocolVersion::V1,
            binding: ProtocolBinding::JsonRpc,
        },
        FixtureMode::ReflectBearerValidators,
    )
    .await;
    let client = A2aClient::new().unwrap();
    let config = config(&state);

    let first = client.refresh_card(&config, None).await.unwrap();
    assert_eq!(first.etag, None);
    assert_eq!(first.last_modified, None);

    // Persisted snapshots from an older client may already contain a reflected credential.
    // They must be scrubbed before both request construction and 304 cache reuse.
    let mut legacy_cache = first;
    legacy_cache.etag = Some(format!("\"server-reflected-{TOKEN}\""));
    legacy_cache.last_modified = Some(format!("Wed, 21 Oct 2015 07:28:00 GMT; {TOKEN}"));
    let refreshed = client
        .refresh_card(&config, Some(&legacy_cache))
        .await
        .unwrap();

    assert_eq!(refreshed.refresh_kind, CardRefreshKind::NotModified);
    assert_eq!(refreshed.etag, None);
    assert_eq!(refreshed.last_modified, None);
    assert_eq!(state.card_gets.load(Ordering::SeqCst), 2);
    assert!(
        state.failures.lock().unwrap().is_empty(),
        "fixture failures: {:?}",
        state.failures.lock().unwrap()
    );
    server.abort();
}

#[tokio::test]
async fn ambiguous_send_body_failure_is_never_replayed() {
    let (state, server) = spawn_fixture(
        Variant {
            version: ProtocolVersion::V1,
            binding: ProtocolBinding::JsonRpc,
        },
        FixtureMode::BrokenSend,
    )
    .await;
    let client = A2aClient::new().unwrap();
    let config = config(&state);
    let result = client
        .invoke(&config, None, InvokeRequest::new("perform one side effect"))
        .await;
    assert_eq!(result.terminal.kind, TerminalKind::OutcomeUnknown);
    assert!(!result.terminal.success);
    assert_eq!(state.sends.load(Ordering::SeqCst), 1);
    assert_eq!(result.responses.len(), 0);
    assert!(result
        .terminal
        .error
        .as_deref()
        .unwrap()
        .contains("not retried"));
    server.abort();
}

#[tokio::test]
async fn v1_json_rpc_get_task_resumes_immediately_then_polls_without_sending() {
    let (state, server) = spawn_fixture(
        Variant {
            version: ProtocolVersion::V1,
            binding: ProtocolBinding::JsonRpc,
        },
        FixtureMode::ResumeWorkingOnce,
    )
    .await;
    let client = A2aClient::with_options(A2aClientOptions {
        connect_timeout: Duration::from_secs(1),
        card_timeout: Duration::from_secs(1),
        poll_initial: Duration::from_millis(1),
        poll_max: Duration::from_millis(2),
        max_polls: 4,
    })
    .unwrap();

    let result = client
        .invoke(&config(&state), None, InvokeRequest::get_task("task-1"))
        .await;

    assert_eq!(result.request.action, InvokeAction::GetTask);
    assert_eq!(result.request.message_id, None);
    assert_eq!(result.terminal.kind, TerminalKind::Task);
    assert!(result.terminal.success, "result was {result:#?}");
    assert_eq!(result.terminal.task_id.as_deref(), Some("task-1"));
    assert_eq!(result.responses.len(), 2, "result was {result:#?}");
    assert!(result
        .responses
        .iter()
        .all(|frame| frame.operation == "GetTask"));
    assert_eq!(
        result.responses[0]
            .payload
            .pointer("/result/status/state")
            .and_then(Value::as_str),
        Some("TASK_STATE_WORKING")
    );
    assert_eq!(
        result.responses[1]
            .payload
            .pointer("/result/status/state")
            .and_then(Value::as_str),
        Some("TASK_STATE_COMPLETED")
    );
    assert_eq!(state.card_gets.load(Ordering::SeqCst), 1);
    assert_eq!(state.sends.load(Ordering::SeqCst), 0);
    assert_eq!(state.gets.load(Ordering::SeqCst), 2);
    assert!(
        state.failures.lock().unwrap().is_empty(),
        "fixture failures: {:?}",
        state.failures.lock().unwrap()
    );
    server.abort();
}

#[tokio::test]
async fn submit_checkpoints_a_working_task_without_polling() {
    let (state, server) = spawn_fixture(
        Variant {
            version: ProtocolVersion::V1,
            binding: ProtocolBinding::JsonRpc,
        },
        FixtureMode::Normal,
    )
    .await;
    let client = A2aClient::new().unwrap();

    let result = client
        .invoke(
            &config(&state),
            None,
            InvokeRequest::submit("start the long-running analysis"),
        )
        .await;

    assert_eq!(result.request.action, InvokeAction::Submit);
    assert!(result.request.message_id.is_some());
    assert_eq!(result.terminal.kind, TerminalKind::TaskPending);
    assert!(result.terminal.success, "result was {result:#?}");
    assert_eq!(result.terminal.task_id.as_deref(), Some("task-1"));
    assert_eq!(result.responses.len(), 1);
    assert_eq!(result.responses[0].operation, "SendMessage");
    assert_eq!(state.card_gets.load(Ordering::SeqCst), 1);
    assert_eq!(state.sends.load(Ordering::SeqCst), 1);
    assert_eq!(state.gets.load(Ordering::SeqCst), 0);
    assert!(state.failures.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn send_and_submit_reject_a_task_id_changed_by_the_remote() {
    for action in [InvokeAction::Send, InvokeAction::Submit] {
        let (state, server) = spawn_fixture(
            Variant {
                version: ProtocolVersion::V1,
                binding: ProtocolBinding::JsonRpc,
            },
            FixtureMode::Normal,
        )
        .await;
        let client = A2aClient::new().unwrap();
        let mut request = match action {
            InvokeAction::Send => InvokeRequest::new("continue the existing task"),
            InvokeAction::Submit => InvokeRequest::submit("continue the existing task"),
            _ => unreachable!(),
        };
        request.task_id = Some("original-task".into());
        request.context_id = Some("context-1".into());

        let result = client.invoke(&config(&state), None, request).await;

        assert_eq!(result.request.action, action);
        assert_eq!(result.terminal.kind, TerminalKind::ProtocolError);
        assert!(!result.terminal.success);
        assert_eq!(result.terminal.task_id.as_deref(), Some("original-task"));
        assert_eq!(result.terminal.context_id.as_deref(), Some("context-1"));
        assert_eq!(result.responses.len(), 1);
        assert_eq!(state.sends.load(Ordering::SeqCst), 1);
        assert_eq!(state.gets.load(Ordering::SeqCst), 0);
        assert!(result
            .terminal
            .error
            .as_deref()
            .is_some_and(|error| error.contains("task-1, expected original-task")));
        server.abort();
    }
}

#[tokio::test]
async fn continuation_send_rejects_a_changed_context_and_preserves_the_original_handle() {
    let (state, server) = spawn_fixture(
        Variant {
            version: ProtocolVersion::V1,
            binding: ProtocolBinding::JsonRpc,
        },
        FixtureMode::Normal,
    )
    .await;
    let client = A2aClient::new().unwrap();
    let mut request = InvokeRequest::submit("continue in the same context");
    request.task_id = Some("task-1".into());
    request.context_id = Some("original-context".into());

    let result = client.invoke(&config(&state), None, request).await;

    assert_eq!(result.terminal.kind, TerminalKind::ProtocolError);
    assert_eq!(result.terminal.task_id.as_deref(), Some("task-1"));
    assert_eq!(
        result.terminal.context_id.as_deref(),
        Some("original-context")
    );
    assert_eq!(result.responses.len(), 1);
    assert_eq!(state.sends.load(Ordering::SeqCst), 1);
    assert_eq!(state.gets.load(Ordering::SeqCst), 0);
    assert!(result
        .terminal
        .error
        .as_deref()
        .is_some_and(|error| error.contains("context-1, expected original-context")));
    server.abort();
}

#[tokio::test]
async fn get_task_protocol_errors_preserve_the_resumable_handle_and_response_frame() {
    let (state, server) = spawn_fixture(
        Variant {
            version: ProtocolVersion::V1,
            binding: ProtocolBinding::JsonRpc,
        },
        FixtureMode::GetTaskRemoteError,
    )
    .await;
    let client = A2aClient::new().unwrap();

    let result = client
        .invoke(&config(&state), None, InvokeRequest::get_task("task-1"))
        .await;

    assert_eq!(result.terminal.kind, TerminalKind::ProtocolError);
    assert_eq!(result.terminal.task_id.as_deref(), Some("task-1"));
    assert_eq!(result.responses.len(), 1);
    assert_eq!(result.responses[0].operation, "GetTask");
    assert_eq!(state.sends.load(Ordering::SeqCst), 0);
    assert_eq!(state.gets.load(Ordering::SeqCst), 1);
    assert!(result
        .terminal
        .error
        .as_deref()
        .is_some_and(|error| error.contains("task store unavailable")));
    server.abort();
}

#[tokio::test]
async fn get_task_rejects_a_changed_context_and_preserves_the_original_handle() {
    let (state, server) = spawn_fixture(
        Variant {
            version: ProtocolVersion::V1,
            binding: ProtocolBinding::JsonRpc,
        },
        FixtureMode::Normal,
    )
    .await;
    let client = A2aClient::new().unwrap();
    let mut request = InvokeRequest::get_task("task-1");
    request.context_id = Some("original-context".into());

    let result = client.invoke(&config(&state), None, request).await;

    assert_eq!(result.terminal.kind, TerminalKind::ProtocolError);
    assert_eq!(result.terminal.task_id.as_deref(), Some("task-1"));
    assert_eq!(
        result.terminal.context_id.as_deref(),
        Some("original-context")
    );
    assert_eq!(result.responses.len(), 1);
    assert_eq!(result.responses[0].operation, "GetTask");
    assert_eq!(state.sends.load(Ordering::SeqCst), 0);
    assert_eq!(state.gets.load(Ordering::SeqCst), 1);
    assert!(result
        .terminal
        .error
        .as_deref()
        .is_some_and(|error| error.contains("context-1, expected original-context")));
    server.abort();
}

#[tokio::test]
async fn v1_json_rpc_cancel_task_is_single_shot_and_never_sends_a_message() {
    let (state, server) = spawn_fixture(
        Variant {
            version: ProtocolVersion::V1,
            binding: ProtocolBinding::JsonRpc,
        },
        FixtureMode::Normal,
    )
    .await;
    let client = A2aClient::new().unwrap();

    let result = client
        .invoke(&config(&state), None, InvokeRequest::cancel_task("task-1"))
        .await;

    assert_eq!(result.request.action, InvokeAction::CancelTask);
    assert_eq!(result.request.message_id, None);
    assert_eq!(result.terminal.kind, TerminalKind::Task);
    assert!(result.terminal.success, "result was {result:#?}");
    assert_eq!(result.terminal.task_id.as_deref(), Some("task-1"));
    assert_eq!(
        result.terminal.state.as_deref(),
        Some("TASK_STATE_CANCELED")
    );
    assert_eq!(result.responses.len(), 1);
    assert_eq!(result.responses[0].operation, "CancelTask");
    assert_eq!(state.sends.load(Ordering::SeqCst), 0);
    assert_eq!(state.gets.load(Ordering::SeqCst), 0);
    assert_eq!(state.cancels.load(Ordering::SeqCst), 1);
    assert!(state.failures.lock().unwrap().is_empty());
    server.abort();
}

#[tokio::test]
async fn both_rest_versions_cancel_with_an_empty_body_and_their_own_enum_spelling() {
    for version in [ProtocolVersion::V1, ProtocolVersion::V03] {
        let (state, server) = spawn_fixture(
            Variant {
                version,
                binding: ProtocolBinding::HttpJson,
            },
            FixtureMode::Normal,
        )
        .await;
        let client = A2aClient::new().unwrap();

        let result = client
            .invoke(&config(&state), None, InvokeRequest::cancel_task("task-1"))
            .await;

        assert_eq!(result.request.action, InvokeAction::CancelTask);
        assert_eq!(result.request.message_id, None);
        assert_eq!(result.terminal.kind, TerminalKind::Task);
        assert!(result.terminal.success, "result was {result:#?}");
        assert_eq!(result.terminal.task_id.as_deref(), Some("task-1"));
        assert_eq!(
            result.terminal.state.as_deref(),
            Some(match version {
                ProtocolVersion::V1 => "TASK_STATE_CANCELED",
                ProtocolVersion::V03 => "TASK_STATE_CANCELLED",
            })
        );
        assert_eq!(result.responses.len(), 1);
        assert_eq!(result.responses[0].operation, "tasks/cancel");
        assert_eq!(state.sends.load(Ordering::SeqCst), 0);
        assert_eq!(state.gets.load(Ordering::SeqCst), 0);
        assert_eq!(state.cancels.load(Ordering::SeqCst), 1);
        assert!(
            state.failures.lock().unwrap().is_empty(),
            "fixture failures: {:?}",
            state.failures.lock().unwrap()
        );
        server.abort();
    }
}

#[derive(Clone)]
struct RegistryCompatState {
    sends: Arc<AtomicUsize>,
    failures: Arc<Mutex<Vec<String>>>,
}

async fn registry_compat_card() -> Json<Value> {
    Json(json!({
        "name": "Registry fixture",
        "description": "Exercises the narrow Registry compatibility path",
        "version": "1",
        "supportedInterfaces": [{
            "url": "http://0.0.0.0:8888/a2a",
            "protocolBinding": "JSONRPC",
            "protocolVersion": "1.0"
        }],
        "capabilities": {"streaming": false},
        "securitySchemes": {
            "apiKey": {"apiKeySecurityScheme":{"in":"header", "name":"X-API-Key"}}
        },
        "securityRequirements": [{"schemes":{"apiKey":{}}}],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{
            "id":"marker", "name":"Marker", "description":"Return a marker", "tags":["test"]
        }]
    }))
}

async fn registry_compat_rpc(
    State(state): State<RegistryCompatState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    state.sends.fetch_add(1, Ordering::SeqCst);
    if headers.contains_key("authorization") || headers.contains_key("x-api-key") {
        state
            .failures
            .lock()
            .unwrap()
            .push("Registry-anonymous invocation sent credentials".into());
    }
    if body.get("method").and_then(Value::as_str) != Some("SendMessage") {
        state
            .failures
            .lock()
            .unwrap()
            .push("Registry invocation did not use canonical SendMessage".into());
    }
    Json(json!({
        "jsonrpc":"2.0",
        "id":body.get("id").cloned().unwrap_or(Value::Null),
        "result": {
            "id":"registry-task", "contextId":"registry-context",
            "status":{"state":"TASK_STATE_INPUT_REQUIRED"},
            "artifacts":[{"artifactId":"marker", "parts":[{"text":"DSS_A2A_E2E_OK"}]}]
        }
    }))
}

#[tokio::test]
async fn registry_entry_point_is_explicit_anonymous_and_preserves_warnings() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = RegistryCompatState {
        sends: Arc::new(AtomicUsize::new(0)),
        failures: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(registry_compat_card))
        .route("/a2a", post(registry_compat_rpc))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let endpoint = format!("http://{address}/a2a");
    let config = A2aAgentConfig {
        id: "registry-fixture".into(),
        name: "Registry fixture".into(),
        endpoint: endpoint.clone(),
        enabled: true,
        bearer_token: None,
        timeout_seconds: 5,
    };
    let client = A2aClient::new().unwrap();

    let strict = client
        .invoke(&config, None, InvokeRequest::new("return marker"))
        .await;
    assert_eq!(strict.terminal.kind, TerminalKind::CardRefreshError);
    assert!(strict.warnings.is_empty());
    assert_eq!(state.sends.load(Ordering::SeqCst), 0);

    let policy = RegistryInvocationPolicy::anonymous_loopback_for_testing(&endpoint).unwrap();
    let registry = client
        .invoke_registry_anonymous(&config, None, &policy, InvokeRequest::new("return marker"))
        .await;
    assert_eq!(registry.terminal.kind, TerminalKind::TaskInterrupted);
    assert!(!registry.is_error(), "result was {registry:#?}");
    assert_eq!(registry.terminal.task_id.as_deref(), Some("registry-task"));
    assert_eq!(
        registry.card.as_ref().unwrap().selected_interface.url,
        endpoint
    );
    assert_eq!(
        registry.warnings,
        vec![
            REGISTRY_ENDPOINT_OVERRIDE_WARNING,
            REGISTRY_API_KEY_WARNING,
            REGISTRY_DIRECT_TASK_WARNING,
        ]
    );
    assert_eq!(state.sends.load(Ordering::SeqCst), 1);
    assert!(state.failures.lock().unwrap().is_empty());
    server.abort();
}

#[derive(Clone)]
struct RoutedHostState {
    interface_url: String,
    expected_host: String,
    failures: Arc<Mutex<Vec<String>>>,
}

impl RoutedHostState {
    fn check_host(&self, headers: &HeaderMap) {
        if headers.get("host").and_then(|value| value.to_str().ok())
            != Some(self.expected_host.as_str())
        {
            self.failures
                .lock()
                .unwrap()
                .push("route override rewrote the HTTP Host header".into());
        }
    }
}

async fn routed_card(State(state): State<RoutedHostState>, headers: HeaderMap) -> Json<Value> {
    state.check_host(&headers);
    Json(json!({
        "name":"Routed fixture", "description":"Checks hostname preservation", "version":"1",
        "supportedInterfaces":[{
            "url":state.interface_url, "protocolBinding":"JSONRPC", "protocolVersion":"1.0"
        }],
        "capabilities":{"streaming":false},
        "securityRequirements":[],
        "defaultInputModes":["text/plain"], "defaultOutputModes":["text/plain"],
        "skills":[{"id":"route", "name":"Route", "description":"Check route", "tags":["test"]}]
    }))
}

async fn routed_rpc(
    State(state): State<RoutedHostState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    state.check_host(&headers);
    Json(json!({
        "jsonrpc":"2.0", "id":body.get("id").cloned().unwrap_or(Value::Null),
        "result":{"task":{
            "id":"route-task", "status":{"state":"TASK_STATE_INPUT_REQUIRED"}
        }}
    }))
}

#[tokio::test]
async fn route_resolution_keeps_the_url_hostname_for_host_and_tls_identity() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let expected_host = format!("route.test:{}", address.port());
    let interface_url = format!("http://{expected_host}/a2a");
    let state = RoutedHostState {
        interface_url: interface_url.clone(),
        expected_host,
        failures: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/.well-known/agent-card.json", get(routed_card))
        .route("/a2a", post(routed_rpc))
        .with_state(state.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut route = A2aRouteOptions::default();
    route.resolve.insert("route.test".into(), address);
    let client = A2aClient::with_options_and_route_options(
        A2aClientOptions {
            connect_timeout: Duration::from_secs(1),
            card_timeout: Duration::from_secs(1),
            poll_initial: Duration::from_millis(1),
            poll_max: Duration::from_millis(2),
            max_polls: 1,
        },
        route,
    )
    .unwrap();
    let config = A2aAgentConfig {
        id: "route-fixture".into(),
        name: "Route fixture".into(),
        endpoint: interface_url,
        enabled: true,
        bearer_token: None,
        timeout_seconds: 5,
    };
    let result = client
        .invoke(&config, None, InvokeRequest::new("check route"))
        .await;
    assert_eq!(result.terminal.kind, TerminalKind::TaskInterrupted);
    assert!(result.warnings.is_empty());
    assert!(
        state.failures.lock().unwrap().is_empty(),
        "fixture failures: {:?}",
        state.failures.lock().unwrap()
    );
    server.abort();
}
