//! Pool 构建 + inline 迁移 + P3 schema（用 deadpool-sqlite）。
//!
//! deadpool-sqlite 用 SyncWrapper<rusqlite::Connection> 解决「rusqlite::Connection 非 Send」，
//! 经 `conn.interact(|c| {...}).await` 在 spawn_blocking 里访问。
//!
//! 迁移策略（data-model.md）：CREATE TABLE IF NOT EXISTS（幂等）；失败向上传播并阻断启动。
//! P3 只建 projects/sessions/session_messages。

use std::path::Path;

use deadpool_sqlite::{Config, Hook, Pool, Runtime};
use tracing::{info, warn};

use crate::error::DbError;

pub type DbPool = Pool;
pub type ConnObj = deadpool_sqlite::Connection;

/// 打开 data_dir/dss.db 的连接池：经 builder 注册 post_create 钩子设 PRAGMA
/// （foreign_keys/busy_timeout 按连接生效；WAL 持久化到文件头）。
pub fn open_pool(data_dir: &Path) -> Result<Pool, DbError> {
    let cfg = Config::new(data_dir.join("dss.db"));
    let builder = cfg
        .builder(Runtime::Tokio1)
        .map_err(|e| DbError::Other(format!("pool builder: {e}")))?;
    let pool = builder
        .post_create(Hook::async_fn(|conn, _meta| {
            Box::pin(async move {
                let _ = conn
                    .interact(|c| {
                        let _ = c.pragma_update(None, "journal_mode", "WAL");
                        let _ = c.pragma_update(None, "foreign_keys", "ON");
                        let _ = c.pragma_update(None, "busy_timeout", 5000i64);
                        Ok::<_, rusqlite::Error>(())
                    })
                    .await
                    .map_err(|e| {
                        deadpool::managed::HookError::<rusqlite::Error>::message(format!(
                            "pragma: {e:?}"
                        ))
                    });
                Ok(())
            })
        }))
        .build()
        .map_err(|e| DbError::Other(format!("pool build: {e}")))?;
    Ok(pool)
}

/// inline 迁移：建 P3 子集表（IF NOT EXISTS 幂等）。
pub async fn run_migrations(pool: &Pool) -> Result<(), DbError> {
    let conn = pool.get().await.map_err(DbError::Pool)?;
    conn.interact(apply_migrations)
        .await
        .map_err(|e| DbError::Other(format!("migration interact: {e:?}")))?
        .map_err(|e| {
            warn!(error = ?e, "migration failed");
            DbError::Sqlite(e)
        })?;
    info!("sqlite pool ready, migrations applied");
    Ok(())
}

pub(crate) fn apply_migrations(c: &mut rusqlite::Connection) -> Result<(), rusqlite::Error> {
    c.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS projects (
                id              TEXT PRIMARY KEY,
                name            TEXT NOT NULL,
                description     TEXT,
                agent_context   TEXT,
                last_session_id TEXT,
                archived        INTEGER NOT NULL DEFAULT 0,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS bots (
                id               TEXT PRIMARY KEY,
                name             TEXT NOT NULL,
                role             TEXT NOT NULL,
                instructions     TEXT NOT NULL DEFAULT '',
                avatar           TEXT NOT NULL DEFAULT '🤖',
                color            TEXT NOT NULL DEFAULT '#4D6BFE',
                project_id       TEXT REFERENCES projects(id) ON DELETE SET NULL,
                model            TEXT,
                thinking_enabled INTEGER,
                thinking_effort  TEXT,
                enabled          INTEGER NOT NULL DEFAULT 1,
                revision         INTEGER NOT NULL DEFAULT 1,
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sessions (
                id            TEXT PRIMARY KEY,
                title         TEXT,
                workspace     TEXT NOT NULL,
                model         TEXT,
                plan_mode     INTEGER NOT NULL DEFAULT 0,
                status        TEXT NOT NULL DEFAULT 'active',
                discoverable  INTEGER NOT NULL DEFAULT 1,
                project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL,
                plan_data     TEXT,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS bot_jobs (
                id                  TEXT PRIMARY KEY,
                bot_id              TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,
                session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                prompt              TEXT NOT NULL,
                requested_plan_mode INTEGER NOT NULL DEFAULT 0,
                priority            INTEGER NOT NULL DEFAULT 0,
                position            INTEGER NOT NULL,
                revision            INTEGER NOT NULL DEFAULT 1,
                status              TEXT NOT NULL DEFAULT 'queued',
                run_id              TEXT,
                last_error          TEXT,
                created_at          TEXT NOT NULL,
                updated_at          TEXT NOT NULL,
                claimed_at          TEXT,
                completed_at        TEXT
            );
            CREATE TABLE IF NOT EXISTS agent_jobs (
                id                  TEXT PRIMARY KEY,
                job_kind            TEXT NOT NULL DEFAULT 'agent_turn',
                profile_id          TEXT REFERENCES bots(id) ON DELETE SET NULL,
                session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                prompt              TEXT NOT NULL,
                requested_plan_mode INTEGER NOT NULL DEFAULT 0,
                priority            INTEGER NOT NULL DEFAULT 0,
                position            INTEGER NOT NULL,
                revision            INTEGER NOT NULL DEFAULT 1,
                status              TEXT NOT NULL DEFAULT 'queued',
                run_id              TEXT,
                attempt             INTEGER NOT NULL DEFAULT 0,
                lease_owner         TEXT,
                lease_expires_at    TEXT,
                last_error          TEXT,
                created_at          TEXT NOT NULL,
                updated_at          TEXT NOT NULL,
                claimed_at          TEXT,
                completed_at        TEXT
            );
            CREATE TABLE IF NOT EXISTS session_runs (
                run_id           TEXT PRIMARY KEY,
                session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                ordinal          INTEGER NOT NULL,
                frame_id         TEXT NOT NULL,
                task_summary     TEXT NOT NULL,
                plan_mode        INTEGER NOT NULL DEFAULT 0,
                status           TEXT NOT NULL,
                kind             TEXT,
                awaiting         TEXT,
                pending_ask_json TEXT,
                error            TEXT,
                input_tokens     INTEGER NOT NULL DEFAULT 0,
                output_tokens    INTEGER NOT NULL DEFAULT 0,
                iterations       INTEGER NOT NULL DEFAULT 0,
                plan_data        TEXT,
                start_seq        INTEGER,
                end_seq          INTEGER,
                started_at       TEXT NOT NULL,
                completed_at     TEXT,
                UNIQUE(session_id, ordinal)
            );
            CREATE TABLE IF NOT EXISTS session_messages (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq            INTEGER NOT NULL,
                run_id         TEXT REFERENCES session_runs(run_id) ON DELETE SET NULL,
                role           TEXT NOT NULL,
                content        TEXT NOT NULL,
                harness_notice INTEGER NOT NULL DEFAULT 0,
                created_at     TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS session_events (
                event_id       TEXT PRIMARY KEY,
                session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq            INTEGER NOT NULL,
                run_id         TEXT,
                frame_id       TEXT,
                event_type     TEXT NOT NULL,
                schema_version INTEGER NOT NULL DEFAULT 1,
                payload        TEXT NOT NULL,
                created_at     TEXT NOT NULL,
                UNIQUE(session_id, seq)
            );
            CREATE TABLE IF NOT EXISTS execution_frames (
                id                TEXT PRIMARY KEY,
                session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                parent_frame_id   TEXT REFERENCES execution_frames(id) ON DELETE RESTRICT,
                root_frame_id     TEXT NOT NULL,
                kind              TEXT NOT NULL DEFAULT 'main',
                profile_id        TEXT REFERENCES bots(id) ON DELETE SET NULL,
                visibility        TEXT NOT NULL DEFAULT 'normal',
                activity          TEXT NOT NULL DEFAULT 'idle',
                active_run_id     TEXT,
                workspace_scope_id TEXT,
                revision          INTEGER NOT NULL DEFAULT 1,
                created_at        TEXT NOT NULL,
                updated_at        TEXT NOT NULL,
                closed_at         TEXT,
                CHECK (activity IN ('idle','running','waiting','suspended','closed')),
                CHECK (visibility IN ('normal','hidden'))
            );
            CREATE TABLE IF NOT EXISTS legacy_frame_aliases (
                legacy_frame_id TEXT PRIMARY KEY,
                actor_frame_id  TEXT NOT NULL REFERENCES execution_frames(id) ON DELETE CASCADE,
                source_run_id   TEXT,
                created_at      TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS run_attempts (
                attempt_id          TEXT PRIMARY KEY,
                run_id              TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
                attempt_no          INTEGER NOT NULL,
                lease_owner         TEXT NOT NULL,
                lease_token         TEXT NOT NULL UNIQUE,
                lease_expires_at    TEXT NOT NULL,
                checkpoint_event_seq INTEGER,
                status              TEXT NOT NULL,
                error               TEXT,
                started_at          TEXT NOT NULL,
                ended_at            TEXT,
                UNIQUE(run_id, attempt_no),
                CHECK (status IN ('running','waiting','completed','failed','cancelled','interrupted','needs_reconciliation'))
            );
            CREATE TABLE IF NOT EXISTS frame_mailbox (
                id                 TEXT PRIMARY KEY,
                session_id         TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                sender_frame_id    TEXT REFERENCES execution_frames(id) ON DELETE SET NULL,
                recipient_frame_id TEXT NOT NULL REFERENCES execution_frames(id) ON DELETE CASCADE,
                message_kind       TEXT NOT NULL,
                payload            TEXT NOT NULL,
                correlation_id     TEXT,
                status             TEXT NOT NULL DEFAULT 'unread',
                created_at         TEXT NOT NULL,
                read_at            TEXT,
                CHECK (status IN ('unread','read','discarded'))
            );
            CREATE TABLE IF NOT EXISTS child_results (
                id              TEXT PRIMARY KEY,
                parent_frame_id TEXT NOT NULL REFERENCES execution_frames(id) ON DELETE CASCADE,
                child_frame_id  TEXT NOT NULL REFERENCES execution_frames(id) ON DELETE CASCADE,
                run_id          TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
                status          TEXT NOT NULL,
                payload         TEXT NOT NULL,
                created_at      TEXT NOT NULL,
                UNIQUE(child_frame_id, run_id)
            );
            CREATE TABLE IF NOT EXISTS child_result_collections (
                result_id         TEXT NOT NULL REFERENCES child_results(id) ON DELETE CASCADE,
                collector_frame_id TEXT NOT NULL REFERENCES execution_frames(id) ON DELETE CASCADE,
                collected_at      TEXT NOT NULL,
                PRIMARY KEY(result_id, collector_frame_id)
            );
            CREATE TABLE IF NOT EXISTS tool_call_attempts (
                call_id          TEXT PRIMARY KEY,
                run_id           TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
                attempt_id       TEXT REFERENCES run_attempts(attempt_id) ON DELETE SET NULL,
                tool_name        TEXT NOT NULL,
                idempotency_key  TEXT,
                effect_class     TEXT NOT NULL,
                status           TEXT NOT NULL,
                input_json       TEXT NOT NULL,
                output_json      TEXT,
                started_at       TEXT NOT NULL,
                settled_at       TEXT,
                CHECK (effect_class IN ('read_only','idempotent','external_side_effect')),
                CHECK (status IN ('started','succeeded','failed','unknown'))
            );
            CREATE TABLE IF NOT EXISTS memories (
                id              TEXT PRIMARY KEY,
                entity          TEXT NOT NULL DEFAULT 'project',
                scope           TEXT,
                entity_type     TEXT NOT NULL DEFAULT 'note',
                body            TEXT NOT NULL,
                project_id      TEXT REFERENCES projects(id) ON DELETE SET NULL,
                confidence      REAL NOT NULL DEFAULT 0.5,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                last_surfaced_at TEXT
            );
            -- memory_events: 记忆生命周期审计（created/approved/rejected/superseded/deleted/surfaced/edited）
            CREATE TABLE IF NOT EXISTS memory_events (
                id          TEXT PRIMARY KEY,
                memory_id   TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                event_type  TEXT NOT NULL,
                actor       TEXT,
                detail      TEXT,
                created_at  TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS logs (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                ts            TEXT NOT NULL,
                level         TEXT NOT NULL,
                source        TEXT NOT NULL,
                kind          TEXT NOT NULL,
                session_id    TEXT,
                frame_id      TEXT,
                iteration     INTEGER,
                message       TEXT NOT NULL,
                detail        TEXT,
                trace_id      TEXT
            );
            "#,
    )?;

    // CREATE TABLE IF NOT EXISTS does not add columns to an older database.
    // Apply the additive migrations before creating indexes that reference
    // those columns. SQLite accepts these ALTERs without rebuilding tables.
    ensure_column(c, "projects", "last_session_id", "TEXT")?;
    ensure_column(c, "projects", "agent_context", "TEXT")?;
    ensure_column(c, "sessions", "plan_mode", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(c, "sessions", "status", "TEXT NOT NULL DEFAULT 'active'")?;
    ensure_column(c, "sessions", "discoverable", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_column(
        c,
        "sessions",
        "project_id",
        "TEXT REFERENCES projects(id) ON DELETE SET NULL",
    )?;
    ensure_column(c, "sessions", "plan_data", "TEXT")?;
    ensure_column(
        c,
        "sessions",
        "bot_id",
        "TEXT REFERENCES bots(id) ON DELETE SET NULL",
    )?;
    // compaction_state：持久化压缩视图态（JSON）。避免重启后首次请求 token 爆炸。
    // 阶段二已为 CompactionState 加 serde derive；完整 restore/save 接线作为增强项。
    ensure_column(c, "sessions", "compaction_state", "TEXT")?;
    ensure_column(c, "sessions", "root_frame_id", "TEXT")?;
    ensure_column(c, "session_runs", "actor_frame_id", "TEXT")?;
    ensure_column(c, "session_runs", "frame_ordinal", "INTEGER")?;
    ensure_column(
        c,
        "session_runs",
        "trigger_kind",
        "TEXT NOT NULL DEFAULT 'user'",
    )?;
    ensure_column(c, "session_runs", "retry_of_run_id", "TEXT")?;
    ensure_column(c, "session_runs", "active_attempt_id", "TEXT")?;
    ensure_column(
        c,
        "session_messages",
        "run_id",
        "TEXT REFERENCES session_runs(run_id) ON DELETE SET NULL",
    )?;
    ensure_column(c, "session_messages", "frame_id", "TEXT")?;
    ensure_column(c, "session_messages", "frame_seq", "INTEGER")?;
    ensure_column(c, "agent_jobs", "target_frame_id", "TEXT")?;
    ensure_column(c, "agent_jobs", "target_run_id", "TEXT")?;
    ensure_column(
        c,
        "agent_jobs",
        "action",
        "TEXT NOT NULL DEFAULT 'start_run'",
    )?;
    ensure_column(
        c,
        "session_messages",
        "harness_notice",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(c, "logs", "trace_id", "TEXT")?;

    // Attach an honest durable-Frame baseline. Historical per-turn frame ids remain untouched
    // and are recorded as aliases instead of being rewritten into a fictitious tree.
    let frame_baseline = c.transaction()?;
    frame_baseline.execute(
        "INSERT OR IGNORE INTO execution_frames (id, session_id, parent_frame_id, root_frame_id, \
             kind, profile_id, visibility, activity, workspace_scope_id, revision, created_at, updated_at) \
         SELECT id, id, NULL, id, 'main', bot_id, 'normal', \
                CASE WHEN status IN ('processing','active') THEN 'idle' \
                     WHEN status LIKE 'awaiting%' THEN 'waiting' \
                     WHEN status IN ('cancelled','interrupted','failed') THEN 'suspended' \
                     ELSE 'idle' END, \
                workspace, 1, created_at, updated_at FROM sessions",
        [],
    )?;
    frame_baseline.execute(
        "UPDATE sessions SET root_frame_id=id WHERE root_frame_id IS NULL",
        [],
    )?;
    frame_baseline.execute(
        "UPDATE session_runs SET actor_frame_id=session_id WHERE actor_frame_id IS NULL",
        [],
    )?;
    frame_baseline.execute(
        "INSERT OR IGNORE INTO legacy_frame_aliases \
             (legacy_frame_id, actor_frame_id, source_run_id, created_at) \
         SELECT frame_id, actor_frame_id, MIN(run_id), \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         FROM session_runs WHERE frame_id <> actor_frame_id GROUP BY frame_id, actor_frame_id",
        [],
    )?;
    frame_baseline.execute(
        "INSERT INTO session_events (event_id, session_id, seq, run_id, frame_id, event_type, \
                                     schema_version, payload, created_at) \
         SELECT lower(hex(randomblob(16))), f.session_id, \
                COALESCE((SELECT MAX(e.seq) FROM session_events e WHERE e.session_id=f.session_id), 0) + 1, \
                NULL, f.id, 'legacy_frame_baseline_attached', 1, \
                json_object('root_frame_id', f.id, 'legacy_alias_count', \
                    (SELECT COUNT(*) FROM legacy_frame_aliases a WHERE a.actor_frame_id=f.id)), \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         FROM execution_frames f WHERE f.kind='main' \
           AND EXISTS (SELECT 1 FROM legacy_frame_aliases a WHERE a.actor_frame_id=f.id) \
           AND NOT EXISTS (SELECT 1 FROM session_events e WHERE e.session_id=f.session_id \
                            AND e.event_type='legacy_frame_baseline_attached')",
        [],
    )?;
    frame_baseline.execute(
        "UPDATE session_messages SET frame_id=COALESCE(\
             (SELECT actor_frame_id FROM session_runs r WHERE r.run_id=session_messages.run_id),\
             session_id) WHERE frame_id IS NULL",
        [],
    )?;
    frame_baseline.execute_batch(
        "WITH numbered AS (\
             SELECT id, ROW_NUMBER() OVER (PARTITION BY frame_id ORDER BY seq, id) AS n \
             FROM session_messages WHERE frame_seq IS NULL\
         ) \
         UPDATE session_messages SET frame_seq=(SELECT n FROM numbered WHERE numbered.id=session_messages.id) \
         WHERE frame_seq IS NULL;",
    )?;
    frame_baseline.commit()?;

    // One-way compatibility migration: Bot identity remains an Agent Profile, while execution
    // moves to the generic JobRuntime table. Legacy rows are retained for forensic inspection
    // but are no longer authoritative after this one-way copy.
    c.execute(
        "INSERT OR IGNORE INTO agent_jobs (\
             id, job_kind, profile_id, session_id, prompt, requested_plan_mode, priority, \
             position, revision, status, run_id, attempt, last_error, created_at, updated_at, \
             claimed_at, completed_at\
         ) SELECT id, 'agent_turn', bot_id, session_id, prompt, requested_plan_mode, priority, \
                  position, revision, status, run_id, CASE WHEN run_id IS NULL THEN 0 ELSE 1 END, \
                  last_error, created_at, updated_at, claimed_at, completed_at FROM bot_jobs",
        [],
    )?;

    // --- memories: L2 Claim Store 升级（status/superseded_by/evidence/origin/valid_time/source_hash）---
    ensure_column(c, "memories", "status", "TEXT NOT NULL DEFAULT 'active'")?;
    ensure_column(c, "memories", "claim_type", "TEXT NOT NULL DEFAULT 'note'")?;
    ensure_column(c, "memories", "evidence_refs", "TEXT")?;
    ensure_column(c, "memories", "origin", "TEXT NOT NULL DEFAULT 'auto'")?;
    ensure_column(c, "memories", "superseded_by", "TEXT")?;
    ensure_column(c, "memories", "valid_from", "TEXT")?;
    ensure_column(c, "memories", "valid_until", "TEXT")?;
    ensure_column(c, "memories", "deleted_at", "TEXT")?;
    ensure_column(c, "memories", "source_hash", "TEXT")?;
    // source_hash 回填由 dss-memory 层惰性完成（SQLite 默认无 sha256 扩展；
    // 哈希逻辑归一在 Rust 侧单点维护）。旧行 status/claim_type/origin 由 DEFAULT 兜底。

    // No run can still be active while startup migrations execute. A crash after a durable tool
    // checkpoint deliberately leaves `processing`; expose it as interrupted on restore instead
    // of pretending the old SSE worker still exists.
    let recovery = c.transaction()?;
    recovery.execute(
        "INSERT INTO session_events (event_id, session_id, seq, run_id, frame_id, event_type, \
                                     schema_version, payload, created_at) \
         SELECT lower(hex(randomblob(16))), r.session_id, \
                COALESCE((SELECT MAX(existing.seq) FROM session_events existing \
                          WHERE existing.session_id=r.session_id), 0) + \
                    ROW_NUMBER() OVER (PARTITION BY r.session_id ORDER BY a.started_at, a.attempt_id), \
                r.run_id, r.actor_frame_id, 'attempt_lease_expired', 1, \
                json_object('attempt_id', a.attempt_id, 'lease_owner', a.lease_owner, \
                            'lease_expires_at', a.lease_expires_at, 'reason', 'process_restart'), \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         FROM run_attempts a JOIN session_runs r ON r.run_id=a.run_id \
         WHERE a.status='running'",
        [],
    )?;
    recovery.execute(
        "INSERT INTO session_events (event_id, session_id, seq, run_id, frame_id, event_type, \
                                     schema_version, payload, created_at) \
         SELECT lower(hex(randomblob(16))), pending.session_id, \
                COALESCE((SELECT MAX(existing.seq) FROM session_events existing \
                          WHERE existing.session_id = pending.session_id), 0) + \
                    ROW_NUMBER() OVER (PARTITION BY pending.session_id ORDER BY pending.ordinal), \
                pending.run_id, COALESCE(pending.actor_frame_id, pending.frame_id), \
                CASE WHEN EXISTS (SELECT 1 FROM tool_call_attempts t \
                                      WHERE t.run_id=pending.run_id AND t.status IN ('started','unknown') \
                                        AND t.effect_class='external_side_effect') \
                     THEN 'tool_reconciliation_required' ELSE 'run_interrupted' END, 1, \
                json_object('reason', CASE WHEN EXISTS (SELECT 1 FROM tool_call_attempts t \
                                                           WHERE t.run_id=pending.run_id AND t.status IN ('started','unknown') \
                                                             AND t.effect_class='external_side_effect') \
                                           THEN 'external_side_effect_outcome_unknown' ELSE 'app_exited' END, \
                            'previous_status', pending.status, \
                            'start_seq', pending.start_seq, 'end_seq', pending.end_seq), \
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         FROM session_runs pending \
         WHERE pending.completed_at IS NULL AND pending.status = 'processing'",
        [],
    )?;
    recovery.execute(
        "UPDATE sessions SET status = CASE WHEN EXISTS (SELECT 1 FROM session_runs r \
                                               JOIN tool_call_attempts t ON t.run_id=r.run_id \
                                               WHERE r.session_id=sessions.id AND r.status='processing' \
                                                 AND t.status IN ('started','unknown') AND t.effect_class='external_side_effect') \
                                           THEN 'needs_reconciliation' ELSE 'interrupted' END \
         WHERE EXISTS (SELECT 1 FROM session_runs pending \
                       WHERE pending.session_id=sessions.id AND pending.status='processing' \
                         AND pending.completed_at IS NULL)",
        [],
    )?;
    recovery.execute(
        "UPDATE session_runs SET status = CASE WHEN EXISTS (SELECT 1 FROM tool_call_attempts t \
                                          WHERE t.run_id=session_runs.run_id AND t.status IN ('started','unknown') \
                                            AND t.effect_class='external_side_effect') \
                                      THEN 'needs_reconciliation' ELSE 'interrupted' END, \
         kind = CASE WHEN EXISTS (SELECT 1 FROM tool_call_attempts t \
                                          WHERE t.run_id=session_runs.run_id AND t.status IN ('started','unknown') \
                                            AND t.effect_class='external_side_effect') \
                                      THEN 'reconciliation' ELSE 'interrupted' END, \
         error = COALESCE(error, CASE WHEN EXISTS (SELECT 1 FROM tool_call_attempts t \
                                          WHERE t.run_id=session_runs.run_id AND t.status IN ('started','unknown') \
                                            AND t.effect_class='external_side_effect') \
                                      THEN 'External side effect outcome is unknown after restart' \
                                      ELSE 'App exited before this run completed' END), \
         active_attempt_id=NULL, completed_at=NULL \
         WHERE completed_at IS NULL AND status = 'processing'",
        [],
    )?;
    recovery.execute(
        "UPDATE run_attempts SET status=CASE WHEN EXISTS (SELECT 1 FROM tool_call_attempts t \
                                                WHERE t.run_id=run_attempts.run_id AND t.status IN ('started','unknown') \
                                                  AND t.effect_class='external_side_effect') \
                                            THEN 'needs_reconciliation' ELSE 'interrupted' END, \
             error=COALESCE(error, 'Execution ownership lost during process restart'), \
             ended_at=strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE status='running'",
        [],
    )?;
    recovery.execute(
        "UPDATE execution_frames SET activity='suspended', revision=revision+1, \
             updated_at=strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE active_run_id IN (SELECT run_id FROM session_runs \
                                  WHERE status IN ('interrupted','needs_reconciliation'))",
        [],
    )?;
    recovery.execute(
        "UPDATE session_runs SET kind = COALESCE(kind, 'awaiting') \
         WHERE completed_at IS NULL AND status LIKE 'awaiting%'",
        [],
    )?;
    recovery.commit()?;
    // No worker survives a backend restart. Requeue a claimed Bot job instead of leaving it
    // permanently invisible; its stable id/revision keeps the resumed attempt auditable.
    c.execute(
        "UPDATE bot_jobs SET status='queued', run_id=NULL, claimed_at=NULL, \
         last_error=COALESCE(last_error, 'App restarted before this job completed'), \
         revision=revision+1, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE status='running'",
        [],
    )?;
    c.execute(
        "UPDATE agent_jobs SET status='queued', run_id=NULL, lease_owner=NULL, \
         lease_expires_at=NULL, claimed_at=NULL, \
         last_error=COALESCE(last_error, 'App restarted before this job completed'), \
         revision=revision+1, updated_at=strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE status='running'",
        [],
    )?;

    // Legacy databases did not enforce contiguous `(session_id, seq)` values. Preserve
    // their stable `(seq, id)` order and resequence when any session contains a duplicate,
    // gap, or non-positive start before adding the unique index. The repair itself is atomic
    // so an interrupted migration can never strand messages on temporary seqs.
    repair_duplicate_message_sequences(c)?;

    // The first Frame draft treated a parked Attempt as live. Parked Runs are resumed by a new
    // Attempt, so only actively executing ownership belongs under the partial unique fence.
    c.execute("DROP INDEX IF EXISTS uq_attempts_one_live", [])?;

    c.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_projects_archived ON projects(archived);
            CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
            CREATE INDEX IF NOT EXISTS idx_sessions_discoverable ON sessions(discoverable);
            CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_bot ON sessions(bot_id);
            CREATE INDEX IF NOT EXISTS idx_bots_project ON bots(project_id);
            CREATE INDEX IF NOT EXISTS idx_bots_updated ON bots(updated_at);
            CREATE INDEX IF NOT EXISTS idx_bot_jobs_session_position ON bot_jobs(session_id, status, position);
            CREATE INDEX IF NOT EXISTS idx_bot_jobs_bot_status ON bot_jobs(bot_id, status);
            CREATE INDEX IF NOT EXISTS idx_agent_jobs_session_position ON agent_jobs(session_id, status, position);
            CREATE INDEX IF NOT EXISTS idx_agent_jobs_profile_status ON agent_jobs(profile_id, status);
            CREATE INDEX IF NOT EXISTS idx_agent_jobs_lease ON agent_jobs(status, lease_expires_at);
            CREATE INDEX IF NOT EXISTS idx_agent_jobs_target_frame ON agent_jobs(target_frame_id, status);
            CREATE INDEX IF NOT EXISTS idx_messages_session ON session_messages(session_id);
            CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON session_messages(session_id, seq);
            CREATE UNIQUE INDEX IF NOT EXISTS uq_messages_session_seq ON session_messages(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_messages_run ON session_messages(run_id);
            CREATE UNIQUE INDEX IF NOT EXISTS uq_messages_frame_seq ON session_messages(frame_id, frame_seq) WHERE frame_id IS NOT NULL AND frame_seq IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_runs_session ON session_runs(session_id);
            CREATE INDEX IF NOT EXISTS idx_runs_actor_frame ON session_runs(actor_frame_id, frame_ordinal);
            CREATE UNIQUE INDEX IF NOT EXISTS uq_runs_session_ordinal ON session_runs(session_id, ordinal);
            CREATE INDEX IF NOT EXISTS idx_events_session_seq ON session_events(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_events_run_seq ON session_events(run_id, seq);
            CREATE INDEX IF NOT EXISTS idx_events_type ON session_events(event_type);
            CREATE INDEX IF NOT EXISTS idx_frames_session ON execution_frames(session_id);
            CREATE INDEX IF NOT EXISTS idx_frames_parent ON execution_frames(parent_frame_id);
            CREATE INDEX IF NOT EXISTS idx_frames_root_activity ON execution_frames(root_frame_id, activity);
            CREATE UNIQUE INDEX IF NOT EXISTS uq_frames_one_active_run ON execution_frames(active_run_id) WHERE active_run_id IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_attempts_run ON run_attempts(run_id, attempt_no);
            CREATE INDEX IF NOT EXISTS idx_attempts_lease ON run_attempts(status, lease_expires_at);
            CREATE UNIQUE INDEX IF NOT EXISTS uq_attempts_one_live ON run_attempts(run_id) WHERE status='running';
            CREATE INDEX IF NOT EXISTS idx_mailbox_recipient ON frame_mailbox(recipient_frame_id, status, created_at);
            CREATE INDEX IF NOT EXISTS idx_child_results_parent ON child_results(parent_frame_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_reconcile ON tool_call_attempts(status, effect_class);
            CREATE INDEX IF NOT EXISTS idx_memories_entity ON memories(entity);
            CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_id);
            CREATE INDEX IF NOT EXISTS idx_memories_scope ON memories(scope);
            CREATE INDEX IF NOT EXISTS idx_memories_status ON memories(status);
            CREATE INDEX IF NOT EXISTS idx_memories_source_hash ON memories(source_hash);
            CREATE INDEX IF NOT EXISTS idx_memories_superseded_by ON memories(superseded_by);
            CREATE INDEX IF NOT EXISTS idx_memory_events_mid ON memory_events(memory_id);
            CREATE INDEX IF NOT EXISTS idx_logs_ts ON logs(ts);
            CREATE INDEX IF NOT EXISTS idx_logs_session_ts ON logs(session_id, ts);
            CREATE INDEX IF NOT EXISTS idx_logs_level ON logs(level);
            CREATE INDEX IF NOT EXISTS idx_logs_source_kind ON logs(source, kind);
            "#,
    )?;
    Ok(())
}

fn repair_duplicate_message_sequences(c: &mut rusqlite::Connection) -> Result<(), rusqlite::Error> {
    let invalid_session_count: i64 = c.query_row(
        "SELECT COUNT(*) FROM (\
             SELECT session_id FROM session_messages GROUP BY session_id \
             HAVING MIN(seq) != 1 OR MAX(seq) != COUNT(*) OR COUNT(DISTINCT seq) != COUNT(*)\
         )",
        [],
        |row| row.get(0),
    )?;
    if invalid_session_count == 0 {
        return Ok(());
    }

    let ordered: Vec<(i64, String, i64)> = {
        let mut stmt = c.prepare(
            "SELECT id, session_id, seq FROM session_messages ORDER BY session_id, seq, id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };

    let tx = c.transaction()?;
    // Move every row out of the positive namespace first so rewritten sequence
    // numbers cannot collide with any untouched legacy value.
    for (id, _, _) in &ordered {
        tx.execute(
            "UPDATE session_messages SET seq = ?1 WHERE id = ?2",
            rusqlite::params![-id, id],
        )?;
    }
    let mut current_session = None::<String>;
    let mut next_seq = 0i64;
    for (id, session_id, _) in ordered {
        if current_session.as_deref() != Some(session_id.as_str()) {
            current_session = Some(session_id);
            next_seq = 1;
        } else {
            next_seq += 1;
        }
        tx.execute(
            "UPDATE session_messages SET seq = ?1 WHERE id = ?2",
            rusqlite::params![next_seq, id],
        )?;
    }
    tx.execute(
        "UPDATE session_runs SET \
         start_seq = (SELECT MIN(seq) FROM session_messages WHERE run_id = session_runs.run_id), \
         end_seq = (SELECT MAX(seq) FROM session_messages WHERE run_id = session_runs.run_id) \
         WHERE EXISTS (SELECT 1 FROM session_messages WHERE run_id = session_runs.run_id)",
        [],
    )?;
    tx.commit()
}

fn ensure_column(
    c: &rusqlite::Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), rusqlite::Error> {
    let mut stmt = c.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column);
    drop(stmt);
    if !exists {
        c.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::apply_migrations;

    #[test]
    fn upgrades_legacy_tables_with_new_columns() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"
            CREATE TABLE projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT,
                archived INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY, title TEXT, workspace TEXT NOT NULL, model TEXT,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE session_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                seq INTEGER NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, created_at TEXT NOT NULL
            );
            INSERT INTO sessions (id, title, workspace, model, created_at, updated_at)
            VALUES ('legacy', NULL, '/tmp/legacy', NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
            INSERT INTO session_messages (session_id, seq, role, content, created_at) VALUES
                ('legacy', 1, 'user', '{"role":"user","content":"first"}', '2026-01-01T00:00:00Z'),
                ('legacy', 1, 'assistant', '{"role":"assistant","content":"second"}', '2026-01-01T00:00:01Z');
            "#,
        )
        .unwrap();

        apply_migrations(&mut c).unwrap();
        apply_migrations(&mut c).unwrap();

        for (table, column) in [
            ("projects", "last_session_id"),
            ("projects", "agent_context"),
            ("sessions", "plan_mode"),
            ("sessions", "status"),
            ("sessions", "discoverable"),
            ("sessions", "project_id"),
            ("sessions", "plan_data"),
            ("session_messages", "run_id"),
            ("session_messages", "harness_notice"),
        ] {
            let count: i64 = c
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                    [column],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing {table}.{column}");
        }

        let legacy_discoverable: i64 = c
            .query_row(
                "SELECT discoverable FROM sessions WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(legacy_discoverable, 1);

        let run_table: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_runs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(run_table, 1);
        let event_table: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_table, 1);

        let seqs = c
            .prepare("SELECT seq FROM session_messages WHERE session_id='legacy' ORDER BY seq")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(seqs, vec![1, 2]);
        let duplicate_insert = c.execute(
            "INSERT INTO session_messages \
             (session_id, seq, role, content, harness_notice, created_at) \
             VALUES ('legacy', 2, 'assistant', '{}', 0, '2026-01-01T00:00:02Z')",
            [],
        );
        assert!(
            duplicate_insert.is_err(),
            "unique sequence index must be active"
        );
    }

    #[test]
    fn startup_marks_checkpointed_processing_sessions_interrupted() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        apply_migrations(&mut c).unwrap();
        c.execute(
            "INSERT INTO sessions \
             (id, title, workspace, model, status, created_at, updated_at) \
             VALUES ('checkpointed', NULL, '/tmp/checkpointed', NULL, 'processing', ?1, ?1)",
            ["2026-01-01T00:00:00Z"],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions \
             (id, title, workspace, model, status, created_at, updated_at) \
             VALUES ('completed', NULL, '/tmp/completed', NULL, 'completed', ?1, ?1)",
            ["2026-01-01T00:00:00Z"],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions \
             (id, title, workspace, model, status, created_at, updated_at) \
             VALUES ('awaiting', NULL, '/tmp/awaiting', NULL, 'awaiting_user_response', ?1, ?1)",
            ["2026-01-01T00:00:00Z"],
        )
        .unwrap();
        c.execute(
            "INSERT INTO session_runs \
             (run_id, session_id, ordinal, frame_id, task_summary, status, started_at) \
             VALUES ('run-processing', 'checkpointed', 1, 'frame-processing', 'work', \
                     'processing', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO session_runs \
             (run_id, session_id, ordinal, frame_id, task_summary, status, awaiting, started_at) \
             VALUES ('run-awaiting', 'awaiting', 1, 'frame-awaiting', 'question', \
                     'awaiting_user_response', 'user_response', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();

        apply_migrations(&mut c).unwrap();

        let checkpointed: String = c
            .query_row(
                "SELECT status FROM sessions WHERE id = 'checkpointed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let completed: String = c
            .query_row(
                "SELECT status FROM sessions WHERE id = 'completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(checkpointed, "interrupted");
        assert_eq!(completed, "completed");
        let processing_run: (String, Option<String>, Option<String>) = c
            .query_row(
                "SELECT status, kind, completed_at FROM session_runs WHERE run_id='run-processing'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(processing_run.0, "interrupted");
        assert_eq!(processing_run.1.as_deref(), Some("interrupted"));
        assert!(processing_run.2.is_none());
        let interrupted_event_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM session_events \
                 WHERE session_id='checkpointed' AND run_id='run-processing' \
                   AND event_type='run_interrupted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(interrupted_event_count, 1);
        let awaiting_run: (String, Option<String>, Option<String>) = c
            .query_row(
                "SELECT status, kind, completed_at FROM session_runs WHERE run_id='run-awaiting'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(awaiting_run.0, "awaiting_user_response");
        assert_eq!(awaiting_run.1.as_deref(), Some("awaiting"));
        assert!(awaiting_run.2.is_none());

        apply_migrations(&mut c).unwrap();
        let idempotent_event_count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM session_events \
                 WHERE session_id='checkpointed' AND run_id='run-processing' \
                   AND event_type='run_interrupted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(idempotent_event_count, 1);
    }

    #[test]
    fn startup_fences_unknown_external_side_effect_without_completing_the_run() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        apply_migrations(&mut c).unwrap();
        crate::repo::create_session(&c, "root", "/tmp/root", None, None).unwrap();
        let attempt = crate::harness::accept_frame_run(
            &c,
            &crate::harness::AcceptFrameRun {
                run_id: "run-external".into(),
                frame_id: "root".into(),
                task_summary: "publish".into(),
                trigger_kind: "user".into(),
                started_at: "2026-08-20T00:00:00Z".into(),
                lease_owner: "old-process".into(),
                lease_expires_at: "2099-01-01T00:00:00Z".into(),
                messages: vec![],
            },
        )
        .unwrap();
        crate::harness::record_tool_call_started(
            &c,
            &crate::harness::ToolCallStart {
                call_id: "publish-1",
                run_id: "run-external",
                attempt_id: &attempt.attempt_id,
                lease_token: &attempt.lease_token,
                tool_name: "publish",
                effect_class: "external_side_effect",
                input: &serde_json::json!({}),
                idempotency_key: None,
            },
        )
        .unwrap();

        apply_migrations(&mut c).unwrap();

        let run: (String, Option<String>, Option<String>) = c
            .query_row(
                "SELECT status, active_attempt_id, completed_at FROM session_runs \
                 WHERE run_id='run-external'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(run, ("needs_reconciliation".into(), None, None));
        let frame: (String, Option<String>) = c
            .query_row(
                "SELECT activity, active_run_id FROM execution_frames WHERE id='root'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(frame.0, "suspended");
        assert_eq!(frame.1.as_deref(), Some("run-external"));
        let session_status: String = c
            .query_row("SELECT status FROM sessions WHERE id='root'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(session_status, "needs_reconciliation");
    }

    #[test]
    fn startup_requeues_orphaned_running_bot_jobs() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        apply_migrations(&mut c).unwrap();
        c.execute(
            "INSERT INTO bots \
             (id, name, role, instructions, avatar, color, enabled, revision, created_at, updated_at) \
             VALUES ('bot-recovery', 'Recovery Bot', 'Executor', '', '🤖', '#4D6BFE', 1, 1, ?1, ?1)",
            ["2026-01-01T00:00:00Z"],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions \
             (id, title, workspace, status, bot_id, created_at, updated_at) \
             VALUES ('bot-recovery-session', 'Bot Chat', '/tmp/bot-recovery', 'processing', \
                     'bot-recovery', ?1, ?1)",
            ["2026-01-01T00:00:00Z"],
        )
        .unwrap();
        c.execute(
            "INSERT INTO bot_jobs \
             (id, bot_id, session_id, prompt, position, revision, status, run_id, \
              created_at, updated_at, claimed_at) \
             VALUES ('bot-recovery-job', 'bot-recovery', 'bot-recovery-session', 'resume me', \
                     1, 2, 'running', 'run-before-restart', ?1, ?1, ?1)",
            ["2026-01-01T00:00:00Z"],
        )
        .unwrap();

        // A backend restart runs migrations again. No worker survives that process boundary,
        // so an in-flight durable Bot job must become eligible for an explicit retry.
        apply_migrations(&mut c).unwrap();

        let restored: (String, Option<String>, Option<String>, Option<String>, i64) = c
            .query_row(
                "SELECT status, run_id, claimed_at, last_error, revision \
                 FROM bot_jobs WHERE id='bot-recovery-job'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(restored.0, "queued");
        assert_eq!(restored.1, None);
        assert_eq!(restored.2, None);
        assert_eq!(
            restored.3.as_deref(),
            Some("App restarted before this job completed")
        );
        assert_eq!(
            restored.4, 3,
            "recovery must advance the optimistic revision"
        );
        let generic: (String, Option<String>, Option<String>, Option<String>, i64) = c
            .query_row(
                "SELECT status, run_id, lease_owner, last_error, revision \
                 FROM agent_jobs WHERE id='bot-recovery-job'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(generic.0, "queued");
        assert_eq!(generic.1, None);
        assert_eq!(generic.2, None);
        assert_eq!(
            generic.3.as_deref(),
            Some("App restarted before this job completed")
        );
        assert_eq!(generic.4, 3);
    }

    #[test]
    fn migration_repairs_legacy_sequence_gaps_without_duplicates() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"
            CREATE TABLE projects (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT,
                archived INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE sessions (
                id TEXT PRIMARY KEY, title TEXT, workspace TEXT NOT NULL, model TEXT,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE session_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                seq INTEGER NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, created_at TEXT NOT NULL
            );
            INSERT INTO sessions (id, title, workspace, model, created_at, updated_at)
            VALUES ('gap', NULL, '/tmp/gap', NULL, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
            INSERT INTO session_messages (session_id, seq, role, content, created_at) VALUES
                ('gap', 1, 'user', '{}', '2026-01-01T00:00:00Z'),
                ('gap', 3, 'assistant', '{}', '2026-01-01T00:00:01Z');
            "#,
        )
        .unwrap();

        apply_migrations(&mut c).unwrap();

        let seqs = c
            .prepare("SELECT seq FROM session_messages WHERE session_id='gap' ORDER BY seq")
            .unwrap()
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(seqs, vec![1, 2]);
    }

    #[test]
    fn migration_creates_memory_events_table_and_claim_store_columns() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        apply_migrations(&mut c).unwrap();
        apply_migrations(&mut c).unwrap(); // 幂等

        // memory_events 表存在
        let events_table: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='memory_events'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(events_table, 1, "memory_events table must exist");

        // memories 的 L2 Claim Store 新列存在
        for column in [
            "status",
            "claim_type",
            "evidence_refs",
            "origin",
            "superseded_by",
            "valid_from",
            "valid_until",
            "deleted_at",
            "source_hash",
        ] {
            let count: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('memories') WHERE name = ?1",
                    [column],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "memories.{column} missing");
        }

        // 新列默认值正确：旧行 status=active, claim_type=note, origin=auto
        c.execute(
            "INSERT INTO memories (id, body, created_at, updated_at) VALUES ('m1', 'x', 't', 't')",
            [],
        )
        .unwrap();
        let (status, claim_type, origin): (String, String, String) = c
            .query_row(
                "SELECT status, claim_type, origin FROM memories WHERE id='m1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "active");
        assert_eq!(claim_type, "note");
        assert_eq!(origin, "auto");

        // memory_events FK 级联：删 memory 应级联删其事件
        c.execute(
            "INSERT INTO memory_events (id, memory_id, event_type, created_at) \
             VALUES ('e1', 'm1', 'created', 't')",
            [],
        )
        .unwrap();
        c.execute("DELETE FROM memories WHERE id='m1'", []).unwrap();
        let orphaned: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM memory_events WHERE memory_id='m1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(orphaned, 0, "memory_events must cascade on memory delete");
    }

    #[test]
    fn migration_adds_compaction_state_column_to_sessions() {
        let mut c = rusqlite::Connection::open_in_memory().unwrap();
        apply_migrations(&mut c).unwrap();
        let count: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'compaction_state'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "sessions.compaction_state missing");
    }
}
