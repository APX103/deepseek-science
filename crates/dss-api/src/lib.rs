//! dss-api: axum HTTP 路由与服务启动。

pub mod bots;
pub mod db;
pub mod logs;
pub mod mcp_endpoints;
pub mod memories;
pub mod meta;
pub mod projects;
pub mod sessions;
pub mod settings_endpoints;
pub mod state;
mod subagents;
pub mod workspace_files;
mod workspace_resolution;

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, uri::Authority, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};
use serde::Serialize;
use state::AppState;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub use state::build_state;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const API_TOKEN_HEADER: &str = "x-dss-token";
const ALLOWED_ORIGINS: [&str; 4] = [
    "tauri://localhost",
    "http://tauri.localhost",
    "http://localhost:5173",
    "http://127.0.0.1:5173",
];

#[derive(Clone)]
struct ApiSecurity {
    token: Option<Arc<str>>,
}

impl ApiSecurity {
    fn from_state(state: &AppState) -> Self {
        Self {
            token: state.api_token.clone(),
        }
    }
}

/// Reject DNS-rebinding/cross-origin traffic before it can reach privileged routes.
/// Packaged launches additionally require an unguessable per-process capability token.
async fn enforce_local_api_security(
    State(security): State<ApiSecurity>,
    request: Request,
    next: Next,
) -> Response {
    if let Some((status, message)) = security_rejection(&request, &security) {
        return (status, message).into_response();
    }
    next.run(request).await
}

fn security_rejection(
    request: &Request,
    security: &ApiSecurity,
) -> Option<(StatusCode, &'static str)> {
    if !has_allowed_host(request.headers()) {
        return Some((StatusCode::FORBIDDEN, "non-local Host is not allowed"));
    }
    if !has_allowed_origin(request.headers()) {
        return Some((StatusCode::FORBIDDEN, "request Origin is not allowed"));
    }

    // CORS preflights carry no capability token and cannot invoke an application handler.
    let token_exempt = request.method() == Method::OPTIONS || request.uri().path() == "/api/health";
    if token_exempt {
        return None;
    }

    let Some(expected) = security.token.as_deref() else {
        // A directly launched CLI server remains compatible when DSS_API_TOKEN is unset.
        return None;
    };
    let mut supplied = request.headers().get_all(API_TOKEN_HEADER).iter();
    let valid = supplied
        .next()
        .is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes()))
        && supplied.next().is_none();
    (!valid).then_some((StatusCode::UNAUTHORIZED, "missing or invalid API token"))
}

fn has_allowed_host(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::HOST).iter();
    let Some(value) = values.next() else {
        return false;
    };
    values.next().is_none() && value.to_str().ok().is_some_and(is_loopback_authority)
}

fn is_loopback_authority(value: &str) -> bool {
    let Ok(authority) = value.parse::<Authority>() else {
        return false;
    };
    let host = authority.host();
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn has_allowed_origin(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(header::ORIGIN).iter();
    let Some(value) = values.next() else {
        // Native/CLI clients do not send Origin.
        return true;
    };
    values.next().is_none()
        && value
            .to_str()
            .ok()
            .is_some_and(|origin| ALLOWED_ORIGINS.contains(&origin))
}

/// Length-oblivious comparison style: always walks the longest input and folds every byte.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let max_len = left.len().max(right.len());
    for index in 0..max_len {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(left_byte ^ right_byte);
    }
    difference == 0
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<HealthResponse> {
    // Readiness probes can be issued frequently by the desktop shell. Keep successful
    // probes out of the default info log so a local client cannot fill backend.log cheaply.
    tracing::debug!("GET /api/health");
    Json(HealthResponse {
        status: "ok",
        version: VERSION,
    })
}

/// `GET /api/config`：供前端判断 LLM 是否可用（P1 最小字段集）。
#[derive(Serialize)]
struct ConfigResponse {
    llm_configured: bool,
    model: String,
    base_url: String,
    revision: u64,
    overridden_fields: Vec<&'static str>,
}

async fn config(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<ConfigResponse> {
    let runtime = state.llm_snapshot().await;
    Json(ConfigResponse {
        llm_configured: runtime.is_configured(),
        model: runtime.settings().model.clone(),
        base_url: runtime.settings().base_url.clone(),
        revision: runtime.revision(),
        overridden_fields: runtime.overridden_fields(),
    })
}

pub fn build_router(state: AppState) -> Router {
    // Only the packaged Tauri origin and the two local Vite development
    // origins may drive this privileged localhost API.
    let cors = CorsLayer::new()
        .allow_origin([
            HeaderValue::from_static("tauri://localhost"),
            HeaderValue::from_static("http://tauri.localhost"),
            HeaderValue::from_static("http://localhost:5173"),
            HeaderValue::from_static("http://127.0.0.1:5173"),
        ])
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::CONTENT_TYPE,
            header::ACCEPT,
            HeaderName::from_static(API_TOKEN_HEADER),
        ]);

    let security = ApiSecurity::from_state(&state);

    Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(config))
        .route(
            "/api/settings",
            get(settings_endpoints::get_settings).post(settings_endpoints::save_settings_http),
        )
        // sessions
        .route(
            "/api/sessions",
            get(sessions::list_sessions).post(sessions::create_session),
        )
        .route(
            "/api/sessions/{sid}",
            get(sessions::get_session)
                .patch(sessions::update_session)
                .delete(sessions::delete_session),
        )
        .route(
            "/api/sessions/{sid}/events",
            get(sessions::list_audit_events),
        )
        .route("/api/sessions/{sid}/frames", get(sessions::list_frames))
        .route(
            "/api/sessions/{sid}/runs/{run_id}/reconcile-tool",
            get(sessions::list_tool_reconciliation).post(sessions::reconcile_tool),
        )
        .route(
            "/api/sessions/{sid}/stream-sse",
            axum::routing::post(sessions::stream_sse),
        )
        .route(
            "/api/sessions/{sid}/cancel",
            axum::routing::post(sessions::cancel_run),
        )
        .route(
            "/api/sessions/{sid}/compile",
            axum::routing::post(sessions::compile),
        )
        .route(
            "/api/sessions/{sid}/approve",
            axum::routing::post(sessions::approve_plan),
        )
        .route(
            "/api/sessions/{sid}/files",
            get(workspace_files::list_files),
        )
        .route(
            "/api/sessions/{sid}/files/{*path}",
            get(workspace_files::read_file).delete(workspace_files::delete_file),
        )
        // Agent Profiles + generic JobRuntime. Historical Bot routes remain compatibility aliases.
        .route(
            "/api/agent-profiles",
            get(bots::list_bots).post(bots::create_bot),
        )
        .route(
            "/api/agent-profiles/{bid}",
            axum::routing::patch(bots::update_bot).delete(bots::delete_bot),
        )
        .route("/api/bots", get(bots::list_bots).post(bots::create_bot))
        .route(
            "/api/bots/{bid}",
            axum::routing::patch(bots::update_bot).delete(bots::delete_bot),
        )
        .route(
            "/api/sessions/{sid}/bot-jobs",
            get(bots::list_jobs).post(bots::enqueue_job),
        )
        .route(
            "/api/sessions/{sid}/jobs",
            get(bots::list_jobs).post(bots::enqueue_job),
        )
        .route(
            "/api/sessions/{sid}/bot-jobs/reorder",
            axum::routing::post(bots::reorder_jobs),
        )
        .route(
            "/api/sessions/{sid}/jobs/reorder",
            axum::routing::post(bots::reorder_jobs),
        )
        .route(
            "/api/sessions/{sid}/bot-jobs/claim",
            axum::routing::post(bots::claim_job),
        )
        .route(
            "/api/sessions/{sid}/jobs/claim",
            axum::routing::post(bots::claim_job),
        )
        .route(
            "/api/bot-jobs/{jid}",
            axum::routing::patch(bots::edit_job).delete(bots::delete_job),
        )
        .route(
            "/api/jobs/{jid}",
            axum::routing::patch(bots::edit_job).delete(bots::delete_job),
        )
        .route(
            "/api/bot-jobs/{jid}/finish",
            axum::routing::post(bots::finish_job),
        )
        .route(
            "/api/jobs/{jid}/finish",
            axum::routing::post(bots::finish_job),
        )
        .route(
            "/api/bot-jobs/{jid}/claim",
            axum::routing::post(bots::claim_selected_job),
        )
        .route(
            "/api/jobs/{jid}/claim",
            axum::routing::post(bots::claim_selected_job),
        )
        // projects
        .route(
            "/api/projects",
            get(projects::list_projects).post(projects::create_project),
        )
        .route(
            "/api/projects/{pid}",
            get(projects::get_project)
                .patch(projects::patch_project)
                .delete(projects::delete_project),
        )
        .route(
            "/api/projects/{pid}/archive",
            axum::routing::post(projects::archive_project),
        )
        .route(
            "/api/projects/{pid}/unarchive",
            axum::routing::post(projects::unarchive_project),
        )
        // memories（Claim Store 治理 API）
        .route(
            "/api/memories",
            get(memories::list_memories).post(memories::create_memory),
        )
        .route(
            "/api/memories/{mid}",
            get(memories::get_memory)
                .patch(memories::edit_memory)
                .delete(memories::delete_memory),
        )
        .route(
            "/api/memories/{mid}/approve",
            axum::routing::post(memories::approve_memory),
        )
        .route(
            "/api/memories/{mid}/reject",
            axum::routing::post(memories::reject_memory),
        )
        .route("/api/memories/{mid}/history", get(memories::memory_history))
        // logs
        .route("/api/logs", get(logs::list_logs).delete(logs::delete_logs))
        .route("/api/logs/{id}", get(logs::get_log))
        // skills / templates
        .route("/api/skills", get(meta::list_skills))
        .route("/api/templates", get(meta::list_templates))
        .route("/api/templates/{id}", get(meta::get_template))
        // MCP
        .route("/api/mcp/{name}/tools", get(mcp_endpoints::mcp_tools))
        .with_state(state)
        .layer(middleware::from_fn_with_state(
            security,
            enforce_local_api_security,
        ))
        .layer(cors)
        .layer(TraceLayer::new_for_http())
}

/// 启动 HTTP 服务，收到 ctrl-c / SIGTERM 时优雅关闭。
pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> std::io::Result<()> {
    axum::serve(listener, build_router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, draining connections");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;

    fn request(path: &str, host: &str, origin: Option<&str>, token: Option<&str>) -> Request {
        let mut builder = Request::builder().uri(path).header(header::HOST, host);
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        if let Some(token) = token {
            builder = builder.header(API_TOKEN_HEADER, token);
        }
        builder.body(Body::empty()).expect("valid test request")
    }

    fn rejection_status(request: &Request, token: Option<&str>) -> Option<StatusCode> {
        let security = ApiSecurity {
            token: token.map(Arc::<str>::from),
        };
        security_rejection(request, &security).map(|(status, _)| status)
    }

    #[test]
    fn configured_token_rejects_missing_and_wrong_values_but_accepts_match() {
        let missing = request("/api/config", "127.0.0.1:17896", None, None);
        let wrong = request(
            "/api/config",
            "127.0.0.1:17896",
            Some("tauri://localhost"),
            Some("wrong"),
        );
        let valid = request(
            "/api/config",
            "localhost:17896",
            Some("tauri://localhost"),
            Some("secret-token"),
        );

        assert_eq!(
            rejection_status(&missing, Some("secret-token")),
            Some(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            rejection_status(&wrong, Some("secret-token")),
            Some(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(rejection_status(&valid, Some("secret-token")), None);
    }

    #[test]
    fn health_and_cli_mode_do_not_require_a_token() {
        let health = request("/api/health", "127.0.0.1:17896", None, None);
        let cli = request("/api/config", "localhost:17896", None, None);

        assert_eq!(rejection_status(&health, Some("secret-token")), None);
        assert_eq!(rejection_status(&cli, None), None);
    }

    #[test]
    fn malicious_host_and_origin_are_rejected() {
        let hostile_host = request(
            "/api/config",
            "deepseek-science.attacker.invalid",
            None,
            Some("secret-token"),
        );
        let hostile_origin = request(
            "/api/config",
            "127.0.0.1:17896",
            Some("http://localhost.attacker.invalid"),
            Some("secret-token"),
        );

        assert_eq!(
            rejection_status(&hostile_host, Some("secret-token")),
            Some(StatusCode::FORBIDDEN)
        );
        assert_eq!(
            rejection_status(&hostile_origin, Some("secret-token")),
            Some(StatusCode::FORBIDDEN)
        );
    }

    #[test]
    fn token_comparison_checks_content_and_length() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"samf"));
        assert!(!constant_time_eq(b"same", b"same-longer"));
    }
}
