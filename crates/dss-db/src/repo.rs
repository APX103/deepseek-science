//! 仓储层：projects / sessions / session_messages 的 CRUD。
//!
//! 这些是同步函数（直接拿 `&rusqlite::Connection`），调用方经 `pool.interact`
//! 或 `spawn_blocking` 在阻塞线程跑（data-model：SQLite 写串行化）。

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::DbError;

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ----------------- 领域行结构 -----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub last_session_id: Option<String>,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub workspace: String,
    pub model: Option<String>,
    pub plan_mode: bool,
    pub status: String,
    pub project_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 一条持久化消息（content 是 OpenAI 协议形态 JSON 字符串）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRow {
    pub seq: i64,
    pub role: String,
    pub content: String,
    pub harness_notice: bool,
}

// ----------------- projects -----------------

/// 启动时确保默认项目存在（不覆盖）。
pub fn ensure_default_project(conn: &Connection) -> Result<ProjectRow, DbError> {
    let now = now();
    conn.execute(
        "INSERT OR IGNORE INTO projects (id, name, description, archived, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 0, ?4, ?4)",
        params!["proj_default", "Default", "默认项目", now],
    )?;
    get_project(conn, "proj_default")?
        .ok_or_else(|| DbError::Other("default project missing".into()))
}

pub fn get_project(conn: &Connection, id: &str) -> Result<Option<ProjectRow>, DbError> {
    let row = conn
        .query_row(
            "SELECT id, name, description, last_session_id, archived, created_at, updated_at \
             FROM projects WHERE id = ?1",
            params![id],
            row_to_project,
        )
        .optional()?;
    Ok(row)
}

pub fn list_projects(
    conn: &Connection,
    include_archived: bool,
) -> Result<Vec<ProjectRow>, DbError> {
    let mut rows = if include_archived {
        conn.prepare(
            "SELECT id, name, description, last_session_id, archived, created_at, updated_at \
             FROM projects ORDER BY (id = 'proj_default') DESC, updated_at DESC",
        )?
    } else {
        conn.prepare(
            "SELECT id, name, description, last_session_id, archived, created_at, updated_at \
             FROM projects WHERE archived = 0 ORDER BY (id = 'proj_default') DESC, updated_at DESC",
        )?
    };
    let list = rows
        .query_map([], row_to_project)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(list)
}

pub fn create_project(
    conn: &Connection,
    name: &str,
    description: Option<&str>,
) -> Result<ProjectRow, DbError> {
    let now = now();
    let id = format!("proj_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    conn.execute(
        "INSERT INTO projects (id, name, description, archived, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 0, ?4, ?4)",
        params![id, name, description, now],
    )?;
    get_project(conn, &id)?.ok_or_else(|| DbError::Other("just-created project missing".into()))
}

pub fn update_project(
    conn: &Connection,
    id: &str,
    name: Option<&str>,
    description: Option<&str>,
    last_session_id: Option<&str>,
) -> Result<ProjectRow, DbError> {
    let now = now();
    // 只更新非 None 字段。
    if let Some(n) = name {
        conn.execute(
            "UPDATE projects SET name=?1, updated_at=?2 WHERE id=?3",
            params![n, now, id],
        )?;
    }
    if let Some(d) = description {
        conn.execute(
            "UPDATE projects SET description=?1, updated_at=?2 WHERE id=?3",
            params![d, now, id],
        )?;
    }
    if let Some(s) = last_session_id {
        conn.execute(
            "UPDATE projects SET last_session_id=?1, updated_at=?2 WHERE id=?3",
            params![s, now, id],
        )?;
    }
    get_project(conn, id)?.ok_or_else(|| DbError::NotFound(format!("project {id}")))
}

pub fn set_project_archived(
    conn: &Connection,
    id: &str,
    archived: bool,
) -> Result<ProjectRow, DbError> {
    if id == "proj_default" {
        return Err(DbError::Conflict(
            "default project cannot be archived".into(),
        ));
    }
    let now = now();
    let n = conn.execute(
        "UPDATE projects SET archived=?1, updated_at=?2 WHERE id=?3",
        params![if archived { 1 } else { 0 }, now, id],
    )?;
    if n == 0 {
        return Err(DbError::NotFound(format!("project {id}")));
    }
    get_project(conn, id)?.ok_or_else(|| DbError::NotFound(format!("project {id}")))
}

/// force=false 且项目有会话 → Conflict；否则删除（sessions 经 ON DELETE SET NULL，
/// 但项目级联删除其会话更符合「删项目连带删会话」语义——这里采用：有 session 则拒绝）。
pub fn delete_project(conn: &Connection, id: &str, force: bool) -> Result<(), DbError> {
    if id == "proj_default" {
        return Err(DbError::Conflict(
            "default project cannot be deleted".into(),
        ));
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sessions WHERE project_id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    if count > 0 && !force {
        return Err(DbError::Conflict(format!(
            "project {id} has {count} session(s); pass force=true to delete"
        )));
    }
    // force：把会话的 project_id 置 NULL（不级联删会话，会话仍可 orphan 存在）。
    if count > 0 {
        conn.execute(
            "UPDATE sessions SET project_id=NULL WHERE project_id=?1",
            params![id],
        )?;
    }
    let n = conn.execute("DELETE FROM projects WHERE id=?1", params![id])?;
    if n == 0 {
        return Err(DbError::NotFound(format!("project {id}")));
    }
    Ok(())
}

/// 项目详情 + 其会话列表（archived=false 的会话）。
pub fn get_project_detail(
    conn: &Connection,
    id: &str,
) -> Result<(ProjectRow, Vec<SessionRow>), DbError> {
    let proj = get_project(conn, id)?.ok_or_else(|| DbError::NotFound(format!("project {id}")))?;
    let mut stmt = conn.prepare(
        "SELECT id, title, workspace, model, plan_mode, status, project_id, created_at, updated_at \
         FROM sessions WHERE project_id = ?1 ORDER BY updated_at DESC",
    )?;
    let sessions = stmt
        .query_map(params![id], row_to_session)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((proj, sessions))
}

fn row_to_project(r: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectRow> {
    Ok(ProjectRow {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        last_session_id: r.get(3)?,
        archived: r.get::<_, i64>(4)? != 0,
        created_at: r.get(5)?,
        updated_at: r.get(6)?,
    })
}

// ----------------- sessions -----------------

pub fn create_session(
    conn: &Connection,
    id: &str,
    workspace: &str,
    model: Option<&str>,
    project_id: Option<&str>,
) -> Result<SessionRow, DbError> {
    let now = now();
    conn.execute(
        "INSERT INTO sessions (id, title, workspace, model, plan_mode, status, project_id, created_at, updated_at) \
         VALUES (?1, NULL, ?2, ?3, 0, 'active', ?4, ?5, ?5)",
        params![id, workspace, model, project_id, now],
    )?;
    get_session(conn, id)?.ok_or_else(|| DbError::Other("just-created session missing".into()))
}

pub fn get_session(conn: &Connection, id: &str) -> Result<Option<SessionRow>, DbError> {
    let row = conn
        .query_row(
            "SELECT id, title, workspace, model, plan_mode, status, project_id, created_at, updated_at \
             FROM sessions WHERE id = ?1",
            params![id],
            row_to_session,
        )
        .optional()?;
    Ok(row)
}

pub fn list_sessions(conn: &Connection) -> Result<Vec<SessionRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, title, workspace, model, plan_mode, status, project_id, created_at, updated_at \
         FROM sessions ORDER BY updated_at DESC",
    )?;
    let list = stmt
        .query_map([], row_to_session)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(list)
}

pub fn set_session_title(conn: &Connection, id: &str, title: &str) -> Result<(), DbError> {
    let now = now();
    conn.execute(
        "UPDATE sessions SET title=?1, updated_at=?2 WHERE id=?3",
        params![title, now, id],
    )?;
    Ok(())
}

pub fn touch_session(conn: &Connection, id: &str) -> Result<(), DbError> {
    let now = now();
    conn.execute(
        "UPDATE sessions SET updated_at=?1 WHERE id=?2",
        params![now, id],
    )?;
    Ok(())
}

pub fn set_session_plan(
    conn: &Connection,
    id: &str,
    plan_data: Option<&str>,
) -> Result<(), DbError> {
    let now = now();
    conn.execute(
        "UPDATE sessions SET plan_data=?1, updated_at=?2 WHERE id=?3",
        params![plan_data, now, id],
    )?;
    Ok(())
}

pub fn get_session_plan(conn: &Connection, id: &str) -> Result<Option<String>, DbError> {
    let row = conn
        .query_row(
            "SELECT plan_data FROM sessions WHERE id = ?1",
            params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(row.flatten())
}

pub fn delete_session(conn: &Connection, id: &str) -> Result<(), DbError> {
    let n = conn.execute("DELETE FROM sessions WHERE id=?1", params![id])?;
    if n == 0 {
        return Err(DbError::NotFound(format!("session {id}")));
    }
    Ok(())
}

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: r.get(0)?,
        title: r.get(1)?,
        workspace: r.get(2)?,
        model: r.get(3)?,
        plan_mode: r.get::<_, i64>(4)? != 0,
        status: r.get(5)?,
        project_id: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

// ----------------- session_messages -----------------

/// 追加一条消息；seq 自动取当前最大+1。返回新 seq。
pub fn append_message(
    conn: &Connection,
    session_id: &str,
    role: &str,
    content: &str,
    harness_notice: bool,
) -> Result<i64, DbError> {
    let now = now();
    let max_seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM session_messages WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let seq = max_seq + 1;
    conn.execute(
        "INSERT INTO session_messages (session_id, seq, role, content, harness_notice, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![session_id, seq, role, content, if harness_notice { 1 } else { 0 }, now],
    )?;
    Ok(seq)
}

pub fn list_messages(conn: &Connection, session_id: &str) -> Result<Vec<MessageRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT seq, role, content, harness_notice FROM session_messages \
         WHERE session_id = ?1 ORDER BY seq ASC",
    )?;
    let list = stmt
        .query_map(params![session_id], |r| {
            Ok(MessageRow {
                seq: r.get(0)?,
                role: r.get(1)?,
                content: r.get(2)?,
                harness_notice: r.get::<_, i64>(3)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(list)
}

// ----------------- memories -----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRow {
    pub id: String,
    pub entity: String,
    pub scope: Option<String>,
    pub entity_type: String,
    pub body: String,
    pub project_id: Option<String>,
    pub confidence: f64,
    pub created_at: String,
    pub updated_at: String,
    pub last_surfaced_at: Option<String>,
}

pub fn append_memory(
    conn: &Connection,
    id: &str,
    body: &str,
    scope: Option<&str>,
    project_id: Option<&str>,
) -> Result<MemoryRow, DbError> {
    let now = now();
    conn.execute(
        "INSERT INTO memories (id, entity, scope, entity_type, body, project_id, confidence, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'note', ?4, ?5, 0.5, ?6, ?6)",
        params![id, scope.unwrap_or("project"), scope, body, project_id, now],
    )?;
    get_memory(conn, id)?.ok_or_else(|| DbError::Other("just-inserted memory missing".into()))
}

pub fn get_memory(conn: &Connection, id: &str) -> Result<Option<MemoryRow>, DbError> {
    let row = conn
        .query_row(
            "SELECT id, entity, scope, entity_type, body, project_id, confidence, created_at, updated_at, last_surfaced_at \
             FROM memories WHERE id = ?1",
            params![id],
            |r| {
                Ok(MemoryRow {
                    id: r.get(0)?,
                    entity: r.get(1)?,
                    scope: r.get(2)?,
                    entity_type: r.get(3)?,
                    body: r.get(4)?,
                    project_id: r.get(5)?,
                    confidence: r.get(6)?,
                    created_at: r.get(7)?,
                    updated_at: r.get(8)?,
                    last_surfaced_at: r.get(9)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// 列记忆：profile(scope=profile，跨项目) + 当前 project 的。entity 过滤可选（Rust 侧过滤）。
pub fn list_memories(
    conn: &Connection,
    project_id: Option<&str>,
    entity: Option<&str>,
) -> Result<Vec<MemoryRow>, DbError> {
    // profile 永可见 + 指定 project 的（SQL 侧），entity 在 Rust 侧过滤。
    let sql = if project_id.is_some() {
        "SELECT id, entity, scope, entity_type, body, project_id, confidence, created_at, updated_at, last_surfaced_at \
         FROM memories WHERE scope = 'profile' OR project_id = ?1 ORDER BY updated_at DESC"
    } else {
        "SELECT id, entity, scope, entity_type, body, project_id, confidence, created_at, updated_at, last_surfaced_at \
         FROM memories WHERE scope = 'profile' ORDER BY updated_at DESC"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows_iter = if project_id.is_some() {
        stmt.query_map(params![project_id], row_to_memory)?
    } else {
        stmt.query_map([], row_to_memory)?
    };
    let mut list: Vec<MemoryRow> = rows_iter.collect::<Result<Vec<_>, _>>()?;
    if let Some(e) = entity {
        list.retain(|m| m.entity == e);
    }
    Ok(list)
}

fn row_to_memory(r: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRow> {
    Ok(MemoryRow {
        id: r.get(0)?,
        entity: r.get(1)?,
        scope: r.get(2)?,
        entity_type: r.get(3)?,
        body: r.get(4)?,
        project_id: r.get(5)?,
        confidence: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        last_surfaced_at: r.get(9)?,
    })
}

pub fn delete_memory(conn: &Connection, id: &str) -> Result<(), DbError> {
    let n = conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(DbError::NotFound(format!("memory {id}")));
    }
    Ok(())
}

/// 取全部记忆（BM25 在 dss-memory 里做；这里只读候选集：profile + project）。
pub fn candidate_memories(
    conn: &Connection,
    project_id: Option<&str>,
) -> Result<Vec<MemoryRow>, DbError> {
    list_memories(conn, project_id, None)
}

// ----------------- logs -----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRow {
    pub id: i64,
    pub ts: String,
    pub level: String,
    pub source: String,
    pub kind: String,
    pub session_id: Option<String>,
    pub frame_id: Option<String>,
    pub iteration: Option<i64>,
    pub message: String,
    pub detail: Option<String>,
    pub trace_id: Option<String>,
}

pub fn append_log(
    conn: &Connection,
    level: &str,
    source: &str,
    kind: &str,
    session_id: Option<&str>,
    frame_id: Option<&str>,
    iteration: Option<i64>,
    message: &str,
    detail: Option<&str>,
) -> Result<i64, DbError> {
    let ts = now();
    conn.execute(
        "INSERT INTO logs (ts, level, source, kind, session_id, frame_id, iteration, message, detail) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![ts, level, source, kind, session_id, frame_id, iteration, message, detail],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 过滤参数：所有字段可选（owned String，便于跨线程 move 给 conn.interact）。
pub struct LogFilter {
    pub session_id: Option<String>,
    pub source: Option<String>,
    pub level: Option<String>,
    pub kind: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

pub fn list_logs(conn: &Connection, f: &LogFilter) -> Result<(Vec<LogRow>, i64), DbError> {
    let mut where_clauses: Vec<String> = vec![];
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![];
    macro_rules! push {
        ($cond:expr, $val:expr) => {
            where_clauses.push($cond.to_string());
            args.push(Box::new($val));
        };
    }
    if let Some(s) = &f.session_id {
        push!("session_id = ?", s.clone());
    }
    if let Some(s) = &f.source {
        push!("source = ?", s.clone());
    }
    if let Some(s) = &f.level {
        push!("level = ?", s.clone());
    }
    if let Some(s) = &f.kind {
        push!("kind = ?", s.clone());
    }
    if let Some(s) = &f.since {
        push!("ts >= ?", s.clone());
    }
    if let Some(s) = &f.until {
        push!("ts <= ?", s.clone());
    }
    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    // total
    let count_sql = format!("SELECT COUNT(*) FROM logs {where_sql}");
    let total: i64 = conn.query_row(
        &count_sql,
        args.iter()
            .map(|b| b.as_ref())
            .collect::<Vec<_>>()
            .as_slice(),
        |r| r.get(0),
    )?;

    // page
    let page_sql = format!(
        "SELECT id, ts, level, source, kind, session_id, frame_id, iteration, message, detail, trace_id \
         FROM logs {where_sql} ORDER BY ts DESC, id DESC LIMIT ? OFFSET ?"
    );
    let mut page_args: Vec<Box<dyn rusqlite::ToSql>> = args;
    page_args.push(Box::new(f.limit));
    page_args.push(Box::new(f.offset));
    let mut stmt = conn.prepare(&page_sql)?;
    let refs: Vec<&dyn rusqlite::ToSql> = page_args.iter().map(|b| b.as_ref()).collect();
    let rows = stmt
        .query_map(refs.as_slice(), |r| {
            Ok(LogRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                level: r.get(2)?,
                source: r.get(3)?,
                kind: r.get(4)?,
                session_id: r.get(5)?,
                frame_id: r.get(6)?,
                iteration: r.get(7)?,
                message: r.get(8)?,
                detail: r.get(9)?,
                trace_id: r.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok((rows, total))
}

pub fn get_log(conn: &Connection, id: i64) -> Result<Option<LogRow>, DbError> {
    let row = conn
        .query_row(
            "SELECT id, ts, level, source, kind, session_id, frame_id, iteration, message, detail, trace_id \
             FROM logs WHERE id = ?1",
            params![id],
            |r| {
                Ok(LogRow {
                    id: r.get(0)?,
                    ts: r.get(1)?,
                    level: r.get(2)?,
                    source: r.get(3)?,
                    kind: r.get(4)?,
                    session_id: r.get(5)?,
                    frame_id: r.get(6)?,
                    iteration: r.get(7)?,
                    message: r.get(8)?,
                    detail: r.get(9)?,
                    trace_id: r.get(10)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// before=Some → 删该时间之前；None → 全清。返回删除条数。
pub fn delete_logs(conn: &Connection, before: Option<&str>) -> Result<i64, DbError> {
    let n = match before {
        Some(b) => conn.execute("DELETE FROM logs WHERE ts <= ?1", params![b])?,
        None => conn.execute("DELETE FROM logs", [])?,
    };
    Ok(n as i64)
}
