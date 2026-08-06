//! MemoryStore：记忆持久化（经 dss-db Pool；conn.interact 内部 spawn_blocking）。
//!
//! 这是 L2 Claim Store 的访问门面。所有写入路径（append_full / consolidate / approve / reject /
//! supersede / soft_delete）都会附带写一条 memory_events 审计记录。
//!
//! 注意：conn.interact 的闭包需 'static（spawn_blocking），因此所有跨闭包数据都先克隆成 owned。

use std::sync::{Arc, RwLock};

use dss_db::repo::{
    self, list_memories_filtered, list_memory_events, MemoryEventRow, MemoryFilter, MemoryRow,
    NewMemory,
};
use dss_db::{Connection, DbError, DbPool};

use crate::bm25::RecallIndex;
use crate::types::{gen_id, memory_hash, EvidenceRef, Origin};

pub struct MemoryStore {
    pool: Arc<DbPool>,
    /// 懒加载的召回倒排索引。任何写入后置 None（下次 recall 重建）。
    /// 本地应用记忆量小，写入失效比增量维护倒排表更简单可靠。
    index: RwLock<Option<RecallIndex>>,
}

impl MemoryStore {
    pub fn new(pool: Arc<DbPool>) -> Self {
        Self {
            pool,
            index: RwLock::new(None),
        }
    }

    pub fn pool(&self) -> &Arc<DbPool> {
        &self.pool
    }

    fn interact_err(e: impl std::fmt::Debug) -> DbError {
        DbError::Other(format!("memory interact: {e:?}"))
    }

    /// 写入路径后调用：失效缓存索引，下次 recall 重建。
    fn invalidate_index(&self) {
        if let Ok(mut guard) = self.index.write() {
            *guard = None;
        }
    }

    // ----------------- 读 -----------------

    pub async fn get(&self, id: String) -> Result<Option<MemoryRow>, DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| repo::get_memory(c, &id))
            .await
            .map_err(Self::interact_err)?
    }

    pub async fn list(
        &self,
        project_id: Option<String>,
        entity: Option<String>,
    ) -> Result<Vec<MemoryRow>, DbError> {
        self.list_filtered(MemoryFilter {
            project_id: project_id.as_deref(),
            entity: entity.as_deref(),
            status: None,
        })
        .await
    }

    pub async fn list_filtered(&self, f: MemoryFilter<'_>) -> Result<Vec<MemoryRow>, DbError> {
        let pid = f.project_id.map(str::to_owned);
        let ent = f.entity.map(str::to_owned);
        let status = f.status.map(str::to_owned);
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| {
            list_memories_filtered(
                c,
                MemoryFilter {
                    project_id: pid.as_deref(),
                    entity: ent.as_deref(),
                    status: status.as_deref(),
                },
            )
        })
        .await
        .map_err(Self::interact_err)?
    }

    /// 取候选记忆（profile + project），用于 BM25 recall。
    pub async fn candidates(&self, project_id: Option<String>) -> Result<Vec<MemoryRow>, DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| repo::candidate_memories(c, project_id.as_deref()))
            .await
            .map_err(Self::interact_err)?
    }

    /// project 是否存在（用于写入前 FK 防御：无效 project_id 降级为 profile，避免静默丢记忆）。
    pub async fn project_exists(&self, project_id: String) -> Result<bool, DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        let exists = conn
            .interact(move |c| repo::get_project(c, &project_id).map(|r| r.is_some()))
            .await
            .map_err(Self::interact_err)?;
        Ok(exists.unwrap_or(false))
    }

    /// 用持久化倒排索引召回（懒加载 + 写入失效）。
    /// 返回 (id, score) 排序结果。只索引 active 记忆（candidate/superseded/deleted 不召回）。
    /// 调用方需自行用 id 去 get() 取完整行（通常 top-N 很小）。
    pub async fn recall_indexed(
        &self,
        query: &str,
        project_id: Option<&str>,
        top_n: usize,
    ) -> Result<Vec<(String, f64)>, DbError> {
        // 懒加载：索引为 None 时从 DB 重建（只含 active）。
        let need_build = self.index.read().map(|g| g.is_none()).unwrap_or(true);
        if need_build {
            let pid = project_id.map(str::to_owned);
            let cands = self.candidates(pid).await?;
            // 只索引 active（recallable）。
            let active: Vec<MemoryRow> =
                cands.into_iter().filter(|m| m.status == "active").collect();
            let idx = RecallIndex::build(&active);
            if let Ok(mut guard) = self.index.write() {
                *guard = Some(idx);
            }
        }
        // 用缓存索引查询。
        let guard = self
            .index
            .read()
            .map_err(|e| DbError::Other(format!("memory index read lock: {e}")))?;
        if let Some(idx) = guard.as_ref() {
            Ok(idx.search(query, top_n))
        } else {
            Ok(Vec::new())
        }
    }

    /// 精确查 source_hash（去重用）。
    pub async fn find_by_hash(
        &self,
        hash: String,
        project_id: Option<String>,
    ) -> Result<Vec<MemoryRow>, DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| repo::find_by_source_hash(c, &hash, project_id.as_deref()))
            .await
            .map_err(Self::interact_err)?
    }

    pub async fn list_events(&self, memory_id: String) -> Result<Vec<MemoryEventRow>, DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| list_memory_events(c, &memory_id))
            .await
            .map_err(Self::interact_err)?
    }

    // ----------------- 写 -----------------

    /// 旧 API：简单 append（status=active, claim_type=note, origin=auto）。保留向后兼容。
    pub async fn append(
        &self,
        body: String,
        scope: Option<String>,
        project_id: Option<String>,
    ) -> Result<MemoryRow, DbError> {
        let id = gen_id("mem_");
        let hash = memory_hash(&body);
        self.append_full(NewMemory {
            id: &id,
            body: &body,
            scope: scope.as_deref(),
            project_id: project_id.as_deref(),
            status: Some("active"),
            origin: Some(Origin::Auto.as_str()),
            source_hash: Some(&hash),
            ..Default::default()
        })
        .await
    }

    /// 完整写入：带 status/claim_type/origin/evidence/source_hash。
    /// 写入成功后追加 created 事件。
    pub async fn append_full(&self, m: NewMemory<'_>) -> Result<MemoryRow, DbError> {
        // interact 闭包需 'static，先把所有引用数据收集成 owned。
        let owned = OwnedNewMemory::from(&m);
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        let row = conn
            .interact(move |c| owned.apply(c))
            .await
            .map_err(Self::interact_err)??;
        self.record_event(&conn, &row.id, "created", None, None)
            .await?;
        self.invalidate_index();
        Ok(row)
    }

    /// 软删除（status=deleted + deleted_at），保留审计。
    pub async fn soft_delete(&self, id: String) -> Result<(), DbError> {
        let id_for_del = id.clone();
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| repo::soft_delete_memory(c, &id_for_del))
            .await
            .map_err(Self::interact_err)??;
        self.record_event(&conn, &id, "deleted", Some("user"), None)
            .await?;
        self.invalidate_index();
        Ok(())
    }

    /// 硬删除（仅清理用，日常请用 soft_delete）。
    pub async fn delete(&self, id: String) -> Result<(), DbError> {
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        let r = conn
            .interact(move |c| repo::delete_memory(c, &id))
            .await
            .map_err(Self::interact_err)?;
        self.invalidate_index();
        r
    }

    /// 标记 old 被 new 替代。
    pub async fn supersede(&self, old_id: String, new_id: String) -> Result<(), DbError> {
        let oid_for = old_id.clone();
        let nid_for = new_id.clone();
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| repo::supersede_memory(c, &oid_for, &nid_for))
            .await
            .map_err(Self::interact_err)??;
        let detail = serde_json::json!({ "by": new_id }).to_string();
        self.record_event(&conn, &old_id, "superseded", Some("system"), Some(&detail))
            .await?;
        self.invalidate_index();
        Ok(())
    }

    /// 更新状态（approve/reject/expire 等）。
    pub async fn update_status(
        &self,
        id: String,
        status: &str,
        actor: Option<&str>,
    ) -> Result<(), DbError> {
        let id_for = id.clone();
        let status_for = status.to_owned();
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| repo::update_memory_status(c, &id_for, &status_for))
            .await
            .map_err(Self::interact_err)??;
        // approve/reject 用专门的事件名，其余用 status 名
        let event = match status {
            "active" => "approved",
            "deleted" => "rejected",
            other => other,
        };
        self.record_event(&conn, &id, event, actor, None).await?;
        self.invalidate_index();
        Ok(())
    }

    /// 编辑 body（同版本订正）；source_hash 重算。新版本应由调用方走 supersede。
    pub async fn edit_body(
        &self,
        id: String,
        body: String,
        actor: Option<&str>,
    ) -> Result<(), DbError> {
        let hash = memory_hash(&body);
        let id_for = id.clone();
        let body_for = body.clone();
        let hash_for = hash.clone();
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| repo::update_memory_body(c, &id_for, &body_for, Some(&hash_for)))
            .await
            .map_err(Self::interact_err)??;
        let detail = serde_json::json!({ "new_hash": hash }).to_string();
        self.record_event(&conn, &id, "edited", actor, Some(&detail))
            .await?;
        self.invalidate_index();
        Ok(())
    }

    /// 批量更新召回时间戳（recall 命中后调用，供 retention 判断使用率）。
    pub async fn touch_surfaced(&self, ids: Vec<String>) -> Result<(), DbError> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.pool.get().await.map_err(DbError::Pool)?;
        conn.interact(move |c| repo::touch_surfaced(c, &ids))
            .await
            .map_err(Self::interact_err)?
    }

    async fn record_event(
        &self,
        conn: &dss_db::ConnObj,
        memory_id: &str,
        event_type: &str,
        actor: Option<&str>,
        detail: Option<&str>,
    ) -> Result<(), DbError> {
        let id = gen_id("mev_");
        let mid = memory_id.to_owned();
        let et = event_type.to_owned();
        let ac = actor.map(str::to_owned);
        let de = detail.map(str::to_owned);
        conn.interact(move |c| {
            repo::append_memory_event(c, &id, &mid, &et, ac.as_deref(), de.as_deref())
        })
        .await
        .map_err(Self::interact_err)?
    }
}

/// NewMemory 的 owned 镜像：可 move 进 'static interact 闭包。
struct OwnedNewMemory {
    id: String,
    body: String,
    scope: Option<String>,
    project_id: Option<String>,
    confidence: Option<f64>,
    claim_type: Option<String>,
    status: Option<String>,
    origin: Option<String>,
    evidence_refs: Option<String>,
    source_hash: Option<String>,
    valid_until: Option<String>,
}

impl OwnedNewMemory {
    fn from(m: &NewMemory<'_>) -> Self {
        Self {
            id: m.id.to_owned(),
            body: m.body.to_owned(),
            scope: m.scope.map(str::to_owned),
            project_id: m.project_id.map(str::to_owned),
            confidence: m.confidence,
            claim_type: m.claim_type.map(str::to_owned),
            status: m.status.map(str::to_owned),
            origin: m.origin.map(str::to_owned),
            evidence_refs: m.evidence_refs.map(str::to_owned),
            source_hash: m.source_hash.map(str::to_owned),
            valid_until: m.valid_until.map(str::to_owned),
        }
    }

    fn apply(self, c: &Connection) -> Result<MemoryRow, DbError> {
        repo::append_memory_full(
            c,
            NewMemory {
                id: &self.id,
                body: &self.body,
                scope: self.scope.as_deref(),
                project_id: self.project_id.as_deref(),
                confidence: self.confidence,
                claim_type: self.claim_type.as_deref(),
                status: self.status.as_deref(),
                origin: self.origin.as_deref(),
                evidence_refs: self.evidence_refs.as_deref(),
                source_hash: self.source_hash.as_deref(),
                valid_until: self.valid_until.as_deref(),
            },
        )
    }
}

/// 把 EvidenceRef 列表序列化成 evidence_refs 列存储的 JSON 字符串。
pub fn evidence_refs_json(refs: &[EvidenceRef]) -> String {
    serde_json::to_string(refs).unwrap_or_else(|_| "[]".into())
}
