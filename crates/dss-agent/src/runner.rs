use dss_llm::{ChatMessage, ChatRequest, LlmClient, StreamEvent, ToolCall, Usage};
use dss_tools::{
    parse_arguments, PendingAsk, PendingToolCall, ToolContext, ToolRegistry, ToolRouter,
};
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::events::{AgentEvent, CompleteKind, ToolCallView, ToolResultView};
use crate::frame::FrameStatus;
use crate::session::Session;

/// dss_tools::ToolDef → dss_llm::ToolDef（两个 crate 各有一份同构定义，
/// 这里做一次值转换；不引跨 crate 类型依赖）。
fn to_llm_tool_defs(reg: &ToolRegistry) -> Vec<dss_llm::ToolDef> {
    reg.definitions()
        .into_iter()
        .map(|td| {
            dss_llm::ToolDef::function(
                td.function.name,
                td.function.description,
                td.function.parameters,
            )
        })
        .collect()
}

/// Runner 主循环的迭代上限（modules.md 的 `max_iterations` 软上限）。
pub const MAX_ITERATIONS: u32 = 25;
/// empty-retry 门：空响应重试上限（modules.md ≤3）。
pub const EMPTY_RETRY_CAP: u32 = 3;
/// 检索熔断：连续纯检索轮数阈值（modules.md ≥6）。
pub const RETRIEVAL_CIRCUIT_BREAKER: u32 = 6;
/// max_tokens 续传门终止档（modules.md ≥5）。
pub const LENGTH_FINISH_HARD_CAP: u32 = 5;
/// max_tokens 续传门缩减档（modules.md ≥3）。
pub const LENGTH_FINISH_TRIM_AT: u32 = 3;
/// plan denial 门：plan_mode 无 plan 的重试上限（modules.md ≤3）。
pub const PLAN_DENIAL_CAP: u32 = 3;
/// terminal barrier veto 上限（P6b verify：自然完成被 veto 后最多再修 1 轮，避免无限循环）。
pub const VETO_CAP: u32 = 1;

/// 检索/阅读类工具（用于检索熔断判断）。写/执行类不在此列。
fn is_retrieval_tool(name: &str) -> bool {
    matches!(
        name,
        "web_search"
            | "fetch_url"
            | "search_papers"
            | "fetch_paper"
            | "search_memory"
            | "read_memory"
            | "search_skills"
            | "list_skills"
            | "list_files"
            | "read_file"
    )
}

/// 构造一条 harness-notice 系统消息（内部调度提示，LLM 可见）。
fn harness_notice(text: &str) -> ChatMessage {
    let mut m = ChatMessage::system(text);
    m.harness_notice = true;
    m
}

/// 一次 run 的结果（事件已发出；outcome 供调用方记录）。
#[derive(Debug)]
pub struct RunOutcome {
    pub kind: CompleteKind,
    pub final_text: String,
    pub usage: Usage,
    pub iterations: u32,
}

/// Runner 主循环。P2：工具循环（tool_use → 执行 → 结果入历史 → 继续）。
pub struct Runner;

impl Runner {
    /// 完整 agent run：把 prompt 发给 LLM，多轮工具调用循环，结束发 complete。
    ///
    /// 取消语义（沿用 P1）：事件 channel 关闭（send 失败）即中止，frame 置 Cancelled。
    pub async fn run(
        session: &mut Session,
        llm: &dyn LlmClient,
        model: &str,
        prompt: &str,
        tools: &ToolRegistry,
        ctx: &ToolContext,
        max_iterations: u32,
        context_window: usize,
        memory: Option<&dss_memory::MemoryStore>,
        project_id: Option<&str>,
        plan_mode: bool,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> RunOutcome {
        session.frame.task_summary = truncate_chars(prompt, 80);

        if !send(
            tx,
            AgentEvent::Start {
                frame_id: session.frame.id.clone(),
                task_summary: session.frame.task_summary.clone(),
            },
        )
        .await
        {
            return cancel(session);
        }

        // —— 记忆召回：用户消息前注入相关记忆（作为 harness-notice system 消息）——
        if let Some(store) = memory {
            match dss_memory::recall(store, prompt, project_id, 5).await {
                Ok(memories) if !memories.is_empty() => {
                    let block = dss_memory::render_recall_block(&memories);
                    if !block.is_empty() {
                        session.messages.push(harness_notice(&block));
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "memory recall failed (continuing)"),
            }
        }

        session.messages.push(ChatMessage::user(prompt));

        let tool_defs: Vec<dss_llm::ToolDef> = to_llm_tool_defs(tools);
        let mut iterations = 0u32;
        let mut final_text = String::new();
        let mut usage = Usage::default();

        while iterations < max_iterations {
            iterations += 1;
            if !send(tx, AgentEvent::Iteration { n: iterations }).await {
                return cancel(session);
            }

            // —— Rolling Compact：每轮 LLM 前压缩（短对话不触发，行为不变）——
            let cw = context_window;
            let compact_outcome = dss_compact::maybe_compact(
                &session.messages,
                &mut session.compaction,
                llm,
                model,
                cw,
            )
            .await;
            if compact_outcome.folded {
                tracing::info!(
                    folds_added = compact_outcome.folds_added,
                    "rolling compact applied L1 fold(s)"
                );
            }
            // projection：给 LLM 的视图（fold 区间替换成 summary）+ microcompact（硬墙截 tool result）。
            let view = dss_compact::projection(&session.messages, &session.compaction);
            let view = dss_compact::microcompact::microcompact(&view);

            // —— 构建 LLM 请求（带工具定义；用 projection 视图）——
            let mut req = ChatRequest::new(model, view);
            if !tool_defs.is_empty() {
                req.tools = Some(tool_defs.clone());
                req.tool_choice = Some("auto".to_string());
            }

            let stream = match llm.chat_stream(req).await {
                Ok(s) => s,
                Err(e) => {
                    return fail(
                        session,
                        tx,
                        iterations,
                        final_text,
                        usage,
                        e.to_string(),
                        None,
                    )
                    .await;
                }
            };

            // —— 流式消费：累积 thinking / text / tool_calls(by index) + finish_reason ——
            let mut text_buf = String::new();
            let mut tool_acc: Vec<AccToolCall> = Vec::new();
            let mut finish_reason: Option<String> = None;

            futures::pin_mut!(stream);
            loop {
                match stream.next().await {
                    Some(Ok(StreamEvent::Thinking(t))) => {
                        if !send(tx, AgentEvent::Thinking { text: t }).await {
                            return cancel(session);
                        }
                    }
                    Some(Ok(StreamEvent::Text(t))) => {
                        text_buf.push_str(&t);
                        if !send(tx, AgentEvent::Text { text: t }).await {
                            return cancel(session);
                        }
                    }
                    Some(Ok(StreamEvent::ToolCallDelta(d))) => {
                        accumulate_tool_delta(&mut tool_acc, d);
                    }
                    Some(Ok(StreamEvent::Usage(u))) => usage = u,
                    Some(Ok(StreamEvent::Finish { reason })) => {
                        finish_reason = reason;
                    }
                    Some(Err(e)) => {
                        return fail(
                            session,
                            tx,
                            iterations,
                            final_text,
                            usage,
                            e.to_string(),
                            None,
                        )
                        .await;
                    }
                    None => break,
                }
            }

            // —— 决策门（顺序严格遵循 modules.md §4）——
            let finalized: Vec<ToolCall> = tool_acc
                .into_iter()
                .filter(|t| t.id.is_some() && t.name.is_some())
                .map(|t| {
                    ToolCall::function(
                        t.id.unwrap(),
                        t.name.unwrap(),
                        t.arguments.unwrap_or_default(),
                    )
                })
                .collect();

            // 门 1：max_tokens 续传（finish_reason == length）。
            // 三档：累计 ≥5 → 终止（MaxIters/Failed）；≥3 → 大幅缩减提示；否则分块继续。
            // 「续传」语义：本轮被截断，注入提示让模型在下一轮继续（分块输出）。
            let is_length = finish_reason.as_deref() == Some("length");
            if is_length {
                session.gate_state.length_finish_count += 1;
                let n = session.gate_state.length_finish_count;
                if n >= 5 {
                    // 终止：截断处即最终回复。
                    final_text = text_buf.clone();
                    if !final_text.is_empty() {
                        session.messages.push(ChatMessage::assistant(&final_text));
                    }
                    session.frame.set_status(FrameStatus::Completed);
                    let _ = send(
                        tx,
                        AgentEvent::Complete {
                            kind: CompleteKind::MaxIters,
                            final_text: final_text.clone(),
                            awaiting: None,
                            error: Some("reached max_tokens continuation cap (5)".into()),
                            usage,
                            iterations,
                            frame_status: session.frame.status,
                            pending_ask: None,
                            plan: None,
                        },
                    )
                    .await;
                    return RunOutcome {
                        kind: CompleteKind::MaxIters,
                        final_text,
                        usage,
                        iterations,
                    };
                }
                // 注入续传提示（n>=3 时要求大幅缩减；否则普通续传）。
                let notice = if n >= 3 {
                    "你的上一条回复因 max_tokens 被截断。请用**显著更短**的篇幅继续并完成，避免再次被截断。"
                } else {
                    "你的上一条回复因 max_tokens 被截断。请从中断处继续，完成剩余内容。"
                };
                if !text_buf.is_empty() {
                    session.messages.push(ChatMessage::assistant(&text_buf));
                }
                session.messages.push(harness_notice(notice));
                continue;
            } else {
                // 非 length：本轮正常结束，重置续传计数。
                session.gate_state.length_finish_count = 0;
            }

            if !finalized.is_empty() {
                // —— 工具路径 ——
                let assistant_msg = if text_buf.is_empty() {
                    ChatMessage::assistant_tool_calls(finalized.clone())
                } else {
                    // 带文字 + tool_calls 的 assistant 消息。
                    let mut m = ChatMessage::assistant_tool_calls(finalized.clone());
                    m.content = Some(text_buf.clone());
                    m
                };
                session.messages.push(assistant_msg);

                // 推 tool_calls 事件给前端。
                let views: Vec<ToolCallView> = finalized
                    .iter()
                    .map(|tc| ToolCallView {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        input: parse_arguments(&tc.function.arguments),
                    })
                    .collect();
                if !send(tx, AgentEvent::ToolCalls { calls: views }).await {
                    return cancel(session);
                }

                // —— 执行工具（并发 + 30s 超时）——
                let pending: Vec<PendingToolCall> = finalized
                    .iter()
                    .map(|tc| PendingToolCall {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        input: parse_arguments(&tc.function.arguments),
                    })
                    .collect();
                let results = ToolRouter::execute_tool_calls(tools, ctx, pending).await;

                // 推 tool_results 事件给前端。
                let result_views: Vec<ToolResultView> =
                    results.iter().cloned().map(ToolResultView::from).collect();
                if !send(
                    tx,
                    AgentEvent::ToolResults {
                        results: result_views,
                    },
                )
                .await
                {
                    return cancel(session);
                }

                // —— 结果入历史（role=tool，配对 tool_call_id）——
                for r in &results {
                    session.messages.push(ChatMessage::tool(
                        &r.tool_use_id,
                        &r.content,
                        // tool 名：从 finalized 里按 id 反查（便于日志，OpenAI 非必需）。
                        finalized
                            .iter()
                            .find(|tc| tc.id == r.tool_use_id)
                            .map(|tc| tc.function.name.clone()),
                    ));
                }

                // 累计 usage 文字（后续多轮的 text 保留为最终回复）。
                if !text_buf.is_empty() {
                    final_text = text_buf;
                }

                // —— ask_user 检测：挂起则转 AwaitingUserResponse 退出 ——
                let pending_ask_guard = ctx.pending_ask.lock().await;
                if let Some(ask) = pending_ask_guard.clone() {
                    drop(pending_ask_guard);
                    // 清空挂起（下次 run 会重新挂）。
                    *ctx.pending_ask.lock().await = None;
                    session.frame.set_status(FrameStatus::AwaitingUserResponse);

                    let event = AgentEvent::Complete {
                        kind: CompleteKind::Awaiting,
                        final_text: final_text.clone(),
                        awaiting: Some("user_response".to_string()),
                        error: None,
                        usage,
                        iterations,
                        frame_status: session.frame.status,
                        pending_ask: Some(ask),
                        plan: None,
                    };
                    let _ = send(tx, event).await;
                    return RunOutcome {
                        kind: CompleteKind::Awaiting,
                        final_text,
                        usage,
                        iterations,
                    };
                }
                drop(pending_ask_guard);

                // —— plan 检测：plan_mode 且 ctx.plan 有未批准 plan → 转 AwaitingPlanApproval ——
                if plan_mode {
                    let plan_guard = ctx.plan.lock().await;
                    if let Some(plan) = plan_guard.clone() {
                        if !plan.approved {
                            drop(plan_guard);
                            // 推 plan_update 事件给前端。
                            let _ = send(tx, AgentEvent::PlanUpdate { plan: plan.clone() }).await;
                            session.frame.set_status(FrameStatus::AwaitingPlanApproval);
                            let _ = send(
                                tx,
                                AgentEvent::Complete {
                                    kind: CompleteKind::Awaiting,
                                    final_text: final_text.clone(),
                                    awaiting: Some("plan_approval".to_string()),
                                    error: None,
                                    usage,
                                    iterations,
                                    frame_status: session.frame.status,
                                    pending_ask: None,
                                    plan: Some(plan),
                                },
                            )
                            .await;
                            return RunOutcome {
                                kind: CompleteKind::Awaiting,
                                final_text,
                                usage,
                                iterations,
                            };
                        }
                    }
                }

                // —— 检索熔断（modules.md：连续纯检索 ≥6 轮强制写作）——
                let all_retrieval = finalized
                    .iter()
                    .all(|tc| is_retrieval_tool(&tc.function.name));
                if all_retrieval {
                    session.gate_state.retrieval_streak += 1;
                } else {
                    session.gate_state.retrieval_streak = 0;
                }
                if session.gate_state.retrieval_streak >= RETRIEVAL_CIRCUIT_BREAKER {
                    session.messages.push(harness_notice(
                    "你已经连续多轮只做检索/阅读，没有产出实际内容。请停止搜索，基于已获取的信息开始写作/回答。",
                ));
                    session.gate_state.retrieval_streak = 0;
                }

                // 否则继续下一轮 LLM。
                debug!(iteration = iterations, "tool loop continues");
            } else {
                // —— 无 tool_use 且非 length：natural completion + empty-retry 门 ——
                if text_buf.trim().is_empty() {
                    // 空响应（thinking-only 也算空，因 thinking 不进 text_buf）。
                    session.gate_state.empty_retry_count += 1;
                    if session.gate_state.empty_retry_count > EMPTY_RETRY_CAP {
                        return fail(
                            session,
                            tx,
                            iterations,
                            final_text,
                            usage,
                            "empty response retry cap exceeded (3)".to_string(),
                            None,
                        )
                        .await;
                    }
                    session.messages.push(harness_notice(
                        "你的上一条回复为空。请基于上下文给出实际回复；若任务已完成请明确说明。",
                    ));
                    continue;
                }
                // 有内容：clean completion（重置 empty_retry）。
                session.gate_state.empty_retry_count = 0;

                // —— plan denial 门：plan_mode 但未生成 plan → ≤3 次提示重生成，超限 Failed ——
                if plan_mode {
                    let has_plan = ctx.plan.lock().await.is_some();
                    if !has_plan {
                        session.gate_state.plan_denial_count += 1;
                        if session.gate_state.plan_denial_count > PLAN_DENIAL_CAP {
                            return fail(
                                session,
                                tx,
                                iterations,
                                final_text,
                                usage,
                                "plan mode requires a plan (denial cap exceeded)".to_string(),
                                None,
                            )
                            .await;
                        }
                        session
                            .messages
                            .push(ChatMessage::assistant(text_buf.clone().as_str()));
                        session.messages.push(harness_notice(
                            "你处于 plan 模式但还没生成计划。请调用 generate_plan 工具给出步骤计划，再继续。",
                        ));
                        continue;
                    }
                }

                final_text = text_buf.clone();

                // —— terminal barrier（P6b verify）：自然完成时 review；veto 则再修一轮（≤1 次）——
                if session.gate_state.veto_count < VETO_CAP {
                    if let Some(verdict) =
                        dss_verify::terminal_barrier(llm, model, prompt, &final_text).await
                    {
                        if !verdict.pass && !verdict.findings.is_empty() {
                            session.gate_state.veto_count += 1;
                            tracing::info!(findings = ?verdict.findings, "terminal barrier veto");
                            session.messages.push(ChatMessage::assistant(&final_text));
                            let notice = format!(
                                "reviewer 发现以下问题，请修复后重新给出最终回复：\n{}",
                                verdict
                                    .findings
                                    .iter()
                                    .map(|f| format!("- {f}"))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            );
                            session.messages.push(harness_notice(&notice));
                            continue;
                        }
                    }
                }

                session.messages.push(ChatMessage::assistant(&final_text));
                session.frame.set_status(FrameStatus::Completed);

                let _ = send(
                    tx,
                    AgentEvent::Complete {
                        kind: CompleteKind::Natural,
                        final_text: final_text.clone(),
                        awaiting: None,
                        error: None,
                        usage,
                        iterations,
                        frame_status: session.frame.status,
                        pending_ask: None,
                        plan: None,
                    },
                )
                .await;
                return RunOutcome {
                    kind: CompleteKind::Natural,
                    final_text,
                    usage,
                    iterations,
                };
            }
        }

        // —— 循环耗尽 ——
        warn!(iterations, "agent hit max_iterations");
        session.frame.set_status(FrameStatus::Completed);
        let event = AgentEvent::Complete {
            kind: CompleteKind::MaxIters,
            final_text: final_text.clone(),
            awaiting: None,
            error: Some(format!("reached max iterations ({max_iterations})")),
            usage,
            iterations,
            frame_status: session.frame.status,
            pending_ask: None,
            plan: None,
        };
        let _ = send(tx, event).await;
        RunOutcome {
            kind: CompleteKind::MaxIters,
            final_text,
            usage,
            iterations,
        }
    }
}

/// 累积中的工具调用（按 index）。
#[derive(Debug, Default, Clone)]
struct AccToolCall {
    index: u32,
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}

fn accumulate_tool_delta(acc: &mut Vec<AccToolCall>, d: dss_llm::ToolCallDelta) {
    // 找到对应 index 的累积槽（保持有序）。
    let pos = acc.iter().position(|t| t.index == d.index);
    let slot = match pos {
        Some(i) => &mut acc[i],
        None => {
            acc.push(AccToolCall {
                index: d.index,
                ..Default::default()
            });
            acc.last_mut().expect("just pushed")
        }
    };
    if d.id.is_some() {
        slot.id = d.id;
    }
    if d.name.is_some() {
        slot.name = d.name;
    }
    if let Some(args) = d.arguments {
        match &mut slot.arguments {
            Some(existing) => existing.push_str(&args),
            None => slot.arguments = Some(args),
        }
    }
}

/// LLM 失败路径：frame Failed + complete kind=error。
#[allow(clippy::too_many_arguments)]
async fn fail(
    session: &mut Session,
    tx: &mpsc::Sender<AgentEvent>,
    iterations: u32,
    final_text: String,
    usage: Usage,
    message: String,
    pending_ask: Option<PendingAsk>,
) -> RunOutcome {
    session.frame.set_status(FrameStatus::Failed);
    let _ = tx
        .send(AgentEvent::Complete {
            kind: CompleteKind::Error,
            final_text: final_text.clone(),
            awaiting: None,
            error: Some(message),
            usage,
            iterations,
            frame_status: session.frame.status,
            pending_ask,
            plan: None,
        })
        .await;
    RunOutcome {
        kind: CompleteKind::Error,
        final_text,
        usage,
        iterations,
    }
}

/// 客户端断开：中止 run（complete 已无处可发）。
fn cancel(session: &mut Session) -> RunOutcome {
    session.frame.set_status(FrameStatus::Cancelled);
    RunOutcome {
        kind: CompleteKind::Cancelled,
        final_text: String::new(),
        usage: Usage::default(),
        iterations: 0,
    }
}

async fn send(tx: &mpsc::Sender<AgentEvent>, event: AgentEvent) -> bool {
    tx.send(event).await.is_ok()
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}
