//! memories 端点：GET /api/memories?entity= 、DELETE /api/memories/{id}。

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::state::AppState;

fn json_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    #[serde(default)]
    entity: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
}

#[derive(Serialize)]
pub struct MemoryItem {
    pub id: String,
    pub entity: String,
    pub scope: Option<String>,
    pub body: String,
    pub project_id: Option<String>,
    pub updated_at: String,
}

/// `GET /api/memories?entity=&project_id=`：列出记忆（profile + 指定 project）。
pub async fn list_memories(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<MemoryItem>>, (StatusCode, Json<Value>)> {
    let rows = state
        .memory
        .list(q.project_id.clone(), q.entity.clone())
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(Json(
        rows.into_iter()
            .map(|m| MemoryItem {
                id: m.id,
                entity: m.entity,
                scope: m.scope,
                body: m.body,
                project_id: m.project_id,
                updated_at: m.updated_at,
            })
            .collect(),
    ))
}

/// `DELETE /api/memories/{id}`。
pub async fn delete_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    match state.memory.delete(id.clone()).await {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(dss_db::DbError::NotFound(m)) => Err(json_error(StatusCode::NOT_FOUND, &m)),
        Err(e) => Err(json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
    }
}
