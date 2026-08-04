//! dss-observability: LogStore（系统 + agent 日志持久化）。
//!
//! P4b：system 日志经 `log_system` helper 显式写（关键点）；agent 日志由 dss-api
//! 在 stream_sse 把 AgentEvent 结构化写入。完整 tracing Layer / mpsc 批量 DEFER。

use std::sync::Arc;

use dss_db::{repo::LogRow, DbError, DbPool};

pub struct LogStore {
    pool: Arc<DbPool>,
}

/// 写一条日志的参数（builder 风格，便于 agent/system 两类共用）。
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub level: String,
    pub source: String, // "system" | "agent"
    pub kind: String,
    pub session_id: Option<String>,
    pub frame_id: Option<String>,
    pub iteration: Option<i64>,
    pub message: String,
    pub detail: Option<serde_json::Value>,
}

impl LogStore {
    pub fn new(pool: Arc<DbPool>) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &Arc<DbPool> {
        &self.pool
    }

    /// 写一条日志（异步、conn.interact）。
    pub async fn append(&self, entry: LogEntry) -> Result<i64, DbError> {
        let detail_str = entry
            .detail
            .as_ref()
            .map(|v| serde_json::to_string(v).unwrap_or_default());
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| {
            dss_db::repo::append_log(
                c,
                &entry.level,
                &entry.source,
                &entry.kind,
                entry.session_id.as_deref(),
                entry.frame_id.as_deref(),
                entry.iteration,
                &entry.message,
                detail_str.as_deref(),
            )
        })
        .await
        .map_err(|e| DbError::Other(format!("log append interact: {e:?}")))?
    }

    pub async fn list(&self, f: dss_db::repo::LogFilter) -> Result<(Vec<LogRow>, i64), DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| dss_db::repo::list_logs(c, &f))
            .await
            .map_err(|e| DbError::Other(format!("log list interact: {e:?}")))?
    }

    pub async fn get(&self, id: i64) -> Result<Option<LogRow>, DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| dss_db::repo::get_log(c, id))
            .await
            .map_err(|e| DbError::Other(format!("log get interact: {e:?}")))?
    }

    pub async fn delete(&self, before: Option<String>) -> Result<i64, DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| dss_db::repo::delete_logs(c, before.as_deref()))
            .await
            .map_err(|e| DbError::Other(format!("log delete interact: {e:?}")))?
    }
}
