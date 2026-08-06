//! memories 端点：Claim Store 治理 API。
//!
//! - GET    /api/memories?entity=&project_id=&status=  列出（可按 status 过滤审批队列）
//! - POST   /api/memories                                显式 remember（origin=explicit, status=active）
//! - GET    /api/memories/{id}                           单条详情
//! - PATCH  /api/memories/{id}                           编辑 body（同版本订正）
//! - DELETE /api/memories/{id}                           软删除（保留审计）
//! - POST   /api/memories/{id}/approve                   candidate → active
//! - POST   /api/memories/{id}/reject                    candidate → deleted
//! - GET    /api/memories/{id}/history                   时间线（memory_events）

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use dss_db::repo::{MemoryEventRow, MemoryRow};
use serde::Deserialize;
use serde_json::{json, Value};

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
    entity: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    /// 可选：active | candidate | superseded | expired | deleted。不传 = 不过滤。
    #[serde(default)]
    status: Option<String>,
}

/// `GET /api/memories`：列出记忆（profile + 指定 project，可按 status 过滤）。
/// 默认排除 superseded/deleted（除非显式查这些 status）。
pub async fn list_memories(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<MemoryRow>>, (StatusCode, Json<Value>)> {
    use dss_db::repo::MemoryFilter;
    let rows = state
        .memory
        .list_filtered(MemoryFilter {
            project_id: q.project_id.as_deref(),
            entity: q.entity.as_deref(),
            status: q.status.as_deref(),
        })
        .await
        .map_err(map_db_err)?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
pub struct CreateBody {
    pub body: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub claim_type: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
}

/// `POST /api/memories`：显式 remember（origin=explicit, status=active, source_hash 自动计算）。
pub async fn create_memory(
    State(state): State<AppState>,
    Json(b): Json<CreateBody>,
) -> Result<(StatusCode, Json<MemoryRow>), (StatusCode, Json<Value>)> {
    use dss_db::repo::NewMemory;
    if b.body.trim().is_empty() {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "body must not be empty",
        ));
    }
    let id = dss_memory::gen_id("mem_");
    let hash = dss_memory::memory_hash(&b.body);
    let scope = b.scope.as_deref().or(if b.project_id.is_some() {
        Some("project")
    } else {
        Some("profile")
    });
    let row = state
        .memory
        .append_full(NewMemory {
            id: &id,
            body: &b.body,
            scope,
            project_id: b.project_id.as_deref(),
            confidence: b.confidence,
            claim_type: b.claim_type.as_deref(),
            status: Some("active"),
            origin: Some(dss_memory::Origin::Explicit.as_str()),
            source_hash: Some(&hash),
            ..Default::default()
        })
        .await
        .map_err(map_db_err)?;
    Ok((StatusCode::CREATED, Json(row)))
}

/// `GET /api/memories/{id}`：单条详情。
pub async fn get_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<MemoryRow>, (StatusCode, Json<Value>)> {
    state
        .memory
        .get(id)
        .await
        .map_err(map_db_err)?
        .map(Json)
        .ok_or_else(|| json_error(StatusCode::NOT_FOUND, "memory not found"))
}

#[derive(Deserialize)]
pub struct PatchBody {
    pub body: String,
}

/// `PATCH /api/memories/{id}`：编辑 body（同版本订正，source_hash 重算）。
pub async fn edit_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<PatchBody>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    if b.body.trim().is_empty() {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "body must not be empty",
        ));
    }
    state
        .memory
        .edit_body(id, b.body, Some("user"))
        .await
        .map_err(map_db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /api/memories/{id}`：软删除（status=deleted + deleted_at，保留审计）。
pub async fn delete_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    state.memory.soft_delete(id).await.map_err(map_db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/memories/{id}/approve`：candidate → active。
pub async fn approve_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    state
        .memory
        .update_status(id, "active", Some("user"))
        .await
        .map_err(map_db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/memories/{id}/reject`：candidate → deleted。
pub async fn reject_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    state
        .memory
        .update_status(id, "deleted", Some("user"))
        .await
        .map_err(map_db_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/memories/{id}/history`：记忆生命周期时间线（memory_events）。
pub async fn memory_history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<MemoryEventRow>>, (StatusCode, Json<Value>)> {
    // 先确认记忆存在
    let exists = state.memory.get(id.clone()).await.map_err(map_db_err)?;
    if exists.is_none() {
        return Err(json_error(StatusCode::NOT_FOUND, "memory not found"));
    }
    let events = state.memory.list_events(id).await.map_err(map_db_err)?;
    Ok(Json(events))
}
