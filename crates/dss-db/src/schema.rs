//! Pool 构建 + inline 迁移 + P3 schema（用 deadpool-sqlite）。
//!
//! deadpool-sqlite 用 SyncWrapper<rusqlite::Connection> 解决「rusqlite::Connection 非 Send」，
//! 经 `conn.interact(|c| {...}).await` 在 spawn_blocking 里访问。
//!
//! 迁移策略（data-model.md）：CREATE TABLE IF NOT EXISTS（幂等）；失败 warn 不阻断启动。
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
    conn.interact(|c| {
        c.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
                id              TEXT PRIMARY KEY,
                name            TEXT NOT NULL,
                description     TEXT,
                last_session_id TEXT,
                archived        INTEGER NOT NULL DEFAULT 0,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_projects_archived ON projects(archived);
            CREATE TABLE IF NOT EXISTS sessions (
                id            TEXT PRIMARY KEY,
                title         TEXT,
                workspace     TEXT NOT NULL,
                model         TEXT,
                plan_mode     INTEGER NOT NULL DEFAULT 0,
                status        TEXT NOT NULL DEFAULT 'active',
                project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL,
                plan_data     TEXT,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);
            CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_id);
            CREATE TABLE IF NOT EXISTS session_messages (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq            INTEGER NOT NULL,
                role           TEXT NOT NULL,
                content        TEXT NOT NULL,
                harness_notice INTEGER NOT NULL DEFAULT 0,
                created_at     TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session ON session_messages(session_id);
            CREATE INDEX IF NOT EXISTS idx_messages_session_seq ON session_messages(session_id, seq);
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
            CREATE INDEX IF NOT EXISTS idx_memories_entity ON memories(entity);
            CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project_id);
            CREATE INDEX IF NOT EXISTS idx_memories_scope ON memories(scope);
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
            CREATE INDEX IF NOT EXISTS idx_logs_ts ON logs(ts);
            CREATE INDEX IF NOT EXISTS idx_logs_session_ts ON logs(session_id, ts);
            CREATE INDEX IF NOT EXISTS idx_logs_level ON logs(level);
            CREATE INDEX IF NOT EXISTS idx_logs_source_kind ON logs(source, kind);
            "#,
        )?;
        Ok::<_, rusqlite::Error>(())
    })
    .await
    .map_err(|e| DbError::Other(format!("migration interact: {e:?}")))?
    .map_err(|e| {
        warn!(error = ?e, "migration failed");
        DbError::Sqlite(e)
    })?;
    info!("sqlite pool ready, migrations applied");
    Ok(())
}
