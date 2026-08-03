//! MemoryStore：记忆持久化（经 dss-db Pool；conn.interact 内部 spawn_blocking）。

use std::sync::Arc;

use dss_db::{repo::MemoryRow, DbError, DbPool};

pub struct MemoryStore {
    pool: Arc<DbPool>,
}

impl MemoryStore {
    pub fn new(pool: Arc<DbPool>) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &Arc<DbPool> {
        &self.pool
    }

    pub async fn append(&self, body: String, scope: Option<String>, project_id: Option<String>) -> Result<MemoryRow, DbError> {
        let id = format!("mem_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| dss_db::repo::append_memory(c, &id, &body, scope.as_deref(), project_id.as_deref()))
            .await
            .map_err(|e| DbError::Other(format!("append interact: {e:?}")))?
    }

    pub async fn list(&self, project_id: Option<String>, entity: Option<String>) -> Result<Vec<MemoryRow>, DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| dss_db::repo::list_memories(c, project_id.as_deref(), entity.as_deref()))
            .await
            .map_err(|e| DbError::Other(format!("list interact: {e:?}")))?
    }

    pub async fn delete(&self, id: String) -> Result<(), DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| dss_db::repo::delete_memory(c, &id))
            .await
            .map_err(|e| DbError::Other(format!("delete interact: {e:?}")))?
    }

    /// 取候选记忆（profile + project），用于 BM25 recall（在内存里排序）。
    pub async fn candidates(&self, project_id: Option<String>) -> Result<Vec<MemoryRow>, DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| dss_db::repo::candidate_memories(c, project_id.as_deref()))
            .await
            .map_err(|e| DbError::Other(format!("candidates interact: {e:?}")))?
    }
}
