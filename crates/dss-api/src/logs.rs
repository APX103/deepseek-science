//! 日志端点：GET /api/logs、GET /api/logs/{id}、DELETE /api/logs?before=。

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
    pub session_id: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Serialize)]
pub struct LogItem {
    pub id: i64,
    pub ts: String,
    pub level: String,
    pub source: String,
    pub kind: String,
    pub session_id: Option<String>,
    pub frame_id: Option<String>,
    pub iteration: Option<i64>,
    pub message: String,
    pub detail: Option<Value>,
}

#[derive(Serialize)]
pub struct ListResp {
    pub logs: Vec<LogItem>,
    pub total: i64,
}

fn row_to_item(r: dss_db::repo::LogRow) -> LogItem {
    let detail = r.detail.and_then(|s| serde_json::from_str(&s).ok());
    LogItem {
        id: r.id,
        ts: r.ts,
        level: r.level,
        source: r.source,
        kind: r.kind,
        session_id: r.session_id,
        frame_id: r.frame_id,
        iteration: r.iteration,
        message: r.message,
        detail,
    }
}

/// `GET /api/logs`：按 query 过滤 + 分页。
pub async fn list_logs(
    State(state): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResp>, (StatusCode, Json<Value>)> {
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let offset = q.offset.unwrap_or(0).max(0);
    let f = dss_db::repo::LogFilter {
        session_id: q.session_id,
        source: q.source,
        level: q.level,
        kind: q.kind,
        since: q.since,
        until: q.until,
        limit,
        offset,
    };
    let (rows, total) = state
        .logs
        .list(f)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(Json(ListResp {
        logs: rows.into_iter().map(row_to_item).collect(),
        total,
    }))
}

/// `GET /api/logs/{id}`：单条详情。
pub async fn get_log(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<LogItem>, (StatusCode, Json<Value>)> {
    let row = state
        .logs
        .get(id)
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
        .ok_or_else(|| json_error(StatusCode::NOT_FOUND, "log not found"))?;
    Ok(Json(row_to_item(row)))
}

#[derive(Deserialize, Default)]
pub struct DeleteQuery {
    #[serde(default)]
    pub before: Option<String>,
}

/// `DELETE /api/logs?before=`：清理（before 之前；缺省全清）。
pub async fn delete_logs(
    State(state): State<AppState>,
    Query(q): Query<DeleteQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let n = state
        .logs
        .delete(q.before.clone())
        .await
        .map_err(|e| json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
    Ok(Json(json!({ "deleted": n })))
}
