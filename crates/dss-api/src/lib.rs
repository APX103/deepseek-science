//! dss-api: axum HTTP 路由与服务启动。

pub mod db;
pub mod logs;
pub mod mcp_endpoints;
pub mod memories;
pub mod meta;
pub mod projects;
pub mod sessions;
pub mod state;

use axum::{routing::get, Json, Router};
use serde::Serialize;
use state::AppState;

pub use state::build_state;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<HealthResponse> {
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
}

async fn config(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Json<ConfigResponse> {
    Json(ConfigResponse {
        llm_configured: state.llm.is_some(),
        model: state.settings.llm.model.clone(),
        base_url: state.settings.llm.base_url.clone(),
    })
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(config))
        // sessions
        .route(
            "/api/sessions",
            get(sessions::list_sessions).post(sessions::create_session),
        )
        .route(
            "/api/sessions/{sid}",
            get(sessions::get_session).delete(sessions::delete_session),
        )
        .route(
            "/api/sessions/{sid}/stream-sse",
            axum::routing::post(sessions::stream_sse),
        )
        .route(
            "/api/sessions/{sid}/compile",
            axum::routing::post(sessions::compile),
        )
        .route(
            "/api/sessions/{sid}/approve",
            axum::routing::post(sessions::approve_plan),
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
        // memories
        .route(
            "/api/memories",
            get(memories::list_memories),
        )
        .route(
            "/api/memories/{mid}",
            axum::routing::delete(memories::delete_memory),
        )
        // logs
        .route(
            "/api/logs",
            get(logs::list_logs).delete(logs::delete_logs),
        )
        .route("/api/logs/{id}", get(logs::get_log))
        // skills / templates
        .route("/api/skills", get(meta::list_skills))
        .route(
            "/api/templates",
            get(meta::list_templates),
        )
        .route("/api/templates/{id}", get(meta::get_template))
        // MCP
        .route("/api/mcp/{name}/tools", get(mcp_endpoints::mcp_tools))
        .with_state(state)
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
