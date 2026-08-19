//! Bot Mode persistence API.
//!
//! A Bot is a durable identity. Sessions bind conversation context to that identity, while
//! bot_jobs form a revisioned, restart-safe execution queue.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db as dbq;
use crate::state::AppState;

type ApiError = (StatusCode, Json<Value>);

fn error(status: StatusCode, message: impl AsRef<str>) -> ApiError {
    (status, Json(json!({ "error": message.as_ref() })))
}

fn map_db_error(error_value: dss_db::DbError) -> ApiError {
    match error_value {
        dss_db::DbError::NotFound(message) => error(StatusCode::NOT_FOUND, message),
        dss_db::DbError::Conflict(message) => error(StatusCode::CONFLICT, message),
        other => error(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
    }
}

fn validate_text(value: &str, field: &str, max_chars: usize) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > max_chars {
        return Err(error(
            StatusCode::BAD_REQUEST,
            format!("{field} must contain 1 to {max_chars} characters"),
        ));
    }
    Ok(trimmed.to_owned())
}

fn validate_optional_text(
    value: Option<String>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>, ApiError> {
    value
        .map(|value| validate_text(&value, field, max_chars))
        .transpose()
}

#[derive(Debug, Deserialize)]
pub struct CreateBotRequest {
    name: String,
    #[serde(default = "default_role")]
    role: String,
    #[serde(default)]
    instructions: String,
    #[serde(default = "default_avatar")]
    avatar: String,
    #[serde(default = "default_color")]
    color: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

fn default_role() -> String {
    "Research assistant".into()
}

fn default_avatar() -> String {
    "🤖".into()
}

fn default_color() -> String {
    "#4D6BFE".into()
}

pub async fn list_bots(
    State(state): State<AppState>,
) -> Result<Json<Vec<dss_db::repo::BotRow>>, ApiError> {
    dbq::list_bots(&state.db)
        .await
        .map(Json)
        .map_err(map_db_error)
}

pub async fn create_bot(
    State(state): State<AppState>,
    Json(request): Json<CreateBotRequest>,
) -> Result<(StatusCode, Json<dss_db::repo::BotRow>), ApiError> {
    let name = validate_text(&request.name, "name", 80)?;
    let role = validate_text(&request.role, "role", 160)?;
    let instructions = if request.instructions.trim().is_empty() {
        String::new()
    } else {
        validate_text(&request.instructions, "instructions", 16_000)?
    };
    let avatar = validate_text(&request.avatar, "avatar", 8)?;
    let color = validate_color(&request.color)?;
    let project_id = request
        .project_id
        .or(Some(dss_db::DEFAULT_PROJECT_ID.to_string()));
    let model = validate_optional_text(request.model, "model", 200)?;
    let row = dbq::create_bot(
        &state.db,
        name,
        role,
        instructions,
        avatar,
        color,
        project_id,
        model,
    )
    .await
    .map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Debug, Deserialize)]
pub struct UpdateBotRequest {
    revision: i64,
    name: String,
    role: String,
    instructions: String,
    avatar: String,
    color: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    thinking_enabled: Option<bool>,
    #[serde(default)]
    thinking_effort: Option<String>,
    #[serde(default = "default_enabled")]
    enabled: bool,
}

fn default_enabled() -> bool {
    true
}

pub async fn update_bot(
    State(state): State<AppState>,
    Path(bot_id): Path<String>,
    Json(request): Json<UpdateBotRequest>,
) -> Result<Json<dss_db::repo::BotRow>, ApiError> {
    if request.revision < 1 {
        return Err(error(StatusCode::BAD_REQUEST, "revision must be positive"));
    }
    let effort = request
        .thinking_effort
        .map(|value| value.trim().to_ascii_lowercase());
    if effort
        .as_deref()
        .is_some_and(|value| !matches!(value, "low" | "high" | "max"))
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "thinking_effort must be low, high, or max",
        ));
    }
    let row = dbq::update_bot(
        &state.db,
        bot_id,
        request.revision,
        validate_text(&request.name, "name", 80)?,
        validate_text(&request.role, "role", 160)?,
        if request.instructions.trim().is_empty() {
            String::new()
        } else {
            validate_text(&request.instructions, "instructions", 16_000)?
        },
        validate_text(&request.avatar, "avatar", 8)?,
        validate_color(&request.color)?,
        request.project_id,
        validate_optional_text(request.model, "model", 200)?,
        request.thinking_enabled,
        effort,
        request.enabled,
    )
    .await
    .map_err(map_db_error)?;
    Ok(Json(row))
}

pub async fn delete_bot(
    State(state): State<AppState>,
    Path(bot_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    dbq::delete_bot(&state.db, bot_id)
        .await
        .map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn validate_color(value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if trimmed.len() == 7
        && trimmed.starts_with('#')
        && trimmed[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        Ok(trimmed.to_ascii_uppercase())
    } else {
        Err(error(
            StatusCode::BAD_REQUEST,
            "color must be a six-digit hex color",
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct EnqueueJobRequest {
    #[serde(default)]
    id: Option<String>,
    bot_id: String,
    prompt: String,
    #[serde(default)]
    plan_mode: bool,
}

pub async fn list_jobs(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<dss_db::repo::BotJobRow>>, ApiError> {
    dbq::list_bot_jobs(&state.db, session_id)
        .await
        .map(Json)
        .map_err(map_db_error)
}

pub async fn enqueue_job(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(request): Json<EnqueueJobRequest>,
) -> Result<(StatusCode, Json<dss_db::repo::BotJobRow>), ApiError> {
    let prompt = validate_text(&request.prompt, "prompt", 100_000)?;
    let requested_id = request.id.map(|id| validate_job_id(&id)).transpose()?;
    let job = dbq::enqueue_bot_job(
        &state.db,
        requested_id,
        request.bot_id,
        session_id,
        prompt,
        request.plan_mode,
    )
    .await
    .map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(job)))
}

fn validate_job_id(value: &str) -> Result<String, ApiError> {
    let trimmed = value.trim();
    if (4..=80).contains(&trimmed.len())
        && trimmed
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        Ok(trimmed.to_owned())
    } else {
        Err(error(StatusCode::BAD_REQUEST, "invalid bot job id"))
    }
}

#[derive(Debug, Deserialize)]
pub struct EditJobRequest {
    revision: i64,
    prompt: String,
    #[serde(default)]
    plan_mode: bool,
}

pub async fn edit_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<EditJobRequest>,
) -> Result<Json<dss_db::repo::BotJobRow>, ApiError> {
    let prompt = validate_text(&request.prompt, "prompt", 100_000)?;
    dbq::edit_bot_job(
        &state.db,
        job_id,
        request.revision,
        prompt,
        request.plan_mode,
    )
    .await
    .map(Json)
    .map_err(map_db_error)
}

#[derive(Debug, Deserialize)]
pub struct DeleteJobRequest {
    revision: i64,
}

pub async fn delete_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<DeleteJobRequest>,
) -> Result<StatusCode, ApiError> {
    dbq::delete_bot_job(&state.db, job_id, request.revision)
        .await
        .map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct ReorderJobsRequest {
    ordered_ids: Vec<String>,
}

pub async fn reorder_jobs(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(request): Json<ReorderJobsRequest>,
) -> Result<Json<Vec<dss_db::repo::BotJobRow>>, ApiError> {
    dbq::reorder_bot_jobs(&state.db, session_id, request.ordered_ids)
        .await
        .map(Json)
        .map_err(map_db_error)
}

#[derive(Debug, Deserialize)]
pub struct ClaimJobRequest {
    run_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ClaimSelectedJobRequest {
    revision: i64,
    run_id: String,
}

pub async fn claim_selected_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<ClaimSelectedJobRequest>,
) -> Result<Json<dss_db::repo::BotJobRow>, ApiError> {
    let run_id = validate_text(&request.run_id, "run_id", 128)?;
    dbq::claim_bot_job(&state.db, job_id, request.revision, run_id)
        .await
        .map(Json)
        .map_err(map_db_error)
}

pub async fn claim_job(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Json(request): Json<ClaimJobRequest>,
) -> Result<Json<Option<dss_db::repo::BotJobRow>>, ApiError> {
    let run_id = validate_text(&request.run_id, "run_id", 128)?;
    dbq::claim_next_bot_job(&state.db, session_id, run_id)
        .await
        .map(Json)
        .map_err(map_db_error)
}

#[derive(Debug, Deserialize)]
pub struct FinishJobRequest {
    run_id: String,
    succeeded: bool,
    #[serde(default)]
    error: Option<String>,
}

pub async fn finish_job(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Json(request): Json<FinishJobRequest>,
) -> Result<Json<dss_db::repo::BotJobRow>, ApiError> {
    let run_id = validate_text(&request.run_id, "run_id", 128)?;
    let error_message = request
        .error
        .map(|value| value.chars().take(2_000).collect::<String>());
    dbq::finish_bot_job(&state.db, job_id, run_id, request.succeeded, error_message)
        .await
        .map(Json)
        .map_err(map_db_error)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use dss_core::{
        settings::{LogSettings, MemorySettings, ServerSettings},
        LlmEnvOverrides, LlmSettings, Settings, ThinkingSettings,
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "dss-bot-mode-test-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&path).expect("create bot mode test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn app() -> (axum::Router, TestDirectory) {
        let directory = TestDirectory::new();
        let state = crate::state::build_state(Settings {
            data_dir: directory.0.clone(),
            data_dir_is_default: false,
            max_iterations: 100,
            thinking: ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: MemorySettings::default(),
            log: LogSettings::default(),
            api_keys: HashMap::new(),
        })
        .await
        .expect("build bot mode test state");
        (crate::build_router(state), directory)
    }

    async fn json_request(
        app: &axum::Router,
        method: &str,
        uri: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("host", "localhost")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("build request"),
            )
            .await
            .expect("route bot request");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_000_000)
            .await
            .expect("read response body");
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("parse response json")
        };
        (status, value)
    }

    #[tokio::test]
    async fn bot_identity_session_and_durable_job_flow_survives_api_boundaries() {
        let (app, _directory) = app().await;
        let (status, bot) = json_request(
            &app,
            "POST",
            "/api/bots",
            json!({
                "name": "Nova",
                "role": "Literature scout",
                "instructions": "Prefer primary sources.",
                "avatar": "🔭",
                "color": "#4d6bfe"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let bot_id = bot["id"].as_str().expect("bot id");
        assert_eq!(bot["revision"], 1);

        let (status, session) = json_request(
            &app,
            "POST",
            "/api/sessions",
            json!({"project_id": "proj_default", "bot_id": bot_id}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let session_id = session["id"].as_str().expect("session id");
        assert_eq!(session["bot_id"], bot_id);

        let (status, job) = json_request(
            &app,
            "POST",
            &format!("/api/sessions/{session_id}/bot-jobs"),
            json!({
                "id": "job-client-stable",
                "bot_id": bot_id,
                "prompt": "Find the newest primary source",
                "plan_mode": true
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(job["id"], "job-client-stable");

        let (status, claimed) = json_request(
            &app,
            "POST",
            "/api/bot-jobs/job-client-stable/claim",
            json!({"revision": 1, "run_id": "run-bot-e2e"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(claimed["status"], "running");

        let (status, completed) = json_request(
            &app,
            "POST",
            "/api/bot-jobs/job-client-stable/finish",
            json!({"run_id": "run-bot-e2e", "succeeded": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(completed["status"], "completed");

        let (status, restored) = json_request(
            &app,
            "GET",
            &format!("/api/sessions/{session_id}"),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(restored["bot_id"], bot_id);
    }
}
