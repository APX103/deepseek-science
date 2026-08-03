//! projects 端点（按 api-contract）：
//! GET /api/projects?archived=false、POST /api/projects、PATCH /api/projects/{pid}、
//! POST /api/projects/{pid}/archive、POST /api/projects/{pid}/unarchive、
//! DELETE /api/projects/{pid}?force=false、GET /api/projects/{pid}。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::db as dbq;
use crate::state::AppState;

fn json_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

fn map_db_err(e: dss_db::DbError) -> (StatusCode, Json<Value>) {
    match e {
        dss_db::DbError::NotFound(m) => json_error(StatusCode::NOT_FOUND, &m),
        dss_db::DbError::Conflict(m) => json_error(StatusCode::CONFLICT, &m),
        e => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    archived: Option<bool>,
}

/// `GET /api/projects?archived=false`
pub async fn list_projects(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<dss_db::repo::ProjectRow>>, (StatusCode, Json<Value>)> {
    // 默认项目置顶由 repo SQL 的 ORDER BY (id='proj_default') 保证。
    let include = q.archived.unwrap_or(false);
    let rows = dbq::list_projects(&state.db, include).await.map_err(map_db_err)?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct CreateProjectReq {
    name: String,
    #[serde(default)]
    description: Option<String>,
}

/// `POST /api/projects`：建项目 proj_<8hex>。
pub async fn create_project(
    State(state): State<AppState>,
    Json(req): Json<CreateProjectReq>,
) -> Result<(StatusCode, Json<dss_db::repo::ProjectRow>), (StatusCode, Json<Value>)> {
    let row = dbq::create_project(&state.db, req.name, req.description)
        .await
        .map_err(map_db_err)?;
    Ok((StatusCode::CREATED, Json(row)))
}

#[derive(Deserialize)]
pub struct PatchProjectReq {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    last_session_id: Option<String>,
}

/// `PATCH /api/projects/{pid}`：改名/描述/last_session_id。
pub async fn patch_project(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    Json(req): Json<PatchProjectReq>,
) -> Result<Json<dss_db::repo::ProjectRow>, (StatusCode, Json<Value>)> {
    let row = dbq::update_project(&state.db, pid, req.name, req.description, req.last_session_id)
        .await
        .map_err(map_db_err)?;
    Ok(Json(row))
}

/// `POST /api/projects/{pid}/archive`（默认项目 400）。
pub async fn archive_project(
    State(state): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<dss_db::repo::ProjectRow>, (StatusCode, Json<Value>)> {
    dbq::set_project_archived(&state.db, pid, true).await.map_err(map_db_err).map(Json)
}

/// `POST /api/projects/{pid}/unarchive`。
pub async fn unarchive_project(
    State(state): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<dss_db::repo::ProjectRow>, (StatusCode, Json<Value>)> {
    dbq::set_project_archived(&state.db, pid, false).await.map_err(map_db_err).map(Json)
}

#[derive(Deserialize)]
pub struct DeleteQuery {
    #[serde(default)]
    force: Option<bool>,
}

/// `DELETE /api/projects/{pid}?force=false`：默认项目 400；有 session 非 force → 409。
pub async fn delete_project(
    State(state): State<AppState>,
    Path(pid): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let force = q.force.unwrap_or(false);
    dbq::delete_project(&state.db, pid, force).await.map_err(map_db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/projects/{pid}`：项目详情 + 会话列表。
pub async fn get_project(
    State(state): State<AppState>,
    Path(pid): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (proj, sessions) = dbq::get_project_detail(&state.db, pid).await.map_err(map_db_err)?;
    Ok(Json(json!({ "project": proj, "sessions": sessions })))
}
