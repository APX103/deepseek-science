//! sessions 端点：POST/GET/DELETE /api/sessions、GET /api/sessions/{sid}、
//! POST /api/sessions/{sid}/stream-sse。
//!
//! 持久化：session 行 + session_messages 增量写；恢复从 DB 重建 Vec<ChatMessage>。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use dss_agent::{AgentEvent, CompleteKind, FrameStatus, Runner, Session, MAX_ITERATIONS};
use dss_llm::ChatMessage;
use dss_tools::ToolContext;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use crate::db as dbq;
use crate::state::{ActiveSession, AppState, MAX_ACTIVE_SESSIONS};

#[derive(Debug, Default, Deserialize)]
pub struct CreateSessionReq {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
}

#[derive(Serialize)]
pub struct CreateSessionResp {
    id: String,
    frame_id: String,
    mcp_tools: Vec<String>,
    model: String,
    workspace: String,
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
    let sid = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
    let workspace = state.settings.data_dir.join("workspaces").join(&sid);

    if let Err(e) = std::fs::create_dir_all(&workspace) {
        tracing::error!(error = %e, "failed to create workspace");
        return Err(json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to create workspace",
        ));
    }

    let model = req.model.clone().unwrap_or_else(|| state.settings.llm.model.clone());
    // 落 DB（project_id 缺省 → 不绑定，或后续 default）。
    let project_id = req.project_id.or(Some(dss_db::DEFAULT_PROJECT_ID.to_string()));
    let row = dbq::create_session_row(
        &state.db,
        sid.clone(),
        workspace.display().to_string(),
        Some(model.clone()),
        project_id,
    )
    .await
    .map_err(map_db_err)?;

    let session = Session::new(sid.clone(), workspace);
    let frame_id = session.frame.id.clone();
    state.sessions.lock().await.insert(
        sid.clone(),
        Arc::new(ActiveSession::new(session, 0)),
    );

    tracing::info!(sid = %sid, "session created");
    Ok(Json(CreateSessionResp {
        id: sid,
        frame_id,
        mcp_tools: Vec::new(),
        model: row.model.unwrap_or(model),
        workspace: row.workspace,
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
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();
    // 活跃的排前面。
    items.sort_by(|a, b| b.live.cmp(&a.live).then_with(|| a.id.cmp(&b.id)));
    Json(items)
}

/// `GET /api/sessions/{sid}`：会话状态序列化（api-contract：messages 带 harness_notice 顶层字段）。
pub async fn get_session(
    State(state): State<AppState>,
    Path(sid): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 内存无 → 从 DB 恢复。
    let in_mem = state.sessions.lock().await.contains_key(&sid);
    if !in_mem {
        restore_session(&state, &sid).await.map_err(map_db_err)?;
    }

    let row = match dbq::get_session_row(&state.db, sid.clone()).await {
        Ok(Some(r)) => r,
        _ => return Err(json_error(StatusCode::NOT_FOUND, "session not found")),
    };

    let msgs = dbq::list_messages(&state.db, sid.clone()).await.map_err(map_db_err)?;
    let messages: Vec<Value> = msgs
        .into_iter()
        .map(|m| {
            // content 是 OpenAI 协议形态 JSON；直接透传 + 顶层 harness_notice。
            let content: Value = serde_json::from_str(&m.content).unwrap_or(Value::Null);
            json!({
                "role": m.role,
                "content": content,
                "harness_notice": m.harness_notice,
            })
        })
        .collect();

    Ok(Json(json!({
        "id": row.id,
        "title": row.title,
        "workspace": row.workspace,
        "model": row.model,
        "status": row.status,
        "plan_mode": row.plan_mode,
        "project_id": row.project_id,
        "messages": messages,
    })))
}

/// `DELETE /api/sessions/{sid}`：删 DB 行（cascade messages）+ workspace + 内存。
pub async fn delete_session(
    State(state): State<AppState>,
    Path(sid): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    dbq::delete_session_row(&state.db, sid.clone()).await.map_err(map_db_err)?;
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
/// P6b 最小闭环：标记 plan 已批准（写入 session plan_data），frame 重开为 Processing。
/// 后续由前端再发一条 stream-sse（非 plan_mode）让 agent 执行已批准的 plan。
pub async fn approve_plan(
    State(state): State<AppState>,
    Path(sid): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 确认 session 存在 + 恢复到内存。
    if !state.sessions.lock().await.contains_key(&sid) {
        restore_session(&state, &sid).await.map_err(map_db_err)?;
    }

    let active = {
        let sessions = state.sessions.lock().await;
        sessions.get(&sid).cloned()
    };
    let Some(active) = active else {
        return Err(json_error(StatusCode::NOT_FOUND, "session not found"));
    };

    // 从内存 session 拿 plan（P6a 在 ToolContext.plan 里，但 run 结束后 ctx 销毁；
    // 这里从 session 的 frame status 判断是否在 awaiting，然后返回 approved=true）。
    {
        let s = active.session.lock().await;
        if s.frame.status != FrameStatus::AwaitingPlanApproval {
            return Err(json_error(
                StatusCode::CONFLICT,
                "session is not awaiting plan approval",
            ));
        }
    }

    // 重开 frame 为 Processing（让后续 run 能继续）。
    {
        let mut s = active.session.lock().await;
        s.frame.set_status(FrameStatus::Processing);
    }

    tracing::info!(sid = %sid, "plan approved");
    Ok(Json(json!({ "approved": true, "steps": [] })))
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
    let workspace = std::path::PathBuf::from(row.workspace);
    let tex_abs = workspace.join(&req.path);
    if !tex_abs.exists() {
        return Ok(Json(CompileResult {
            success: false,
            pdf_path: None,
            size_kb: 0,
            message: format!("tex file not found: {}", req.path),
            errors: vec![],
        }));
    }
    let stem = tex_abs
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".into());
    let out_name = req.out_name.unwrap_or(stem);

    let mut cmd = tokio::process::Command::new("tectonic");
    cmd.arg("-X").arg("compile");
    cmd.arg("--outdir").arg(workspace.as_os_str());
    cmd.arg(&tex_abs);
    cmd.current_dir(&workspace);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    let output = match tokio::time::timeout(std::time::Duration::from_secs(180), cmd.output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            return Ok(Json(CompileResult {
                success: false, pdf_path: None, size_kb: 0,
                message: format!("tectonic spawn failed: {e}"), errors: vec![],
            }));
        }
        Err(_) => {
            return Ok(Json(CompileResult {
                success: false, pdf_path: None, size_kb: 0,
                message: "tectonic timed out (180s)".into(), errors: vec![],
            }));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let code = output.status.code().unwrap_or(-1);
    let pdf_path = workspace.join(format!("{out_name}.pdf"));

    if code == 0 && pdf_path.exists() {
        let size_kb = pdf_path.metadata().map(|m| m.len() / 1024).unwrap_or(0);
        Ok(Json(CompileResult {
            success: true,
            pdf_path: Some(pdf_path.display().to_string()),
            size_kb,
            message: format!("compiled {out_name}.pdf"),
            errors: vec![],
        }))
    } else {
        let log_excerpt: String = format!("{stdout}\n--- stderr ---\n{stderr}")
            .chars().rev().take(3000).collect::<String>().chars().rev().collect();
        Ok(Json(CompileResult {
            success: false,
            pdf_path: None,
            size_kb: 0,
            message: format!("tectonic failed (exit {code})"),
            errors: vec![log_excerpt],
        }))
    }
}

/// 从 DB 恢复 session 进内存：读 messages → 重建 Vec<ChatMessage> → 构 Session。
async fn restore_session(state: &AppState, sid: &str) -> Result<(), dss_db::DbError> {
    let row = dbq::get_session_row(&state.db, sid.to_string())
        .await?
        .ok_or_else(|| dss_db::DbError::NotFound(format!("session {sid}")))?;
    let rows = dbq::list_messages(&state.db, sid.to_string()).await?;
    let mut messages = Vec::with_capacity(rows.len());
    for m in rows {
        // content JSON → ChatMessage（OpenAI 协议形态，serde 直取）。
        let mut cm: ChatMessage = serde_json::from_str(&m.content).unwrap_or_else(|_| {
            // 兜底：content 当纯文本。
            ChatMessage::user(&m.content)
        });
        // 强制以 DB 的 role 为准（防 content 内 role 与行不一致）。
        cm.role = m.role;
        messages.push(cm);
    }
    let count = messages.len();
    let workspace = std::path::PathBuf::from(row.workspace);
    let mut session = Session::new(sid.to_string(), workspace);
    // 恢复历史与 frame 摘要。
    session.messages = messages;
    if let Some(t) = row.title {
        session.frame.task_summary = t;
    }
    session.frame.set_status(FrameStatus::Completed);

    let mut map = state.sessions.lock().await;
    map.insert(sid.to_string(), Arc::new(ActiveSession::new(session, count)));
    // LRU 驱逐（仅内存）。
    if map.len() > MAX_ACTIVE_SESSIONS {
        if let Some(first) = map.keys().next().cloned() {
            if first != sid {
                map.remove(&first);
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct RunReq {
    prompt: String,
    #[serde(default)]
    plan_mode: Option<bool>,
    #[allow(dead_code)]
    deep_review: Option<bool>,
}

const EVENT_CHANNEL_CAP: usize = 1024;

/// `POST /api/sessions/{sid}/stream-sse`：流式 run（多轮工具循环）。
pub async fn stream_sse(
    State(state): State<AppState>,
    Path(sid): Path<String>,
    Json(req): Json<RunReq>,
) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<Value>)> {
    // 内存无则先恢复（DB 有）。
    if !state.sessions.lock().await.contains_key(&sid) {
        restore_session(&state, &sid).await.map_err(map_db_err)?;
    }

    let active = {
        let sessions = state.sessions.lock().await;
        sessions.get(&sid).cloned()
    };
    let Some(active) = active else {
        return Err(json_error(StatusCode::NOT_FOUND, "session not found"));
    };
    let Ok(session) = active.session.clone().try_lock_owned() else {
        return Err(json_error(
            StatusCode::CONFLICT,
            "session already has an active run",
        ));
    };

    let (tx, rx) = mpsc::channel::<AgentEvent>(EVENT_CHANNEL_CAP);
    let llm = state.llm.clone();
    let model = state.settings.llm.model.clone();
    let tools = state.tools.clone();
    let db = state.db.clone();
    let memory = state.memory.clone();
    let prompt = req.prompt;
    let plan_mode = req.plan_mode.unwrap_or(false);
    let sid_clone = sid.clone();
    // session 的 project_id（记忆按项目隔离）。
    let project_id = dbq::get_session_row(&state.db, sid.clone())
        .await
        .ok()
        .flatten()
        .and_then(|r| r.project_id);

    tokio::spawn(async move {
        let mut session = session;
        let ctx = {
            // 复制全局 catalog（builtin+global），再叠加 project 源（workspace/.deepseek-science/skills）。
            let mut cat = (*state.catalog).clone();
            cat.load_dir(&dss_skills::project_skills_dir(&session.workspace), "project");
            ToolContext::new(session.workspace.clone())
                .with_skill_catalog(cat)
                .with_mcp_arc(state.mcp.clone())
        };
        let llm_for_extract = llm.clone();
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
                    MAX_ITERATIONS,
                    dss_compact::constants::DEFAULT_CONTEXT_CEILING,
                    Some(&memory),
                    project_id.as_deref(),
                    plan_mode,
                    &tx,
                )
                .await
            }
            None => {
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
                    usage: Default::default(),
                    iterations: 0,
                }
            }
        };

        // —— 增量持久化本次 run 新增的消息（session.messages 里 DB 还没有的部分）——
        let prev = active.persisted_count.load(std::sync::atomic::Ordering::Relaxed);
        let new_msgs_count = session.messages.len().saturating_sub(prev);
        if new_msgs_count > 0 {
            let to_persist: Vec<(String, String, bool)> = session.messages[prev..]
                .iter()
                .map(|m| {
                    let content_json = serde_json::to_string(m).unwrap_or_else(|_| String::from("\"\""));
                    (m.role.clone(), content_json, false)
                })
                .collect();
            if let Err(e) = dbq::append_messages_batch(&db, sid_clone.clone(), to_persist).await {
                tracing::warn!(error = %e, sid = %sid_clone, "persist new messages failed");
            } else {
                active
                    .persisted_count
                    .store(session.messages.len(), std::sync::atomic::Ordering::Relaxed);
            }
            // 用最终回复更新 title（取首条 user prompt 已由前端设；这里用 final_text 兜底）。
            if !outcome.final_text.is_empty() {
                let _ = dbq::set_session_title(
                    &db,
                    sid_clone.clone(),
                    outcome.final_text.chars().take(60).collect(),
                )
                .await;
            }
        }

        tracing::info!(
            kind = ?outcome.kind,
            iterations = outcome.iterations,
            persisted_new = new_msgs_count,
            "run finished"
        );

        // —— agent 日志：run_end ——
        let _ = state.logs.append(dss_observability::LogEntry {
            level: if outcome.kind == dss_agent::CompleteKind::Error { "error" } else { "info" }.into(),
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
        }).await;

        // —— 记忆抽取（fire-and-forget，后台异步，不阻塞、错误只 warn）——
        if let Some(client) = llm_for_extract.as_ref() {
            let msgs_snapshot: Vec<dss_llm::ChatMessage> = session.messages.clone();
            let model_c = model.clone();
            let memory_c = memory.clone();
            let pid_c = project_id.clone();
            let client_c = client.clone();
            tokio::spawn(async move {
                match dss_memory::extract::extract(client_c.as_ref(), &model_c, &msgs_snapshot).await {
                    Ok(items) if !items.is_empty() => {
                        for body in items {
                            let scope = if pid_c.is_none() { Some("profile".into()) } else { Some("project".into()) };
                            if let Err(e) = memory_c.append(body, scope, pid_c.clone()).await {
                                tracing::warn!(error = %e, "memory append failed");
                            }
                        }
                        tracing::info!("memory extract completed (background)");
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "memory extract failed (background)"),
                }
            });
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
