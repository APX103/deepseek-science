//! sessions 端点：POST/GET/DELETE /api/sessions、GET /api/sessions/{sid}、
//! POST /api/sessions/{sid}/stream-sse。
//!
//! 持久化：session 行 + session_messages 增量写；恢复从 DB 重建 Vec<ChatMessage>。

use std::convert::Infallible;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use dss_agent::{AgentEvent, CompleteKind, FrameStatus, Runner, Session};
use dss_llm::ChatMessage;
use dss_tools::{HistoryCheckpoint, SecureWorkspace, ToolContext, ToolError};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_stream::wrappers::ReceiverStream;

use crate::db as dbq;
use crate::state::{
    ActiveRunControl, ActiveSession, AppState, RunPersistenceState, MAX_ACTIVE_SESSIONS,
};
use crate::workspace_resolution::resolve_session_workspace;

#[derive(Debug, Default, Deserialize)]
pub struct CreateSessionReq {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    bot_id: Option<String>,
}

#[derive(Serialize)]
pub struct CreateSessionResp {
    id: String,
    frame_id: String,
    mcp_tools: Vec<String>,
    model: String,
    workspace: String,
    bot_id: Option<String>,
}

fn json_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": message })))
}

fn map_db_err(e: dss_db::DbError) -> (StatusCode, Json<Value>) {
    match e {
        dss_db::DbError::NotFound(m) => json_error(StatusCode::NOT_FOUND, &m),
        dss_db::DbError::Conflict(m) => json_error(StatusCode::CONFLICT, &m),
        e => json_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

/// `POST /api/sessions`：建会话（sid=uuid4()[:12]，建 workspace，落 DB）。
pub async fn create_session(
    State(state): State<AppState>,
    body: Option<Json<CreateSessionReq>>,
) -> Result<Json<CreateSessionResp>, (StatusCode, Json<Value>)> {
    let req = body.map(|b| b.0).unwrap_or_default();
    let bot = if let Some(bot_id) = req.bot_id.as_ref() {
        let bot = dbq::get_bot(&state.db, bot_id.clone())
            .await
            .map_err(map_db_err)?
            .ok_or_else(|| json_error(StatusCode::NOT_FOUND, "bot not found"))?;
        if !bot.enabled {
            return Err(json_error(StatusCode::CONFLICT, "bot is disabled"));
        }
        Some(bot)
    } else {
        None
    };
    let sid = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let workspace = state.settings.data_dir.join("workspaces").join(&sid);

    if let Err(e) = std::fs::create_dir_all(&workspace) {
        tracing::error!(error = %e, "failed to create workspace");
        return Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to create workspace",
        ));
    }

    let runtime = state.llm_snapshot().await;
    let model = req
        .model
        .or_else(|| bot.as_ref().and_then(|bot| bot.model.clone()))
        .unwrap_or_else(|| runtime.settings().model.clone());
    // 落 DB（project_id 缺省 → 不绑定，或后续 default）。
    let project_id = req
        .project_id
        .or_else(|| bot.as_ref().and_then(|bot| bot.project_id.clone()))
        .or(Some(dss_db::DEFAULT_PROJECT_ID.to_string()));
    let bot_id = bot.as_ref().map(|bot| bot.id.clone());
    let row = if let Some(bot_id) = bot_id.clone() {
        dbq::create_bot_session_row(
            &state.db,
            sid.clone(),
            workspace.display().to_string(),
            Some(model.clone()),
            project_id,
            bot_id,
        )
        .await
    } else {
        dbq::create_session_row(
            &state.db,
            sid.clone(),
            workspace.display().to_string(),
            Some(model.clone()),
            project_id,
        )
        .await
    }
    .map_err(map_db_err)?;

    let session = Session::new(sid.clone(), workspace);
    let frame_id = session.frame.id.clone();
    state
        .sessions
        .lock()
        .await
        .insert(sid.clone(), Arc::new(ActiveSession::new(session, 0)));

    tracing::info!(sid = %sid, "session created");
    Ok(Json(CreateSessionResp {
        id: sid,
        frame_id,
        mcp_tools: Vec::new(),
        model: row.model.unwrap_or(model),
        workspace: row.workspace,
        bot_id: row.bot_id,
    }))
}

#[derive(Serialize)]
pub struct SessionListItem {
    id: String,
    title: Option<String>,
    frame_id: Option<String>,
    model: Option<String>,
    workspace: String,
    live: bool,
    status: String,
    project_id: Option<String>,
    bot_id: Option<String>,
    created_at: String,
    updated_at: String,
}

/// `GET /api/sessions`：DB 全部会话 + 内存 live 标记。
pub async fn list_sessions(State(state): State<AppState>) -> Json<Vec<SessionListItem>> {
    let rows = dbq::list_session_rows(&state.db).await.unwrap_or_default();
    let live_ids: std::collections::HashSet<String> =
        state.sessions.lock().await.keys().cloned().collect();

    let mut items: Vec<SessionListItem> = rows
        .into_iter()
        .map(|r| SessionListItem {
            id: r.id.clone(),
            title: r.title,
            frame_id: None,
            model: r.model,
            workspace: r.workspace,
            live: live_ids.contains(&r.id),
            status: r.status,
            project_id: r.project_id,
            bot_id: r.bot_id,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();
    // Keep active sessions easy to reach, then preserve durable recency within
    // each group. The id tie-breaker makes equal-timestamp ordering stable.
    items.sort_by(|a, b| {
        b.live
            .cmp(&a.live)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    Json(items)
}

/// `GET /api/sessions/{sid}`：会话状态序列化（api-contract：messages 带 harness_notice 顶层字段）。
pub async fn get_session(
    State(state): State<AppState>,
    Path(sid): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let active = restore_session(&state, &sid).await.map_err(map_db_err)?;

    // Serialize the DB snapshot with an active run. The run keeps this same
    // session mutex until its newly appended messages and status are
    // persisted, so a GET cannot combine a new frame with stale DB messages.
    let session = active.session.lock().await;
    let row = match dbq::get_session_row(&state.db, sid.clone()).await {
        Ok(Some(r)) => r,
        _ => return Err(json_error(StatusCode::NOT_FOUND, "session not found")),
    };

    let msgs = dbq::list_messages(&state.db, sid.clone())
        .await
        .map_err(map_db_err)?;
    let runs = dbq::list_runs(&state.db, sid.clone())
        .await
        .map_err(map_db_err)?;
    let messages: Vec<Value> = msgs
        .into_iter()
        .map(|m| {
            // content 是 OpenAI 协议形态 JSON；直接透传 + 顶层 harness_notice。
            let content: Value = serde_json::from_str(&m.content).unwrap_or(Value::Null);
            json!({
                "seq": m.seq,
                "run_id": m.run_id,
                "role": m.role,
                "content": content,
                "harness_notice": m.harness_notice,
            })
        })
        .collect();
    let runs: Vec<Value> = runs.into_iter().map(run_row_json).collect();
    let (frame_id, frame_status, plan) = (
        session.frame.id.clone(),
        session.frame.status,
        session.plan.clone(),
    );

    Ok(Json(json!({
        "id": row.id,
        "frame_id": frame_id,
        "title": row.title,
        "workspace": row.workspace,
        "model": row.model,
        "status": frame_status,
        "plan_mode": row.plan_mode,
        "plan": plan,
        "project_id": row.project_id,
        "bot_id": row.bot_id,
        "artifacts": {},
        "messages": messages,
        "runs": runs,
    })))
}

fn parse_optional_json(value: Option<String>) -> Option<Value> {
    value.and_then(|json| serde_json::from_str(&json).ok())
}

fn run_row_json(run: dss_db::repo::RunRow) -> Value {
    let pending_ask = parse_optional_json(run.pending_ask_json);
    let plan = parse_optional_json(run.plan_data);
    json!({
        "run_id": run.run_id,
        "ordinal": run.ordinal,
        "frame_id": run.frame_id,
        "task_summary": run.task_summary,
        "plan_mode": run.plan_mode,
        "status": run.status,
        "kind": run.kind,
        "awaiting": run.awaiting,
        "pending_ask": pending_ask,
        "error": run.error,
        "usage": {
            "input_tokens": run.input_tokens,
            "output_tokens": run.output_tokens,
        },
        "iterations": run.iterations,
        "plan": plan,
        "start_seq": run.start_seq,
        "end_seq": run.end_seq,
        "started_at": run.started_at,
        "completed_at": run.completed_at,
    })
}

/// `DELETE /api/sessions/{sid}`：删 DB 行（cascade messages）+ workspace + 内存。
pub async fn delete_session(
    State(state): State<AppState>,
    Path(sid): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    // Hold the same per-session capability used by runs from the existence check through the
    // database/workspace deletion. A running tool must never lose its transcript row or cwd
    // underneath it, and a stream that races after this lock will fail before acceptance.
    let active = restore_session(&state, &sid).await.map_err(map_db_err)?;
    let Ok(_session_guard) = active.session.clone().try_lock_owned() else {
        return Err(json_error(
            StatusCode::CONFLICT,
            "session has an active run and cannot be deleted",
        ));
    };
    dbq::delete_session_row(&state.db, sid.clone())
        .await
        .map_err(map_db_err)?;
    // workspace 目录
    let ws = state.settings.data_dir.join("workspaces").join(&sid);
    if ws.exists() {
        let _ = std::fs::remove_dir_all(&ws);
    }
    state.sessions.lock().await.remove(&sid);
    tracing::info!(sid = %sid, "session deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/sessions/{sid}/approve`：批准 plan，返回 {approved, steps}。
///
/// Approval only advances to `AwaitingPlanExecution`. A later explicit
/// `execute_plan=true` request moves the session to Processing after that run
/// has passed validation and can be accepted.
pub async fn approve_plan(
    State(state): State<AppState>,
    Path(sid): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let active = restore_session(&state, &sid).await.map_err(map_db_err)?;

    // Keep the session lock until the matching plan+status UPDATE commits so a
    // concurrent GET cannot observe a half-approved in-memory state.
    let steps = {
        let mut s = active.session.lock().await;
        if s.frame.status != FrameStatus::AwaitingPlanApproval {
            return Err(json_error(
                StatusCode::CONFLICT,
                "session is not awaiting plan approval",
            ));
        }
        let Some(mut approved_plan) = s.plan.clone() else {
            return Err(json_error(
                StatusCode::CONFLICT,
                "no plan to approve; generate a plan first",
            ));
        };
        approved_plan.approved = true;
        let plan_data = serde_json::to_string(&approved_plan).map_err(|error| {
            tracing::error!(%error, sid = %sid, "serialize approved plan failed");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to persist approved plan",
            )
        })?;
        dbq::set_session_plan_and_status(
            &state.db,
            sid.clone(),
            Some(plan_data),
            "awaiting_plan_execution".into(),
        )
        .await
        .map_err(map_db_err)?;

        let steps_json: Vec<serde_json::Value> = approved_plan
            .steps
            .iter()
            .map(|step| json!({ "title": step.title, "status": step.status }))
            .collect();
        s.plan = Some(approved_plan);
        s.frame.set_status(FrameStatus::AwaitingPlanExecution);
        steps_json
    };

    tracing::info!(sid = %sid, "plan approved");
    Ok(Json(json!({ "approved": true, "steps": steps })))
}

#[derive(Deserialize)]
pub struct CompileReq {
    path: String,
    #[serde(default)]
    out_name: Option<String>,
}

#[derive(Serialize)]
pub struct CompileResult {
    success: bool,
    pdf_path: Option<String>,
    size_kb: u64,
    message: String,
    errors: Vec<String>,
    log_excerpt: String,
}

/// `POST /api/sessions/{sid}/compile`：Tectonic 编译 .tex → PDF。
pub async fn compile(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<CompileReq>,
) -> Result<Json<CompileResult>, (StatusCode, Json<Value>)> {
    // 确认 session 存在 + 拿 workspace。
    let row = dbq::get_session_row(&state.db, sid.clone())
        .await
        .map_err(map_db_err)?
        .ok_or_else(|| json_error(StatusCode::NOT_FOUND, "session not found"))?;
    let workspace = resolve_session_workspace(&state, &row)
        .await
        .map_err(map_db_err)?;
    let secure_workspace = SecureWorkspace::open(&workspace).map_err(|error| {
        json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("workspace unavailable: {error}"),
        )
    })?;
    let source_rel = req.path.trim();
    match secure_workspace.open_file(source_rel) {
        Ok(file) => drop(file),
        Err(ToolError::NotFound(_)) => {
            return Ok(Json(CompileResult {
                success: false,
                pdf_path: None,
                size_kb: 0,
                message: format!("tex file not found: {}", req.path),
                errors: vec![],
                log_excerpt: String::new(),
            }));
        }
        Err(ToolError::PathEscape(_) | ToolError::InvalidArgs(_)) => {
            return Err(json_error(StatusCode::FORBIDDEN, "invalid tex path"));
        }
        Err(error) => {
            return Err(json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("could not validate tex path: {error}"),
            ));
        }
    }
    let source_path = std::path::Path::new(source_rel);
    if source_path.extension().and_then(|s| s.to_str()) != Some("tex") {
        return Ok(Json(CompileResult {
            success: false,
            pdf_path: None,
            size_kb: 0,
            message: "compile path must point to a .tex file".into(),
            errors: vec![],
            log_excerpt: String::new(),
        }));
    }
    let stem = source_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".into());
    let out_name = normalize_compile_output_name(req.out_name.as_deref(), &stem)
        .map_err(|m| json_error(StatusCode::BAD_REQUEST, &m))?;
    let pdf_rel = format!("{out_name}.pdf");
    let pdf_path = workspace.join(&pdf_rel);

    // Keep one implementation of the privileged compiler path. The registered tool owns
    // Seatbelt isolation, offline/untrusted Tectonic flags, environment clearing, output
    // bounds, process-group teardown, and fail-closed behavior.
    let Some(tool) = state.tools.get("compile_pdf") else {
        return Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "compile_pdf tool is unavailable",
        ));
    };
    let tool_ctx = ToolContext::new(workspace.clone());
    let tool_args = json!({ "path": req.path, "out_name": out_name.clone() });
    let timeout = tool.timeout(&tool_args);
    let result = match tokio::time::timeout(timeout, tool.call(&tool_ctx, tool_args)).await {
        Ok(result) => result,
        Err(_) => Err(dss_tools::ToolError::Timeout(timeout.as_secs())),
    };

    let output = match result {
        Ok(output) => output,
        Err(error) => {
            let message = format!("compile_pdf failed: {error}");
            return Ok(Json(CompileResult {
                success: false,
                pdf_path: None,
                size_kb: 0,
                message: message.clone(),
                errors: vec![message.clone()],
                log_excerpt: message,
            }));
        }
    };

    if !output.is_error {
        // The tool's exclusive guard is released on return. Verify through a no-follow file
        // handle so a later replacement symlink cannot turn this success check into outside
        // metadata access.
        if let Ok(pdf) = secure_workspace.open_file(&pdf_rel) {
            let size_kb = pdf.metadata().map(|m| m.len() / 1024).unwrap_or(0);
            return Ok(Json(CompileResult {
                success: true,
                pdf_path: Some(pdf_path.display().to_string()),
                size_kb,
                message: format!("compiled {out_name}.pdf"),
                errors: vec![],
                log_excerpt: String::new(),
            }));
        }
    }
    let log_excerpt: String = output
        .content
        .chars()
        .rev()
        .take(3000)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    Ok(Json(CompileResult {
        success: false,
        pdf_path: None,
        size_kb: 0,
        message: "compile_pdf failed".into(),
        errors: vec![log_excerpt.clone()],
        log_excerpt,
    }))
}

fn normalize_compile_output_name(
    requested: Option<&str>,
    fallback: &str,
) -> Result<String, String> {
    let name = requested.unwrap_or(fallback).trim();
    let name = name.strip_suffix(".pdf").unwrap_or(name);
    let path = std::path::Path::new(name);
    if name.is_empty()
        || path.is_absolute()
        || path.components().count() != 1
        || matches!(name, "." | "..")
    {
        return Err("out_name must be a plain file name without directories".into());
    }
    Ok(name.to_string())
}

/// 从 DB 恢复 session 进内存：读 messages → 重建 Vec<ChatMessage> → 构 Session。
async fn restore_session(
    state: &AppState,
    sid: &str,
) -> Result<Arc<ActiveSession>, dss_db::DbError> {
    if let Some(active) = state.sessions.lock().await.get(sid).cloned() {
        return Ok(active);
    }

    // One cold loader at a time, with a second check after entering the
    // single-flight section. This prevents two GET/stream requests from each
    // receiving a different mutex for the same session id.
    let _restore_guard = state.session_restore_lock.lock().await;
    if let Some(active) = state.sessions.lock().await.get(sid).cloned() {
        return Ok(active);
    }

    let row = dbq::get_session_row(&state.db, sid.to_string())
        .await?
        .ok_or_else(|| dss_db::DbError::NotFound(format!("session {sid}")))?;
    let rows = dbq::list_messages(&state.db, sid.to_string()).await?;
    let runs = dbq::list_runs(&state.db, sid.to_string()).await?;
    let mut messages = Vec::with_capacity(rows.len());
    for m in rows {
        // content JSON → ChatMessage（OpenAI 协议形态，serde 直取）。
        let mut cm: ChatMessage = serde_json::from_str(&m.content).unwrap_or_else(|_| {
            // 兜底：content 当纯文本。
            ChatMessage::user(&m.content)
        });
        // 强制以 DB 的 role 为准（防 content 内 role 与行不一致）。
        cm.role = m.role;
        // 恢复 harness_notice 标记（该字段不在 content JSON 中序列化，以 DB 列为准）。
        cm.harness_notice = m.harness_notice;
        messages.push(cm);
    }
    let count = messages.len();
    let workspace = resolve_session_workspace(state, &row).await?;
    let mut session = Session::new(sid.to_string(), workspace);
    // 恢复历史与 frame 摘要。
    session.messages = messages;
    if let Some(latest_run) = runs.last() {
        session.frame.id = latest_run.frame_id.clone();
        session.frame.task_summary = latest_run.task_summary.clone();
        session.frame.root_frame_id = Some(latest_run.frame_id.clone());
    } else if let Some(t) = row.title {
        session.frame.task_summary = t;
    }
    let restored_status = match row.status.as_str() {
        "processing" | "active" => FrameStatus::Processing,
        "failed" => FrameStatus::Failed,
        "cancelled" | "interrupted" => FrameStatus::Cancelled,
        "awaiting_plan_approval" => FrameStatus::AwaitingPlanApproval,
        "awaiting_plan_execution" => FrameStatus::AwaitingPlanExecution,
        "awaiting_user_response" | "awaiting" => FrameStatus::AwaitingUserResponse,
        _ => FrameStatus::Completed,
    };
    session.frame.set_status(restored_status);
    // 恢复 plan 状态（P6）。
    if let Ok(Some(json)) = dbq::get_session_plan(&state.db, sid.to_string()).await {
        if let Ok(plan) = serde_json::from_str::<dss_tools::PlanState>(&json) {
            session.plan = Some(plan);
        }
    }

    let active = Arc::new(ActiveSession::new(session, count));
    let mut map = state.sessions.lock().await;
    map.insert(sid.to_string(), active.clone());
    // LRU 驱逐（仅内存）。
    if map.len() > MAX_ACTIVE_SESSIONS {
        if let Some(first) = map.keys().next().cloned() {
            if first != sid {
                map.remove(&first);
            }
        }
    }
    Ok(active)
}

#[derive(Deserialize)]
pub struct RunReq {
    /// Client-generated id used to linearize stream acceptance with Stop.
    run_id: String,
    prompt: String,
    #[serde(default)]
    plan_mode: Option<bool>,
    /// Explicit, one-run capability to execute the currently approved plan.
    /// Ordinary follow-up prompts must never inherit an old or cancelled plan.
    #[serde(default)]
    execute_plan: Option<bool>,
    #[allow(dead_code)]
    deep_review: Option<bool>,
}

const EVENT_CHANNEL_CAP: usize = 1024;
const PERSISTENCE_ERROR_MESSAGE: &str =
    "保存本轮会话失败，本轮结果未写入数据库。请重试；若问题持续，请重新打开该会话。";

#[derive(Clone)]
struct DurableSessionSnapshot {
    messages_len: usize,
    frame_id: String,
    parent_frame_id: Option<String>,
    root_frame_id: Option<String>,
    agent_name: String,
    frame_status: FrameStatus,
    task_summary: String,
    compaction: dss_compact::CompactionState,
    gate_state: dss_agent::session::GateState,
    plan: Option<dss_tools::PlanState>,
}

impl DurableSessionSnapshot {
    fn capture(session: &Session) -> Self {
        Self {
            messages_len: session.messages.len(),
            frame_id: session.frame.id.clone(),
            parent_frame_id: session.frame.parent_frame_id.clone(),
            root_frame_id: session.frame.root_frame_id.clone(),
            agent_name: session.frame.agent_name.clone(),
            frame_status: session.frame.status,
            task_summary: session.frame.task_summary.clone(),
            compaction: session.compaction.clone(),
            gate_state: session.gate_state.clone(),
            plan: session.plan.clone(),
        }
    }

    fn restore(&self, session: &mut Session) {
        session.messages.truncate(self.messages_len);
        session.frame.id.clone_from(&self.frame_id);
        session
            .frame
            .parent_frame_id
            .clone_from(&self.parent_frame_id);
        session.frame.root_frame_id.clone_from(&self.root_frame_id);
        session.frame.agent_name.clone_from(&self.agent_name);
        session.frame.status = self.frame_status;
        session.frame.task_summary.clone_from(&self.task_summary);
        session.compaction.clone_from(&self.compaction);
        session.gate_state.clone_from(&self.gate_state);
        session.plan.clone_from(&self.plan);
    }
}

#[derive(Clone)]
struct DurableToolCheckpointSnapshot {
    messages_len: usize,
    frame_id: String,
    parent_frame_id: Option<String>,
    root_frame_id: Option<String>,
    agent_name: String,
    frame_status: FrameStatus,
    task_summary: String,
    plan: Option<dss_tools::PlanState>,
}

impl DurableToolCheckpointSnapshot {
    fn restore(&self, session: &mut Session, run_start: &DurableSessionSnapshot) {
        let messages = session
            .messages
            .get(..self.messages_len)
            .unwrap_or(&session.messages)
            .to_vec();
        run_start.restore(session);
        session.messages = messages;
        session.frame.id.clone_from(&self.frame_id);
        session
            .frame
            .parent_frame_id
            .clone_from(&self.parent_frame_id);
        session.frame.root_frame_id.clone_from(&self.root_frame_id);
        session.frame.agent_name.clone_from(&self.agent_name);
        session.frame.status = self.frame_status;
        session.frame.task_summary.clone_from(&self.task_summary);
        session.plan.clone_from(&self.plan);
    }
}

fn validated_persistence_cursor(
    active: &ActiveSession,
    snapshot: &DurableSessionSnapshot,
) -> Result<usize, (usize, usize)> {
    let persisted = active
        .persisted_count
        .load(std::sync::atomic::Ordering::Relaxed);
    if persisted == snapshot.messages_len {
        Ok(persisted)
    } else {
        Err((persisted, snapshot.messages_len))
    }
}

struct RunFinishGuard(Arc<ActiveRunControl>);

impl Drop for RunFinishGuard {
    fn drop(&mut self) {
        // Declared before the owned Session guard in the run task, so Rust's
        // reverse local drop order releases the session mutex before this ack.
        self.0.finish();
    }
}

async fn wait_for_cancel(cancel_rx: &mut watch::Receiver<bool>) {
    loop {
        if *cancel_rx.borrow_and_update() {
            return;
        }
        if cancel_rx.changed().await.is_err() {
            return;
        }
    }
}

/// Decouple the agent's cancellation boundary from the HTTP transport. Terminal
/// events are held until persistence finishes and the session lock is released,
/// so an immediate follow-up cannot race the previous run's cleanup.
async fn relay_agent_events(
    mut agent_rx: mpsc::Receiver<AgentEvent>,
    sse_tx: mpsc::Sender<AgentEvent>,
    mut cancel_rx: watch::Receiver<bool>,
    control: Arc<ActiveRunControl>,
) {
    'relay: loop {
        let event = tokio::select! {
            biased;
            _ = wait_for_cancel(&mut cancel_rx) => break,
            _ = sse_tx.closed() => break,
            event = agent_rx.recv() => event,
        };
        let Some(event) = event else {
            // Sender disappearance without a terminal event normally means the
            // worker panicked or was aborted. Pending is fail-closed.
            control.wait_finished().await;
            if let Some(message) = persistence_failure_message(&control) {
                let _ = sse_tx.send(AgentEvent::Error { message }).await;
            }
            break;
        };

        let terminal = matches!(
            event,
            AgentEvent::Complete { .. } | AgentEvent::Error { .. }
        );
        if terminal {
            // If Stop won the linearization race, dropping agent_rx makes the
            // Runner observe tx.closed() and take its cancellation path.
            if !control.mark_terminal() {
                break;
            }
            control.wait_finished().await;
            let terminal_event = persistence_failure_message(&control)
                .map(|message| AgentEvent::Error { message })
                .unwrap_or(event);
            let _ = sse_tx.send(terminal_event).await;
            break;
        }

        tokio::select! {
            biased;
            _ = wait_for_cancel(&mut cancel_rx) => break 'relay,
            _ = sse_tx.closed() => break 'relay,
            sent = sse_tx.send(event) => {
                if sent.is_err() {
                    break;
                }
            }
        }
    }
}

fn persistence_failure_message(control: &ActiveRunControl) -> Option<String> {
    match control.persistence_state() {
        RunPersistenceState::Committed => None,
        RunPersistenceState::Pending => Some(PERSISTENCE_ERROR_MESSAGE.into()),
        RunPersistenceState::Failed(message) => Some(message),
    }
}

fn should_extract_memory(kind: CompleteKind) -> bool {
    kind == CompleteKind::Natural
}

fn complete_kind_name(kind: CompleteKind) -> &'static str {
    match kind {
        CompleteKind::Natural => "natural",
        CompleteKind::Awaiting => "awaiting",
        CompleteKind::MaxIters => "max_iters",
        CompleteKind::Error => "error",
        CompleteKind::Cancelled => "cancelled",
    }
}

fn rollback_failed_run(
    session: &mut Session,
    durable_snapshot: &DurableSessionSnapshot,
    active: &ActiveSession,
    durable_checkpoint: &StdMutex<Option<DurableToolCheckpointSnapshot>>,
    control: &ActiveRunControl,
) {
    let checkpointed_count = active
        .persisted_count
        .load(std::sync::atomic::Ordering::Relaxed);
    if checkpointed_count <= durable_snapshot.messages_len {
        durable_snapshot.restore(session);
    } else if let Some(checkpoint) = durable_checkpoint
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
    {
        checkpoint.restore(session, durable_snapshot);
    } else {
        // The atomic cursor must never advance without its matching durable snapshot.
        // Fail closed to the run-start state rather than inventing mixed metadata.
        tracing::error!(
            checkpointed_count,
            run_start = durable_snapshot.messages_len,
            "missing in-memory metadata for a committed tool checkpoint"
        );
        durable_snapshot.restore(session);
    }
    control.set_persistence_error(PERSISTENCE_ERROR_MESSAGE);
}

fn finalize_run_persistence(
    result: Result<dbq::PersistRunResult, dss_db::DbError>,
    active: &ActiveSession,
    session: &mut Session,
    durable_snapshot: &DurableSessionSnapshot,
    durable_checkpoint: &StdMutex<Option<DurableToolCheckpointSnapshot>>,
    control: &ActiveRunControl,
) -> Result<usize, dss_db::DbError> {
    match result {
        Ok(result) => {
            active
                .persisted_count
                .store(session.messages.len(), std::sync::atomic::Ordering::Relaxed);
            control.mark_persistence_committed();
            Ok(result.messages_written)
        }
        Err(error) => {
            rollback_failed_run(
                session,
                durable_snapshot,
                active,
                durable_checkpoint,
                control,
            );
            Err(error)
        }
    }
}

struct CheckpointWorkerContext {
    db: Arc<dss_db::DbPool>,
    session_id: String,
    run_id: String,
    plan_mode: bool,
    started_at: String,
    active: Arc<ActiveSession>,
    durable_checkpoint: Arc<StdMutex<Option<DurableToolCheckpointSnapshot>>>,
    initial_count: usize,
    initial_title: String,
}

async fn persist_history_checkpoints(
    mut receiver: mpsc::Receiver<HistoryCheckpoint>,
    context: CheckpointWorkerContext,
) {
    let mut expected_count = context.initial_count;
    let mut title = Some(context.initial_title);
    while let Some(checkpoint) = receiver.recv().await {
        let HistoryCheckpoint {
            messages,
            frame_id,
            parent_frame_id,
            root_frame_id,
            agent_name,
            task_summary,
            plan,
            pending_ask,
            status,
            awaiting,
            ack,
        } = checkpoint;
        let serialized = messages
            .into_iter()
            .map(|message| {
                serde_json::to_string(&message).map(|content| dbq::PersistMessage {
                    role: message.role,
                    content,
                    harness_notice: message.harness_notice,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string());
        let plan_data = plan
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| error.to_string());
        let pending_ask_json = pending_ask
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| error.to_string());
        let result = match serialized {
            Ok(messages) => match (plan_data, pending_ask_json) {
                (Ok(plan_data), Ok(pending_ask_json)) => dbq::append_history_checkpoint(
                    &context.db,
                    dbq::PersistCheckpointRequest {
                        run_id: context.run_id.clone(),
                        session_id: context.session_id.clone(),
                        frame_id: frame_id.clone(),
                        task_summary: task_summary.clone(),
                        plan_mode: context.plan_mode,
                        status: status.clone(),
                        awaiting: awaiting.clone(),
                        pending_ask_json,
                        plan_data,
                        title: title.take(),
                        started_at: context.started_at.clone(),
                        expected_count,
                        messages,
                    },
                )
                .await
                .map_err(|error| error.to_string()),
                (Err(error), _) | (_, Err(error)) => Err(error),
            },
            Err(error) => Err(error),
        };
        match result {
            Ok(next_count) => {
                expected_count = next_count;
                context
                    .active
                    .persisted_count
                    .store(next_count, std::sync::atomic::Ordering::Relaxed);
                let frame_status = match status.as_str() {
                    "awaiting_plan_approval" => FrameStatus::AwaitingPlanApproval,
                    "awaiting_plan_execution" => FrameStatus::AwaitingPlanExecution,
                    "awaiting_user_response" | "awaiting" => FrameStatus::AwaitingUserResponse,
                    "failed" => FrameStatus::Failed,
                    "cancelled" | "interrupted" => FrameStatus::Cancelled,
                    "completed" => FrameStatus::Completed,
                    _ => FrameStatus::Processing,
                };
                *context
                    .durable_checkpoint
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some(DurableToolCheckpointSnapshot {
                        messages_len: next_count,
                        frame_id,
                        parent_frame_id,
                        root_frame_id,
                        agent_name,
                        frame_status,
                        task_summary,
                        plan,
                    });
                let _ = ack.send(Ok(()));
            }
            Err(error) => {
                let _ = ack.send(Err(error));
                break;
            }
        }
    }
}

/// `POST /api/sessions/{sid}/stream-sse`：流式 run（多轮工具循环）。
pub async fn stream_sse(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<RunReq>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)>
{
    let active = restore_session(&state, &sid).await.map_err(map_db_err)?;

    let run_id = req.run_id.trim().to_owned();
    if run_id.is_empty() || run_id.len() > 128 {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "run_id must contain 1 to 128 characters",
        ));
    }

    // Declare the lifecycle guard before the owned session guard. Rust drops
    // locals in reverse declaration order, so handler cancellation at any
    // acceptance await releases the mutex before publishing the finish ack.
    let (run_control, cancel_rx) = ActiveRunControl::new();
    let finish_guard = RunFinishGuard(run_control.clone());
    let Ok(mut session) = active.session.clone().try_lock_owned() else {
        return Err(json_error(
            StatusCode::CONFLICT,
            "session already has an active run",
        ));
    };

    let plan_mode = req.plan_mode.unwrap_or(false);
    let execute_plan = req.execute_plan.unwrap_or(false);
    let initial_plan = select_run_plan(session.plan.as_ref(), plan_mode, execute_plan).map_err(
        |error| match error {
            RunPlanError::ConflictingModes => json_error(
                StatusCode::BAD_REQUEST,
                "plan_mode and execute_plan cannot both be enabled",
            ),
            RunPlanError::NoExecutablePlan => json_error(
                StatusCode::CONFLICT,
                "no approved unfinished plan is available to execute",
            ),
        },
    )?;
    let original_plan_request = if execute_plan {
        find_original_plan_request(&session.messages, initial_plan.as_ref())
    } else {
        None
    };

    // Capture app and MCP capabilities under the same settings linearization lock. A save keeps
    // this lock through its derived catalog/MCP rebuild, so a run can never pair a newly revoked
    // Registry setting with the previous connected manager (or vice versa).
    let settings_capture_guard = state.settings_save_lock.lock().await;
    let runtime = state.runtime_snapshot_with_refreshed_a2a().await;
    let llm_runtime = runtime.llm().clone();
    let llm = llm_runtime.client().cloned();
    let model = llm_runtime.settings().model.clone();
    let mcp_runtime = state.mcp_runtime_snapshot().await;
    drop(settings_capture_guard);
    // Build a private registry overlay so configured/Registry A2A tools cannot mutate or shadow
    // the process-wide base.
    let mut run_tools = mcp_runtime.tools.snapshot();
    dss_tools::builtin::a2a::register_tools(
        &mut run_tools,
        runtime.a2a(),
        state.a2a_client.as_ref(),
    )
    .map_err(|error| json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    dss_tools::builtin::agent_registry::register_tool_if_available(
        &mut run_tools,
        mcp_runtime.manager.as_ref(),
        state.a2a_client.as_ref(),
    )
    .await
    .map_err(|error| json_error(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()))?;
    let tools = Arc::new(run_tools);
    let a2a_catalog_notice = dss_tools::builtin::a2a::harness_catalog_notice(runtime.a2a());

    // Publish cancellation before the first fallible/awaiting acceptance step.
    // An earlier exact-id Stop is consumed atomically here, so a stream request
    // that loses the accept/cancel race can never start late.
    if !state
        .run_controls
        .register(&sid, &run_id, run_control.clone())
    {
        return Err(json_error(
            StatusCode::CONFLICT,
            "run was cancelled before it started",
        ));
    }

    // A fresh plan request or an ordinary follow-up supersedes any old plan. Clear it
    // before accepting the run so a crash cannot resurrect stale approval from the DB.
    if !execute_plan && session.plan.is_some() {
        if let Err(error) = dbq::set_session_plan(&state.db, sid.clone(), None).await {
            return Err(map_db_err(error));
        }
        session.plan = None;
    }

    // Approval is durable and retryable. Do not mark it processing until this
    // explicit execution request has passed every fallible acceptance check.
    if execute_plan {
        if let Err(error) =
            dbq::set_session_status(&state.db, sid.clone(), "processing".into()).await
        {
            return Err(map_db_err(error));
        }
        session.frame.set_status(FrameStatus::Processing);
    }

    let (tx, agent_rx) = mpsc::channel::<AgentEvent>(EVENT_CHANNEL_CAP);
    let (sse_tx, rx) = mpsc::channel::<AgentEvent>(EVENT_CHANNEL_CAP);
    tokio::spawn(relay_agent_events(
        agent_rx,
        sse_tx,
        cancel_rx,
        run_control.clone(),
    ));
    let db = state.db.clone();
    let memory = state.memory.clone();
    let prompt = req.prompt;
    let sid_clone = sid.clone();
    let run_id_for_persist = run_id.clone();
    let run_id_for_extract = run_id.clone();
    let run_started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let persistence_control = run_control.clone();
    // session 的 project_id（记忆按项目隔离）。
    let session_row = dbq::get_session_row(&state.db, sid.clone())
        .await
        .ok()
        .flatten();
    // A session may pin a Bot-specific model while still sharing the configured provider.
    let model = session_row
        .as_ref()
        .and_then(|row| row.model.clone())
        .unwrap_or(model);
    let project_id = session_row.as_ref().and_then(|r| r.project_id.clone());
    let bot_context = if let Some(bot_id) = session_row.as_ref().and_then(|row| row.bot_id.clone())
    {
        dbq::get_bot(&state.db, bot_id)
            .await
            .ok()
            .flatten()
            .map(|bot| {
                let instructions = bot.instructions.trim();
                if instructions.is_empty() {
                    format!(
                        "[Bot Identity]\nYou are {}. Your role is: {}. Stay consistent with this identity across the conversation.",
                        bot.name, bot.role
                    )
                } else {
                    format!(
                        "[Bot Identity]\nName: {}\nRole: {}\nInstructions:\n{}\n\nStay consistent with this identity across the conversation.",
                        bot.name, bot.role, instructions
                    )
                }
            })
    } else {
        None
    };
    let set_initial_title = session_row.as_ref().is_some_and(|r| r.title.is_none());
    let project_context = if let Some(pid) = project_id.clone() {
        dbq::get_project(&state.db, pid)
            .await
            .ok()
            .flatten()
            .and_then(|p| p.agent_context)
            .filter(|text| !text.trim().is_empty())
    } else {
        None
    };

    tokio::spawn(async move {
        let _finish_guard = finish_guard;
        let mut session = session;

        // Project context and an approved plan are per-run system inputs. They
        // are passed separately so rolling-compaction indexes always refer to
        // the canonical, append-only conversation history.
        let mut run_context = Vec::new();
        if let Some(context) = bot_context.as_deref() {
            let mut message = ChatMessage::system(context);
            message.harness_notice = true;
            run_context.push(message);
        }
        if let Some(context) = project_context.as_deref() {
            let mut message = ChatMessage::system(format!("[Project Context]\n{context}"));
            message.harness_notice = true;
            run_context.push(message);
        }
        if let Some(catalog) = a2a_catalog_notice.as_deref() {
            let mut message = ChatMessage::system(catalog);
            message.harness_notice = true;
            run_context.push(message);
        }

        if let Some(original_request) = original_plan_request.as_deref() {
            let mut message = ChatMessage::system(format!(
                "[Original approved-plan request and constraints]\n{original_request}\n\nTreat these as active execution and review constraints. The short approval command does not replace them."
            ));
            message.harness_notice = true;
            run_context.push(message);
        }

        // Only the explicit post-approval execution request receives plan context.
        // This prevents a stopped plan from contaminating an unrelated next prompt.
        if let Some(plan) = initial_plan.as_ref() {
            let summary = format!(
                "[已批准的计划]\n{}",
                plan.steps
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("{}. {} ({})", i + 1, s.title, s.status))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let mut m = ChatMessage::system(summary);
            m.harness_notice = true;
            run_context.push(m);
        }

        // Always-on 记忆注入（默认关）：把高价值 profile 记忆注入 system prefix。
        // ⚠️ 前缀缓存红线：此注入会改变 system prefix，破坏 DeepSeek 前缀缓存命中（招牌特性）。
        // 因此默认 always_on=false；启用前需评估缓存命中率下降的 token 成本。
        // on-demand BM25 召回（放 user 消息之后，runner.rs，不破坏缓存）仍是主路径。
        if state.settings.memory.enabled && state.settings.memory.always_on {
            if let Ok(profile_mems) = state
                .memory
                .list_filtered(dss_db::repo::MemoryFilter {
                    project_id: None,
                    entity: None,
                    status: Some("active"),
                })
                .await
            {
                let top: Vec<_> = profile_mems
                    .into_iter()
                    .filter(|m| m.scope.as_deref() == Some("profile"))
                    .take(3)
                    .collect();
                if !top.is_empty() {
                    let block = dss_memory::render_recall_block(&top);
                    if !block.is_empty() {
                        let mut m = ChatMessage::system(block);
                        m.harness_notice = true;
                        run_context.push(m);
                    }
                }
            }
        }

        let (history_checkpoint_tx, history_checkpoint_rx) = mpsc::channel(1);
        let ctx = {
            // 复制全局 catalog（builtin+global+外部/custom），再叠加 project 源（workspace/.deepseek-science/skills）。
            let mut cat = (*state.catalog_snapshot().await).clone();
            cat.load_dir(
                &dss_skills::project_skills_dir(&session.workspace),
                "project",
            );
            let mut tc = ToolContext::new(session.workspace.clone())
                .with_skill_catalog(cat)
                .with_mcp_arc(mcp_runtime.manager.clone())
                .with_history_checkpoint(history_checkpoint_tx);
            tc = tc.with_plan(initial_plan).await;
            // 注入 LLM（delegate 工具用）。
            if let Some(client) = &llm {
                tc = tc.with_llm(
                    client.clone() as std::sync::Arc<dyn dss_llm::LlmClient>,
                    model.clone(),
                );
            }
            // 注入记忆库（search_memory/read_memory 工具用）。enabled=false 时不注入（工具报未启用）。
            if state.settings.memory.enabled {
                tc = tc.with_memory(state.memory.clone(), project_id.clone());
            }
            // 注入数据源 API keys（search_papers 等）。
            tc = tc.with_api_keys(runtime.api_keys().clone());
            tc
        };
        let llm_for_extract = llm.clone();
        // Memory extraction is scoped to this turn. Re-sending the entire
        // append-only transcript on every completion duplicates memories,
        // increases cost quadratically, and can exceed the provider context.
        // This is the exact durable in-memory boundary. If the terminal SQLite
        // transaction fails, restoring the whole snapshot also rolls back frame,
        // plan, compaction, and gate state—not only the appended messages.
        let durable_snapshot = DurableSessionSnapshot::capture(&session);
        let persisted_before = match validated_persistence_cursor(&active, &durable_snapshot) {
            Ok(cursor) => cursor,
            Err((persisted_before, messages_len)) => {
                tracing::error!(
                    sid = %sid_clone,
                    persisted_before,
                        messages_len,
                    "refusing run with a drifted persistence cursor"
                );
                persistence_control.set_persistence_error(PERSISTENCE_ERROR_MESSAGE);
                return;
            }
        };
        let run_message_start = session.messages.len();
        let durable_checkpoint = Arc::new(StdMutex::new(None));
        let checkpoint_worker = tokio::spawn(persist_history_checkpoints(
            history_checkpoint_rx,
            CheckpointWorkerContext {
                db: db.clone(),
                session_id: sid_clone.clone(),
                run_id: run_id_for_persist.clone(),
                plan_mode,
                started_at: run_started_at.clone(),
                active: active.clone(),
                durable_checkpoint: durable_checkpoint.clone(),
                initial_count: persisted_before,
                initial_title: prompt.chars().take(60).collect(),
            },
        ));
        // —— agent 日志：run_start ——
        let _ = state.logs.append(dss_observability::LogEntry {
            level: "info".into(),
            source: "agent".into(),
            kind: "run_start".into(),
            session_id: Some(sid.clone()),
            frame_id: Some(session.frame.id.clone()),
            iteration: None,
            message: format!("run started: {}", prompt.chars().take(80).collect::<String>()),
            detail: Some(serde_json::json!({ "prompt_summary": prompt.chars().take(120).collect::<String>() })),
        }).await;

        let outcome = match llm {
            Some(client) => {
                Runner::run(
                    &mut session,
                    client.as_ref(),
                    &model,
                    &prompt,
                    tools.as_ref(),
                    &ctx,
                    runtime.max_iterations(),
                    dss_compact::constants::DEFAULT_CONTEXT_CEILING,
                    Some(&memory),
                    project_id.as_deref(),
                    &run_context,
                    plan_mode,
                    &tx,
                )
                .await
            }
            None => {
                session
                    .frame
                    .begin_run(prompt.chars().take(80).collect::<String>());
                session.messages.push(ChatMessage::user(&prompt));
                let _ = tx
                    .send(AgentEvent::Start {
                        frame_id: session.frame.id.clone(),
                        task_summary: prompt.chars().take(80).collect(),
                    })
                    .await;
                session.frame.set_status(FrameStatus::Failed);
                let _ = tx
                    .send(AgentEvent::Complete {
                        kind: CompleteKind::Error,
                        final_text: String::new(),
                        awaiting: None,
                        error: Some(
                            "LLM not configured: set DEEPSEEK_API_KEY env or \
                             settings.json llm.api_key"
                                .to_string(),
                        ),
                        usage: Default::default(),
                        iterations: 0,
                        frame_status: session.frame.status,
                        pending_ask: None,
                        plan: None,
                    })
                    .await;
                dss_agent::RunOutcome {
                    kind: CompleteKind::Error,
                    final_text: String::new(),
                    awaiting: None,
                    pending_ask: None,
                    error: Some(
                        "LLM not configured: set DEEPSEEK_API_KEY env or settings.json llm.api_key"
                            .into(),
                    ),
                    usage: Default::default(),
                    iterations: 0,
                }
            }
        };
        drop(ctx);
        if let Err(error) = checkpoint_worker.await {
            tracing::error!(%error, sid = %sid_clone, "history checkpoint worker panicked");
        }

        // Commit the complete run snapshot in one SQLite transaction. The
        // persisted cursor advances only after every message and matching
        // session/project/run field has committed successfully.
        let prev = active
            .persisted_count
            .load(std::sync::atomic::Ordering::Relaxed);
        let checkpoint_start_seq = (prev > persisted_before).then_some(persisted_before as i64 + 1);
        let persisted_status = match outcome.kind {
            CompleteKind::Natural => "completed",
            CompleteKind::Awaiting => match session.frame.status {
                FrameStatus::AwaitingPlanApproval => "awaiting_plan_approval",
                FrameStatus::AwaitingPlanExecution => "awaiting_plan_execution",
                _ => "awaiting_user_response",
            },
            CompleteKind::Cancelled => "cancelled",
            CompleteKind::Error => "failed",
            CompleteKind::MaxIters => "failed",
        };
        // Runner owns the plan commit boundary. In particular, cancellation may leave
        // an in-flight tool mutation in ToolContext that was never delivered to the UI;
        // persisting from `ctx` here would turn that ghost mutation into durable state.
        let plan_json = session
            .plan
            .as_ref()
            .and_then(|p| serde_json::to_string(p).ok());
        let pending_ask_json = outcome
            .pending_ask
            .as_ref()
            .and_then(|ask| serde_json::to_string(ask).ok());
        let serialized_messages = session
            .messages
            .get(prev..)
            .unwrap_or(&[])
            .iter()
            .map(|message| {
                serde_json::to_string(message).map(|content| dbq::PersistMessage {
                    role: message.role.clone(),
                    content,
                    harness_notice: message.harness_notice,
                })
            })
            .collect::<Result<Vec<_>, _>>();
        let mut persisted_new = 0usize;
        let mut persistence_error = None::<String>;
        match serialized_messages {
            Ok(messages) => {
                let request = dbq::PersistRunRequest {
                    run_id: run_id_for_persist,
                    session_id: sid_clone.clone(),
                    frame_id: session.frame.id.clone(),
                    task_summary: session.frame.task_summary.clone(),
                    plan_mode,
                    status: persisted_status.into(),
                    kind: Some(complete_kind_name(outcome.kind).into()),
                    awaiting: outcome.awaiting.clone(),
                    pending_ask_json,
                    error: outcome.error.clone(),
                    input_tokens: i64::from(outcome.usage.input_tokens),
                    output_tokens: i64::from(outcome.usage.output_tokens),
                    iterations: i64::from(outcome.iterations),
                    plan_data: plan_json,
                    title: set_initial_title.then(|| prompt.chars().take(60).collect::<String>()),
                    started_at: run_started_at,
                    checkpoint_start_seq,
                    messages,
                };
                match finalize_run_persistence(
                    dbq::persist_run(&db, request).await,
                    active.as_ref(),
                    &mut session,
                    &durable_snapshot,
                    durable_checkpoint.as_ref(),
                    persistence_control.as_ref(),
                ) {
                    Ok(messages_written) => persisted_new = messages_written,
                    Err(error) => {
                        tracing::error!(%error, sid = %sid_clone, "persist run transaction failed");
                        persistence_error = Some(error.to_string());
                    }
                }
            }
            Err(error) => {
                tracing::error!(%error, sid = %sid_clone, "serialize run messages failed");
                rollback_failed_run(
                    &mut session,
                    &durable_snapshot,
                    active.as_ref(),
                    durable_checkpoint.as_ref(),
                    persistence_control.as_ref(),
                );
                persistence_error = Some(error.to_string());
            }
        }

        let persistence_succeeded = persistence_error.is_none();
        if let Some(error) = persistence_error.as_deref() {
            tracing::error!(%error, sid = %sid_clone, "rolled back in-memory run after persistence failure");
        }

        tracing::info!(
            kind = ?outcome.kind,
            iterations = outcome.iterations,
            persisted_new,
            "run finished"
        );

        // —— agent 日志：run_end ——
        let _ = state
            .logs
            .append(dss_observability::LogEntry {
                level:
                    if outcome.kind == dss_agent::CompleteKind::Error || !persistence_succeeded {
                        "error"
                    } else {
                        "info"
                    }
                    .into(),
                source: "agent".into(),
                kind: "run_end".into(),
                session_id: Some(sid.clone()),
                frame_id: Some(session.frame.id.clone()),
                iteration: Some(outcome.iterations as i64),
                message: format!("run ended: {:?}", outcome.kind),
                detail: Some(serde_json::json!({
                    "kind": format!("{:?}", outcome.kind),
                    "iterations": outcome.iterations,
                    "input_tokens": outcome.usage.input_tokens,
                    "output_tokens": outcome.usage.output_tokens,
                })),
            })
            .await;

        // Only a naturally completed answer may influence long-term memory.
        // Cancelled/failed/awaiting runs are incomplete work and extracting them
        // would add cost while contaminating later research sessions.
        if persistence_succeeded
            && should_extract_memory(outcome.kind)
            && state.settings.memory.enabled
        {
            if let Some(client) = llm_for_extract.as_ref() {
                let msgs_snapshot: Vec<dss_llm::ChatMessage> =
                    session.messages[run_message_start.min(session.messages.len())..].to_vec();
                // extract_model 优先于主模型（可用更便宜/更快的模型抽取记忆）。
                let model_c = state
                    .settings
                    .memory
                    .extract_model
                    .clone()
                    .unwrap_or_else(|| model.clone());
                let memory_c = memory.clone();
                let pid_c = project_id.clone();
                let client_c = client.clone();
                let sid_c = sid_clone.clone();
                let rid_c = run_id_for_extract.clone();
                let logs_c = state.logs.clone();
                let seq_lo = checkpoint_start_seq.unwrap_or(1).max(1);
                let seq_hi = seq_lo + msgs_snapshot.len() as i64;
                let memory_settings = state.settings.memory.clone();
                tokio::spawn(async move {
                    match dss_memory::extract::extract(client_c.as_ref(), &model_c, &msgs_snapshot)
                        .await
                    {
                        Ok(items) if !items.is_empty() => {
                            // 证据回溯：本次 run 的消息范围（供 L4 证据展开）。
                            let evidence = vec![dss_memory::EvidenceRef {
                                session_id: sid_c.clone(),
                                run_id: Some(rid_c),
                                seq_start: seq_lo,
                                seq_end: seq_hi,
                            }];
                            let cfg = dss_memory::consolidate::ConsolidateConfig {
                                auto_promote_threshold: memory_settings.auto_promote_threshold,
                                dedupe_similarity: memory_settings.dedupe_similarity,
                                trust_high_risk_approve: memory_settings.trust_high_risk_approve,
                            };
                            let stats = dss_memory::consolidate::promote_candidates(
                                &memory_c, items, pid_c, &evidence, &cfg,
                            )
                            .await;
                            tracing::info!(
                                active = stats.promoted_active,
                                candidate = stats.promoted_candidate,
                                superseded = stats.superseded,
                                duplicates = stats.duplicates,
                                errors = stats.errors,
                                "memory consolidate completed (background)"
                            );
                            // 评测埋点：把巩固统计写进 logs（source=agent, kind=memory），
                            // 供文章 §7 评测框架（capture precision / supersede 率 / candidate 积压）。
                            let total = stats.promoted_active
                                + stats.promoted_candidate
                                + stats.superseded
                                + stats.duplicates;
                            let _ = logs_c
                                .append(dss_observability::LogEntry {
                                    level: "info".into(),
                                    source: "agent".into(),
                                    kind: "memory".into(),
                                    session_id: Some(sid_c.clone()),
                                    frame_id: None,
                                    iteration: None,
                                    message: format!(
                                    "记忆巩固：{} 条候选 → {} 生效 / {} 待审 / {} 替代 / {} 重复",
                                    total,
                                    stats.promoted_active,
                                    stats.promoted_candidate,
                                    stats.superseded,
                                    stats.duplicates,
                                ),
                                    detail: Some(serde_json::json!({
                                        "promoted_active": stats.promoted_active,
                                        "promoted_candidate": stats.promoted_candidate,
                                        "superseded": stats.superseded,
                                        "duplicates": stats.duplicates,
                                        "errors": stats.errors,
                                    })),
                                })
                                .await;
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "memory extract failed (background)"),
                    }
                });
            }
        }
    });

    let stream = ReceiverStream::new(rx).map(|event| {
        let data = serde_json::to_string(&event)
            .unwrap_or_else(|_| r#"{"type":"error","message":"serialize failed"}"#.to_string());
        Ok::<_, Infallible>(Event::default().data(data))
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

#[derive(Serialize)]
pub struct CancelRunResp {
    /// False means the run had already committed its terminal event; the UI
    /// must let that normal completion arrive instead of masking it as stopped.
    cancelled: bool,
}

#[derive(Deserialize)]
pub struct CancelRunReq {
    run_id: String,
}

/// Explicitly cancel the active run and do not acknowledge until its session
/// mutex has been released. This is the ordering guarantee the next prompt uses.
pub async fn cancel_run(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<CancelRunReq>,
) -> Result<Json<CancelRunResp>, (StatusCode, Json<Value>)> {
    let run_id = req.run_id.trim();
    if run_id.is_empty() || run_id.len() > 128 {
        return Err(json_error(
            StatusCode::BAD_REQUEST,
            "run_id must contain 1 to 128 characters",
        ));
    }

    let Some(control) = state.run_controls.find_or_pre_cancel(&sid, run_id) else {
        // Stop won before stream registration. The retained marker guarantees
        // that a matching stream request arriving later is rejected.
        return Ok(Json(CancelRunResp { cancelled: true }));
    };
    let cancelled = control.request_cancel();
    tokio::time::timeout(Duration::from_secs(15), control.wait_finished())
        .await
        .map_err(|_| {
            json_error(
                StatusCode::GATEWAY_TIMEOUT,
                "timed out while waiting for the active run to stop",
            )
        })?;
    if persistence_failure_message(&control).is_some() {
        return Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            PERSISTENCE_ERROR_MESSAGE,
        ));
    }
    Ok(Json(CancelRunResp { cancelled }))
}

fn plan_requires_execution(plan: &dss_tools::PlanState) -> bool {
    plan.approved && plan.steps.iter().any(|step| step.status != "done")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunPlanError {
    ConflictingModes,
    NoExecutablePlan,
}

/// Select the only plan snapshot this run is authorized to see.
///
/// Plan creation always starts empty. Approved plans are injected only for the explicit
/// post-approval execution request; ordinary requests receive no legacy plan context.
fn select_run_plan(
    current: Option<&dss_tools::PlanState>,
    plan_mode: bool,
    execute_plan: bool,
) -> Result<Option<dss_tools::PlanState>, RunPlanError> {
    if plan_mode && execute_plan {
        return Err(RunPlanError::ConflictingModes);
    }
    if !execute_plan {
        return Ok(None);
    }
    current
        .filter(|plan| plan_requires_execution(plan))
        .cloned()
        .map(Some)
        .ok_or(RunPlanError::NoExecutablePlan)
}

/// Recover the user request that actually produced the durable Plan snapshot. Approval and
/// retry prompts are intentionally generic, so using the most recent user message would weaken
/// constraints such as no-network, allowed dependencies, or a soft iteration budget.
fn find_original_plan_request(
    messages: &[ChatMessage],
    plan: Option<&dss_tools::PlanState>,
) -> Option<String> {
    let generate_plan_index = messages.iter().rposition(|message| {
        message.tool_calls.as_ref().is_some_and(|calls| {
            calls
                .iter()
                .any(|call| call.function.name == "generate_plan")
        })
    });

    generate_plan_index
        .and_then(|index| {
            messages[..index]
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .and_then(|message| message.content.as_deref())
                .map(str::trim)
                .filter(|content| !content.is_empty())
                .map(ToOwned::to_owned)
        })
        .or_else(|| {
            plan.and_then(|plan| plan.research_question.as_deref())
                .map(str::trim)
                .filter(|content| !content.is_empty())
                .map(ToOwned::to_owned)
        })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::body::to_bytes;
    use axum::extract::{Path as AxumPath, State};
    use axum::http::header;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::Json;
    use axum::Router;
    use dss_agent::{AgentEvent, CompleteKind, FrameStatus, Session};
    use dss_core::settings::ServerSettings;
    use dss_core::{LlmEnvOverrides, LlmSettings, Settings};
    use dss_llm::{ChatMessage, ToolCall};
    use dss_tools::{HistoryCheckpoint, PendingAsk, PlanState, PlanStep};

    use super::{
        approve_plan, cancel_run, create_session, delete_session, find_original_plan_request,
        get_session, plan_requires_execution, relay_agent_events, restore_session, select_run_plan,
        should_extract_memory, stream_sse, CancelRunReq, RunPlanError, RunReq,
    };
    use crate::state::{ActiveRunControl, ActiveSession};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "deepseek-science-{label}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn plan(approved: bool, statuses: &[&str]) -> PlanState {
        PlanState {
            approved,
            steps: statuses
                .iter()
                .enumerate()
                .map(|(index, status)| PlanStep {
                    title: format!("step {index}"),
                    status: (*status).to_string(),
                })
                .collect(),
            research_question: None,
        }
    }

    async fn repeated_tool_call_response(
        State(request_count): State<Arc<AtomicUsize>>,
    ) -> impl IntoResponse {
        let request_number = request_count.fetch_add(1, Ordering::SeqCst) + 1;
        let body = format!(
            concat!(
                "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,",
                "\"id\":\"call-{}\",\"type\":\"function\",",
                "\"function\":{{\"name\":\"list_files\",\"arguments\":\"{{}}\"}}}}]}},",
                "\"finish_reason\":null}}]}}\n\n",
                "data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n",
                "data: [DONE]\n\n"
            ),
            request_number
        );
        ([(header::CONTENT_TYPE, "text/event-stream")], body)
    }

    #[test]
    fn only_injects_approved_plan_with_unfinished_steps() {
        assert!(plan_requires_execution(&plan(true, &["done", "pending"])));
        assert!(plan_requires_execution(&plan(true, &["running"])));
        assert!(plan_requires_execution(&plan(true, &["done", "failed"])));
        assert!(!plan_requires_execution(&plan(false, &["pending"])));
        assert!(!plan_requires_execution(&plan(true, &["done", "done"])));
        assert!(!plan_requires_execution(&plan(true, &[])));
    }

    #[test]
    fn plan_context_requires_explicit_execution_intent() {
        let approved = plan(true, &["done", "pending"]);

        assert!(select_run_plan(Some(&approved), false, false)
            .unwrap()
            .is_none());
        assert!(select_run_plan(Some(&approved), true, false)
            .unwrap()
            .is_none());
        let selected = select_run_plan(Some(&approved), false, true)
            .unwrap()
            .expect("approved plan");
        assert!(selected.approved);
        assert_eq!(selected.steps.len(), 2);
        assert_eq!(selected.steps[1].status, "pending");
        assert!(matches!(
            select_run_plan(Some(&approved), true, true),
            Err(RunPlanError::ConflictingModes)
        ));
    }

    #[test]
    fn execution_rejects_missing_unapproved_or_finished_plan() {
        assert!(matches!(
            select_run_plan(None, false, true),
            Err(RunPlanError::NoExecutablePlan)
        ));
        assert!(matches!(
            select_run_plan(Some(&plan(false, &["pending"])), false, true),
            Err(RunPlanError::NoExecutablePlan)
        ));
        assert!(matches!(
            select_run_plan(Some(&plan(true, &["done", "done"])), false, true),
            Err(RunPlanError::NoExecutablePlan)
        ));
        assert!(select_run_plan(Some(&plan(true, &["failed"])), false, true).is_ok());
    }

    #[test]
    fn approved_execution_recovers_the_plan_generating_request_not_retry_commands() {
        let original = "Analyze lightcurve.csv without network in ≤6 iterations.";
        let messages = vec![
            ChatMessage::user(original),
            ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                "plan-call",
                "generate_plan",
                r#"{"steps":[{"title":"analyze"}]}"#.to_string(),
            )]),
            ChatMessage::tool(
                "plan-call",
                "plan generated",
                Some("generate_plan".to_string()),
            ),
            ChatMessage::user("请按照已批准的计划开始执行。"),
            ChatMessage::assistant("a previous execution attempt failed"),
            ChatMessage::user("retry the approved plan"),
        ];

        assert_eq!(
            find_original_plan_request(&messages, Some(&plan(true, &["pending"]))),
            Some(original.to_string())
        );
    }

    #[test]
    fn legacy_plan_without_generate_trace_uses_research_question_fallback() {
        let mut legacy = plan(true, &["pending"]);
        legacy.research_question = Some("legacy scientific question".to_string());

        assert_eq!(
            find_original_plan_request(&[ChatMessage::user("execute plan")], Some(&legacy)),
            Some("legacy scientific question".to_string())
        );
    }

    #[test]
    fn incomplete_runs_never_trigger_memory_extraction() {
        assert!(should_extract_memory(CompleteKind::Natural));
        assert!(!should_extract_memory(CompleteKind::Cancelled));
        assert!(!should_extract_memory(CompleteKind::Error));
        assert!(!should_extract_memory(CompleteKind::MaxIters));
        assert!(!should_extract_memory(CompleteKind::Awaiting));
    }

    #[tokio::test]
    async fn stream_sse_uses_the_captured_runtime_iteration_limit() {
        let request_count = Arc::new(AtomicUsize::new(0));
        let provider = Router::new()
            .route("/chat/completions", post(repeated_tool_call_response))
            .with_state(request_count.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake provider");
        let provider_base_url = format!(
            "http://{}",
            listener.local_addr().expect("fake provider address")
        );
        let provider_task = tokio::spawn(async move {
            axum::serve(listener, provider)
                .await
                .expect("serve fake provider");
        });

        let test_dir = TestDir::new("dynamic-runtime-iteration-limit");
        let model = "fake-openai-compatible".to_string();
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: 2,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings {
                base_url: provider_base_url.clone(),
                model: model.clone(),
                api_key: Some("test-only-key".into()),
            },
            providers: vec![dss_core::LlmProvider {
                id: "fake".into(),
                name: "Fake".into(),
                base_url: provider_base_url,
                model,
                api_key: Some("test-only-key".into()),
                enabled: true,
            }],
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");
        assert_eq!(state.runtime_snapshot().await.max_iterations(), 2);

        let Json(created) = create_session(State(state.clone()), None)
            .await
            .expect("create test session");
        let response = stream_sse(
            State(state),
            AxumPath(created.id),
            Json(RunReq {
                run_id: "dynamic-limit-run".into(),
                prompt: "keep calling the listed tool".into(),
                plan_mode: Some(false),
                execute_plan: Some(false),
                deep_review: None,
            }),
        )
        .await
        .expect("start streamed run")
        .into_response();
        let body = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("collect streamed events");
        let body = String::from_utf8(body.to_vec()).expect("SSE is UTF-8");

        assert!(body.contains("\"kind\":\"max_iters\""), "{body}");
        assert!(body.contains("\"iterations\":2"), "{body}");
        assert!(body.contains("reached max iterations (2)"), "{body}");
        assert_eq!(request_count.load(Ordering::SeqCst), 2);

        provider_task.abort();
    }

    #[tokio::test]
    async fn approved_plan_survives_reload_as_retryable_awaiting_execution() {
        let test_dir = TestDir::new("plan-approval-reload");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");

        let created = create_session(State(state.clone()), None)
            .await
            .expect("create session")
            .0;
        let sid = created.id;
        let active = state
            .sessions
            .lock()
            .await
            .get(&sid)
            .cloned()
            .expect("active session");
        {
            let mut session = active.session.lock().await;
            session.plan = Some(plan(false, &["pending"]));
            session
                .frame
                .set_status(dss_agent::FrameStatus::AwaitingPlanApproval);
        }

        let approved = approve_plan(State(state.clone()), AxumPath(sid.clone()))
            .await
            .expect("approve plan")
            .0;
        assert_eq!(approved["approved"], true);

        let row = crate::db::get_session_row(&state.db, sid.clone())
            .await
            .expect("read session row")
            .expect("session row");
        assert_eq!(row.status, "awaiting_plan_execution");
        let persisted_plan = crate::db::get_session_plan(&state.db, sid.clone())
            .await
            .expect("read plan")
            .expect("persisted plan");
        let persisted_plan: PlanState =
            serde_json::from_str(&persisted_plan).expect("deserialize plan");
        assert!(persisted_plan.approved);

        // Simulate an app restart/LRU eviction. GET must restore the durable
        // approval gate instead of reporting Processing or hiding the plan.
        drop(active);
        state.sessions.lock().await.remove(&sid);
        let restored = get_session(State(state), AxumPath(sid))
            .await
            .expect("restore session")
            .0;
        assert_eq!(restored["status"], "awaiting_plan_execution");
        assert_eq!(restored["plan"]["approved"], true);
        assert_eq!(restored["plan"]["steps"][0]["status"], "pending");
    }

    #[tokio::test]
    async fn get_session_restores_persisted_run_metadata_messages_and_frame() {
        let test_dir = TestDir::new("run-metadata-reload");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");

        let sid = create_session(State(state.clone()), None)
            .await
            .expect("create session")
            .0
            .id;
        let plan_json = serde_json::to_string(&plan(false, &["pending"])).unwrap();
        let pending_ask = serde_json::json!({
            "question": "Which dataset?",
            "header": "Dataset",
            "options": [{"label": "A", "description": "first"}],
        });
        crate::db::persist_run(
            &state.db,
            crate::db::PersistRunRequest {
                run_id: "run-restored".into(),
                session_id: sid.clone(),
                frame_id: "frame-restored".into(),
                task_summary: "choose a dataset".into(),
                plan_mode: true,
                status: "awaiting_user_response".into(),
                kind: Some("awaiting".into()),
                awaiting: Some("user_response".into()),
                pending_ask_json: Some(pending_ask.to_string()),
                error: None,
                input_tokens: 21,
                output_tokens: 8,
                iterations: 3,
                plan_data: Some(plan_json),
                title: Some("choose a dataset".into()),
                started_at: "2026-08-04T10:00:00.000Z".into(),
                checkpoint_start_seq: None,
                messages: vec![
                    crate::db::PersistMessage {
                        role: "user".into(),
                        content: r#"{"role":"user","content":"choose a dataset"}"#.into(),
                        harness_notice: false,
                    },
                    crate::db::PersistMessage {
                        role: "assistant".into(),
                        content: r#"{"role":"assistant","content":null,"tool_calls":[{"id":"ask-1","type":"function","function":{"name":"ask_user","arguments":"{\"question\":\"Which dataset?\"}"}}],"usage":{"input_tokens":21,"output_tokens":8}}"#.into(),
                        harness_notice: false,
                    },
                    crate::db::PersistMessage {
                        role: "tool".into(),
                        content: r#"{"role":"tool","content":"[asked user] Which dataset?","tool_call_id":"ask-1","name":"ask_user","is_error":false}"#.into(),
                        harness_notice: false,
                    },
                ],
            },
        )
        .await
        .expect("persist terminal run");

        // Force the same cold-restore path used after an app restart/LRU eviction.
        state.sessions.lock().await.remove(&sid);
        let restored = get_session(State(state.clone()), AxumPath(sid.clone()))
            .await
            .expect("restore session")
            .0;

        assert_eq!(restored["frame_id"], "frame-restored");
        assert_eq!(restored["status"], "awaiting_user_response");
        assert_eq!(restored["plan_mode"], true);
        assert_eq!(restored["messages"][0]["seq"], 1);
        assert_eq!(restored["messages"][0]["run_id"], "run-restored");
        assert_eq!(restored["messages"][2]["role"], "tool");
        assert_eq!(restored["runs"][0]["run_id"], "run-restored");
        assert_eq!(restored["runs"][0]["ordinal"], 1);
        assert_eq!(restored["runs"][0]["awaiting"], "user_response");
        assert_eq!(
            restored["runs"][0]["pending_ask"]["question"],
            "Which dataset?"
        );
        assert_eq!(restored["runs"][0]["usage"]["input_tokens"], 21);
        assert_eq!(restored["runs"][0]["iterations"], 3);
        assert_eq!(restored["runs"][0]["start_seq"], 1);
        assert_eq!(restored["runs"][0]["end_seq"], 3);
        assert_eq!(restored["runs"][0]["plan"]["approved"], false);

        let project = crate::db::get_project(&state.db, dss_db::DEFAULT_PROJECT_ID.into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(project.last_session_id.as_deref(), Some(sid.as_str()));
    }

    #[tokio::test]
    async fn concurrent_cold_restore_returns_one_shared_session_arc() {
        let test_dir = TestDir::new("single-flight-restore");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");
        let sid = create_session(State(state.clone()), None)
            .await
            .expect("create session")
            .0
            .id;
        state.sessions.lock().await.remove(&sid);

        let (first, second) =
            tokio::join!(restore_session(&state, &sid), restore_session(&state, &sid));
        let first = first.expect("first restore");
        let second = second.expect("second restore");
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        let stored = state
            .sessions
            .lock()
            .await
            .get(&sid)
            .cloned()
            .expect("stored restored session");
        assert!(std::sync::Arc::ptr_eq(&first, &stored));
    }

    #[tokio::test]
    async fn delete_rejects_an_active_session_before_touching_database_or_workspace() {
        let test_dir = TestDir::new("delete-active-session");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");
        let created = create_session(State(state.clone()), None).await.unwrap().0;
        let sid = created.id;
        let workspace = PathBuf::from(created.workspace);
        let active = restore_session(&state, &sid).await.unwrap();
        let guard = active.session.clone().lock_owned().await;

        let (status, _) = delete_session(State(state.clone()), AxumPath(sid.clone()))
            .await
            .expect_err("active session deletion must fail");
        assert_eq!(status, axum::http::StatusCode::CONFLICT);
        assert!(crate::db::get_session_row(&state.db, sid.clone())
            .await
            .unwrap()
            .is_some());
        assert!(workspace.exists());

        drop(guard);
        assert_eq!(
            delete_session(State(state.clone()), AxumPath(sid.clone()))
                .await
                .unwrap(),
            axum::http::StatusCode::NO_CONTENT
        );
        assert!(crate::db::get_session_row(&state.db, sid)
            .await
            .unwrap()
            .is_none());
        assert!(!workspace.exists());
    }

    #[tokio::test]
    async fn cold_restore_rebases_a_missing_workspace_after_data_directory_move() {
        let test_dir = TestDir::new("relocated-workspace-restore");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");
        let created = create_session(State(state.clone()), None)
            .await
            .expect("create session")
            .0;
        let sid = created.id;
        let marker = test_dir
            .path()
            .join("workspaces")
            .join(&sid)
            .join("paper.md");
        std::fs::write(&marker, "restored paper").expect("write workspace marker");
        let obsolete = test_dir
            .path()
            .join("previous-data-root/workspaces")
            .join(&sid)
            .to_string_lossy()
            .into_owned();
        assert!(crate::db::rebase_session_workspace(
            &state.db,
            sid.clone(),
            created.workspace.clone(),
            obsolete,
        )
        .await
        .expect("install obsolete persisted path"));

        // Simulate a restart: no in-memory session remains, while the copied workspace now lives
        // under this process's current data directory.
        state.sessions.lock().await.remove(&sid);
        let restored = get_session(State(state.clone()), AxumPath(sid.clone()))
            .await
            .expect("restore relocated session")
            .0;
        let expected_workspace = test_dir.path().join("workspaces").join(&sid);
        assert_eq!(restored["workspace"].as_str(), expected_workspace.to_str());
        let active = state
            .sessions
            .lock()
            .await
            .get(&sid)
            .cloned()
            .expect("restored active session");
        assert_eq!(active.session.lock().await.workspace, expected_workspace);
        let stored = crate::db::get_session_row(&state.db, sid)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.workspace, expected_workspace.to_string_lossy());
    }

    #[tokio::test]
    async fn approval_database_failure_is_returned_without_mutating_memory() {
        let test_dir = TestDir::new("plan-approval-db-error");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");

        let sid = create_session(State(state.clone()), None)
            .await
            .expect("create session")
            .0
            .id;
        let active = state
            .sessions
            .lock()
            .await
            .get(&sid)
            .cloned()
            .expect("active session");
        {
            let mut session = active.session.lock().await;
            session.plan = Some(plan(false, &["pending"]));
            session
                .frame
                .set_status(dss_agent::FrameStatus::AwaitingPlanApproval);
        }
        crate::db::delete_session_row(&state.db, sid.clone())
            .await
            .expect("remove backing row");

        let (status, _) = approve_plan(State(state), AxumPath(sid))
            .await
            .expect_err("approval must surface database failure");
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
        let session = active.session.lock().await;
        assert_eq!(
            session.frame.status,
            dss_agent::FrameStatus::AwaitingPlanApproval
        );
        assert!(!session.plan.as_ref().expect("plan retained").approved);
    }

    #[tokio::test]
    async fn cancel_ack_is_sent_only_after_the_session_lock_is_released() {
        let test_dir = TestDir::new("cancel-ack-ordering");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");
        let sid = create_session(State(state.clone()), None)
            .await
            .expect("create session")
            .0
            .id;
        let active = state
            .sessions
            .lock()
            .await
            .get(&sid)
            .cloned()
            .expect("active session");
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn({
            let active = active.clone();
            let run_controls = state.run_controls.clone();
            let sid = sid.clone();
            async move {
                let session = active.session.clone().lock_owned().await;
                let (control, mut cancel_rx) = ActiveRunControl::new();
                assert!(run_controls.register(&sid, "run-ordering", control.clone()));
                ready_tx.send(()).expect("signal run ready");
                cancel_rx.changed().await.expect("receive cancellation");
                drop(session);
                control.mark_persistence_committed();
                control.finish();
            }
        });
        ready_rx.await.expect("run became active");

        let response = cancel_run(
            State(state),
            AxumPath(sid),
            Json(CancelRunReq {
                run_id: "run-ordering".into(),
            }),
        )
        .await
        .expect("cancel endpoint")
        .0;
        assert!(response.cancelled);
        assert!(active.session.try_lock().is_ok());
        worker.await.expect("run worker");
    }

    #[tokio::test]
    async fn cancel_surfaces_a_failed_terminal_persistence_state() {
        let test_dir = TestDir::new("cancel-persistence-error");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");
        let (control, _cancel_rx) = ActiveRunControl::new();
        assert!(state
            .run_controls
            .register("session", "run-failed", control.clone()));
        control.set_persistence_error(super::PERSISTENCE_ERROR_MESSAGE);
        control.finish();

        let result = cancel_run(
            State(state),
            AxumPath("session".into()),
            Json(CancelRunReq {
                run_id: "run-failed".into(),
            }),
        )
        .await;
        let (status, body) = match result {
            Err(error) => error,
            Ok(_) => panic!("failed persistence must not acknowledge cancellation as durable"),
        };
        assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.0["error"], super::PERSISTENCE_ERROR_MESSAGE);
    }

    #[tokio::test]
    async fn dropped_sse_transport_closes_the_agent_channel_immediately() {
        let (control, cancel_rx) = ActiveRunControl::new();
        let (agent_tx, agent_rx) = tokio::sync::mpsc::channel(1);
        let (sse_tx, sse_rx) = tokio::sync::mpsc::channel(1);
        drop(sse_rx);

        let relay = tokio::spawn(relay_agent_events(agent_rx, sse_tx, cancel_rx, control));
        tokio::time::timeout(std::time::Duration::from_secs(1), agent_tx.closed())
            .await
            .expect("relay should observe transport closure");
        relay.await.expect("relay task");
    }

    #[tokio::test]
    async fn persistence_failure_replaces_buffered_complete_at_the_relay() {
        let (control, cancel_rx) = ActiveRunControl::new();
        let (agent_tx, agent_rx) = tokio::sync::mpsc::channel(2);
        let (sse_tx, mut sse_rx) = tokio::sync::mpsc::channel(2);
        let relay = tokio::spawn(relay_agent_events(
            agent_rx,
            sse_tx,
            cancel_rx,
            control.clone(),
        ));

        agent_tx
            .send(AgentEvent::Complete {
                kind: CompleteKind::Natural,
                final_text: "looks successful".into(),
                awaiting: None,
                error: None,
                usage: Default::default(),
                iterations: 1,
                frame_status: FrameStatus::Completed,
                pending_ask: None,
                plan: None,
            })
            .await
            .expect("queue terminal event");
        tokio::task::yield_now().await;
        assert!(
            sse_rx.try_recv().is_err(),
            "terminal event must wait for persistence"
        );

        control.set_persistence_error(super::PERSISTENCE_ERROR_MESSAGE);
        control.finish();
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(1), sse_rx.recv())
            .await
            .expect("relay timeout")
            .expect("terminal event");
        match terminal {
            AgentEvent::Error { message } => {
                assert_eq!(message, super::PERSISTENCE_ERROR_MESSAGE)
            }
            _ => panic!("successful Complete must be replaced"),
        }
        relay.await.expect("relay task");
    }

    #[tokio::test]
    async fn committed_persistence_releases_the_original_complete() {
        let (control, cancel_rx) = ActiveRunControl::new();
        let (agent_tx, agent_rx) = tokio::sync::mpsc::channel(1);
        let (sse_tx, mut sse_rx) = tokio::sync::mpsc::channel(1);
        let relay = tokio::spawn(relay_agent_events(
            agent_rx,
            sse_tx,
            cancel_rx,
            control.clone(),
        ));
        agent_tx
            .send(AgentEvent::Complete {
                kind: CompleteKind::Natural,
                final_text: "durable".into(),
                awaiting: None,
                error: None,
                usage: Default::default(),
                iterations: 1,
                frame_status: FrameStatus::Completed,
                pending_ask: None,
                plan: None,
            })
            .await
            .expect("queue terminal event");
        control.mark_persistence_committed();
        control.finish();
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(1), sse_rx.recv())
            .await
            .expect("relay timeout")
            .expect("terminal event");
        assert!(matches!(
            terminal,
            AgentEvent::Complete {
                kind: CompleteKind::Natural,
                ..
            }
        ));
        relay.await.expect("relay task");
    }

    #[tokio::test]
    async fn pending_persistence_is_fail_closed_after_worker_finish() {
        let (control, cancel_rx) = ActiveRunControl::new();
        let (agent_tx, agent_rx) = tokio::sync::mpsc::channel(2);
        let (sse_tx, mut sse_rx) = tokio::sync::mpsc::channel(2);
        let relay = tokio::spawn(relay_agent_events(
            agent_rx,
            sse_tx,
            cancel_rx,
            control.clone(),
        ));
        agent_tx
            .send(AgentEvent::Complete {
                kind: CompleteKind::Natural,
                final_text: "must not escape".into(),
                awaiting: None,
                error: None,
                usage: Default::default(),
                iterations: 1,
                frame_status: FrameStatus::Completed,
                pending_ask: None,
                plan: None,
            })
            .await
            .expect("queue terminal event");

        // Simulates a worker abort/panic: FinishGuard runs, but no code ever
        // marks the run Committed or Failed.
        control.finish();
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(1), sse_rx.recv())
            .await
            .expect("relay timeout")
            .expect("terminal event");
        assert!(matches!(terminal, AgentEvent::Error { .. }));
        relay.await.expect("relay task");
    }

    #[tokio::test]
    async fn missing_agent_terminal_with_pending_persistence_emits_error() {
        let (control, cancel_rx) = ActiveRunControl::new();
        let (agent_tx, agent_rx) = tokio::sync::mpsc::channel(1);
        let (sse_tx, mut sse_rx) = tokio::sync::mpsc::channel(1);
        let relay = tokio::spawn(relay_agent_events(
            agent_rx,
            sse_tx,
            cancel_rx,
            control.clone(),
        ));
        drop(agent_tx);
        control.finish();
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), sse_rx.recv())
            .await
            .expect("relay timeout")
            .expect("error event");
        assert!(matches!(event, AgentEvent::Error { .. }));
        relay.await.expect("relay task");
    }

    #[test]
    fn persistence_cursor_drift_is_rejected() {
        let session = dss_agent::Session::new("session", std::path::PathBuf::from("/tmp/session"));
        let snapshot = super::DurableSessionSnapshot::capture(&session);
        let active = ActiveSession::new(session, 1);
        assert_eq!(
            super::validated_persistence_cursor(&active, &snapshot),
            Err((1, 0))
        );
    }

    #[tokio::test]
    async fn checkpoint_worker_atomically_persists_run_messages_plan_and_pending_ask() {
        let test_dir = TestDir::new("tool-checkpoint-state");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");
        let sid = "checkpoint-state".to_string();
        let workspace = test_dir.path().join("workspaces").join(&sid);
        std::fs::create_dir_all(&workspace).unwrap();
        crate::db::create_session_row(
            &state.db,
            sid.clone(),
            workspace.display().to_string(),
            None,
            None,
        )
        .await
        .unwrap();
        let active = Arc::new(ActiveSession::new(Session::new(sid.clone(), workspace), 0));
        let durable = Arc::new(Mutex::new(None));
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let worker = tokio::spawn(super::persist_history_checkpoints(
            rx,
            super::CheckpointWorkerContext {
                db: state.db.clone(),
                session_id: sid.clone(),
                run_id: "run-checkpoint-state".into(),
                plan_mode: true,
                started_at: "2026-08-05T10:00:00.000Z".into(),
                active: active.clone(),
                durable_checkpoint: durable.clone(),
                initial_count: 0,
                initial_title: "checkpoint title".into(),
            },
        ));
        let plan = plan(false, &["pending"]);
        let ask = PendingAsk {
            question: "Continue?".into(),
            options: Vec::new(),
            header: Some("Gate".into()),
        };
        let (ack, ack_rx) = tokio::sync::oneshot::channel();
        tx.send(HistoryCheckpoint {
            messages: vec![
                ChatMessage::user("delegate"),
                ChatMessage::tool(
                    "a2a-call",
                    "remote result",
                    Some("a2a_agent_nuclear".into()),
                ),
            ],
            frame_id: "frame-checkpoint-state".into(),
            parent_frame_id: Some("frame-parent".into()),
            root_frame_id: Some("frame-root".into()),
            agent_name: "MAIN".into(),
            task_summary: "delegate".into(),
            plan: Some(plan.clone()),
            pending_ask: Some(ask),
            status: "awaiting_user_response".into(),
            awaiting: Some("user_response".into()),
            ack,
        })
        .await
        .unwrap();
        ack_rx.await.unwrap().expect("durable checkpoint ack");
        drop(tx);
        worker.await.unwrap();

        let messages = crate::db::list_messages(&state.db, sid.clone())
            .await
            .unwrap();
        assert_eq!(messages.len(), 2);
        assert!(messages
            .iter()
            .all(|row| row.run_id.as_deref() == Some("run-checkpoint-state")));
        let runs = crate::db::list_runs(&state.db, sid.clone()).await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, "awaiting_user_response");
        assert_eq!(
            runs[0]
                .pending_ask_json
                .as_deref()
                .map(|json| json.contains("Continue?")),
            Some(true)
        );
        assert_eq!(
            runs[0]
                .plan_data
                .as_deref()
                .map(|json| json.contains("pending")),
            Some(true)
        );
        let session = crate::db::get_session_row(&state.db, sid)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.status, "awaiting_user_response");
        assert_eq!(
            active
                .persisted_count
                .load(std::sync::atomic::Ordering::Relaxed),
            2
        );
        let snapshot = durable
            .lock()
            .unwrap()
            .clone()
            .expect("checkpoint snapshot");
        assert_eq!(snapshot.messages_len, 2);
        assert_eq!(snapshot.frame_status, FrameStatus::AwaitingUserResponse);
        let snapshot_plan = snapshot.plan.expect("checkpoint plan");
        assert!(!snapshot_plan.approved);
        assert_eq!(snapshot_plan.steps[0].status, plan.steps[0].status);
    }

    #[test]
    fn terminal_failure_restores_the_last_checkpoint_even_from_a_completed_base_frame() {
        let mut session = Session::new("rollback-checkpoint", PathBuf::from("/tmp"));
        session.messages.push(ChatMessage::user("old"));
        session.frame.status = FrameStatus::Completed;
        let run_start = super::DurableSessionSnapshot::capture(&session);

        session.frame.begin_run("new run");
        let checkpoint_frame_id = session.frame.id.clone();
        session.messages.push(ChatMessage::user("delegate"));
        session.messages.push(ChatMessage::tool(
            "a2a-call",
            "remote result",
            Some("a2a_agent_nuclear".into()),
        ));
        session
            .messages
            .push(ChatMessage::assistant("uncommitted final"));
        session.plan = Some(plan(false, &["pending"]));

        let active = ActiveSession::new(
            Session::new("cursor", PathBuf::from("/tmp")),
            run_start.messages_len + 2,
        );
        let durable_checkpoint = Mutex::new(Some(super::DurableToolCheckpointSnapshot {
            messages_len: run_start.messages_len + 2,
            frame_id: checkpoint_frame_id.clone(),
            parent_frame_id: session.frame.parent_frame_id.clone(),
            root_frame_id: session.frame.root_frame_id.clone(),
            agent_name: session.frame.agent_name.clone(),
            frame_status: FrameStatus::Processing,
            task_summary: "new run".into(),
            plan: session.plan.clone(),
        }));
        let (control, _cancel_rx) = ActiveRunControl::new();

        super::rollback_failed_run(
            &mut session,
            &run_start,
            &active,
            &durable_checkpoint,
            control.as_ref(),
        );

        assert_eq!(session.messages.len(), run_start.messages_len + 2);
        assert_eq!(session.frame.id, checkpoint_frame_id);
        assert_eq!(session.frame.status, FrameStatus::Processing);
        assert_eq!(session.frame.task_summary, "new run");
        assert_eq!(session.plan.as_ref().unwrap().steps[0].status, "pending");
    }

    #[tokio::test]
    async fn persistence_fault_rolls_back_memory_and_marks_terminal_error() {
        let test_dir = TestDir::new("run-persistence-fault");
        let state = crate::state::build_state(Settings {
            data_dir: test_dir.path().to_path_buf(),
            data_dir_is_default: false,
            max_iterations: dss_core::DEFAULT_MAX_ITERATIONS,
            thinking: dss_core::ThinkingSettings::default(),
            server: ServerSettings::default(),
            llm: LlmSettings::default(),
            providers: Vec::new(),
            llm_env_overrides: LlmEnvOverrides::default(),
            log_level: None,
            mcp_servers: Vec::new(),
            a2a_agents: Vec::new(),
            memory: dss_core::settings::MemorySettings::default(),
            log: dss_core::settings::LogSettings::default(),
            api_keys: std::collections::HashMap::new(),
        })
        .await
        .expect("build test application state");
        let sid = create_session(State(state.clone()), None)
            .await
            .expect("create session")
            .0
            .id;
        let active = state
            .sessions
            .lock()
            .await
            .get(&sid)
            .cloned()
            .expect("active session");

        let connection = state.db.get().await.expect("database connection");
        connection
            .interact(|conn| {
                conn.execute_batch(
                    r#"
                    CREATE TRIGGER reject_test_run_terminal
                    BEFORE UPDATE ON sessions
                    BEGIN
                        SELECT RAISE(ABORT, 'forced persistence failure');
                    END;
                    "#,
                )
            })
            .await
            .expect("database interaction")
            .expect("install failure trigger");

        let (control, _cancel_rx) = ActiveRunControl::new();
        let mut session = active.session.lock().await;
        let durable_snapshot = super::DurableSessionSnapshot::capture(&session);
        session.frame.task_summary = "uncommitted run".into();
        session.frame.set_status(FrameStatus::Completed);
        session
            .messages
            .push(dss_llm::ChatMessage::user("not durable"));
        session.gate_state.retrieval_streak = 6;
        session.plan = Some(plan(false, &["pending"]));

        let result = crate::db::persist_run(
            &state.db,
            crate::db::PersistRunRequest {
                run_id: "run-fault".into(),
                session_id: sid.clone(),
                frame_id: session.frame.id.clone(),
                task_summary: session.frame.task_summary.clone(),
                plan_mode: true,
                status: "completed".into(),
                kind: Some("natural".into()),
                awaiting: None,
                pending_ask_json: None,
                error: None,
                input_tokens: 1,
                output_tokens: 1,
                iterations: 1,
                plan_data: None,
                title: Some("uncommitted run".into()),
                started_at: "2026-08-04T10:00:00.000Z".into(),
                checkpoint_start_seq: None,
                messages: vec![crate::db::PersistMessage {
                    role: "user".into(),
                    content: r#"{"role":"user","content":"not durable"}"#.into(),
                    harness_notice: false,
                }],
            },
        )
        .await;
        let error = super::finalize_run_persistence(
            result,
            active.as_ref(),
            &mut session,
            &durable_snapshot,
            &std::sync::Mutex::new(None),
            control.as_ref(),
        )
        .expect_err("trigger must abort the transaction");
        assert!(matches!(error, dss_db::DbError::Sqlite(_)));
        assert_eq!(session.messages.len(), durable_snapshot.messages_len);
        assert_eq!(session.frame.status, durable_snapshot.frame_status);
        assert_eq!(session.frame.task_summary, durable_snapshot.task_summary);
        assert_eq!(
            session.gate_state.retrieval_streak,
            durable_snapshot.gate_state.retrieval_streak
        );
        assert!(session.plan.is_none());
        assert_eq!(
            active
                .persisted_count
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(
            control.persistence_state(),
            crate::state::RunPersistenceState::Failed(super::PERSISTENCE_ERROR_MESSAGE.into())
        );
        drop(session);

        assert!(crate::db::list_messages(&state.db, sid.clone())
            .await
            .expect("list messages")
            .is_empty());
        assert!(crate::db::list_runs(&state.db, sid)
            .await
            .expect("list runs")
            .is_empty());
    }

    #[tokio::test]
    async fn acceptance_future_drop_releases_lock_before_finish_ack() {
        let session = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let (control, _cancel_rx) = ActiveRunControl::new();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn({
            let session = session.clone();
            let control = control.clone();
            async move {
                let _finish_guard = super::RunFinishGuard(control);
                let _session_guard = session.lock_owned().await;
                ready_tx.send(()).expect("acceptance guard ready");
                std::future::pending::<()>().await;
            }
        });
        ready_rx.await.expect("acceptance task reached await");

        task.abort();
        let _ = task.await;
        control.wait_finished().await;
        assert!(
            session.try_lock().is_ok(),
            "finish acknowledgement must follow mutex release"
        );
    }
}
