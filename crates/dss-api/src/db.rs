//! DB 异步封装：用 deadpool-sqlite 的 `conn.interact(|c| {...}).await`
//! 在 spawn_blocking 里跑同步 repo 函数（rusqlite::Connection 非 Send，必须这样访问）。

use dss_db::{
    events::{NewSessionEvent, SessionEventRow},
    repo,
    repo::{BotJobRow, BotRow, MessageRow, ProjectRow, RunRow, SessionRow},
    DbError, DbPool,
};

pub use dss_db::events::SessionEventKind;
pub use dss_db::repo::{
    PersistAttemptLease, PersistCheckpointRequest, PersistMessage, PersistRunRequest,
    PersistRunResult,
};

pub async fn resolve_tool_reconciliation(
    pool: &DbPool,
    run_id: String,
    call_id: String,
    succeeded: bool,
    output: serde_json::Value,
) -> Result<bool, DbError> {
    with_conn(pool, move |conn| {
        dss_db::harness::resolve_tool_reconciliation(conn, &run_id, &call_id, succeeded, &output)
    })
    .await
}

pub async fn list_frame_tree(
    pool: &DbPool,
    root_frame_id: String,
) -> Result<Vec<dss_db::harness::ExecutionFrameRow>, DbError> {
    with_conn(pool, move |conn| {
        dss_db::harness::list_frame_tree(conn, &root_frame_id)
    })
    .await
}

/// 取连接并跑一个拿 &Connection 的闭包；interact 内部 spawn_blocking。
pub(crate) async fn with_conn<F, T>(pool: &DbPool, f: F) -> Result<T, DbError>
where
    F: FnOnce(&rusqlite::Connection) -> Result<T, DbError> + Send + 'static,
    T: Send + 'static,
{
    let conn = pool.get().await.map_err(DbError::Pool)?; // deadpool_sqlite::Connection
    conn.interact(move |c| f(c))
        .await
        .map_err(|e| DbError::Other(format!("db interact: {e:?}")))?
}

// ---------------- bots / durable jobs ----------------

#[allow(clippy::too_many_arguments)]
pub async fn create_bot(
    pool: &DbPool,
    name: String,
    role: String,
    instructions: String,
    avatar: String,
    color: String,
    project_id: Option<String>,
    model: Option<String>,
) -> Result<BotRow, DbError> {
    with_conn(pool, move |conn| {
        repo::create_agent_profile(
            conn,
            &name,
            &role,
            &instructions,
            &avatar,
            &color,
            project_id.as_deref(),
            model.as_deref(),
        )
    })
    .await
}

pub async fn list_bots(pool: &DbPool) -> Result<Vec<BotRow>, DbError> {
    with_conn(pool, repo::list_agent_profiles).await
}

pub async fn get_bot(pool: &DbPool, id: String) -> Result<Option<BotRow>, DbError> {
    with_conn(pool, move |conn| repo::get_agent_profile(conn, &id)).await
}

#[allow(clippy::too_many_arguments)]
pub async fn update_bot(
    pool: &DbPool,
    id: String,
    expected_revision: i64,
    name: String,
    role: String,
    instructions: String,
    avatar: String,
    color: String,
    project_id: Option<String>,
    model: Option<String>,
    thinking_enabled: Option<bool>,
    thinking_effort: Option<String>,
    enabled: bool,
) -> Result<BotRow, DbError> {
    with_conn(pool, move |conn| {
        repo::update_agent_profile(
            conn,
            &id,
            expected_revision,
            &name,
            &role,
            &instructions,
            &avatar,
            &color,
            project_id.as_deref(),
            model.as_deref(),
            thinking_enabled,
            thinking_effort.as_deref(),
            enabled,
        )
    })
    .await
}

pub async fn delete_bot(pool: &DbPool, id: String) -> Result<(), DbError> {
    with_conn(pool, move |conn| repo::delete_agent_profile(conn, &id)).await
}

pub async fn enqueue_bot_job(
    pool: &DbPool,
    requested_id: Option<String>,
    bot_id: String,
    session_id: String,
    prompt: String,
    requested_plan_mode: bool,
) -> Result<BotJobRow, DbError> {
    with_conn(pool, move |conn| {
        repo::enqueue_agent_job(
            conn,
            requested_id.as_deref(),
            &bot_id,
            &session_id,
            &prompt,
            requested_plan_mode,
        )
    })
    .await
}

pub async fn list_bot_jobs(pool: &DbPool, session_id: String) -> Result<Vec<BotJobRow>, DbError> {
    with_conn(pool, move |conn| repo::list_agent_jobs(conn, &session_id)).await
}

pub async fn edit_bot_job(
    pool: &DbPool,
    id: String,
    expected_revision: i64,
    prompt: String,
    requested_plan_mode: bool,
) -> Result<BotJobRow, DbError> {
    with_conn(pool, move |conn| {
        repo::edit_agent_job(conn, &id, expected_revision, &prompt, requested_plan_mode)
    })
    .await
}

pub async fn delete_bot_job(
    pool: &DbPool,
    id: String,
    expected_revision: i64,
) -> Result<(), DbError> {
    with_conn(pool, move |conn| {
        repo::delete_agent_job(conn, &id, expected_revision)
    })
    .await
}

pub async fn reorder_bot_jobs(
    pool: &DbPool,
    session_id: String,
    ordered_ids: Vec<String>,
) -> Result<Vec<BotJobRow>, DbError> {
    with_conn(pool, move |conn| {
        repo::reorder_agent_jobs(conn, &session_id, &ordered_ids)
    })
    .await
}

pub async fn claim_next_bot_job(
    pool: &DbPool,
    session_id: String,
    run_id: String,
) -> Result<Option<BotJobRow>, DbError> {
    with_conn(pool, move |conn| {
        repo::claim_next_agent_job(conn, &session_id, &run_id)
    })
    .await
}

pub async fn finish_bot_job(
    pool: &DbPool,
    id: String,
    run_id: String,
    succeeded: bool,
    error: Option<String>,
) -> Result<BotJobRow, DbError> {
    with_conn(pool, move |conn| {
        repo::settle_agent_job(conn, &id, &run_id, succeeded, error.as_deref())
    })
    .await
}

pub async fn claim_bot_job(
    pool: &DbPool,
    id: String,
    expected_revision: i64,
    run_id: String,
) -> Result<BotJobRow, DbError> {
    with_conn(pool, move |conn| {
        repo::claim_agent_job(conn, &id, expected_revision, &run_id)
    })
    .await
}

// ---------------- projects ----------------

pub async fn ensure_default_project(pool: &DbPool) -> Result<ProjectRow, DbError> {
    with_conn(pool, repo::ensure_default_project).await
}

pub async fn list_projects(
    pool: &DbPool,
    include_archived: bool,
) -> Result<Vec<ProjectRow>, DbError> {
    with_conn(pool, move |c| repo::list_projects(c, include_archived)).await
}

pub async fn get_project(pool: &DbPool, id: String) -> Result<Option<ProjectRow>, DbError> {
    with_conn(pool, move |c| repo::get_project(c, &id)).await
}

pub async fn create_project(
    pool: &DbPool,
    name: String,
    description: Option<String>,
    agent_context: Option<String>,
) -> Result<ProjectRow, DbError> {
    with_conn(pool, move |c| {
        repo::create_project(c, &name, description.as_deref(), agent_context.as_deref())
    })
    .await
}

pub async fn update_project(
    pool: &DbPool,
    id: String,
    name: Option<String>,
    description: Option<String>,
    agent_context: Option<String>,
    last_session_id: Option<String>,
) -> Result<ProjectRow, DbError> {
    with_conn(pool, move |c| {
        repo::update_project(
            c,
            &id,
            name.as_deref(),
            description.as_deref(),
            agent_context.as_deref(),
            last_session_id.as_deref(),
        )
    })
    .await
}

pub async fn set_project_archived(
    pool: &DbPool,
    id: String,
    archived: bool,
) -> Result<ProjectRow, DbError> {
    with_conn(pool, move |c| repo::set_project_archived(c, &id, archived)).await
}

pub async fn delete_project(pool: &DbPool, id: String, force: bool) -> Result<(), DbError> {
    with_conn(pool, move |c| repo::delete_project(c, &id, force)).await
}

pub async fn get_project_detail(
    pool: &DbPool,
    id: String,
) -> Result<(ProjectRow, Vec<SessionRow>), DbError> {
    with_conn(pool, move |c| repo::get_project_detail(c, &id)).await
}

// ---------------- sessions ----------------

pub async fn create_session_row(
    pool: &DbPool,
    id: String,
    workspace: String,
    model: Option<String>,
    project_id: Option<String>,
) -> Result<SessionRow, DbError> {
    with_conn(pool, move |c| {
        repo::create_session(c, &id, &workspace, model.as_deref(), project_id.as_deref())
    })
    .await
}

pub async fn create_bot_session_row(
    pool: &DbPool,
    id: String,
    workspace: String,
    model: Option<String>,
    project_id: Option<String>,
    bot_id: String,
) -> Result<SessionRow, DbError> {
    with_conn(pool, move |c| {
        repo::create_session_for_bot(
            c,
            &id,
            &workspace,
            model.as_deref(),
            project_id.as_deref(),
            Some(&bot_id),
        )
    })
    .await
}

pub async fn get_session_row(pool: &DbPool, id: String) -> Result<Option<SessionRow>, DbError> {
    with_conn(pool, move |c| repo::get_session(c, &id)).await
}

pub async fn list_session_rows(pool: &DbPool) -> Result<Vec<SessionRow>, DbError> {
    with_conn(pool, repo::list_sessions).await
}

pub async fn set_session_title(pool: &DbPool, id: String, title: String) -> Result<(), DbError> {
    with_conn(pool, move |c| repo::set_session_title(c, &id, &title)).await
}

pub async fn set_session_status(pool: &DbPool, id: String, status: String) -> Result<(), DbError> {
    with_conn(pool, move |c| repo::set_session_status(c, &id, &status)).await
}

pub async fn rebase_session_workspace(
    pool: &DbPool,
    id: String,
    expected_workspace: String,
    new_workspace: String,
) -> Result<bool, DbError> {
    with_conn(pool, move |c| {
        repo::rebase_session_workspace(c, &id, &expected_workspace, &new_workspace)
    })
    .await
}

pub async fn set_session_plan(
    pool: &DbPool,
    id: String,
    plan_data: Option<String>,
) -> Result<(), DbError> {
    with_conn(pool, move |c| {
        repo::set_session_plan(c, &id, plan_data.as_deref())
    })
    .await
}

pub async fn set_session_plan_and_status(
    pool: &DbPool,
    id: String,
    plan_data: Option<String>,
    status: String,
) -> Result<(), DbError> {
    with_conn(pool, move |c| {
        repo::set_session_plan_and_status(c, &id, plan_data.as_deref(), &status)
    })
    .await
}

pub async fn get_session_plan(pool: &DbPool, id: String) -> Result<Option<String>, DbError> {
    with_conn(pool, move |c| repo::get_session_plan(c, &id)).await
}

pub async fn get_session_compaction_state(
    pool: &DbPool,
    id: String,
) -> Result<Option<String>, DbError> {
    with_conn(pool, move |c| repo::get_session_compaction_state(c, &id)).await
}

pub async fn append_session_event(
    pool: &DbPool,
    event: NewSessionEvent,
) -> Result<SessionEventRow, DbError> {
    with_conn(pool, move |c| {
        dss_db::events::append_session_event(c, &event)
    })
    .await
}

pub async fn list_session_events(
    pool: &DbPool,
    session_id: String,
    after_seq: i64,
    limit: usize,
) -> Result<Vec<SessionEventRow>, DbError> {
    with_conn(pool, move |c| {
        dss_db::events::list_session_events(c, &session_id, after_seq, limit)
    })
    .await
}

pub async fn delete_session_row(pool: &DbPool, id: String) -> Result<(), DbError> {
    with_conn(pool, move |c| repo::delete_session(c, &id)).await
}

// ---------------- messages ----------------

pub async fn append_message(
    pool: &DbPool,
    session_id: String,
    role: String,
    content: String,
    harness_notice: bool,
) -> Result<i64, DbError> {
    with_conn(pool, move |c| {
        repo::append_message(c, &session_id, &role, &content, harness_notice)
    })
    .await
}

pub async fn list_messages(pool: &DbPool, session_id: String) -> Result<Vec<MessageRow>, DbError> {
    with_conn(pool, move |c| repo::list_messages(c, &session_id)).await
}

pub async fn list_runs(pool: &DbPool, session_id: String) -> Result<Vec<RunRow>, DbError> {
    with_conn(pool, move |c| repo::list_runs(c, &session_id)).await
}

pub async fn persist_run(
    pool: &DbPool,
    request: PersistRunRequest,
) -> Result<PersistRunResult, DbError> {
    with_conn(pool, move |c| repo::persist_run(c, &request)).await
}

pub async fn append_history_checkpoint(
    pool: &DbPool,
    request: PersistCheckpointRequest,
) -> Result<usize, DbError> {
    with_conn(pool, move |c| repo::append_history_checkpoint(c, &request)).await
}

pub async fn record_tool_call_started(
    pool: &DbPool,
    call_id: String,
    run_id: String,
    attempt: PersistAttemptLease,
    tool_name: String,
    effect_class: String,
    input: serde_json::Value,
) -> Result<(), DbError> {
    with_conn(pool, move |conn| {
        dss_db::harness::record_tool_call_started(
            conn,
            &dss_db::harness::ToolCallStart {
                call_id: &call_id,
                run_id: &run_id,
                attempt_id: &attempt.attempt_id,
                lease_token: &attempt.lease_token,
                tool_name: &tool_name,
                effect_class: &effect_class,
                input: &input,
                idempotency_key: None,
            },
        )
    })
    .await
}

pub async fn record_tool_call_settled(
    pool: &DbPool,
    call_id: String,
    run_id: String,
    attempt: PersistAttemptLease,
    succeeded: bool,
    output: serde_json::Value,
) -> Result<(), DbError> {
    with_conn(pool, move |conn| {
        dss_db::harness::record_tool_call_settled(
            conn,
            &call_id,
            &run_id,
            &attempt.attempt_id,
            &attempt.lease_token,
            succeeded,
            &output,
        )
    })
    .await
}

pub async fn record_tool_call_uncertain(
    pool: &DbPool,
    call_id: String,
    run_id: String,
    attempt: PersistAttemptLease,
    reason: String,
    detail: serde_json::Value,
) -> Result<(), DbError> {
    with_conn(pool, move |conn| {
        dss_db::harness::record_tool_call_uncertain(
            conn,
            &call_id,
            &run_id,
            &attempt.attempt_id,
            &attempt.lease_token,
            &reason,
            &detail,
        )
    })
    .await
}

pub async fn list_unresolved_tool_calls(
    pool: &DbPool,
    run_id: String,
) -> Result<Vec<dss_db::harness::UnresolvedToolCallRow>, DbError> {
    with_conn(pool, move |conn| {
        dss_db::harness::list_unresolved_tool_calls(conn, &run_id)
    })
    .await
}

/// 批量顺序写入若干消息（一个 interact 任务里，避免多次取连接）。
pub async fn append_messages_batch(
    pool: &DbPool,
    session_id: String,
    msgs: Vec<(String, String, bool)>,
) -> Result<(), DbError> {
    if msgs.is_empty() {
        return Ok(());
    }
    with_conn(pool, move |c| {
        for (role, content, hn) in msgs {
            repo::append_message(c, &session_id, &role, &content, hn)?;
        }
        Ok::<_, DbError>(())
    })
    .await
}
