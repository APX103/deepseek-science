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
    pub agent_context: Option<String>,
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
    pub bot_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotRow {
    pub id: String,
    pub name: String,
    pub role: String,
    pub instructions: String,
    pub avatar: String,
    pub color: String,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub thinking_enabled: Option<bool>,
    pub thinking_effort: Option<String>,
    pub enabled: bool,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotJobRow {
    pub id: String,
    pub bot_id: String,
    pub session_id: String,
    pub prompt: String,
    pub requested_plan_mode: bool,
    pub priority: i64,
    pub position: i64,
    pub revision: i64,
    pub status: String,
    pub run_id: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub claimed_at: Option<String>,
    pub completed_at: Option<String>,
}

/// 一条持久化消息（content 是 OpenAI 协议形态 JSON 字符串）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageRow {
    pub seq: i64,
    pub run_id: Option<String>,
    pub role: String,
    pub content: String,
    pub harness_notice: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRow {
    pub run_id: String,
    pub session_id: String,
    pub ordinal: i64,
    pub frame_id: String,
    pub task_summary: String,
    pub plan_mode: bool,
    pub status: String,
    pub kind: Option<String>,
    pub awaiting: Option<String>,
    pub pending_ask_json: Option<String>,
    pub error: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub iterations: i64,
    pub plan_data: Option<String>,
    pub start_seq: Option<i64>,
    pub end_seq: Option<i64>,
    pub started_at: String,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PersistMessage {
    pub role: String,
    pub content: String,
    pub harness_notice: bool,
}

#[derive(Debug, Clone)]
pub struct PersistRunRequest {
    pub run_id: String,
    pub session_id: String,
    pub frame_id: String,
    pub task_summary: String,
    pub plan_mode: bool,
    pub status: String,
    pub kind: Option<String>,
    pub awaiting: Option<String>,
    pub pending_ask_json: Option<String>,
    pub error: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub iterations: i64,
    pub plan_data: Option<String>,
    pub title: Option<String>,
    pub started_at: String,
    /// First sequence already written by crash-safe tool checkpoints for this run.
    pub checkpoint_start_seq: Option<i64>,
    pub messages: Vec<PersistMessage>,
}

#[derive(Debug, Clone)]
pub struct PersistCheckpointRequest {
    pub run_id: String,
    pub session_id: String,
    pub frame_id: String,
    pub task_summary: String,
    pub plan_mode: bool,
    pub status: String,
    pub awaiting: Option<String>,
    pub pending_ask_json: Option<String>,
    pub plan_data: Option<String>,
    pub title: Option<String>,
    pub started_at: String,
    pub expected_count: usize,
    pub messages: Vec<PersistMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistRunResult {
    pub messages_written: usize,
    pub start_seq: Option<i64>,
    pub end_seq: Option<i64>,
    pub ordinal: i64,
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
            "SELECT id, name, description, agent_context, last_session_id, archived, created_at, updated_at \
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
            "SELECT id, name, description, agent_context, last_session_id, archived, created_at, updated_at \
             FROM projects ORDER BY (id = 'proj_default') DESC, updated_at DESC",
        )?
    } else {
        conn.prepare(
            "SELECT id, name, description, agent_context, last_session_id, archived, created_at, updated_at \
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
    agent_context: Option<&str>,
) -> Result<ProjectRow, DbError> {
    let now = now();
    let id = format!("proj_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    conn.execute(
        "INSERT INTO projects (id, name, description, agent_context, archived, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?5)",
        params![id, name, description, agent_context, now],
    )?;
    get_project(conn, &id)?.ok_or_else(|| DbError::Other("just-created project missing".into()))
}

pub fn update_project(
    conn: &Connection,
    id: &str,
    name: Option<&str>,
    description: Option<&str>,
    agent_context: Option<&str>,
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
    if let Some(context) = agent_context {
        conn.execute(
            "UPDATE projects SET agent_context=?1, updated_at=?2 WHERE id=?3",
            params![context, now, id],
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

/// force=false 且项目有会话 → Conflict；force=true 时先把会话迁移到默认项目，
/// 再删除项目。迁移与删除在同一事务内，避免留下 orphan 会话。
pub fn delete_project(conn: &Connection, id: &str, force: bool) -> Result<(), DbError> {
    if id == "proj_default" {
        return Err(DbError::Conflict(
            "default project cannot be deleted".into(),
        ));
    }

    let tx = conn.unchecked_transaction()?;
    let count: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sessions WHERE project_id = ?1",
        params![id],
        |r| r.get(0),
    )?;
    if count > 0 && !force {
        return Err(DbError::Conflict(format!(
            "project {id} has {count} session(s); pass force=true to delete"
        )));
    }

    if count > 0 {
        ensure_default_project(&tx)?;
        tx.execute(
            "UPDATE sessions SET project_id=?1 WHERE project_id=?2",
            params![crate::DEFAULT_PROJECT_ID, id],
        )?;
    }
    let n = tx.execute("DELETE FROM projects WHERE id=?1", params![id])?;
    if n == 0 {
        return Err(DbError::NotFound(format!("project {id}")));
    }
    tx.commit()?;
    Ok(())
}

/// 项目详情 + 可发现的会话列表。
pub fn get_project_detail(
    conn: &Connection,
    id: &str,
) -> Result<(ProjectRow, Vec<SessionRow>), DbError> {
    let proj = get_project(conn, id)?.ok_or_else(|| DbError::NotFound(format!("project {id}")))?;
    let mut stmt = conn.prepare(
        "SELECT id, title, workspace, model, plan_mode, status, project_id, bot_id, created_at, updated_at \
         FROM sessions WHERE project_id = ?1 AND discoverable = 1 ORDER BY updated_at DESC",
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
        agent_context: r.get(3)?,
        last_session_id: r.get(4)?,
        archived: r.get::<_, i64>(5)? != 0,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

// ----------------- bots / durable jobs -----------------

#[allow(clippy::too_many_arguments)]
pub fn create_bot(
    conn: &Connection,
    name: &str,
    role: &str,
    instructions: &str,
    avatar: &str,
    color: &str,
    project_id: Option<&str>,
    model: Option<&str>,
) -> Result<BotRow, DbError> {
    let now = now();
    let id = format!("bot_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    conn.execute(
        "INSERT INTO bots \
         (id, name, role, instructions, avatar, color, project_id, model, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        params![
            id,
            name,
            role,
            instructions,
            avatar,
            color,
            project_id,
            model,
            now
        ],
    )?;
    get_bot(conn, &id)?.ok_or_else(|| DbError::Other("just-created bot missing".into()))
}

pub fn get_bot(conn: &Connection, id: &str) -> Result<Option<BotRow>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id, name, role, instructions, avatar, color, project_id, model, \
             thinking_enabled, thinking_effort, enabled, revision, created_at, updated_at \
             FROM bots WHERE id = ?1",
            params![id],
            row_to_bot,
        )
        .optional()?)
}

pub fn list_bots(conn: &Connection) -> Result<Vec<BotRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, role, instructions, avatar, color, project_id, model, \
         thinking_enabled, thinking_effort, enabled, revision, created_at, updated_at \
         FROM bots ORDER BY enabled DESC, updated_at DESC, id ASC",
    )?;
    let bots = stmt
        .query_map([], row_to_bot)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(bots)
}

#[allow(clippy::too_many_arguments)]
pub fn update_bot(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
    name: &str,
    role: &str,
    instructions: &str,
    avatar: &str,
    color: &str,
    project_id: Option<&str>,
    model: Option<&str>,
    thinking_enabled: Option<bool>,
    thinking_effort: Option<&str>,
    enabled: bool,
) -> Result<BotRow, DbError> {
    let changed = conn.execute(
        "UPDATE bots SET name=?1, role=?2, instructions=?3, avatar=?4, color=?5, \
         project_id=?6, model=?7, thinking_enabled=?8, thinking_effort=?9, enabled=?10, \
         revision=revision+1, updated_at=?11 WHERE id=?12 AND revision=?13",
        params![
            name,
            role,
            instructions,
            avatar,
            color,
            project_id,
            model,
            thinking_enabled.map(i64::from),
            thinking_effort,
            i64::from(enabled),
            now(),
            id,
            expected_revision
        ],
    )?;
    if changed == 0 {
        return if get_bot(conn, id)?.is_some() {
            Err(DbError::Conflict(format!(
                "bot {id} revision no longer matches"
            )))
        } else {
            Err(DbError::NotFound(format!("bot {id}")))
        };
    }
    get_bot(conn, id)?.ok_or_else(|| DbError::NotFound(format!("bot {id}")))
}

pub fn delete_bot(conn: &Connection, id: &str) -> Result<(), DbError> {
    if conn.execute("DELETE FROM bots WHERE id=?1", params![id])? == 0 {
        return Err(DbError::NotFound(format!("bot {id}")));
    }
    Ok(())
}

pub fn enqueue_bot_job(
    conn: &Connection,
    requested_id: Option<&str>,
    bot_id: &str,
    session_id: &str,
    prompt: &str,
    requested_plan_mode: bool,
) -> Result<BotJobRow, DbError> {
    let tx = conn.unchecked_transaction()?;
    let belongs: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND bot_id=?2)",
        params![session_id, bot_id],
        |row| row.get(0),
    )?;
    if !belongs {
        return Err(DbError::Conflict(
            "job session must belong to the selected bot".into(),
        ));
    }
    let position: i64 = tx.query_row(
        "SELECT COALESCE(MAX(position), 0) + 1 FROM bot_jobs \
         WHERE session_id=?1 AND status='queued'",
        params![session_id],
        |row| row.get(0),
    )?;
    let id = requested_id
        .map(str::to_owned)
        .unwrap_or_else(|| format!("job_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]));
    let timestamp = now();
    tx.execute(
        "INSERT INTO bot_jobs \
         (id, bot_id, session_id, prompt, requested_plan_mode, position, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            id,
            bot_id,
            session_id,
            prompt,
            i64::from(requested_plan_mode),
            position,
            timestamp
        ],
    )?;
    tx.commit()?;
    get_bot_job(conn, &id)?.ok_or_else(|| DbError::Other("just-created bot job missing".into()))
}

pub fn get_bot_job(conn: &Connection, id: &str) -> Result<Option<BotJobRow>, DbError> {
    Ok(conn
        .query_row(
            "SELECT id, bot_id, session_id, prompt, requested_plan_mode, priority, position, \
             revision, status, run_id, last_error, created_at, updated_at, claimed_at, completed_at \
             FROM bot_jobs WHERE id=?1",
            params![id],
            row_to_bot_job,
        )
        .optional()?)
}

pub fn list_bot_jobs(conn: &Connection, session_id: &str) -> Result<Vec<BotJobRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, bot_id, session_id, prompt, requested_plan_mode, priority, position, \
         revision, status, run_id, last_error, created_at, updated_at, claimed_at, completed_at \
         FROM bot_jobs WHERE session_id=?1 AND status IN ('queued','running','failed') \
         ORDER BY CASE status WHEN 'running' THEN 0 WHEN 'queued' THEN 1 ELSE 2 END, \
         priority DESC, position ASC, created_at ASC",
    )?;
    let jobs = stmt
        .query_map(params![session_id], row_to_bot_job)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(jobs)
}

pub fn edit_bot_job(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
    prompt: &str,
    requested_plan_mode: bool,
) -> Result<BotJobRow, DbError> {
    let changed = conn.execute(
        "UPDATE bot_jobs SET prompt=?1, requested_plan_mode=?2, revision=revision+1, updated_at=?3 \
         WHERE id=?4 AND revision=?5 AND status='queued'",
        params![prompt, i64::from(requested_plan_mode), now(), id, expected_revision],
    )?;
    if changed == 0 {
        return Err(DbError::Conflict(format!(
            "bot job {id} is stale or no longer queued"
        )));
    }
    get_bot_job(conn, id)?.ok_or_else(|| DbError::NotFound(format!("bot job {id}")))
}

pub fn delete_bot_job(conn: &Connection, id: &str, expected_revision: i64) -> Result<(), DbError> {
    let changed = conn.execute(
        "DELETE FROM bot_jobs WHERE id=?1 AND revision=?2 AND status IN ('queued','failed')",
        params![id, expected_revision],
    )?;
    if changed == 0 {
        return Err(DbError::Conflict(format!(
            "bot job {id} is stale or cannot be deleted"
        )));
    }
    Ok(())
}

pub fn reorder_bot_jobs(
    conn: &Connection,
    session_id: &str,
    ordered_ids: &[String],
) -> Result<Vec<BotJobRow>, DbError> {
    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
        "SELECT id FROM bot_jobs WHERE session_id=?1 AND status='queued' ORDER BY id ASC",
    )?;
    let mut existing = stmt
        .query_map(params![session_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut requested = ordered_ids.to_vec();
    existing.sort();
    requested.sort();
    requested.dedup();
    if existing != requested || requested.len() != ordered_ids.len() {
        return Err(DbError::Conflict(
            "reorder must contain every queued job exactly once".into(),
        ));
    }
    let timestamp = now();
    for (index, id) in ordered_ids.iter().enumerate() {
        tx.execute(
            "UPDATE bot_jobs SET position=?1, revision=revision+1, updated_at=?2 \
             WHERE id=?3 AND session_id=?4 AND status='queued'",
            params![index as i64 + 1, timestamp, id, session_id],
        )?;
    }
    tx.commit()?;
    list_bot_jobs(conn, session_id)
}

pub fn claim_next_bot_job(
    conn: &Connection,
    session_id: &str,
    run_id: &str,
) -> Result<Option<BotJobRow>, DbError> {
    let tx = conn.unchecked_transaction()?;
    let next_id = tx
        .query_row(
            "SELECT id FROM bot_jobs WHERE session_id=?1 AND status='queued' \
             ORDER BY priority DESC, position ASC, created_at ASC LIMIT 1",
            params![session_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let Some(id) = next_id else {
        return Ok(None);
    };
    let timestamp = now();
    tx.execute(
        "UPDATE bot_jobs SET status='running', run_id=?1, revision=revision+1, \
         claimed_at=?2, updated_at=?2 WHERE id=?3 AND status='queued'",
        params![run_id, timestamp, id],
    )?;
    tx.commit()?;
    get_bot_job(conn, &id)
}

pub fn finish_bot_job(
    conn: &Connection,
    id: &str,
    run_id: &str,
    succeeded: bool,
    error: Option<&str>,
) -> Result<BotJobRow, DbError> {
    let timestamp = now();
    let status = if succeeded { "completed" } else { "failed" };
    let changed = conn.execute(
        "UPDATE bot_jobs SET status=?1, last_error=?2, revision=revision+1, \
         completed_at=?3, updated_at=?3 WHERE id=?4 AND run_id=?5 AND status='running'",
        params![status, error, timestamp, id, run_id],
    )?;
    if changed == 0 {
        return Err(DbError::Conflict(format!(
            "bot job {id} is not owned by run {run_id}"
        )));
    }
    get_bot_job(conn, id)?.ok_or_else(|| DbError::NotFound(format!("bot job {id}")))
}

pub fn claim_bot_job(
    conn: &Connection,
    id: &str,
    expected_revision: i64,
    run_id: &str,
) -> Result<BotJobRow, DbError> {
    let timestamp = now();
    let changed = conn.execute(
        "UPDATE bot_jobs SET status='running', run_id=?1, revision=revision+1, \
         claimed_at=?2, updated_at=?2 WHERE id=?3 AND revision=?4 AND status='queued'",
        params![run_id, timestamp, id, expected_revision],
    )?;
    if changed == 0 {
        return Err(DbError::Conflict(format!(
            "bot job {id} is stale or no longer queued"
        )));
    }
    get_bot_job(conn, id)?.ok_or_else(|| DbError::NotFound(format!("bot job {id}")))
}

fn row_to_bot(row: &rusqlite::Row<'_>) -> rusqlite::Result<BotRow> {
    Ok(BotRow {
        id: row.get(0)?,
        name: row.get(1)?,
        role: row.get(2)?,
        instructions: row.get(3)?,
        avatar: row.get(4)?,
        color: row.get(5)?,
        project_id: row.get(6)?,
        model: row.get(7)?,
        thinking_enabled: row.get::<_, Option<i64>>(8)?.map(|value| value != 0),
        thinking_effort: row.get(9)?,
        enabled: row.get::<_, i64>(10)? != 0,
        revision: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
    })
}

fn row_to_bot_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<BotJobRow> {
    Ok(BotJobRow {
        id: row.get(0)?,
        bot_id: row.get(1)?,
        session_id: row.get(2)?,
        prompt: row.get(3)?,
        requested_plan_mode: row.get::<_, i64>(4)? != 0,
        priority: row.get(5)?,
        position: row.get(6)?,
        revision: row.get(7)?,
        status: row.get(8)?,
        run_id: row.get(9)?,
        last_error: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        claimed_at: row.get(13)?,
        completed_at: row.get(14)?,
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
    create_session_for_bot(conn, id, workspace, model, project_id, None)
}

pub fn create_session_for_bot(
    conn: &Connection,
    id: &str,
    workspace: &str,
    model: Option<&str>,
    project_id: Option<&str>,
    bot_id: Option<&str>,
) -> Result<SessionRow, DbError> {
    let now = now();
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO sessions (id, title, workspace, model, plan_mode, status, project_id, bot_id, created_at, updated_at) \
         VALUES (?1, NULL, ?2, ?3, 0, 'active', ?4, ?5, ?6, ?6)",
        params![id, workspace, model, project_id, bot_id, now],
    )?;
    if let Some(project_id) = project_id {
        let changed = tx.execute(
            "UPDATE projects SET last_session_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![id, now, project_id],
        )?;
        if changed == 0 {
            return Err(DbError::NotFound(format!("project {project_id}")));
        }
    }
    tx.commit()?;
    get_session(conn, id)?.ok_or_else(|| DbError::Other("just-created session missing".into()))
}

pub fn get_session(conn: &Connection, id: &str) -> Result<Option<SessionRow>, DbError> {
    let row = conn
        .query_row(
            "SELECT id, title, workspace, model, plan_mode, status, project_id, bot_id, created_at, updated_at \
             FROM sessions WHERE id = ?1",
            params![id],
            row_to_session,
        )
        .optional()?;
    Ok(row)
}

pub fn list_sessions(conn: &Connection) -> Result<Vec<SessionRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.title, s.workspace, s.model, s.plan_mode, s.status, s.project_id, s.bot_id, s.created_at, s.updated_at \
         FROM sessions AS s \
         LEFT JOIN projects AS p ON p.id = s.project_id \
         WHERE s.discoverable = 1 \
           AND (s.project_id IS NULL OR COALESCE(p.archived, 0) = 0) \
         ORDER BY s.updated_at DESC",
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

pub fn set_session_status(conn: &Connection, id: &str, status: &str) -> Result<(), DbError> {
    let now = now();
    conn.execute(
        "UPDATE sessions SET status=?1, updated_at=?2 WHERE id=?3",
        params![status, now, id],
    )?;
    Ok(())
}

/// Rebase a session workspace only when it still contains the path observed by the caller.
///
/// Restoring a copied data directory may require replacing an obsolete absolute path. The
/// compare-and-swap guard keeps two concurrent restorers from overwriting a newer, valid path.
/// Workspace rebasing is storage repair rather than user activity, so it deliberately preserves
/// `updated_at` and therefore does not reorder the session list.
pub fn rebase_session_workspace(
    conn: &Connection,
    id: &str,
    expected_workspace: &str,
    new_workspace: &str,
) -> Result<bool, DbError> {
    let changed = conn.execute(
        "UPDATE sessions SET workspace=?1 WHERE id=?2 AND workspace=?3",
        params![new_workspace, id, expected_workspace],
    )?;
    Ok(changed == 1)
}

/// Control whether a session appears in project/global discovery without deleting it.
/// Exact-id reads, messages, runs, workspace and project scope remain unchanged.
pub fn set_session_discoverable(
    conn: &Connection,
    id: &str,
    discoverable: bool,
) -> Result<(), DbError> {
    let now = now();
    let tx = conn.unchecked_transaction()?;
    let project_id = tx
        .query_row(
            "SELECT project_id FROM sessions WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("session {id}")))?;
    tx.execute(
        "UPDATE sessions SET discoverable = ?1, updated_at = ?2 WHERE id = ?3",
        params![if discoverable { 1 } else { 0 }, now, id],
    )?;

    if !discoverable {
        if let Some(project_id) = project_id {
            let replacement = tx
                .query_row(
                    "SELECT id FROM sessions \
                     WHERE project_id = ?1 AND discoverable = 1 AND id != ?2 \
                     ORDER BY updated_at DESC, id ASC LIMIT 1",
                    params![project_id, id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            tx.execute(
                "UPDATE projects SET last_session_id = ?1, updated_at = ?2 \
                 WHERE id = ?3 AND last_session_id = ?4",
                params![replacement, now, project_id, id],
            )?;
        }
    }

    tx.commit()?;
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

/// Atomically persist a plan and its matching session status.
///
/// Plan approval is a two-phase workflow: the approved plan must remain
/// retryable until a later execution request is accepted. Keeping both fields
/// in one UPDATE prevents a crash or database error from exposing a
/// half-approved session.
pub fn set_session_plan_and_status(
    conn: &Connection,
    id: &str,
    plan_data: Option<&str>,
    status: &str,
) -> Result<(), DbError> {
    let now = now();
    let changed = conn.execute(
        "UPDATE sessions SET plan_data=?1, status=?2, updated_at=?3 WHERE id=?4",
        params![plan_data, status, now, id],
    )?;
    if changed == 0 {
        return Err(DbError::NotFound(format!("session {id}")));
    }
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
        bot_id: r.get(7)?,
        created_at: r.get(8)?,
        updated_at: r.get(9)?,
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
        "INSERT INTO session_messages \
         (session_id, seq, run_id, role, content, harness_notice, created_at) \
         VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
        params![
            session_id,
            seq,
            role,
            content,
            if harness_notice { 1 } else { 0 },
            now
        ],
    )?;
    Ok(seq)
}

pub fn list_messages(conn: &Connection, session_id: &str) -> Result<Vec<MessageRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT seq, run_id, role, content, harness_notice FROM session_messages \
         WHERE session_id = ?1 ORDER BY seq ASC",
    )?;
    let list = stmt
        .query_map(params![session_id], |r| {
            Ok(MessageRow {
                seq: r.get(0)?,
                run_id: r.get(1)?,
                role: r.get(2)?,
                content: r.get(3)?,
                harness_notice: r.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(list)
}

/// Atomically append canonical messages produced through one delivered tool batch. Rows remain
/// temporarily unowned (`run_id = NULL`) until terminal `persist_run` attaches them; if the
/// process dies first they are still restorable as an interrupted legacy-style turn.
pub fn append_history_checkpoint(
    conn: &Connection,
    request: &PersistCheckpointRequest,
) -> Result<usize, DbError> {
    let now = now();
    let tx = conn.unchecked_transaction()?;
    let (count, max_seq): (i64, i64) = tx.query_row(
        "SELECT COUNT(*), COALESCE(MAX(seq), 0) FROM session_messages WHERE session_id = ?1",
        params![request.session_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if count != request.expected_count as i64 || max_seq != request.expected_count as i64 {
        return Err(DbError::Other(format!(
            "history checkpoint cursor drift: expected {}, found count={count}, max_seq={max_seq}",
            request.expected_count
        )));
    }
    let existing_run = tx
        .query_row(
            "SELECT session_id, ordinal, start_seq, end_seq, completed_at \
             FROM session_runs WHERE run_id = ?1",
            params![request.run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let appended_start = (!request.messages.is_empty()).then_some(max_seq + 1);
    let appended_end =
        (!request.messages.is_empty()).then_some(max_seq + request.messages.len() as i64);
    match existing_run {
        Some((session_id, _ordinal, start_seq, end_seq, completed_at)) => {
            if session_id != request.session_id {
                return Err(DbError::Other(format!(
                    "checkpoint run {} belongs to another session",
                    request.run_id
                )));
            }
            if completed_at.is_some() {
                return Err(DbError::Other(format!(
                    "checkpoint run {} is already terminal",
                    request.run_id
                )));
            }
            if end_seq != Some(max_seq) {
                return Err(DbError::Other(format!(
                    "checkpoint run cursor drift: expected end_seq={max_seq}, found {end_seq:?}"
                )));
            }
            tx.execute(
                "UPDATE session_runs SET frame_id=?1, task_summary=?2, plan_mode=?3, status=?4, \
                 awaiting=?5, pending_ask_json=?6, plan_data=?7, \
                 start_seq=COALESCE(start_seq, ?8), end_seq=COALESCE(?9, end_seq) \
                 WHERE run_id=?10",
                params![
                    request.frame_id,
                    request.task_summary,
                    if request.plan_mode { 1 } else { 0 },
                    request.status,
                    request.awaiting,
                    request.pending_ask_json,
                    request.plan_data,
                    start_seq.or(appended_start),
                    appended_end,
                    request.run_id,
                ],
            )?;
        }
        None => {
            let ordinal: i64 = tx.query_row(
                "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM session_runs WHERE session_id = ?1",
                params![request.session_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO session_runs (\
                     run_id, session_id, ordinal, frame_id, task_summary, plan_mode, status, kind, \
                     awaiting, pending_ask_json, error, input_tokens, output_tokens, iterations, \
                     plan_data, start_seq, end_seq, started_at, completed_at\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, NULL, 0, 0, 0, \
                           ?10, ?11, ?12, ?13, NULL)",
                params![
                    request.run_id,
                    request.session_id,
                    ordinal,
                    request.frame_id,
                    request.task_summary,
                    if request.plan_mode { 1 } else { 0 },
                    request.status,
                    request.awaiting,
                    request.pending_ask_json,
                    request.plan_data,
                    appended_start,
                    appended_end,
                    request.started_at,
                ],
            )?;
        }
    }
    for (offset, message) in request.messages.iter().enumerate() {
        let seq = max_seq + offset as i64 + 1;
        tx.execute(
            "INSERT INTO session_messages \
             (session_id, seq, run_id, role, content, harness_notice, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                request.session_id,
                seq,
                request.run_id,
                message.role,
                message.content,
                if message.harness_notice { 1 } else { 0 },
                now,
            ],
        )?;
    }
    let changed = tx.execute(
        "UPDATE sessions SET \
         title = CASE WHEN title IS NULL THEN ?1 ELSE title END, \
         plan_mode = ?2, status = ?3, plan_data = ?4, updated_at = ?5 WHERE id = ?6",
        params![
            request.title,
            if request.plan_mode { 1 } else { 0 },
            request.status,
            request.plan_data,
            now,
            request.session_id,
        ],
    )?;
    if changed == 0 {
        return Err(DbError::NotFound(format!("session {}", request.session_id)));
    }
    tx.commit()?;
    Ok(request.expected_count + request.messages.len())
}

/// Atomically commit one completed/awaiting/failed/cancelled run and every
/// canonical message it added. Session state and project recency advance in
/// the same transaction, so GET/list can never observe a terminal run with a
/// stale transcript (or vice versa).
pub fn persist_run(
    conn: &Connection,
    request: &PersistRunRequest,
) -> Result<PersistRunResult, DbError> {
    let completed_at = now();
    let tx = conn.unchecked_transaction()?;

    let (project_id, discoverable) = tx
        .query_row(
            "SELECT project_id, discoverable FROM sessions WHERE id = ?1",
            params![request.session_id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("session {}", request.session_id)))?;

    let existing_checkpoint = tx
        .query_row(
            "SELECT session_id, ordinal, start_seq, end_seq, completed_at \
             FROM session_runs WHERE run_id = ?1",
            params![request.run_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?;
    let ordinal: i64 = match existing_checkpoint.as_ref() {
        Some((session_id, ordinal, _, _, completed_at)) => {
            if session_id != &request.session_id {
                return Err(DbError::Other(format!(
                    "run {} belongs to another session",
                    request.run_id
                )));
            }
            if completed_at.is_some() {
                return Err(DbError::Other(format!(
                    "run {} is already terminal",
                    request.run_id
                )));
            }
            *ordinal
        }
        None => tx.query_row(
            "SELECT COALESCE(MAX(ordinal), 0) + 1 FROM session_runs WHERE session_id = ?1",
            params![request.session_id],
            |row| row.get(0),
        )?,
    };
    let previous_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM session_messages WHERE session_id = ?1",
        params![request.session_id],
        |row| row.get(0),
    )?;
    let appended_start_seq = (!request.messages.is_empty()).then_some(previous_seq + 1);
    let existing_start_seq = existing_checkpoint.as_ref().and_then(|row| row.2);
    let existing_end_seq = existing_checkpoint.as_ref().and_then(|row| row.3);
    if existing_checkpoint.is_some() {
        if existing_start_seq != request.checkpoint_start_seq {
            return Err(DbError::Other(format!(
                "checkpoint start mismatch: request={:?}, stored={existing_start_seq:?}",
                request.checkpoint_start_seq
            )));
        }
        if existing_end_seq != Some(previous_seq) {
            return Err(DbError::Other(format!(
                "checkpoint end mismatch: stored={existing_end_seq:?}, messages end={previous_seq}"
            )));
        }
    }
    let start_seq = existing_start_seq
        .or(request.checkpoint_start_seq)
        .or(appended_start_seq);
    let end_seq = if request.messages.is_empty() {
        existing_end_seq.or(request.checkpoint_start_seq.map(|_| previous_seq))
    } else {
        Some(previous_seq + request.messages.len() as i64)
    };

    if existing_checkpoint.is_some() {
        tx.execute(
            "UPDATE session_runs SET frame_id=?1, task_summary=?2, plan_mode=?3, status=?4, \
             kind=?5, awaiting=?6, pending_ask_json=?7, error=?8, input_tokens=?9, \
             output_tokens=?10, iterations=?11, plan_data=?12, start_seq=?13, end_seq=?14, \
             started_at=?15, completed_at=?16 WHERE run_id=?17",
            params![
                request.frame_id,
                request.task_summary,
                if request.plan_mode { 1 } else { 0 },
                request.status,
                request.kind,
                request.awaiting,
                request.pending_ask_json,
                request.error,
                request.input_tokens,
                request.output_tokens,
                request.iterations,
                request.plan_data,
                start_seq,
                end_seq,
                request.started_at,
                completed_at,
                request.run_id,
            ],
        )?;
    } else {
        tx.execute(
            "INSERT INTO session_runs (\
                 run_id, session_id, ordinal, frame_id, task_summary, plan_mode, status, kind, \
                 awaiting, pending_ask_json, error, input_tokens, output_tokens, iterations, \
                 plan_data, start_seq, end_seq, started_at, completed_at\
             ) VALUES (\
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                 ?15, ?16, ?17, ?18, ?19\
             )",
            params![
                request.run_id,
                request.session_id,
                ordinal,
                request.frame_id,
                request.task_summary,
                if request.plan_mode { 1 } else { 0 },
                request.status,
                request.kind,
                request.awaiting,
                request.pending_ask_json,
                request.error,
                request.input_tokens,
                request.output_tokens,
                request.iterations,
                request.plan_data,
                start_seq,
                end_seq,
                request.started_at,
                completed_at,
            ],
        )?;
    }

    let checkpoint_messages = if let Some(checkpoint_start) = request.checkpoint_start_seq {
        if checkpoint_start <= 0 || checkpoint_start > previous_seq {
            return Err(DbError::Other(format!(
                "invalid checkpoint range {checkpoint_start}..={previous_seq}"
            )));
        }
        let expected = previous_seq - checkpoint_start + 1;
        let changed: i64 = tx.query_row(
            "SELECT COUNT(*) FROM session_messages \
             WHERE session_id = ?1 AND seq >= ?2 AND seq <= ?3 AND run_id = ?4",
            params![
                request.session_id,
                checkpoint_start,
                previous_seq,
                request.run_id,
            ],
            |row| row.get(0),
        )?;
        if changed != expected {
            return Err(DbError::Other(format!(
                "checkpoint ownership mismatch: expected {expected} rows, attached {changed}"
            )));
        }
        expected as usize
    } else {
        0
    };

    for (offset, message) in request.messages.iter().enumerate() {
        let seq = previous_seq + offset as i64 + 1;
        tx.execute(
            "INSERT INTO session_messages \
             (session_id, seq, run_id, role, content, harness_notice, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                request.session_id,
                seq,
                request.run_id,
                message.role,
                message.content,
                if message.harness_notice { 1 } else { 0 },
                completed_at,
            ],
        )?;
    }

    let changed = tx.execute(
        "UPDATE sessions SET \
             title = CASE WHEN title IS NULL THEN ?1 ELSE title END, \
             plan_mode = ?2, status = ?3, plan_data = ?4, updated_at = ?5 \
         WHERE id = ?6",
        params![
            request.title,
            if request.plan_mode { 1 } else { 0 },
            request.status,
            request.plan_data,
            completed_at,
            request.session_id,
        ],
    )?;
    if changed == 0 {
        return Err(DbError::NotFound(format!("session {}", request.session_id)));
    }

    if discoverable {
        if let Some(project_id) = project_id {
            tx.execute(
                "UPDATE projects SET last_session_id = ?1, updated_at = ?2 WHERE id = ?3",
                params![request.session_id, completed_at, project_id],
            )?;
        }
    }

    tx.commit()?;
    Ok(PersistRunResult {
        messages_written: checkpoint_messages + request.messages.len(),
        start_seq,
        end_seq,
        ordinal,
    })
}

pub fn list_runs(conn: &Connection, session_id: &str) -> Result<Vec<RunRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT run_id, session_id, ordinal, frame_id, task_summary, plan_mode, status, kind, \
                awaiting, pending_ask_json, error, input_tokens, output_tokens, iterations, \
                plan_data, start_seq, end_seq, started_at, completed_at \
         FROM session_runs WHERE session_id = ?1 ORDER BY ordinal ASC",
    )?;
    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok(RunRow {
                run_id: row.get(0)?,
                session_id: row.get(1)?,
                ordinal: row.get(2)?,
                frame_id: row.get(3)?,
                task_summary: row.get(4)?,
                plan_mode: row.get::<_, i64>(5)? != 0,
                status: row.get(6)?,
                kind: row.get(7)?,
                awaiting: row.get(8)?,
                pending_ask_json: row.get(9)?,
                error: row.get(10)?,
                input_tokens: row.get(11)?,
                output_tokens: row.get(12)?,
                iterations: row.get(13)?,
                plan_data: row.get(14)?,
                start_seq: row.get(15)?,
                end_seq: row.get(16)?,
                started_at: row.get(17)?,
                completed_at: row.get(18)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
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
    // --- L2 Claim Store 扩展字段 ---
    pub status: String,
    pub claim_type: String,
    pub evidence_refs: Option<String>,
    pub origin: String,
    pub superseded_by: Option<String>,
    pub valid_from: Option<String>,
    pub valid_until: Option<String>,
    pub deleted_at: Option<String>,
    pub source_hash: Option<String>,
}

/// 记忆写入参数：把 Claim Store 的丰富维度集中到一个结构，避免 append 签名爆炸。
#[derive(Debug, Clone, Default)]
pub struct NewMemory<'a> {
    pub id: &'a str,
    pub body: &'a str,
    pub scope: Option<&'a str>,
    pub project_id: Option<&'a str>,
    pub confidence: Option<f64>,
    pub claim_type: Option<&'a str>,
    pub status: Option<&'a str>,
    pub origin: Option<&'a str>,
    pub evidence_refs: Option<&'a str>,
    pub source_hash: Option<&'a str>,
    pub valid_until: Option<&'a str>,
}

/// memories 的统一 SELECT 列表（与 row_to_memory 的列序严格对齐）。
const MEM_COLS: &str = "id, entity, scope, entity_type, body, project_id, confidence, \
     created_at, updated_at, last_surfaced_at, status, claim_type, evidence_refs, origin, \
     superseded_by, valid_from, valid_until, deleted_at, source_hash";

pub fn append_memory_full(conn: &Connection, m: NewMemory<'_>) -> Result<MemoryRow, DbError> {
    let now = now();
    let scope = m.scope.unwrap_or("project");
    let confidence = m.confidence.unwrap_or(0.5);
    let claim_type = m.claim_type.unwrap_or("note");
    let status = m.status.unwrap_or("active");
    let origin = m.origin.unwrap_or("auto");
    conn.execute(
        "INSERT INTO memories (id, entity, scope, entity_type, body, project_id, confidence, \
         created_at, updated_at, status, claim_type, evidence_refs, origin, valid_until, source_hash) \
         VALUES (?1, ?2, ?3, 'note', ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            m.id,
            scope,
            scope,
            m.body,
            m.project_id,
            confidence,
            now,
            status,
            claim_type,
            m.evidence_refs,
            origin,
            m.valid_until,
            m.source_hash,
        ],
    )?;
    get_memory(conn, m.id)?.ok_or_else(|| DbError::Other("just-inserted memory missing".into()))
}

pub fn append_memory(
    conn: &Connection,
    id: &str,
    body: &str,
    scope: Option<&str>,
    project_id: Option<&str>,
) -> Result<MemoryRow, DbError> {
    append_memory_full(
        conn,
        NewMemory {
            id,
            body,
            scope,
            project_id,
            ..Default::default()
        },
    )
}

pub fn get_memory(conn: &Connection, id: &str) -> Result<Option<MemoryRow>, DbError> {
    let sql = format!("SELECT {MEM_COLS} FROM memories WHERE id = ?1");
    let row = conn
        .query_row(&sql, params![id], row_to_memory)
        .optional()?;
    Ok(row)
}

/// 列记忆：profile(scope=profile，跨项目) + 当前 project 的。
/// 可选过滤 status（默认只看 active + candidate，排除被替代/已删的）。
pub fn list_memories(
    conn: &Connection,
    project_id: Option<&str>,
    entity: Option<&str>,
) -> Result<Vec<MemoryRow>, DbError> {
    list_memories_filtered(
        conn,
        MemoryFilter {
            project_id,
            entity,
            status: None,
        },
    )
}

#[derive(Debug, Clone, Default)]
pub struct MemoryFilter<'a> {
    pub project_id: Option<&'a str>,
    pub entity: Option<&'a str>,
    /// None = 不按 status 过滤（兼容旧调用）；Some = 仅返回该 status。
    pub status: Option<&'a str>,
}

pub fn list_memories_filtered(
    conn: &Connection,
    f: MemoryFilter<'_>,
) -> Result<Vec<MemoryRow>, DbError> {
    let sql = format!(
        "SELECT {MEM_COLS} FROM memories WHERE (scope = 'profile' OR (?1 IS NULL OR project_id = ?1)) \
         AND (?2 IS NULL OR status = ?2) ORDER BY updated_at DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let pid = f.project_id;
    let rows = stmt.query_map(params![pid, f.status], row_to_memory)?;
    let mut list: Vec<MemoryRow> = rows.collect::<Result<Vec<_>, _>>()?;
    if let Some(e) = f.entity {
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
        status: r.get(10)?,
        claim_type: r.get(11)?,
        evidence_refs: r.get(12)?,
        origin: r.get(13)?,
        superseded_by: r.get(14)?,
        valid_from: r.get(15)?,
        valid_until: r.get(16)?,
        deleted_at: r.get(17)?,
        source_hash: r.get(18)?,
    })
}

/// 软删除：置 status=deleted + deleted_at，保留行用于审计。
pub fn soft_delete_memory(conn: &Connection, id: &str) -> Result<(), DbError> {
    let now = now();
    let n = conn.execute(
        "UPDATE memories SET status = 'deleted', deleted_at = ?2, updated_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    if n == 0 {
        return Err(DbError::NotFound(format!("memory {id}")));
    }
    Ok(())
}

pub fn delete_memory(conn: &Connection, id: &str) -> Result<(), DbError> {
    let n = conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
    if n == 0 {
        return Err(DbError::NotFound(format!("memory {id}")));
    }
    Ok(())
}

/// 标记被替代：old.status=superseded + old.superseded_by=new_id。
pub fn supersede_memory(conn: &Connection, old_id: &str, new_id: &str) -> Result<(), DbError> {
    let now = now();
    let n = conn.execute(
        "UPDATE memories SET status = 'superseded', superseded_by = ?2, updated_at = ?3 WHERE id = ?1 \
         AND status IN ('active', 'candidate')",
        params![old_id, new_id, now],
    )?;
    if n == 0 {
        return Err(DbError::NotFound(format!(
            "active/candidate memory {old_id}"
        )));
    }
    Ok(())
}

/// 更新状态（active/candidate/superseded/expired/deleted）。
pub fn update_memory_status(conn: &Connection, id: &str, status: &str) -> Result<(), DbError> {
    let now = now();
    let n = conn.execute(
        "UPDATE memories SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, status, now],
    )?;
    if n == 0 {
        return Err(DbError::NotFound(format!("memory {id}")));
    }
    Ok(())
}

/// 编辑 body（创建新版本由调用方处理，这里只改文本，用于同版本订正）。
/// source_hash 由调用方（dss-memory 层）用 memory_hash() 计算后传入，保证与去重路径一致。
pub fn update_memory_body(
    conn: &Connection,
    id: &str,
    body: &str,
    source_hash: Option<&str>,
) -> Result<(), DbError> {
    let now = now();
    let n = conn.execute(
        "UPDATE memories SET body = ?2, source_hash = COALESCE(?3, source_hash), updated_at = ?4 WHERE id = ?1",
        params![id, body, source_hash, now],
    )?;
    if n == 0 {
        return Err(DbError::NotFound(format!("memory {id}")));
    }
    Ok(())
}

/// 更新召回时间戳（批量）。
pub fn touch_surfaced(conn: &Connection, ids: &[String]) -> Result<(), DbError> {
    if ids.is_empty() {
        return Ok(());
    }
    let now = now();
    let tx = conn.unchecked_transaction()?;
    for id in ids {
        tx.execute(
            "UPDATE memories SET last_surfaced_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// 精确查 source_hash（去重用）。
pub fn find_by_source_hash(
    conn: &Connection,
    hash: &str,
    project_id: Option<&str>,
) -> Result<Vec<MemoryRow>, DbError> {
    let sql = format!(
        "SELECT {MEM_COLS} FROM memories WHERE source_hash = ?1 \
         AND (scope = 'profile' OR (?2 IS NULL OR project_id = ?2)) \
         AND status IN ('active', 'candidate')"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![hash, project_id], row_to_memory)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// 取全部记忆（BM25 在 dss-memory 里做；这里只读候选集：profile + project）。
pub fn candidate_memories(
    conn: &Connection,
    project_id: Option<&str>,
) -> Result<Vec<MemoryRow>, DbError> {
    list_memories(conn, project_id, None)
}

// ----------------- memory_events -----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEventRow {
    pub id: String,
    pub memory_id: String,
    pub event_type: String,
    pub actor: Option<String>,
    pub detail: Option<String>,
    pub created_at: String,
}

pub fn append_memory_event(
    conn: &Connection,
    id: &str,
    memory_id: &str,
    event_type: &str,
    actor: Option<&str>,
    detail: Option<&str>,
) -> Result<(), DbError> {
    let now = now();
    conn.execute(
        "INSERT INTO memory_events (id, memory_id, event_type, actor, detail, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, memory_id, event_type, actor, detail, now],
    )?;
    Ok(())
}

pub fn list_memory_events(
    conn: &Connection,
    memory_id: &str,
) -> Result<Vec<MemoryEventRow>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT id, memory_id, event_type, actor, detail, created_at FROM memory_events \
         WHERE memory_id = ?1 ORDER BY created_at ASC",
    )?;
    let rows = stmt.query_map(params![memory_id], |r| {
        Ok(MemoryEventRow {
            id: r.get(0)?,
            memory_id: r.get(1)?,
            event_type: r.get(2)?,
            actor: r.get(3)?,
            detail: r.get(4)?,
            created_at: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
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

// The repository boundary keeps the SQL columns explicit; collapsing these
// into an untyped tuple would make call sites and migrations harder to audit.
#[allow(clippy::too_many_arguments)]
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

/// 保留策略清理统计（D-T07：按天 + 按量双限制）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PruneStats {
    /// 按「过期天数」删除的条数。
    pub by_age: i64,
    /// 按「最大条数」删除的条数（删最旧的）。
    pub by_count: i64,
}

impl PruneStats {
    pub fn total(&self) -> i64 {
        self.by_age + self.by_count
    }
}

/// 保留策略清理（D-T07）。
///
/// - `before_iso`：删 `ts < before_iso` 的行（ISO8601 字典序即时间序）。
/// - `max_rows`：按天删完后，若总条数仍超 `max_rows`，删最旧的直到不超。
///
/// 幂等，可周期调用。两步分别记数，便于 observability。
pub fn prune_logs(
    conn: &Connection,
    before_iso: &str,
    max_rows: u32,
) -> Result<PruneStats, DbError> {
    // 1) 按天：删 ts < before_iso。
    let by_age = conn.execute("DELETE FROM logs WHERE ts < ?1", params![before_iso])? as i64;

    // 2) 按量：若总条数仍超 max_rows，删最旧的到 max_rows。
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))?;
    let max_rows = max_rows as i64;
    let by_count = if total > max_rows {
        let excess = total - max_rows;
        conn.execute(
            "DELETE FROM logs WHERE id IN (\
                SELECT id FROM logs ORDER BY ts ASC LIMIT ?1\
            )",
            params![excess],
        )? as i64
    } else {
        0
    };

    Ok(PruneStats { by_age, by_count })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_connection() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE projects (
                id              TEXT PRIMARY KEY,
                name            TEXT NOT NULL,
                description     TEXT,
                agent_context   TEXT,
                last_session_id TEXT,
                archived        INTEGER NOT NULL DEFAULT 0,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );
            CREATE TABLE sessions (
                id         TEXT PRIMARY KEY,
                title      TEXT,
                workspace  TEXT NOT NULL,
                model      TEXT,
                plan_mode  INTEGER NOT NULL DEFAULT 0,
                status     TEXT NOT NULL DEFAULT 'active',
                discoverable INTEGER NOT NULL DEFAULT 1,
                project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
                bot_id TEXT REFERENCES bots(id) ON DELETE SET NULL,
                plan_data  TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE bots (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL,
                instructions TEXT NOT NULL DEFAULT '', avatar TEXT NOT NULL DEFAULT '🤖',
                color TEXT NOT NULL DEFAULT '#4D6BFE', project_id TEXT REFERENCES projects(id),
                model TEXT, thinking_enabled INTEGER, thinking_effort TEXT,
                enabled INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE bot_jobs (
                id TEXT PRIMARY KEY, bot_id TEXT NOT NULL REFERENCES bots(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                prompt TEXT NOT NULL, requested_plan_mode INTEGER NOT NULL DEFAULT 0,
                priority INTEGER NOT NULL DEFAULT 0, position INTEGER NOT NULL,
                revision INTEGER NOT NULL DEFAULT 1, status TEXT NOT NULL DEFAULT 'queued',
                run_id TEXT, last_error TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                claimed_at TEXT, completed_at TEXT
            );
            CREATE TABLE session_runs (
                run_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL,
                frame_id TEXT NOT NULL,
                task_summary TEXT NOT NULL,
                plan_mode INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                kind TEXT,
                awaiting TEXT,
                pending_ask_json TEXT,
                error TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                iterations INTEGER NOT NULL DEFAULT 0,
                plan_data TEXT,
                start_seq INTEGER,
                end_seq INTEGER,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                UNIQUE(session_id, ordinal)
            );
            CREATE TABLE session_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq INTEGER NOT NULL,
                run_id TEXT REFERENCES session_runs(run_id) ON DELETE SET NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                harness_notice INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                UNIQUE(session_id, seq)
            );
            "#,
        )
        .expect("create repository test schema");
        ensure_default_project(&conn).expect("create default project");
        conn
    }

    #[test]
    fn force_delete_migrates_sessions_to_default_project() {
        let conn = test_connection();
        let project =
            create_project(&conn, "Research", Some("experiment"), None).expect("create project");
        let first = create_session(
            &conn,
            "session-one",
            "/workspaces/session-one",
            Some("deepseek-chat"),
            Some(&project.id),
        )
        .expect("create first session");
        let second = create_session(
            &conn,
            "session-two",
            "/workspaces/session-two",
            Some("deepseek-reasoner"),
            Some(&project.id),
        )
        .expect("create second session");

        delete_project(&conn, &project.id, true).expect("force delete project");

        assert!(
            get_project(&conn, &project.id)
                .expect("look up deleted project")
                .is_none(),
            "force delete must remove the source project"
        );
        for before in [first, second] {
            let after = get_session(&conn, &before.id)
                .expect("look up migrated session")
                .expect("migrated session must be preserved");
            assert_eq!(after.project_id.as_deref(), Some(crate::DEFAULT_PROJECT_ID));
            assert_eq!(after.workspace, before.workspace);
            assert_eq!(after.model, before.model);
            assert_eq!(after.status, before.status);
            assert_eq!(after.created_at, before.created_at);
        }

        let migrated_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE project_id = ?1",
                params![crate::DEFAULT_PROJECT_ID],
                |row| row.get(0),
            )
            .expect("count migrated sessions");
        assert_eq!(migrated_count, 2);
    }

    #[test]
    fn create_session_atomically_updates_project_last_session() {
        let conn = test_connection();
        let project = create_project(&conn, "Research", None, None).unwrap();

        create_session(
            &conn,
            "empty-session",
            "/workspaces/empty-session",
            None,
            Some(&project.id),
        )
        .unwrap();

        assert_eq!(
            get_project(&conn, &project.id)
                .unwrap()
                .unwrap()
                .last_session_id
                .as_deref(),
            Some("empty-session")
        );
    }

    #[test]
    fn discovery_hides_archived_projects_and_internal_sessions_without_breaking_restore() {
        let conn = test_connection();
        let visible = create_project(&conn, "Visible", None, None).unwrap();
        let archived = create_project(&conn, "Archived", None, None).unwrap();

        create_session(
            &conn,
            "visible-session",
            "/workspaces/visible",
            None,
            Some(&visible.id),
        )
        .unwrap();
        create_session(
            &conn,
            "archived-session",
            "/workspaces/archived",
            None,
            Some(&archived.id),
        )
        .unwrap();
        create_session(
            &conn,
            "unassigned-session",
            "/workspaces/unassigned",
            None,
            None,
        )
        .unwrap();
        create_session(
            &conn,
            "internal-session",
            "/workspaces/internal",
            None,
            Some(&visible.id),
        )
        .unwrap();
        set_session_discoverable(&conn, "internal-session", false).unwrap();
        assert_eq!(
            get_project(&conn, &visible.id)
                .unwrap()
                .unwrap()
                .last_session_id
                .as_deref(),
            Some("visible-session")
        );
        persist_run(&conn, &run_request("internal-session", "internal-run")).unwrap();
        assert_eq!(
            get_project(&conn, &visible.id)
                .unwrap()
                .unwrap()
                .last_session_id
                .as_deref(),
            Some("visible-session"),
            "running a hidden session must not make it the project's entry point"
        );
        set_project_archived(&conn, &archived.id, true).unwrap();

        let listed_ids = list_sessions(&conn)
            .unwrap()
            .into_iter()
            .map(|session| session.id)
            .collect::<Vec<_>>();
        assert!(listed_ids.contains(&"visible-session".to_string()));
        assert!(listed_ids.contains(&"unassigned-session".to_string()));
        assert!(!listed_ids.contains(&"archived-session".to_string()));
        assert!(!listed_ids.contains(&"internal-session".to_string()));

        let restored = get_session(&conn, "archived-session")
            .unwrap()
            .expect("an archived project's session remains directly restorable");
        assert_eq!(restored.project_id.as_deref(), Some(archived.id.as_str()));

        let restored_internal = get_session(&conn, "internal-session")
            .unwrap()
            .expect("a non-discoverable session remains directly restorable");
        assert_eq!(
            restored_internal.project_id.as_deref(),
            Some(visible.id.as_str())
        );

        let (_, visible_project_sessions) = get_project_detail(&conn, &visible.id).unwrap();
        assert_eq!(
            visible_project_sessions
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            vec!["visible-session"]
        );
    }

    #[test]
    fn non_force_delete_rejects_non_empty_and_default_projects() {
        let conn = test_connection();
        let project =
            create_project(&conn, "Research", None, None).expect("create non-empty project");
        create_session(
            &conn,
            "session-one",
            "/workspaces/session-one",
            None,
            Some(&project.id),
        )
        .expect("create project session");

        let error = delete_project(&conn, &project.id, false)
            .expect_err("non-force delete must reject a non-empty project");
        assert!(matches!(error, DbError::Conflict(_)));
        assert!(get_project(&conn, &project.id)
            .expect("look up retained project")
            .is_some());
        assert_eq!(
            get_session(&conn, "session-one")
                .expect("look up retained session")
                .expect("session must be retained")
                .project_id
                .as_deref(),
            Some(project.id.as_str())
        );

        for force in [false, true] {
            let error = delete_project(&conn, crate::DEFAULT_PROJECT_ID, force)
                .expect_err("default project must never be deleted");
            assert!(matches!(error, DbError::Conflict(_)));
        }
        assert!(get_project(&conn, crate::DEFAULT_PROJECT_ID)
            .expect("look up default project")
            .is_some());
    }

    #[test]
    fn plan_and_status_are_persisted_as_one_retryable_state() {
        let conn = test_connection();
        create_session(
            &conn,
            "session-plan",
            "/workspaces/session-plan",
            None,
            Some(crate::DEFAULT_PROJECT_ID),
        )
        .expect("create session");

        let approved_plan = r#"{"approved":true,"steps":[{"title":"run","status":"pending"}]}"#;
        set_session_plan_and_status(
            &conn,
            "session-plan",
            Some(approved_plan),
            "awaiting_plan_execution",
        )
        .expect("persist approved plan");

        let row = get_session(&conn, "session-plan")
            .expect("read session")
            .expect("session exists");
        assert_eq!(row.status, "awaiting_plan_execution");
        assert_eq!(
            get_session_plan(&conn, "session-plan").expect("read plan"),
            Some(approved_plan.to_string())
        );

        let error = set_session_plan_and_status(
            &conn,
            "missing",
            Some(approved_plan),
            "awaiting_plan_execution",
        )
        .expect_err("missing session must not look persisted");
        assert!(matches!(error, DbError::NotFound(_)));
    }

    fn run_request(session_id: &str, run_id: &str) -> PersistRunRequest {
        PersistRunRequest {
            run_id: run_id.into(),
            session_id: session_id.into(),
            frame_id: "frame-one".into(),
            task_summary: "research task".into(),
            plan_mode: true,
            status: "awaiting_user_response".into(),
            kind: Some("awaiting".into()),
            awaiting: Some("user_response".into()),
            pending_ask_json: Some(r#"{"question":"Choose","options":[{"label":"A"}]}"#.into()),
            error: None,
            input_tokens: 12,
            output_tokens: 7,
            iterations: 2,
            plan_data: Some(r#"{"approved":false,"steps":[]}"#.into()),
            title: Some("research task".into()),
            started_at: "2026-08-04T10:00:00Z".into(),
            checkpoint_start_seq: None,
            messages: vec![
                PersistMessage {
                    role: "user".into(),
                    content: r#"{"role":"user","content":"research task"}"#.into(),
                    harness_notice: false,
                },
                PersistMessage {
                    role: "assistant".into(),
                    content: r#"{"role":"assistant","content":"working"}"#.into(),
                    harness_notice: false,
                },
            ],
        }
    }

    #[test]
    fn persist_run_commits_messages_terminal_state_and_project_recency_together() {
        let conn = test_connection();
        create_session(
            &conn,
            "session-run",
            "/workspaces/session-run",
            None,
            Some(crate::DEFAULT_PROJECT_ID),
        )
        .unwrap();

        let result = persist_run(&conn, &run_request("session-run", "run-one")).unwrap();
        assert_eq!(
            result,
            PersistRunResult {
                messages_written: 2,
                start_seq: Some(1),
                end_seq: Some(2),
                ordinal: 1,
            }
        );

        let messages = list_messages(&conn, "session-run").unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages
            .iter()
            .all(|message| message.run_id.as_deref() == Some("run-one")));
        let runs = list_runs(&conn, "session-run").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].pending_ask_json.as_deref(),
            Some(r#"{"question":"Choose","options":[{"label":"A"}]}"#)
        );
        assert_eq!(runs[0].start_seq, Some(1));
        assert_eq!(runs[0].end_seq, Some(2));

        let session = get_session(&conn, "session-run").unwrap().unwrap();
        assert_eq!(session.title.as_deref(), Some("research task"));
        assert!(session.plan_mode);
        assert_eq!(session.status, "awaiting_user_response");
        assert_eq!(
            get_project(&conn, crate::DEFAULT_PROJECT_ID)
                .unwrap()
                .unwrap()
                .last_session_id
                .as_deref(),
            Some("session-run")
        );
    }

    #[test]
    fn tool_checkpoint_is_immediately_restorable_and_terminal_commit_attaches_it_to_the_run() {
        let conn = test_connection();
        create_session(
            &conn,
            "session-checkpoint",
            "/workspaces/session-checkpoint",
            None,
            Some(crate::DEFAULT_PROJECT_ID),
        )
        .unwrap();
        let checkpoint = vec![
            PersistMessage {
                role: "user".into(),
                content: r#"{"role":"user","content":"delegate"}"#.into(),
                harness_notice: false,
            },
            PersistMessage {
                role: "assistant".into(),
                content: r#"{"role":"assistant","tool_calls":[{"id":"a2a-1"}]}"#.into(),
                harness_notice: false,
            },
            PersistMessage {
                role: "tool".into(),
                content: r#"{"role":"tool","content":"{\"schema\":\"dss.a2a.tool-result.v1\"}"}"#
                    .into(),
                harness_notice: false,
            },
        ];
        assert_eq!(
            append_history_checkpoint(
                &conn,
                &PersistCheckpointRequest {
                    run_id: "run-checkpoint".into(),
                    session_id: "session-checkpoint".into(),
                    frame_id: "frame-checkpoint".into(),
                    task_summary: "delegate".into(),
                    plan_mode: false,
                    status: "processing".into(),
                    awaiting: None,
                    pending_ask_json: None,
                    plan_data: None,
                    title: Some("delegate".into()),
                    started_at: "2026-08-04T10:00:00Z".into(),
                    expected_count: 0,
                    messages: checkpoint,
                },
            )
            .unwrap(),
            3
        );
        let interrupted_rows = list_messages(&conn, "session-checkpoint").unwrap();
        assert_eq!(interrupted_rows.len(), 3);
        assert!(interrupted_rows
            .iter()
            .all(|row| row.run_id.as_deref() == Some("run-checkpoint")));
        let provisional_runs = list_runs(&conn, "session-checkpoint").unwrap();
        assert_eq!(provisional_runs.len(), 1);
        assert_eq!(provisional_runs[0].status, "processing");
        assert!(provisional_runs[0].completed_at.is_none());
        assert_eq!(
            append_history_checkpoint(
                &conn,
                &PersistCheckpointRequest {
                    run_id: "run-checkpoint".into(),
                    session_id: "session-checkpoint".into(),
                    frame_id: "frame-checkpoint".into(),
                    task_summary: "delegate".into(),
                    plan_mode: false,
                    status: "processing".into(),
                    awaiting: None,
                    pending_ask_json: None,
                    plan_data: None,
                    title: None,
                    started_at: "2026-08-04T10:00:00Z".into(),
                    expected_count: 3,
                    messages: vec![PersistMessage {
                        role: "assistant".into(),
                        content: r#"{"role":"assistant","content":"continuing"}"#.into(),
                        harness_notice: false,
                    }],
                },
            )
            .unwrap(),
            4
        );
        let twice_checkpointed_runs = list_runs(&conn, "session-checkpoint").unwrap();
        assert_eq!(twice_checkpointed_runs.len(), 1);
        assert_eq!(twice_checkpointed_runs[0].ordinal, 1);
        assert_eq!(twice_checkpointed_runs[0].start_seq, Some(1));
        assert_eq!(twice_checkpointed_runs[0].end_seq, Some(4));
        let interrupted_session = get_session(&conn, "session-checkpoint").unwrap().unwrap();
        assert_eq!(interrupted_session.status, "processing");
        assert_eq!(interrupted_session.title.as_deref(), Some("delegate"));

        let mut request = run_request("session-checkpoint", "run-checkpoint");
        request.checkpoint_start_seq = Some(1);
        request.messages = vec![PersistMessage {
            role: "assistant".into(),
            content: r#"{"role":"assistant","content":"final"}"#.into(),
            harness_notice: false,
        }];
        let result = persist_run(&conn, &request).unwrap();
        assert_eq!(result.messages_written, 5);
        assert_eq!(result.start_seq, Some(1));
        assert_eq!(result.end_seq, Some(5));
        let final_rows = list_messages(&conn, "session-checkpoint").unwrap();
        assert_eq!(final_rows.len(), 5);
        assert!(final_rows
            .iter()
            .all(|row| row.run_id.as_deref() == Some("run-checkpoint")));
    }

    #[test]
    fn terminal_failure_preserves_restorable_provisional_checkpoint_run() {
        let conn = test_connection();
        create_session(
            &conn,
            "session-checkpoint-rollback",
            "/workspaces/session-checkpoint-rollback",
            None,
            Some(crate::DEFAULT_PROJECT_ID),
        )
        .unwrap();
        let checkpoint = vec![
            PersistMessage {
                role: "user".into(),
                content: r#"{"role":"user","content":"delegate"}"#.into(),
                harness_notice: false,
            },
            PersistMessage {
                role: "tool".into(),
                content: r#"{"role":"tool","content":"remote result"}"#.into(),
                harness_notice: false,
            },
        ];
        append_history_checkpoint(
            &conn,
            &PersistCheckpointRequest {
                run_id: "run-checkpoint-rollback".into(),
                session_id: "session-checkpoint-rollback".into(),
                frame_id: "frame-checkpoint-rollback".into(),
                task_summary: "delegate".into(),
                plan_mode: false,
                status: "processing".into(),
                awaiting: None,
                pending_ask_json: None,
                plan_data: None,
                title: Some("delegate".into()),
                started_at: "2026-08-04T10:00:00Z".into(),
                expected_count: 0,
                messages: checkpoint,
            },
        )
        .unwrap();
        conn.execute_batch(
            r#"
            CREATE TRIGGER reject_checkpoint_terminal_update
            BEFORE UPDATE ON sessions
            WHEN NEW.id = 'session-checkpoint-rollback' AND NEW.status != 'processing'
            BEGIN
                SELECT RAISE(ABORT, 'forced terminal failure');
            END;
            "#,
        )
        .unwrap();

        let mut request = run_request("session-checkpoint-rollback", "run-checkpoint-rollback");
        request.checkpoint_start_seq = Some(1);
        request.messages = vec![PersistMessage {
            role: "assistant".into(),
            content: r#"{"role":"assistant","content":"final"}"#.into(),
            harness_notice: false,
        }];
        let error = persist_run(&conn, &request).expect_err("terminal update must fail");
        assert!(matches!(error, DbError::Sqlite(_)));

        let rows = list_messages(&conn, "session-checkpoint-rollback").unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows
            .iter()
            .all(|row| row.run_id.as_deref() == Some("run-checkpoint-rollback")));
        let runs = list_runs(&conn, "session-checkpoint-rollback").unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "processing");
        assert!(runs[0].completed_at.is_none());
        let session = get_session(&conn, "session-checkpoint-rollback")
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "processing");
    }

    #[test]
    fn persist_run_rolls_back_every_write_when_terminal_update_fails() {
        let conn = test_connection();
        create_session(
            &conn,
            "session-rollback",
            "/workspaces/session-rollback",
            None,
            Some(crate::DEFAULT_PROJECT_ID),
        )
        .unwrap();
        conn.execute_batch(
            r#"
            CREATE TRIGGER reject_terminal_update
            BEFORE UPDATE ON sessions
            WHEN NEW.id = 'session-rollback'
            BEGIN
                SELECT RAISE(ABORT, 'forced terminal failure');
            END;
            "#,
        )
        .unwrap();

        let error = persist_run(&conn, &run_request("session-rollback", "run-rollback"))
            .expect_err("trigger must abort the transaction");
        assert!(matches!(error, DbError::Sqlite(_)));
        assert!(list_messages(&conn, "session-rollback").unwrap().is_empty());
        assert!(list_runs(&conn, "session-rollback").unwrap().is_empty());
        let session = get_session(&conn, "session-rollback").unwrap().unwrap();
        assert_eq!(session.status, "active");
        assert!(session.title.is_none());
        assert_eq!(
            get_project(&conn, crate::DEFAULT_PROJECT_ID)
                .unwrap()
                .unwrap()
                .last_session_id
                .as_deref(),
            Some("session-rollback")
        );
    }

    #[test]
    fn bot_identity_revision_and_session_binding_are_durable() {
        let conn = test_connection();
        let bot = create_bot(
            &conn,
            "Nova",
            "Literature scout",
            "Prefer primary sources.",
            "🔭",
            "#4D6BFE",
            Some(crate::DEFAULT_PROJECT_ID),
            Some("deepseek-v4-flash"),
        )
        .unwrap();
        let session = create_session_for_bot(
            &conn,
            "bot-session",
            "/workspaces/bot-session",
            bot.model.as_deref(),
            bot.project_id.as_deref(),
            Some(&bot.id),
        )
        .unwrap();
        assert_eq!(session.bot_id.as_deref(), Some(bot.id.as_str()));

        let updated = update_bot(
            &conn,
            &bot.id,
            bot.revision,
            "Nova",
            "Principal literature scout",
            "Prefer primary sources and preserve citations.",
            "🔭",
            "#5A67D8",
            bot.project_id.as_deref(),
            bot.model.as_deref(),
            Some(true),
            Some("high"),
            true,
        )
        .unwrap();
        assert_eq!(updated.revision, bot.revision + 1);
        assert!(matches!(
            update_bot(
                &conn,
                &bot.id,
                bot.revision,
                "stale",
                "stale",
                "",
                "🤖",
                "#4D6BFE",
                None,
                None,
                None,
                None,
                true,
            ),
            Err(DbError::Conflict(_))
        ));
    }

    #[test]
    fn durable_bot_queue_reorders_claims_and_finishes_exactly_once() {
        let conn = test_connection();
        let bot = create_bot(
            &conn,
            "Queue Bot",
            "Executor",
            "",
            "⚙️",
            "#4D6BFE",
            Some(crate::DEFAULT_PROJECT_ID),
            None,
        )
        .unwrap();
        create_session_for_bot(
            &conn,
            "queue-session",
            "/workspaces/queue-session",
            None,
            Some(crate::DEFAULT_PROJECT_ID),
            Some(&bot.id),
        )
        .unwrap();
        let first = enqueue_bot_job(&conn, None, &bot.id, "queue-session", "first", false).unwrap();
        let second =
            enqueue_bot_job(&conn, None, &bot.id, "queue-session", "second", true).unwrap();

        let reordered = reorder_bot_jobs(
            &conn,
            "queue-session",
            &[second.id.clone(), first.id.clone()],
        )
        .unwrap();
        assert_eq!(reordered[0].id, second.id);
        let claimed = claim_next_bot_job(&conn, "queue-session", "run-1")
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, second.id);
        assert_eq!(claimed.status, "running");
        let completed = finish_bot_job(&conn, &claimed.id, "run-1", true, None).unwrap();
        assert_eq!(completed.status, "completed");
        assert!(matches!(
            finish_bot_job(&conn, &claimed.id, "run-1", true, None),
            Err(DbError::Conflict(_))
        ));
        assert_eq!(list_bot_jobs(&conn, "queue-session").unwrap().len(), 1);
    }

    // ---------- logs prune 测试（D-T07） ----------

    fn logs_test_connection() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(
            r#"
            CREATE TABLE logs (
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
        )
        .expect("create logs test schema");
        conn
    }

    fn insert_log(conn: &Connection, ts: &str, message: &str) {
        append_log(
            conn, "info", "system", "startup", None, None, None, message, None,
        )
        .expect("insert log");
        // append_log 用 now() 写 ts；手动覆盖以构造历史时间戳。
        conn.execute(
            "UPDATE logs SET ts = ?1 WHERE id = last_insert_rowid()",
            params![ts],
        )
        .expect("set ts");
    }

    #[test]
    fn prune_logs_by_age() {
        let conn = logs_test_connection();
        insert_log(&conn, "2026-01-01T00:00:00Z", "old-1");
        insert_log(&conn, "2026-01-02T00:00:00Z", "old-2");
        insert_log(&conn, "2026-02-01T00:00:00Z", "recent-1");

        let stats = prune_logs(&conn, "2026-02-01T00:00:00Z", 100_000).expect("prune");
        assert_eq!(stats.by_age, 2, "two rows older than cutoff deleted");
        assert_eq!(stats.by_count, 0, "under max_rows, no count-based delete");
        assert_eq!(stats.total(), 2);

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 1, "only the recent row remains");
    }

    #[test]
    fn prune_logs_by_count() {
        let conn = logs_test_connection();
        for i in 0..10 {
            insert_log(
                &conn,
                &format!("2026-02-{:02}T00:00:00Z", i + 1),
                &format!("row-{i}"),
            );
        }
        // 不超期（before 设很早），但总量超 5 → 删最旧的 5 条。
        let stats = prune_logs(&conn, "2000-01-01T00:00:00Z", 5).expect("prune");
        assert_eq!(stats.by_age, 0, "nothing older than the very-early cutoff");
        assert_eq!(
            stats.by_count, 5,
            "excess rows over max deleted (oldest first)"
        );

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 5, "row count trimmed to max_rows");
    }

    #[test]
    fn prune_logs_age_and_count_combined() {
        let conn = logs_test_connection();
        // 3 条过期 + 5 条近期，共 8 条；max_rows=4。
        insert_log(&conn, "2026-01-01T00:00:00Z", "expired-1");
        insert_log(&conn, "2026-01-02T00:00:00Z", "expired-2");
        insert_log(&conn, "2026-01-03T00:00:00Z", "expired-3");
        insert_log(&conn, "2026-02-01T00:00:00Z", "recent-1");
        insert_log(&conn, "2026-02-02T00:00:00Z", "recent-2");
        insert_log(&conn, "2026-02-03T00:00:00Z", "recent-3");
        insert_log(&conn, "2026-02-04T00:00:00Z", "recent-4");
        insert_log(&conn, "2026-02-05T00:00:00Z", "recent-5");

        // 先按天删 3 条过期，剩 5 条；再按量删到 4，删最旧 1 条（recent-1）。
        let stats = prune_logs(&conn, "2026-02-01T00:00:00Z", 4).expect("prune");
        assert_eq!(stats.by_age, 3);
        assert_eq!(stats.by_count, 1);

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM logs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 4);
        // 剩余应为 recent-2..recent-5。
        let msgs: Vec<String> = conn
            .prepare("SELECT message FROM logs ORDER BY ts ASC")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(msgs, vec!["recent-2", "recent-3", "recent-4", "recent-5"]);
    }

    #[test]
    fn prune_logs_idempotent() {
        let conn = logs_test_connection();
        insert_log(&conn, "2026-02-01T00:00:00Z", "row-1");
        let stats1 = prune_logs(&conn, "2026-02-01T00:00:00Z", 100_000).expect("prune 1");
        let stats2 = prune_logs(&conn, "2026-02-01T00:00:00Z", 100_000).expect("prune 2");
        assert_eq!(stats1.total(), 0, "nothing to delete");
        assert_eq!(stats2.total(), 0, "second sweep is a no-op");
    }
}
