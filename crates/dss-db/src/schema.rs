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

fn apply_migrations(c: &mut rusqlite::Connection) -> Result<(), rusqlite::Error> {
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
        "session_messages",
        "run_id",
        "TEXT REFERENCES session_runs(run_id) ON DELETE SET NULL",
    )?;
    ensure_column(
        c,
        "session_messages",
        "harness_notice",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(c, "logs", "trace_id", "TEXT")?;

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
    c.execute(
        "UPDATE sessions SET status = 'interrupted' WHERE status = 'processing'",
        [],
    )?;
    c.execute(
        "UPDATE session_runs SET status = 'interrupted', kind = 'cancelled', \
         error = COALESCE(error, 'App exited before this run completed'), \
         completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE completed_at IS NULL AND status = 'processing'",
        [],
    )?;
    c.execute(
        "UPDATE session_runs SET kind = COALESCE(kind, 'awaiting'), \
         completed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
         WHERE completed_at IS NULL AND status LIKE 'awaiting%'",
        [],
    )?;

    // Legacy databases did not enforce contiguous `(session_id, seq)` values. Preserve
    // their stable `(seq, id)` order and resequence when any session contains a duplicate,
    // gap, or non-positive start before adding the unique index. The repair itself is atomic
    // so an interrupted migration can never strand messages on temporary seqs.
    repair_duplicate_message_sequences(c)?;

    c.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_projects_archived ON projects(archived);
            CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
            CREATE INDEX IF NOT EXISTS idx_sessions_discoverable ON sessions(discoverable);
            CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_id);
            CREATE INDEX IF NOT EXISTS idx_messages_session ON session_messages(session_id);
            CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON session_messages(session_id, seq);
            CREATE UNIQUE INDEX IF NOT EXISTS uq_messages_session_seq ON session_messages(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_messages_run ON session_messages(run_id);
            CREATE INDEX IF NOT EXISTS idx_runs_session ON session_runs(session_id);
            CREATE UNIQUE INDEX IF NOT EXISTS uq_runs_session_ordinal ON session_runs(session_id, ordinal);
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
        assert_eq!(processing_run.1.as_deref(), Some("cancelled"));
        assert!(processing_run.2.is_some());
        let awaiting_run: (String, Option<String>, Option<String>) = c
            .query_row(
                "SELECT status, kind, completed_at FROM session_runs WHERE run_id='run-awaiting'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(awaiting_run.0, "awaiting_user_response");
        assert_eq!(awaiting_run.1.as_deref(), Some("awaiting"));
        assert!(awaiting_run.2.is_some());
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
}
