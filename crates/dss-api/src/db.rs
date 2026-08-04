//! DB 异步封装：用 deadpool-sqlite 的 `conn.interact(|c| {...}).await`
//! 在 spawn_blocking 里跑同步 repo 函数（rusqlite::Connection 非 Send，必须这样访问）。

use dss_db::{
    repo,
    repo::{MessageRow, ProjectRow, SessionRow},
    DbError, DbPool,
};

/// 取连接并跑一个拿 &Connection 的闭包；interact 内部 spawn_blocking。
async fn with_conn<F, T>(pool: &DbPool, f: F) -> Result<T, DbError>
where
    F: FnOnce(&rusqlite::Connection) -> Result<T, DbError> + Send + 'static,
    T: Send + 'static,
{
    let conn = pool.get().await.map_err(DbError::Pool)?; // deadpool_sqlite::Connection
    conn.interact(move |c| f(c))
        .await
        .map_err(|e| DbError::Other(format!("db interact: {e:?}")))?
}

// ---------------- projects ----------------

pub async fn ensure_default_project(pool: &DbPool) -> Result<ProjectRow, DbError> {
    with_conn(pool, |c| repo::ensure_default_project(c)).await
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
) -> Result<ProjectRow, DbError> {
    with_conn(pool, move |c| {
        repo::create_project(c, &name, description.as_deref())
    })
    .await
}

pub async fn update_project(
    pool: &DbPool,
    id: String,
    name: Option<String>,
    description: Option<String>,
    last_session_id: Option<String>,
) -> Result<ProjectRow, DbError> {
    with_conn(pool, move |c| {
        repo::update_project(
            c,
            &id,
            name.as_deref(),
            description.as_deref(),
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

pub async fn get_session_row(pool: &DbPool, id: String) -> Result<Option<SessionRow>, DbError> {
    with_conn(pool, move |c| repo::get_session(c, &id)).await
}

pub async fn list_session_rows(pool: &DbPool) -> Result<Vec<SessionRow>, DbError> {
    with_conn(pool, |c| repo::list_sessions(c)).await
}

pub async fn set_session_title(pool: &DbPool, id: String, title: String) -> Result<(), DbError> {
    with_conn(pool, move |c| repo::set_session_title(c, &id, &title)).await
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

pub async fn get_session_plan(pool: &DbPool, id: String) -> Result<Option<String>, DbError> {
    with_conn(pool, move |c| repo::get_session_plan(c, &id)).await
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
