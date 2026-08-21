use std::collections::{BTreeMap, HashMap, HashSet};

use dss_llm::{ChatMessage, ChatRequest, LlmClient, StreamEvent, ToolCall, Usage};
use dss_tools::{
    parse_arguments, PendingAsk, PendingToolCall, ToolBatchPolicy, ToolContext, ToolRegistry,
    ToolResult, ToolRouter,
};
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::dsml::{
    DsmlError, IncrementalAssistantTextGuard, IncrementalAssistantTextResult,
    MAX_ASSISTANT_TEXT_BYTES, MAX_TOOL_ARGUMENT_BYTES_PER_CALL, MAX_TOOL_ARGUMENT_BYTES_TOTAL,
    MAX_TOOL_CALLS_PER_TURN, MAX_TOOL_CALL_ID_BYTES, MAX_TOOL_NAME_BYTES,
};
use crate::events::{AgentEvent, CompleteKind, ToolCallView, ToolResultView};
use crate::frame::FrameStatus;
use crate::session::Session;

/// dss_tools::ToolDef → dss_llm::ToolDef（两个 crate 各有一份同构定义，
/// 这里做一次值转换；不引跨 crate 类型依赖）。
fn to_llm_tool_defs(
    reg: &ToolRegistry,
    plan_mode: bool,
    plan_step_updates_allowed: bool,
) -> Vec<dss_llm::ToolDef> {
    reg.definitions()
        .into_iter()
        .filter(|td| is_tool_available(&td.function.name, plan_mode, plan_step_updates_allowed))
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
///
/// This remains the public compatibility name for the default. Production callers may pass a
/// persisted value up to `dss_core::MAX_CONFIGURABLE_ITERATIONS` to `Runner::run`.
pub const MAX_ITERATIONS: u32 = dss_core::DEFAULT_MAX_ITERATIONS;
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
/// Approved-plan execution may receive this many corrective terminal prompts before it returns
/// to the durable, retryable AwaitingPlanExecution state.
pub const PLAN_COMPLETION_RETRY_CAP: u32 = 3;
/// terminal barrier veto 上限（P6b verify：自然完成被 veto 后最多再修 1 轮，避免无限循环）。
pub const VETO_CAP: u32 = 1;
/// One text-only artifact-repair miss receives a deterministic reminder before
/// the run fails closed. The reminder does not consume a second reviewer call.
const ARTIFACT_REPAIR_REMINDER_CAP: u32 = 1;

const SCIENCE_EXECUTION_POLICY: &str = r#"[Deepseek Science execution policy]
Work as an evidence-first scientific research agent.
- Read the task constraints and the relevant local inputs before making quantitative claims. Preserve raw inputs and work only inside the authorized workspace.
- For confirmatory analyses, state hypotheses, metrics, decision rules, and multiplicity handling before inspecting outcomes. If a rule changes after results are visible, label it post-hoc/exploratory rather than preregistered.
- Start with the smallest cheap validation or a reduced test case before expensive computation. Estimate complexity, avoid brute force when an exact/analytic/vectorized alternative exists, and do not repeat a failed command without changing the diagnosis or method.
- Respect explicit user limits on network access, dependencies, time, samples, and agent iterations. Finish as soon as the requested evidence and artifacts are verified; do not add tools or scope merely because budget remains.
- After writing code or an artifact, run the smallest relevant check, then read back the decisive result. Keep inputs unchanged unless the user explicitly asks to edit them.
- Separate observation, association, and causation; quantify uncertainty and limitations. With finite resampling/permutation/Monte Carlo, zero exceedances is not a true p/FAP of zero: report the finite resolution and an appropriate corrected estimate or bound.
- Never claim to have read, run, validated, or temporally ordered a step unless the actual tool trace supports it.
"#;

const TOOL_ERROR_RECOVERY_NOTICE: &str = "本轮已经出现多次工具失败。先停止扩大脚本或重复同一命令：用最小复现定位根因，检查参数/数据形状/复杂度，再选择更小的验证或替代方法。保留已经验证的结果，并优先完成用户要求的最小可交付物。";

const CANCELLED_REQUEST_BOUNDARY: &str =
    "上一条用户请求已由用户取消。不要继续该请求，除非最新用户消息明确要求恢复；下一轮只处理最新的用户请求。";
const CANCELLED_TOOL_RESULT: &str =
    "工具调用已发布，但在结果返回界面前本轮被取消；没有持久化未发布的工具结果。";
const PLAN_DENIAL_NOTICE: &str =
    "你处于 plan 模式但还没生成计划。请调用 generate_plan 工具给出步骤计划，再继续；如确实缺少必要信息，可调用 ask_user。";
const ASK_SUPERSEDED_BY_PLAN_RESULT: &str = "[ask superseded] The generated plan is authoritative; no separate user response is pending. This run is awaiting plan approval.";
/// Native tool streams are allowed to fragment argument JSON down to individual
/// bytes. Bound only deltas that do not add any new accumulator state; useful
/// argument growth is already bounded by the per-call and per-turn byte caps.
const MAX_NATIVE_TOOL_NO_PROGRESS_DELTAS_PER_TURN: usize = 4096;

/// Fresh Plan mode is a capability boundary, not just an instruction. Keep
/// this exact-name allowlist default closed so newly registered tools cannot
/// become available before approval by accident.
fn is_preapproval_plan_tool(name: &str) -> bool {
    matches!(name, "generate_plan" | "ask_user")
}

/// An approved plan that still has work is the only state in which status
/// mutation is meaningful. Ordinary follow-up runs deliberately receive no
/// legacy plan context, so exposing this tool there only invites useless
/// `no plan` calls and needlessly consumes an iteration.
fn has_updatable_approved_plan(plan: Option<&dss_tools::PlanState>) -> bool {
    plan.is_some_and(|plan| plan.approved && plan.steps.iter().any(|step| step.status != "done"))
}

/// Schema filtering is advisory because providers may still emit an
/// undeclared call. Keep the exact same capability predicate at the execution
/// boundary so an inactive-plan update cannot reach the tool implementation.
fn is_tool_available(name: &str, plan_mode: bool, plan_step_updates_allowed: bool) -> bool {
    if plan_mode {
        return is_preapproval_plan_tool(name);
    }
    name != "update_step_status" || plan_step_updates_allowed
}

fn rejected_preapproval_results(calls: &[ToolCall]) -> Vec<ToolResult> {
    calls
        .iter()
        .map(|call| ToolResult {
            tool_use_id: call.id.clone(),
            content: format!(
                "tool `{}` was not executed: Plan approval is required before execution; only `generate_plan` and `ask_user` are available before approval",
                call.function.name
            ),
            is_error: true,
            outcome_unknown: false,
        })
        .collect()
}

fn rejected_inactive_plan_update_results(calls: &[ToolCall]) -> Vec<ToolResult> {
    calls
        .iter()
        .map(|call| ToolResult {
            tool_use_id: call.id.clone(),
            content: if call.function.name == "update_step_status" {
                "tool `update_step_status` was not executed: it is available only while an approved plan has unfinished steps".to_string()
            } else {
                format!(
                    "tool `{}` was not executed: its batch included `update_step_status`, which is available only while an approved plan has unfinished steps",
                    call.function.name
                )
            },
            is_error: true,
            outcome_unknown: false,
        })
        .collect()
}

/// Exclusive tools own a durability/side-effect boundary and must finish before another tool can
/// block the same assistant turn. Reject the whole batch before Router execution, preserving one
/// paired error for every model-declared call and deterministic next-turn recovery instructions.
fn rejected_exclusive_tool_batch_results(calls: &[ToolCall]) -> Vec<ToolResult> {
    calls
        .iter()
        .map(|call| ToolResult {
            tool_use_id: call.id.clone(),
            content: format!(
                "tool `{}` was not executed: an exclusive remote tool must be the only tool call in its model turn. On the next turn, call exactly one exclusive tool by itself; retry any other tools in separate later turns",
                call.function.name
            ),
            is_error: true,
            outcome_unknown: false,
        })
        .collect()
}

/// Count one unsuccessful planning iteration and decide whether it must fail
/// now. The fourth denial preserves the existing three-notice budget; a lower
/// hard iteration budget fails with the same Plan-specific diagnosis on its
/// final available attempt instead of falling through to generic max_iters.
fn record_plan_denial(
    session: &mut Session,
    iterations: u32,
    max_iterations: u32,
) -> Option<String> {
    session.gate_state.plan_denial_count += 1;
    if session.gate_state.plan_denial_count > PLAN_DENIAL_CAP {
        return Some("plan mode requires a plan (denial cap exceeded)".to_string());
    }
    if iterations >= max_iterations {
        return Some(format!(
            "plan mode requires a plan before the iteration budget is exhausted ({max_iterations})"
        ));
    }
    None
}

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
            | "mcp_list_resources"
            | "mcp_read_resource"
    )
}

/// Tools whose successful result changes the shared plan snapshot.
fn is_plan_mutation_tool(name: &str) -> bool {
    matches!(name, "generate_plan" | "update_step_status")
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
    pub awaiting: Option<String>,
    pub pending_ask: Option<PendingAsk>,
    pub error: Option<String>,
    pub usage: Usage,
    pub iterations: u32,
}

/// Evidence from the current model iteration that has crossed the UI delivery
/// boundary but has not yet entered canonical history.
///
/// Tool-call deltas are deliberately absent: a tool call becomes durable
/// evidence only after the complete `ToolCalls` event is accepted. Actual tool
/// results keep their existing, stricter commit point after `ToolResults` is
/// accepted.
#[derive(Default)]
struct PublishedTurn {
    thinking: String,
    text: String,
    tool_calls: Option<Vec<ToolCall>>,
}

impl PublishedTurn {
    fn is_empty(&self) -> bool {
        self.thinking.is_empty()
            && self.text.is_empty()
            && self.tool_calls.as_ref().is_none_or(Vec::is_empty)
    }

    fn into_message(self, usage: Usage) -> Option<ChatMessage> {
        if self.is_empty() {
            return None;
        }

        let mut message = if let Some(tool_calls) = self.tool_calls {
            let mut message = ChatMessage::assistant_tool_calls(tool_calls);
            if !self.text.is_empty() {
                message.content = Some(self.text);
            }
            message
        } else {
            // Keep an empty content string for thinking-only evidence. It is a
            // valid assistant history item and lets the restored UI render the
            // reasoning that was already shown before an error/cancellation.
            ChatMessage::assistant(self.text)
        };
        message.reasoning_content = nonempty(self.thinking);
        if usage.input_tokens != 0 || usage.output_tokens != 0 {
            message.usage = Some(usage);
        }
        Some(message)
    }
}

/// Per-run progress used by every terminal path. `usage` always includes the
/// latest snapshot from the current iteration plus all completed iterations.
#[derive(Default)]
struct RunProgress {
    iterations: u32,
    usage: Usage,
    iteration_usage: Usage,
    published: PublishedTurn,
}

impl RunProgress {
    fn begin_iteration(&mut self) {
        debug_assert!(
            self.published.is_empty(),
            "published evidence must be committed or hidden before the next iteration"
        );
        self.iterations += 1;
        self.iteration_usage = Usage::default();
    }

    fn record_usage(&mut self, next: Usage) {
        // Providers normally send one final usage snapshot, but replacing the
        // current contribution also handles multiple snapshots without double
        // counting them.
        self.usage.input_tokens = self
            .usage
            .input_tokens
            .saturating_sub(self.iteration_usage.input_tokens)
            .saturating_add(next.input_tokens);
        self.usage.output_tokens = self
            .usage
            .output_tokens
            .saturating_sub(self.iteration_usage.output_tokens)
            .saturating_add(next.output_tokens);
        // 前缀缓存命中/未命中是输入 token 的细分，按同一替换式语义累计。
        self.usage.cache_hit_tokens = self
            .usage
            .cache_hit_tokens
            .saturating_sub(self.iteration_usage.cache_hit_tokens)
            .saturating_add(next.cache_hit_tokens);
        self.usage.cache_miss_tokens = self
            .usage
            .cache_miss_tokens
            .saturating_sub(self.iteration_usage.cache_miss_tokens)
            .saturating_add(next.cache_miss_tokens);
        self.iteration_usage = next;
    }

    fn take_published_message(&mut self) -> Option<ChatMessage> {
        std::mem::take(&mut self.published).into_message(self.iteration_usage)
    }

    fn commit_published(&mut self, session: &mut Session) {
        if let Some(message) = self.take_published_message() {
            session.messages.push(message);
        }
    }

    /// Preserve a published tool-call batch without replaying any result or
    /// side effect that did not cross the UI boundary. Synthetic error results
    /// keep the OpenAI tool-call history well-formed for the next request.
    fn commit_cancelled(&mut self, session: &mut Session) {
        let tool_calls = self.published.tool_calls.clone().unwrap_or_default();
        self.commit_published(session);
        for call in tool_calls {
            let mut result =
                ChatMessage::tool(call.id, CANCELLED_TOOL_RESULT, Some(call.function.name));
            result.is_error = Some(true);
            session.messages.push(result);
        }
    }

    fn hide_published(&mut self, session: &mut Session) {
        if let Some(mut message) = self.take_published_message() {
            message.harness_notice = true;
            session.messages.push(message);
        }
    }
}

/// Runner 主循环。P2：工具循环（tool_use → 执行 → 结果入历史 → 继续）。
pub struct Runner;

impl Runner {
    /// 完整 agent run：把 prompt 发给 LLM，多轮工具调用循环，结束发 complete。
    ///
    /// 取消语义（沿用 P1）：事件 channel 关闭（send 失败）即中止，frame 置 Cancelled。
    #[allow(clippy::too_many_arguments)] // Explicit orchestration dependencies keep run state visible.
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
        run_context: &[ChatMessage],
        plan_mode: bool,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> RunOutcome {
        Self::run_inner(
            session,
            llm,
            model,
            prompt,
            tools,
            ctx,
            max_iterations,
            context_window,
            memory,
            project_id,
            run_context,
            plan_mode,
            tx,
            false,
        )
        .await
    }

    /// Continue a run whose frame and user prompt were durably accepted by the harness before
    /// the provider call. This is the API crash-recovery path; ordinary in-process callers use
    /// `run`, which still appends the prompt itself.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_accepted(
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
        run_context: &[ChatMessage],
        plan_mode: bool,
        tx: &mpsc::Sender<AgentEvent>,
    ) -> RunOutcome {
        Self::run_inner(
            session,
            llm,
            model,
            prompt,
            tools,
            ctx,
            max_iterations,
            context_window,
            memory,
            project_id,
            run_context,
            plan_mode,
            tx,
            true,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_inner(
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
        run_context: &[ChatMessage],
        plan_mode: bool,
        tx: &mpsc::Sender<AgentEvent>,
        prompt_already_appended: bool,
    ) -> RunOutcome {
        // Decision gates bound retries within one user request. Carrying their counters into
        // a later request would silently disable review or consume that request's retry budget.
        session.gate_state = Default::default();
        session.frame.start_run(truncate_chars(prompt, 80));

        if !send(
            tx,
            AgentEvent::Start {
                frame_id: session.frame.id.clone(),
                task_summary: session.frame.task_summary.clone(),
            },
        )
        .await
        {
            return cancel_before_prompt(session);
        }

        // Per-run system inputs are request context, never canonical history.
        // Keeping them outside `session.messages` is essential because rolling
        // compaction stores stable indexes into that append-only vector.
        let mut run_context = run_context.to_vec();
        // A stable execution contract keeps scientific behavior consistent across direct,
        // Plan, restored, and retried runs. It is request context rather than chat history,
        // so it cannot leak into the visible transcript or disturb compaction indexes.
        run_context.insert(0, harness_notice(SCIENCE_EXECUTION_POLICY));

        // —— 记忆召回：作为本轮 harness-notice 上下文，放在请求视图末尾 ——
        // 记忆块内容随查询（BM25 召回）变化。若插在 system 前缀与历史之间，会打断
        // DeepSeek 的前缀缓存单元（中间任何字节变化让其后全部 miss，包括整段历史）。
        // 放在最新用户消息之后，它只影响本就未命中的尾部，历史前缀保持稳定命中。
        let memory_block = if let Some(store) = memory {
            match dss_memory::recall(store, prompt, project_id, 5).await {
                Ok(memories) if !memories.is_empty() => {
                    let block = dss_memory::render_recall_block(&memories);
                    if block.is_empty() {
                        None
                    } else {
                        Some(harness_notice(&block))
                    }
                }
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!(error = %e, "memory recall failed (continuing)");
                    None
                }
            }
        } else {
            None
        };

        // The terminal reviewer receives only this request's canonical audit
        // trail, not unrelated earlier conversation. Tool-backed iterations
        // are committed in order before natural completion.
        let run_trace_start = if prompt_already_appended {
            debug_assert!(session.messages.last().is_some_and(|message| {
                message.role == "user" && message.content.as_deref() == Some(prompt)
            }));
            session.messages.len().saturating_sub(1)
        } else {
            let start = session.messages.len();
            session.messages.push(ChatMessage::user(prompt));
            start
        };
        let mut history_checkpoint_cursor = session.messages.len();

        let mut final_text = String::new();
        let mut progress = RunProgress::default();
        let initial_approved_plan = ctx.plan.lock().await.clone().filter(|plan| plan.approved);
        let enforce_plan_completion = initial_approved_plan.is_some();
        let mut plan_completion_retry_count = 0u32;
        let explicit_iteration_limit = std::iter::once(prompt)
            .chain(
                run_context
                    .iter()
                    .filter_map(|message| message.content.as_deref()),
            )
            .filter_map(parse_explicit_iteration_limit)
            .min();
        let (agent_iteration_limit, user_iteration_cap_active) =
            effective_iteration_limit(explicit_iteration_limit, max_iterations);
        let mut tool_error_count = 0u32;
        let mut last_veto_findings = Vec::new();
        let mut artifact_repair_trace_start = None;
        let mut artifact_repair_reminders = 0u32;

        while progress.iterations < agent_iteration_limit {
            progress.begin_iteration();
            let iterations = progress.iterations;
            let is_user_final_iteration =
                user_iteration_cap_active && iterations == agent_iteration_limit;
            if !send(tx, AgentEvent::Iteration { n: iterations }).await {
                return cancel(session, &mut progress);
            }

            if is_user_final_iteration {
                session.messages.push(harness_notice(&format!(
                    "这是用户明确限制的最后一轮（{iterations} 轮）。工具已关闭；不要再扩展范围，只能在本轮收束为最终回复。若无法完成，请明确报告阻塞与已验证证据。"
                )));
            }

            // Plan state may change after a successful update. Recompute the
            // capability projection every iteration so a completed plan no
            // longer advertises a stale status-mutation tool.
            let plan_step_updates_allowed =
                has_updatable_approved_plan(ctx.plan.lock().await.as_ref());
            // Use the same capability-filtered schema for both the model
            // request and hard-wall reservation. Otherwise hidden schemas can
            // either leak capabilities or skew the context budget.
            let tool_defs = to_llm_tool_defs(tools, plan_mode, plan_step_updates_allowed);
            let tool_schema_tokens = serde_json::to_string(&tool_defs)
                .map(|json| dss_compact::tokens::estimate_tokens(&json))
                .unwrap_or(0);
            let mut reserved_request_tokens =
                dss_compact::tokens::estimate_messages_tokens(&run_context)
                    .saturating_add(tool_schema_tokens);
            // 记忆块是本轮固定开销（挂视图末尾），计入压缩预算。
            if let Some(block) = &memory_block {
                reserved_request_tokens = reserved_request_tokens
                    .saturating_add(dss_compact::tokens::estimate_message_tokens(block));
            }

            // —— Rolling Compact：每轮 LLM 前压缩 ——
            // 缓存优先顺序：先对视图做免费 microcompact 减负（截长 tool result /
            // 已落盘写参数，无 LLM 调用），再判断是否仍超触发阈值；只有免费手段
            // 不够时才付费 summarize 折叠。这样「免费优先、付费兜底」，且免费减负
            // 到触发线以下时本轮完全不调 summarize。
            let cw = context_window;
            let hard_wall_tokens = dss_compact::chunk::hard_wall_tokens(cw);
            // 视图 = 稳定 system 前缀（run_context）+ 折叠投影历史 + 记忆块（可变尾部）。
            let build_view = |messages: &[ChatMessage], folded: &dss_compact::CompactionState| {
                let projected = dss_compact::projection(messages, folded);
                let mut view = Vec::with_capacity(run_context.len() + projected.len() + 1);
                view.extend(run_context.iter().cloned());
                view.extend(projected);
                if let Some(block) = &memory_block {
                    view.push(block.clone());
                }
                dss_compact::microcompact::microcompact(&view)
            };
            let free_view = build_view(&session.messages, &session.compaction);
            let free_view_tokens = dss_compact::tokens::estimate_messages_tokens(&free_view)
                .saturating_add(tool_schema_tokens);

            let compact_outcome = if dss_compact::chunk::is_over_trigger(free_view_tokens, cw) {
                tokio::select! {
                    biased;
                    _ = tx.closed() => return cancel(session, &mut progress),
                    outcome = dss_compact::maybe_compact_with_reserved_tokens(
                        &session.messages,
                        &mut session.compaction,
                        llm,
                        model,
                        cw,
                        reserved_request_tokens,
                    ) => outcome,
                }
            } else {
                dss_compact::CompactionOutcome::default()
            };
            if compact_outcome.folded {
                tracing::info!(
                    folds_added = compact_outcome.folds_added,
                    ranges = ?compact_outcome.folded_ranges,
                    "rolling compact applied L1 fold(s)"
                );
                // 阶段二 hook：被折叠的消息范围（历史，可能跨 run）。
                // 这些原始消息将被摘要替换，重要信息有丢失风险。
                // 当前的 run-end extract 只覆盖本次 run 的消息（run_message_start..），
                // 因此被折叠的历史消息不会自动进记忆。真正的 compaction flush
                // （对 folded_ranges 做后台 extract+consolidate）作为独立增强项，
                // 此处仅记录 hook 点与可观测性。
                if !compact_outcome.folded_ranges.is_empty() {
                    let folded_msg_count: usize = compact_outcome
                        .folded_ranges
                        .iter()
                        .map(|(s, e)| e.saturating_sub(*s))
                        .sum();
                    tracing::debug!(
                        folded_msg_count,
                        "compaction folded history not yet flushed to memory (hook pending)"
                    );
                }
            }
            // 折叠后的最终视图（若未折叠，与 free_view 等价但重新构建，避免持有旧借用）。
            let view = build_view(&session.messages, &session.compaction);
            let view_tokens = dss_compact::tokens::estimate_messages_tokens(&view)
                .saturating_add(tool_schema_tokens);
            if view_tokens > hard_wall_tokens {
                return fail(
                    session,
                    tx,
                    &mut progress,
                    final_text,
                    format!(
                        "上下文压缩后仍为 {view_tokens} tokens，超过 {hard_wall_tokens} tokens 硬墙；请缩短本轮输入或新建会话"
                    ),
                    None,
                )
                .await;
            }

            // —— 构建 LLM 请求（带工具定义；用 projection 视图）——
            let mut req = ChatRequest::new(model, view);
            if !tool_defs.is_empty() && !is_user_final_iteration {
                req.tools = Some(tool_defs.clone());
                req.tool_choice = Some("auto".to_string());
            }

            // The HTTP request may still be resolving DNS, connecting, or
            // waiting for response headers when the browser aborts its SSE
            // fetch. Race it with receiver closure so the session guard is
            // released immediately instead of waiting for the network.
            let stream_result = tokio::select! {
                biased;
                _ = tx.closed() => return cancel(session, &mut progress),
                result = llm.chat_stream(req) => result,
            };
            let stream = match stream_result {
                Ok(s) => s,
                Err(e) => {
                    return fail(session, tx, &mut progress, final_text, e.to_string(), None).await;
                }
            };

            // —— 流式消费：累积 thinking / text / tool_calls(by index) + finish_reason ——
            let mut tool_acc = ToolCallAccumulator::default();
            let mut finish_reason: Option<String> = None;
            let mut saw_finish = false;
            // Each channel retains its complete bounded turn for the canonical
            // DSML parse while releasing only irrevocably ordinary prefixes.
            let mut text_guard = IncrementalAssistantTextGuard::new();
            let mut thinking_guard = IncrementalAssistantTextGuard::new();
            let mut thinking_len_at_text_first_lt: Option<usize> = None;

            futures::pin_mut!(stream);
            loop {
                // A healthy SSE connection may have no next item yet. Receiver
                // closure must win even when the upstream stream is stalled.
                let next_event = tokio::select! {
                    biased;
                    _ = tx.closed() => return cancel(session, &mut progress),
                    event = stream.next() => event,
                };
                match next_event {
                    Some(Ok(StreamEvent::Thinking(t))) => {
                        if saw_finish {
                            return fail_invalidated_candidate(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                "assistant_protocol_invalid",
                                "provider emitted reasoning after the Finish event".to_string(),
                            )
                            .await;
                        }
                        if thinking_guard
                            .buffered_len()
                            .saturating_add(text_guard.buffered_len())
                            .saturating_add(t.len())
                            > MAX_ASSISTANT_TEXT_BYTES
                        {
                            return fail_invalidated_candidate(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                "assistant_protocol_invalid",
                                DsmlError::TooLarge.to_string(),
                            )
                            .await;
                        }
                        let released = match thinking_guard.push(&t) {
                            Ok(released) => released,
                            Err(error) => {
                                return fail_invalidated_candidate(
                                    session,
                                    tx,
                                    &mut progress,
                                    final_text,
                                    "assistant_protocol_invalid",
                                    error.to_string(),
                                )
                                .await;
                            }
                        };
                        if thinking_guard.publication_is_frozen() {
                            // Thinking and answer are one control-plane
                            // boundary: a marker in either channel must stop
                            // both before the next await can publish data.
                            text_guard.freeze_publication();
                        }
                        if let Some(released) = released {
                            if !send(
                                tx,
                                AgentEvent::Thinking {
                                    text: released.clone(),
                                },
                            )
                            .await
                            {
                                return cancel(session, &mut progress);
                            }
                            progress.published.thinking.push_str(&released);
                        }
                    }
                    Some(Ok(StreamEvent::Text(t))) => {
                        if saw_finish {
                            return fail_invalidated_candidate(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                "assistant_protocol_invalid",
                                "provider emitted assistant text after the Finish event"
                                    .to_string(),
                            )
                            .await;
                        }
                        if thinking_guard
                            .buffered_len()
                            .saturating_add(text_guard.buffered_len())
                            .saturating_add(t.len())
                            > MAX_ASSISTANT_TEXT_BYTES
                        {
                            return fail_invalidated_candidate(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                "assistant_protocol_invalid",
                                DsmlError::TooLarge.to_string(),
                            )
                            .await;
                        }
                        let text_had_observed_raw_lt = text_guard.has_observed_raw_lt();
                        let released = match text_guard.push(&t) {
                            Ok(released) => released,
                            Err(error) => {
                                return fail_invalidated_candidate(
                                    session,
                                    tx,
                                    &mut progress,
                                    final_text,
                                    "assistant_protocol_invalid",
                                    error.to_string(),
                                )
                                .await;
                            }
                        };
                        if !text_had_observed_raw_lt && text_guard.has_observed_raw_lt() {
                            // This byte offset is an event boundary: every
                            // reasoning byte already buffered happened before
                            // Text first entered the possible control plane.
                            thinking_len_at_text_first_lt = Some(thinking_guard.buffered_len());
                        }
                        if text_guard.publication_is_frozen() {
                            thinking_guard.freeze_publication();
                        }
                        if let Some(released) = released {
                            if !send(
                                tx,
                                AgentEvent::Text {
                                    text: released.clone(),
                                },
                            )
                            .await
                            {
                                return cancel(session, &mut progress);
                            }
                            progress.published.text.push_str(&released);
                        }
                    }
                    Some(Ok(StreamEvent::AssistantDelta { thinking, text })) => {
                        if saw_finish {
                            return fail_invalidated_candidate(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                "assistant_protocol_invalid",
                                "provider emitted an assistant delta after the Finish event"
                                    .to_string(),
                            )
                            .await;
                        }
                        if thinking_guard
                            .buffered_len()
                            .saturating_add(text_guard.buffered_len())
                            .saturating_add(thinking.len())
                            .saturating_add(text.len())
                            > MAX_ASSISTANT_TEXT_BYTES
                        {
                            return fail_invalidated_candidate(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                "assistant_protocol_invalid",
                                DsmlError::TooLarge.to_string(),
                            )
                            .await;
                        }

                        // Preserve the provider's atomic delta boundary. If
                        // either sibling contains a possible control opener,
                        // freeze both before either guard can return publishable
                        // bytes. No await is allowed until both guards have
                        // consumed the complete delta and synchronized state.
                        let thinking_len_at_event_start = thinking_guard.buffered_len();
                        let text_len_at_event_start = text_guard.buffered_len();
                        let text_had_observed_raw_lt = text_guard.has_observed_raw_lt();
                        if thinking.as_bytes().contains(&b'<') || text.as_bytes().contains(&b'<') {
                            thinking_guard.freeze_publication();
                            text_guard.freeze_publication();
                        }

                        let released_thinking = match thinking_guard.push(&thinking) {
                            Ok(released) => released,
                            Err(error) => {
                                return fail_invalidated_candidate(
                                    session,
                                    tx,
                                    &mut progress,
                                    final_text,
                                    "assistant_protocol_invalid",
                                    error.to_string(),
                                )
                                .await;
                            }
                        };
                        let released_text = match text_guard.push(&text) {
                            Ok(released) => released,
                            Err(error) => {
                                return fail_invalidated_candidate(
                                    session,
                                    tx,
                                    &mut progress,
                                    final_text,
                                    "assistant_protocol_invalid",
                                    error.to_string(),
                                )
                                .await;
                            }
                        };

                        if !text_had_observed_raw_lt && text_guard.has_observed_raw_lt() {
                            debug_assert!(text_guard.buffered_len() > text_len_at_event_start);
                            // Exclude this same provider delta's reasoning from
                            // the P2 pre-Text-latch allowance. It is a sibling
                            // of the control opener, not earlier evidence.
                            thinking_len_at_text_first_lt = Some(thinking_len_at_event_start);
                        }
                        if thinking_guard.publication_is_frozen()
                            || text_guard.publication_is_frozen()
                        {
                            thinking_guard.freeze_publication();
                            text_guard.freeze_publication();
                        }

                        if let Some(released) = released_thinking {
                            if !send(
                                tx,
                                AgentEvent::Thinking {
                                    text: released.clone(),
                                },
                            )
                            .await
                            {
                                return cancel(session, &mut progress);
                            }
                            progress.published.thinking.push_str(&released);
                        }
                        if let Some(released) = released_text {
                            if !send(
                                tx,
                                AgentEvent::Text {
                                    text: released.clone(),
                                },
                            )
                            .await
                            {
                                return cancel(session, &mut progress);
                            }
                            progress.published.text.push_str(&released);
                        }
                    }
                    Some(Ok(StreamEvent::ToolCallDelta(d))) => {
                        if saw_finish {
                            return fail_invalidated_candidate(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                "assistant_protocol_invalid",
                                "provider emitted a tool-call delta after the Finish event"
                                    .to_string(),
                            )
                            .await;
                        }
                        if let Err(error) = tool_acc.push(d) {
                            return fail_invalidated_candidate(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                "assistant_protocol_invalid",
                                format!("provider emitted invalid native tool protocol: {error}"),
                            )
                            .await;
                        }
                    }
                    Some(Ok(StreamEvent::Usage(u))) => progress.record_usage(u),
                    Some(Ok(StreamEvent::Finish { reason })) => {
                        if saw_finish {
                            return fail_invalidated_candidate(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                "assistant_protocol_invalid",
                                "provider emitted more than one Finish event".to_string(),
                            )
                            .await;
                        }
                        saw_finish = true;
                        finish_reason = reason;
                    }
                    Some(Err(e)) => {
                        return fail(session, tx, &mut progress, final_text, e.to_string(), None)
                            .await;
                    }
                    None => break,
                }
            }

            if !saw_finish {
                return fail(
                    session,
                    tx,
                    &mut progress,
                    final_text,
                    "provider stream ended before a Finish event".to_string(),
                    None,
                )
                .await;
            }

            // Linearize cancellation before committing any assistant/tool
            // message derived from the just-finished upstream response.
            if tx.is_closed() {
                return cancel(session, &mut progress);
            }

            // —— 决策门（顺序严格遵循 modules.md §4）——
            // Parse both complete bounded channels before any cross-channel
            // frozen suffix can cross the event boundary.
            let thinking_released_cursor = thinking_guard.released_cursor();
            let thinking_before_text_lt_remainder_len = thinking_len_at_text_first_lt
                .and_then(|cutoff| cutoff.checked_sub(thinking_released_cursor));
            let parsed_thinking = thinking_guard.finish();
            let parsed_text = text_guard.finish();
            let mut thinking_remainder = match parsed_thinking {
                Ok(IncrementalAssistantTextResult::Plain(remainder)) => remainder,
                Ok(IncrementalAssistantTextResult::ToolCalls { .. }) => {
                    return fail_invalidated_candidate(
                        session,
                        tx,
                        &mut progress,
                        final_text,
                        "assistant_protocol_invalid",
                        "provider emitted textual DSML protocol in the reasoning channel"
                            .to_string(),
                    )
                    .await;
                }
                Err(error) => {
                    return fail_invalidated_candidate(
                        session,
                        tx,
                        &mut progress,
                        final_text,
                        "assistant_protocol_invalid",
                        format!("provider emitted invalid reasoning protocol: {error}"),
                    )
                    .await;
                }
            };

            let had_native_tool_deltas = !tool_acc.is_empty();
            let parsed_text = match parsed_text {
                Ok(parsed) => parsed,
                Err(error) => {
                    return fail_invalidated_candidate(
                        session,
                        tx,
                        &mut progress,
                        final_text,
                        "assistant_protocol_invalid",
                        format!("provider emitted invalid textual DSML protocol: {error}"),
                    )
                    .await;
                }
            };
            let both_channels_plain =
                matches!(&parsed_text, IncrementalAssistantTextResult::Plain(_));
            if had_native_tool_deltas
                && matches!(
                    &parsed_text,
                    IncrementalAssistantTextResult::ToolCalls { .. }
                )
            {
                return fail_invalidated_candidate(
                    session,
                    tx,
                    &mut progress,
                    final_text,
                    "assistant_protocol_invalid",
                    "provider emitted both native and textual DSML tool protocols in one turn"
                        .to_string(),
                )
                .await;
            }

            let mut finalized = match tool_acc.finalize() {
                Ok(calls) => calls,
                Err(error) => {
                    return fail_invalidated_candidate(
                        session,
                        tx,
                        &mut progress,
                        final_text,
                        "assistant_protocol_invalid",
                        format!("provider emitted invalid native tool protocol: {error}"),
                    )
                    .await;
                }
            };

            let text_remainder = match parsed_text {
                IncrementalAssistantTextResult::Plain(remainder) => remainder,
                IncrementalAssistantTextResult::ToolCalls {
                    visible_text,
                    calls,
                } => {
                    finalized = calls
                        .into_iter()
                        .map(|call| {
                            ToolCall::function(
                                format!("dsml-{}", uuid::Uuid::new_v4().simple()),
                                call.name,
                                call.arguments,
                            )
                        })
                        .collect();
                    visible_text
                }
            };

            // A user-authored iteration budget is a hard control-plane boundary. The final
            // request deliberately advertises no tools; providers can still hallucinate an
            // undeclared call, so reject it before publishing or executing any side effect.
            if is_user_final_iteration && !finalized.is_empty() {
                let attempted = finalized
                    .iter()
                    .map(|call| call.function.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return fail_invalidated_candidate(
                    session,
                    tx,
                    &mut progress,
                    final_text,
                    "assistant_capability_violation",
                    format!(
                        "user-specified agent iteration budget exhausted ({agent_iteration_limit}): final iteration attempted tool call(s) after tools were disabled: {attempted}"
                    ),
                )
                .await;
            }

            // Flush cross-channel frozen suffixes only when both canonical
            // channels are plain. For canonical Text tool calls, retain only
            // reasoning bytes that predate Text's first raw `<`; any later
            // sibling-latched suffix stays private. Every protocol/capability
            // gate above must accept the turn first. Native tool deltas remain
            // private until the complete batch below.
            if tx.is_closed() {
                return cancel(session, &mut progress);
            }
            if !both_channels_plain {
                let safe_len = thinking_before_text_lt_remainder_len
                    .filter(|length| {
                        *length <= thinking_remainder.len()
                            && thinking_remainder.is_char_boundary(*length)
                    })
                    .unwrap_or(0);
                thinking_remainder.truncate(safe_len);
            }
            if !thinking_remainder.is_empty() {
                if !send(
                    tx,
                    AgentEvent::Thinking {
                        text: thinking_remainder.clone(),
                    },
                )
                .await
                {
                    return cancel(session, &mut progress);
                }
                progress.published.thinking.push_str(&thinking_remainder);
            }
            if both_channels_plain && !text_remainder.is_empty() {
                if !send(
                    tx,
                    AgentEvent::Text {
                        text: text_remainder.clone(),
                    },
                )
                .await
                {
                    return cancel(session, &mut progress);
                }
                progress.published.text.push_str(&text_remainder);
            }

            // 门 1：max_tokens 续传（finish_reason == length）。
            // 三档：累计 ≥5 → 终止（MaxIters/Failed）；≥3 → 大幅缩减提示；否则分块继续。
            // 「续传」语义：本轮被截断，注入提示让模型在下一轮继续（分块输出）。
            let is_length = finish_reason.as_deref() == Some("length");
            if is_length {
                if is_user_final_iteration {
                    return fail(
                        session,
                        tx,
                        &mut progress,
                        final_text,
                        format!(
                            "user-specified agent iteration budget exhausted ({agent_iteration_limit}): final response was truncated and cannot consume another agent iteration"
                        ),
                        None,
                    )
                    .await;
                }
                session.gate_state.length_finish_count += 1;
                let n = session.gate_state.length_finish_count;
                if n >= 5 {
                    // 终止：截断处即最终回复。
                    final_text = progress.published.text.clone();
                    if !send(
                        tx,
                        AgentEvent::Complete {
                            kind: CompleteKind::MaxIters,
                            final_text: final_text.clone(),
                            awaiting: None,
                            error: Some("reached max_tokens continuation cap (5)".into()),
                            usage: progress.usage,
                            iterations,
                            frame_status: FrameStatus::Failed,
                            pending_ask: None,
                            plan: session.plan.clone(),
                        },
                    )
                    .await
                    {
                        return cancel(session, &mut progress);
                    }
                    progress.commit_published(session);
                    session.frame.set_status(FrameStatus::Failed);
                    return RunOutcome {
                        kind: CompleteKind::MaxIters,
                        final_text,
                        awaiting: None,
                        pending_ask: None,
                        error: Some("reached max_tokens continuation cap (5)".into()),
                        usage: progress.usage,
                        iterations,
                    };
                }
                // 注入续传提示（n>=3 时要求大幅缩减；否则普通续传）。
                let notice = if n >= 3 {
                    "你的上一条回复因 max_tokens 被截断。请用**显著更短**的篇幅继续并完成，避免再次被截断。"
                } else {
                    "你的上一条回复因 max_tokens 被截断。请从中断处继续，完成剩余内容。"
                };
                progress.commit_published(session);
                session.messages.push(harness_notice(notice));
                continue;
            } else {
                // 非 length：本轮正常结束，重置续传计数。
                session.gate_state.length_finish_count = 0;
            }

            if !finalized.is_empty() {
                // —— 工具路径 ——
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
                    return cancel(session, &mut progress);
                }
                // Only the complete call batch—not streamed argument deltas—has
                // crossed the UI boundary. Retain it until real results are
                // published or cancellation synthesizes paired error results.
                progress.published.tool_calls = Some(finalized.clone());

                // Runner authorization is the mandatory Plan boundary. Schema
                // filtering above is only advisory because a provider may
                // still return an undeclared call. Reject mixed batches
                // atomically before constructing/spawning any Router work.
                let preapproval_batch_denied = plan_mode
                    && finalized
                        .iter()
                        .any(|call| !is_preapproval_plan_tool(&call.function.name));
                let inactive_plan_update_batch_denied = !plan_mode
                    && finalized.iter().any(|call| {
                        !is_tool_available(
                            &call.function.name,
                            plan_mode,
                            plan_step_updates_allowed,
                        )
                    });
                let exclusive_tool_batch_denied = finalized.len() > 1
                    && finalized.iter().any(|call| {
                        tools.batch_policy(&call.function.name) == Some(ToolBatchPolicy::Exclusive)
                    });
                let mut results = if preapproval_batch_denied {
                    rejected_preapproval_results(&finalized)
                } else if inactive_plan_update_batch_denied {
                    rejected_inactive_plan_update_results(&finalized)
                } else if exclusive_tool_batch_denied {
                    rejected_exclusive_tool_batch_results(&finalized)
                } else {
                    // —— 执行工具（并发 + 30s 超时）——
                    let pending: Vec<PendingToolCall> = finalized
                        .iter()
                        .map(|tc| PendingToolCall {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            input: parse_arguments(&tc.function.arguments),
                        })
                        .collect();
                    tokio::select! {
                        biased;
                        _ = tx.closed() => return cancel(session, &mut progress),
                        results = ToolRouter::execute_tool_calls(tools, ctx, pending) => results,
                    }
                };

                // generate_plan and ask_user are both legal before approval, and a
                // provider may emit them together. If both succeeded and their shared
                // state coexists, Plan approval wins. Rewrite the ask result before it
                // crosses either the UI or history boundary so the audit trail does not
                // falsely claim that a separate user response is still pending.
                let generated_plan_in_batch = finalized.iter().any(|call| {
                    call.function.name == "generate_plan"
                        && results
                            .iter()
                            .any(|result| result.tool_use_id == call.id && !result.is_error)
                });
                let asked_user_in_batch = finalized.iter().any(|call| {
                    call.function.name == "ask_user"
                        && results
                            .iter()
                            .any(|result| result.tool_use_id == call.id && !result.is_error)
                });
                if plan_mode && generated_plan_in_batch && asked_user_in_batch {
                    let has_unapproved_plan = ctx
                        .plan
                        .lock()
                        .await
                        .as_ref()
                        .is_some_and(|plan| !plan.approved);
                    let has_pending_ask = ctx.pending_ask.lock().await.is_some();
                    if has_unapproved_plan && has_pending_ask {
                        *ctx.pending_ask.lock().await = None;
                        for result in &mut results {
                            let is_ask_result = finalized.iter().any(|call| {
                                call.id == result.tool_use_id && call.function.name == "ask_user"
                            });
                            if is_ask_result && !result.is_error {
                                result.content = ASK_SUPERSEDED_BY_PLAN_RESULT.to_string();
                            }
                        }
                    }
                }

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
                    return cancel(session, &mut progress);
                }

                // Commit the assistant tool call and every paired tool result
                // only after the receiver accepted the result event. This is
                // the terminal point for cancellation: an aborted UI cannot
                // leave a ghost or unpaired assistant tool-call in history.
                let iteration_text = progress.published.text.clone();
                let assistant_msg = progress
                    .take_published_message()
                    .expect("published tool calls always produce an assistant message");
                session.messages.push(assistant_msg);
                for r in &results {
                    let mut message = ChatMessage::tool(
                        &r.tool_use_id,
                        &r.content,
                        finalized
                            .iter()
                            .find(|tc| tc.id == r.tool_use_id)
                            .map(|tc| tc.function.name.clone()),
                    );
                    message.is_error = Some(r.is_error);
                    session.messages.push(message);
                }

                // Plan and ask tools mutate ToolContext while executing. Snapshot those
                // mutations at the same durable boundary as their tool-result messages so a
                // crash cannot restore the transcript while losing its actionable state.
                let plan_changed = finalized.iter().any(|call| {
                    is_plan_mutation_tool(&call.function.name)
                        && results
                            .iter()
                            .any(|result| result.tool_use_id == call.id && !result.is_error)
                });
                let committed_plan_after_batch = if plan_changed {
                    let committed = ctx.plan.lock().await.clone();
                    session.plan = committed.clone();
                    committed
                } else {
                    session.plan.clone()
                };
                let checkpoint_pending_ask = ctx.pending_ask.lock().await.clone();
                let (checkpoint_status, checkpoint_awaiting) = if plan_mode
                    && committed_plan_after_batch
                        .as_ref()
                        .is_some_and(|plan| !plan.approved)
                {
                    ("awaiting_plan_approval", Some("plan_approval".to_string()))
                } else if checkpoint_pending_ask.is_some() {
                    ("awaiting_user_response", Some("user_response".to_string()))
                } else {
                    ("processing", None)
                };

                let checkpoint_messages = session
                    .messages
                    .get(history_checkpoint_cursor..)
                    .unwrap_or(&[])
                    .to_vec();
                if let Err(error) = ctx
                    .checkpoint_history(
                        checkpoint_messages,
                        dss_tools::HistoryCheckpointState {
                            frame_id: session.frame.id.clone(),
                            parent_frame_id: session.frame.parent_frame_id.clone(),
                            root_frame_id: session.frame.root_frame_id.clone(),
                            agent_name: session.frame.agent_name.clone(),
                            task_summary: session.frame.task_summary.clone(),
                            plan: committed_plan_after_batch.clone(),
                            pending_ask: checkpoint_pending_ask,
                            status: checkpoint_status.into(),
                            awaiting: checkpoint_awaiting,
                            compaction_state: serde_json::to_string(&session.compaction).ok(),
                        },
                    )
                    .await
                {
                    return fail(
                        session,
                        tx,
                        &mut progress,
                        final_text,
                        format!("保存工具轨迹检查点失败：{error}"),
                        None,
                    )
                    .await;
                }
                history_checkpoint_cursor = session.messages.len();

                // A timeout/panic after a possibly-mutating tool crossed its execution boundary
                // is not an ordinary tool error. The paired assistant/tool messages are already
                // durably checkpointed above; stop before another model turn can retry or mask
                // the unknown external outcome.
                if results.iter().any(|result| result.outcome_unknown) {
                    if !iteration_text.is_empty() {
                        final_text = iteration_text;
                    }
                    let error = "外部工具可能已经执行，但没有收到可确认的结果。请先人工对账，再继续此 Run。"
                        .to_string();
                    if !send(
                        tx,
                        AgentEvent::Complete {
                            kind: CompleteKind::NeedsReconciliation,
                            final_text: final_text.clone(),
                            awaiting: Some("tool_reconciliation".to_string()),
                            error: Some(error.clone()),
                            usage: progress.usage,
                            iterations,
                            frame_status: FrameStatus::NeedsReconciliation,
                            pending_ask: None,
                            plan: committed_plan_after_batch,
                        },
                    )
                    .await
                    {
                        return cancel(session, &mut progress);
                    }
                    session.frame.set_status(FrameStatus::NeedsReconciliation);
                    return RunOutcome {
                        kind: CompleteKind::NeedsReconciliation,
                        final_text,
                        awaiting: Some("tool_reconciliation".into()),
                        pending_ask: None,
                        error: Some(error),
                        usage: progress.usage,
                        iterations,
                    };
                }

                let errors_in_batch =
                    results.iter().filter(|result| result.is_error).count() as u32;
                if errors_in_batch > 0 {
                    tool_error_count = tool_error_count.saturating_add(errors_in_batch);
                    if tool_error_count >= 2 {
                        session
                            .messages
                            .push(harness_notice(TOOL_ERROR_RECOVERY_NOTICE));
                    }
                }

                // Plan tools mutate ToolContext while executing. ToolResults delivery is
                // the commit point: only after the UI accepted that batch may the durable
                // session snapshot and its matching PlanUpdate become visible.
                let plan_update_published = if plan_changed {
                    let committed_plan = committed_plan_after_batch;
                    if let Some(plan) = committed_plan {
                        if !send(tx, AgentEvent::PlanUpdate { plan }).await {
                            return cancel(session, &mut progress);
                        }
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };

                // 累计 usage 文字（后续多轮的 text 保留为最终回复）。
                if !iteration_text.is_empty() {
                    final_text = iteration_text;
                }

                // —— plan 检测：plan_mode 且 ctx.plan 有未批准 plan → 转 AwaitingPlanApproval ——
                // A valid plan is authoritative when the model emits generate_plan and
                // ask_user in one batch. Otherwise the ask state strands the already
                // persisted plan: the UI renders approval, while the run is awaiting a
                // user response. Keep both tool results as audit evidence, but discard
                // the superseded pending ask before publishing the terminal state.
                if plan_mode {
                    let plan_guard = ctx.plan.lock().await;
                    if let Some(plan) = plan_guard.clone() {
                        if !plan.approved {
                            drop(plan_guard);
                            *ctx.pending_ask.lock().await = None;
                            // A successful generate_plan already published the
                            // committed snapshot above. Preserve publication
                            // for a pre-existing unapproved plan, but never
                            // emit the same snapshot twice in one iteration.
                            if !plan_update_published
                                && !send(tx, AgentEvent::PlanUpdate { plan: plan.clone() }).await
                            {
                                return cancel(session, &mut progress);
                            }
                            if !send(
                                tx,
                                AgentEvent::Complete {
                                    kind: CompleteKind::Awaiting,
                                    final_text: final_text.clone(),
                                    awaiting: Some("plan_approval".to_string()),
                                    error: None,
                                    usage: progress.usage,
                                    iterations,
                                    frame_status: FrameStatus::AwaitingPlanApproval,
                                    pending_ask: None,
                                    plan: Some(plan),
                                },
                            )
                            .await
                            {
                                return cancel(session, &mut progress);
                            }
                            session.frame.set_status(FrameStatus::AwaitingPlanApproval);
                            return RunOutcome {
                                kind: CompleteKind::Awaiting,
                                final_text,
                                awaiting: Some("plan_approval".into()),
                                pending_ask: None,
                                error: None,
                                usage: progress.usage,
                                iterations,
                            };
                        }
                    }
                }

                // —— ask_user 检测：挂起则转 AwaitingUserResponse 退出 ——
                let pending_ask_guard = ctx.pending_ask.lock().await;
                if let Some(ask) = pending_ask_guard.clone() {
                    drop(pending_ask_guard);
                    // 清空挂起（下次 run 会重新挂）。
                    *ctx.pending_ask.lock().await = None;

                    let event = AgentEvent::Complete {
                        kind: CompleteKind::Awaiting,
                        final_text: final_text.clone(),
                        awaiting: Some("user_response".to_string()),
                        error: None,
                        usage: progress.usage,
                        iterations,
                        frame_status: FrameStatus::AwaitingUserResponse,
                        pending_ask: Some(ask.clone()),
                        plan: session.plan.clone(),
                    };
                    if !send(tx, event).await {
                        return cancel(session, &mut progress);
                    }
                    session.frame.set_status(FrameStatus::AwaitingUserResponse);
                    return RunOutcome {
                        kind: CompleteKind::Awaiting,
                        final_text,
                        awaiting: Some("user_response".into()),
                        pending_ask: Some(ask),
                        error: None,
                        usage: progress.usage,
                        iterations,
                    };
                }
                drop(pending_ask_guard);

                // Every unsuccessful tool-backed planning turn consumes one
                // denial attempt, including invalid allowed calls and batches
                // rejected by the pre-approval capability boundary.
                if plan_mode && ctx.plan.lock().await.is_none() {
                    if let Some(message) =
                        record_plan_denial(session, iterations, agent_iteration_limit)
                    {
                        return fail(session, tx, &mut progress, final_text, message, None).await;
                    }
                    session.messages.push(harness_notice(PLAN_DENIAL_NOTICE));
                    continue;
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
                if progress.published.text.trim().is_empty() {
                    // 空响应（thinking-only 也算空）仍可能已经向 UI
                    // 发布 reasoning，因此在重试/失败前保留该证据。
                    session.gate_state.empty_retry_count += 1;
                    let missing_plan = plan_mode && ctx.plan.lock().await.is_none();
                    if missing_plan {
                        // An empty response is still an unsuccessful planning
                        // attempt. Plan failure takes precedence on the final
                        // available iteration (and on the shared fourth
                        // denial), while non-Plan empty retry behavior remains
                        // unchanged.
                        if let Some(message) =
                            record_plan_denial(session, iterations, agent_iteration_limit)
                        {
                            return fail(session, tx, &mut progress, final_text, message, None)
                                .await;
                        }
                    }
                    if session.gate_state.empty_retry_count > EMPTY_RETRY_CAP {
                        return fail(
                            session,
                            tx,
                            &mut progress,
                            final_text,
                            "empty response retry cap exceeded (3)".to_string(),
                            None,
                        )
                        .await;
                    }
                    if is_user_final_iteration {
                        return fail(
                            session,
                            tx,
                            &mut progress,
                            final_text,
                            format!(
                                "user-specified agent iteration budget exhausted ({agent_iteration_limit}): final iteration returned no answer"
                            ),
                            None,
                        )
                        .await;
                    }
                    progress.commit_published(session);
                    let notice = if missing_plan {
                        PLAN_DENIAL_NOTICE
                    } else {
                        "你的上一条回复为空。请基于上下文给出实际回复；若任务已完成请明确说明。"
                    };
                    session.messages.push(harness_notice(notice));
                    continue;
                }
                // 有内容：clean completion（重置 empty_retry）。
                session.gate_state.empty_retry_count = 0;

                // —— plan denial 门：plan_mode 但未生成 plan → ≤3 次提示重生成，超限 Failed ——
                if plan_mode {
                    let has_plan = ctx.plan.lock().await.is_some();
                    if !has_plan {
                        if let Some(message) =
                            record_plan_denial(session, iterations, agent_iteration_limit)
                        {
                            return fail(session, tx, &mut progress, final_text, message, None)
                                .await;
                        }
                        progress.commit_published(session);
                        session.messages.push(harness_notice(PLAN_DENIAL_NOTICE));
                        continue;
                    }
                }

                // An explicit approved-plan run cannot claim Natural completion while any step
                // remains pending/running/failed. Give the model a bounded chance to reconcile
                // the plan through update_step_status; then return the approved unfinished plan
                // to a durable retryable state instead of falsely marking the session completed.
                if enforce_plan_completion {
                    let current_plan = tokio::select! {
                        biased;
                        _ = tx.closed() => return cancel(session, &mut progress),
                        plan = ctx.plan.lock() => plan.clone(),
                    };
                    if !approved_plan_is_complete(current_plan.as_ref()) {
                        plan_completion_retry_count += 1;

                        if is_user_final_iteration {
                            return fail(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                format!(
                                    "user-specified agent iteration budget exhausted ({agent_iteration_limit}) with approved plan still incomplete:\n{}",
                                    render_plan_status(current_plan.as_ref())
                                ),
                                None,
                            )
                            .await;
                        }

                        // The reviewer/plan gate has rejected this draft. Hide
                        // it before attempting DraftReset so cancellation can
                        // never reclassify it as a visible assistant answer.
                        progress.hide_published(session);
                        if !send(
                            tx,
                            AgentEvent::DraftReset {
                                reason: "plan_incomplete".into(),
                            },
                        )
                        .await
                        {
                            return cancel(session, &mut progress);
                        }

                        let retryable_plan = current_plan
                            .filter(|plan| plan.approved)
                            .or_else(|| initial_approved_plan.clone());
                        if plan_completion_retry_count <= PLAN_COMPLETION_RETRY_CAP {
                            session.messages.push(harness_notice(&format!(
                                "已批准计划尚未完成，不能结束本轮。请继续实际执行未完成步骤，并用 update_step_status 更新状态；只有全部步骤均为 done 才能给出最终回复。\n{}",
                                render_plan_status(retryable_plan.as_ref())
                            )));
                            continue;
                        }

                        session.messages.push(harness_notice(
                            "计划完成状态连续校验失败，本轮已暂停。保留已批准的未完成计划，等待用户点击“执行计划/重试”。",
                        ));
                        if let Some(plan) = retryable_plan.as_ref() {
                            if !send(tx, AgentEvent::PlanUpdate { plan: plan.clone() }).await {
                                return cancel(session, &mut progress);
                            }
                        }
                        if !send(
                            tx,
                            AgentEvent::Complete {
                                kind: CompleteKind::Awaiting,
                                final_text: String::new(),
                                awaiting: Some("plan_execution".into()),
                                error: None,
                                usage: progress.usage,
                                iterations,
                                frame_status: FrameStatus::AwaitingPlanExecution,
                                pending_ask: None,
                                plan: retryable_plan.clone(),
                            },
                        )
                        .await
                        {
                            return cancel(session, &mut progress);
                        }
                        session.plan = retryable_plan;
                        session.frame.set_status(FrameStatus::AwaitingPlanExecution);
                        return RunOutcome {
                            kind: CompleteKind::Awaiting,
                            final_text: String::new(),
                            awaiting: Some("plan_execution".into()),
                            pending_ask: None,
                            error: None,
                            usage: progress.usage,
                            iterations,
                        };
                    }
                }

                final_text = progress.published.text.clone();

                // A reviewer can explicitly declare that the workspace artifact,
                // rather than only the prose answer, must be repaired. Do not spend
                // the single corrective re-review on a text-only assertion of success:
                // require a successful write/edit and a later, separate read/check.
                if let Some(trace_start) = artifact_repair_trace_start {
                    let repair_progress =
                        artifact_repair_progress(&session.messages[trace_start..]);
                    if repair_progress != ArtifactRepairProgress::Complete {
                        let guidance = artifact_repair_guidance(repair_progress);
                        if is_user_final_iteration {
                            return fail(
                                session,
                                tx,
                                &mut progress,
                                final_text,
                                format!(
                                    "user-specified agent iteration budget exhausted ({agent_iteration_limit}) before reviewer-required artifact repair completed: {guidance}"
                                ),
                                None,
                            )
                            .await;
                        }

                        if artifact_repair_reminders < ARTIFACT_REPAIR_REMINDER_CAP {
                            artifact_repair_reminders += 1;
                            progress.hide_published(session);
                            if !send(
                                tx,
                                AgentEvent::DraftReset {
                                    reason: "reviewer_artifact_repair_required".into(),
                                },
                            )
                            .await
                            {
                                return cancel(session, &mut progress);
                            }
                            session.messages.push(harness_notice(&format!(
                                "reviewer 要求修复 workspace 产物，但修订轨迹尚未满足可复审条件：{guidance}\n必须先实际调用 write_file 或 edit_file 并成功，再在后续独立工具轮调用 read_file、python 或 bash 成功核验；不要只重复最终回复或声称文件已经修改。完成后再给最终回复。"
                            )));
                            continue;
                        }

                        return fail(
                            session,
                            tx,
                            &mut progress,
                            final_text,
                            format!(
                                "reviewer-required artifact repair was not completed after the corrective reminder: {guidance}"
                            ),
                            None,
                        )
                        .await;
                    }
                }

                // —— terminal barrier（P6b verify）：自然完成时 review；首次 veto 后允许修一轮。
                // The corrected draft is reviewed again. Silently accepting a second failed
                // review would turn the retry cap into a quality bypass; instead surface a
                // durable Failed run with the draft preserved for an explicit retry.
                let verdict = tokio::select! {
                    biased;
                    _ = tx.closed() => return cancel(session, &mut progress),
                    verdict = dss_verify::terminal_barrier(
                        llm,
                        model,
                        prompt,
                        &final_text,
                        &run_context,
                        &session.messages[run_trace_start..],
                    ) => verdict,
                };
                match verdict {
                    Some(verdict) if !verdict.pass => {
                        let requires_tool_action = verdict.requires_tool_action;
                        let findings = if verdict.findings.is_empty() {
                            vec!["reviewer 判定结果未通过，但未返回具体 finding；请重新核对证据、约束和结论。".to_string()]
                        } else {
                            verdict.findings
                        };
                        if session.gate_state.veto_count < VETO_CAP {
                            session.gate_state.veto_count += 1;
                            if is_user_final_iteration {
                                return fail(
                                    session,
                                    tx,
                                    &mut progress,
                                    final_text,
                                    format!(
                                        "user-specified agent iteration budget exhausted ({agent_iteration_limit}) before reviewer findings could be corrected: {}",
                                        findings.join("; ")
                                    ),
                                    None,
                                )
                                .await;
                            }
                            last_veto_findings = findings.clone();
                            tracing::info!(findings = ?findings, "terminal barrier veto");
                            progress.hide_published(session);
                            if !send(
                                tx,
                                AgentEvent::DraftReset {
                                    reason: "reviewer_veto".into(),
                                },
                            )
                            .await
                            {
                                return cancel(session, &mut progress);
                            }
                            let notice = format!(
                                "reviewer 发现以下问题，请修复后重新给出最终回复：\n{}{}",
                                findings
                                    .iter()
                                    .map(|f| format!("- {f}"))
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                if requires_tool_action {
                                    "\n\n本次裁决的 repair_scope=artifact。下一次最终回复前，必须实际调用 write_file 或 edit_file 修正产物，并在后续独立工具轮用 read_file、python 或 bash 成功核验；工具仍然可用。不要只改最终回复措辞、重复旧答案，或把既有多次调用伪称为一次。"
                                } else {
                                    ""
                                }
                            );
                            if requires_tool_action {
                                artifact_repair_trace_start = Some(session.messages.len());
                                artifact_repair_reminders = 0;
                            }
                            session.messages.push(harness_notice(&notice));
                            continue;
                        }

                        let message = format!(
                            "reviewer verification still failed after {VETO_CAP} corrective attempt(s): {}",
                            findings.join("; ")
                        );
                        return fail(session, tx, &mut progress, final_text, message, None).await;
                    }
                    None if session.gate_state.veto_count > 0 => {
                        let unresolved = if last_veto_findings.is_empty() {
                            "the prior reviewer veto remains unresolved".to_string()
                        } else {
                            last_veto_findings.join("; ")
                        };
                        return fail(
                            session,
                            tx,
                            &mut progress,
                            final_text,
                            format!(
                                "reviewer verification was unavailable after a prior veto; failing closed with unresolved findings: {unresolved}"
                            ),
                            None,
                        )
                        .await;
                    }
                    _ => {}
                }

                // Tool-backed state (especially plan step updates) lives in
                // ToolContext while the run is active. Publish and copy the
                // latest snapshot before the terminal event so the UI does
                // not render the stale, pre-run plan until a page reload.
                let completed_plan = tokio::select! {
                    biased;
                    _ = tx.closed() => return cancel(session, &mut progress),
                    plan = ctx.plan.lock() => plan.clone(),
                };
                if let Some(plan) = completed_plan.as_ref() {
                    if !send(tx, AgentEvent::PlanUpdate { plan: plan.clone() }).await {
                        return cancel(session, &mut progress);
                    }
                }

                if !send(
                    tx,
                    AgentEvent::Complete {
                        kind: CompleteKind::Natural,
                        final_text: final_text.clone(),
                        awaiting: None,
                        error: None,
                        usage: progress.usage,
                        iterations,
                        frame_status: FrameStatus::Completed,
                        pending_ask: None,
                        plan: completed_plan.clone(),
                    },
                )
                .await
                {
                    return cancel(session, &mut progress);
                }
                // Successful terminal event delivery is the commit point.
                // Only now persist the assistant and make the frame terminal.
                progress.commit_published(session);
                session.plan = completed_plan;
                session.frame.set_status(FrameStatus::Completed);
                return RunOutcome {
                    kind: CompleteKind::Natural,
                    final_text,
                    awaiting: None,
                    pending_ask: None,
                    error: None,
                    usage: progress.usage,
                    iterations,
                };
            }
        }

        // —— 循环耗尽 ——
        let iterations = progress.iterations;
        let iteration_error = if user_iteration_cap_active {
            format!("reached user-specified agent iteration budget ({agent_iteration_limit})")
        } else {
            format!("reached max iterations ({max_iterations})")
        };
        warn!(
            iterations,
            limit = agent_iteration_limit,
            "agent hit iteration limit"
        );
        let event = AgentEvent::Complete {
            kind: CompleteKind::MaxIters,
            final_text: final_text.clone(),
            awaiting: None,
            error: Some(iteration_error.clone()),
            usage: progress.usage,
            iterations,
            frame_status: FrameStatus::Failed,
            pending_ask: None,
            plan: session.plan.clone(),
        };
        if !send(tx, event).await {
            return cancel(session, &mut progress);
        }
        session.frame.set_status(FrameStatus::Failed);
        RunOutcome {
            kind: CompleteKind::MaxIters,
            final_text,
            awaiting: None,
            pending_ask: None,
            error: Some(iteration_error),
            usage: progress.usage,
            iterations,
        }
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactRepairProgress {
    MissingMutation,
    MissingVerification,
    Complete,
}

/// Check only successful tool results after the reviewer-veto boundary. A read
/// in the same assistant tool batch as a write is not proof of the post-write
/// artifact, because batched tools may execute concurrently.
fn artifact_repair_progress(messages: &[ChatMessage]) -> ArtifactRepairProgress {
    let mut call_sites = HashMap::<String, (String, usize)>::new();
    for (message_index, message) in messages.iter().enumerate() {
        if let Some(tool_calls) = message.tool_calls.as_ref() {
            for call in tool_calls {
                call_sites.insert(call.id.clone(), (call.function.name.clone(), message_index));
            }
        }
    }

    let mut latest_mutation_turn = None;
    let mut latest_verification_turn = None;
    for message in messages {
        if message.role != "tool" || message.is_error != Some(false) {
            continue;
        }
        let Some((fallback_name, call_turn)) = message
            .tool_call_id
            .as_ref()
            .and_then(|call_id| call_sites.get(call_id))
        else {
            continue;
        };
        let name = message.name.as_deref().unwrap_or(fallback_name);
        if is_artifact_mutation_tool(name) {
            latest_mutation_turn = Some(*call_turn);
        } else if is_artifact_verification_tool(name) {
            latest_verification_turn = Some(*call_turn);
        }
    }

    let Some(mutation_turn) = latest_mutation_turn else {
        return ArtifactRepairProgress::MissingMutation;
    };
    if latest_verification_turn.is_some_and(|verification_turn| verification_turn > mutation_turn) {
        ArtifactRepairProgress::Complete
    } else {
        ArtifactRepairProgress::MissingVerification
    }
}

fn is_artifact_mutation_tool(name: &str) -> bool {
    matches!(name, "write_file" | "edit_file")
}

fn is_artifact_verification_tool(name: &str) -> bool {
    matches!(name, "read_file" | "python" | "bash")
}

fn artifact_repair_guidance(progress: ArtifactRepairProgress) -> &'static str {
    match progress {
        ArtifactRepairProgress::MissingMutation => {
            "修订边界之后没有成功的 write_file/edit_file 结果"
        }
        ArtifactRepairProgress::MissingVerification => {
            "已有成功写入/编辑，但没有在其后的独立工具轮成功 read_file/python/bash 核验"
        }
        ArtifactRepairProgress::Complete => "产物修复与后续核验均已完成",
    }
}

fn approved_plan_is_complete(plan: Option<&dss_tools::PlanState>) -> bool {
    plan.is_some_and(|plan| {
        plan.approved
            && !plan.steps.is_empty()
            && plan
                .steps
                .iter()
                .all(|step| step.status.eq_ignore_ascii_case("done"))
    })
}

fn render_plan_status(plan: Option<&dss_tools::PlanState>) -> String {
    let Some(plan) = plan else {
        return "- 已批准计划快照缺失；请恢复原计划，不要直接结束。".into();
    };
    plan.steps
        .iter()
        .enumerate()
        .map(|(index, step)| format!("- {}. {} [{}]", index + 1, step.title, step.status))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Extract a user-authored hard iteration budget without treating every bare number as a
/// control-plane limit. This intentionally recognizes only nearby, explicit limiting language;
/// the global hard safety ceiling remains independent.
fn parse_explicit_iteration_limit(prompt: &str) -> Option<u32> {
    let normalized = prompt.to_lowercase();
    let markers = ["iterations", "iteration", "轮"];
    let mut limits = Vec::new();

    for marker in markers {
        for (marker_start, _) in normalized.match_indices(marker) {
            let prefix = &normalized[..marker_start];
            let digit_end = prefix
                .char_indices()
                .rev()
                .find(|(_, ch)| ch.is_ascii_digit())
                .map(|(index, ch)| index + ch.len_utf8());
            let Some(digit_end) = digit_end else {
                continue;
            };
            let digit_start = prefix[..digit_end]
                .char_indices()
                .rev()
                .take_while(|(_, ch)| ch.is_ascii_digit())
                .last()
                .map(|(index, _)| index)
                .unwrap_or(digit_end);
            let Ok(limit) = prefix[digit_start..digit_end].parse::<u32>() else {
                continue;
            };
            if limit == 0 || limit > dss_core::MAX_CONFIGURABLE_ITERATIONS {
                continue;
            }

            // Pair the marker with an actual iteration count, not an arbitrary earlier
            // number. The supported joiners cover the product's user-facing Chinese and
            // English forms (for example `12 个 agent iterations`).
            if !is_iteration_count_joiner(&prefix[digit_end..]) {
                continue;
            }

            // The limiting phrase must grammatically govern this number. A broad look-behind
            // is unsafe: in `最多 12 ...（硬上限）\n前 4 个 iteration`, the `限` from the
            // first clause used to contaminate the schedule count and silently lower 12 to 4.
            let hint_prefix = prefix[..digit_start].trim_end_matches(|ch: char| {
                ch.is_whitespace() || matches!(ch, ':' | '：' | '=' | '(' | '（')
            });
            if has_iteration_limit_hint(hint_prefix) {
                limits.push(limit);
            }
        }
    }

    limits.into_iter().min()
}

/// Apply a user-authored limit only as a further restriction; natural language can never expand
/// the configured hard wall. The boolean preserves the existing dedicated final-iteration UX
/// only when the user limit is the binding constraint.
fn effective_iteration_limit(explicit: Option<u32>, configured: u32) -> (u32, bool) {
    (
        explicit.map_or(configured, |limit| limit.min(configured)),
        explicit.is_some_and(|limit| limit <= configured),
    )
}

fn is_iteration_count_joiner(joiner: &str) -> bool {
    let compact: String = joiner.chars().filter(|ch| !ch.is_whitespace()).collect();
    matches!(
        compact.as_str(),
        "" | "个"
            | "次"
            | "agent"
            | "个agent"
            | "次agent"
            | "llm"
            | "个llm"
            | "次llm"
            | "model"
            | "个model"
            | "次model"
    )
}

fn has_iteration_limit_hint(prefix: &str) -> bool {
    const LIMIT_HINTS: &[&str] = &[
        "no more than",
        "at most",
        "within",
        "up to",
        "hard maximum of",
        "hard maximum",
        "maximum of",
        "maximum",
        "hard limit of",
        "hard limit",
        "max",
        "<=",
        "≤",
        "不得超过",
        "不超过",
        "至多",
        "最多为",
        "最多",
        "硬上限为",
        "硬上限",
        "上限为",
        "上限",
        "限制为",
        "限制在",
        "限",
    ];

    LIMIT_HINTS.iter().any(|hint| {
        let Some(before_hint) = prefix.strip_suffix(hint) else {
            return false;
        };
        // Do not accept an English hint as the tail of another word (for example
        // `climax 4 iterations`). Chinese limiting phrases do not use ASCII word
        // boundaries and are already explicit suffixes.
        !hint
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphanumeric())
            || before_hint
                .chars()
                .next_back()
                .is_none_or(|ch| !ch.is_ascii_alphanumeric())
    })
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum NativeToolProtocolError {
    #[error("native tool-call stream exceeds its configured boundary")]
    BoundaryExceeded,
    #[error("native tool-call id changed within one index")]
    IdMutation,
    #[error("native tool-call name changed within one index")]
    NameMutation,
    #[error("native tool-call stream ended with an incomplete call")]
    Incomplete,
    #[error("native tool-call stream reused an id across indexes")]
    DuplicateId,
}

#[derive(Debug, Default, Clone)]
struct AccToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[derive(Debug, Default)]
struct ToolCallAccumulator {
    calls: BTreeMap<u32, AccToolCall>,
    total_argument_bytes: usize,
    no_progress_delta_count: usize,
}

impl ToolCallAccumulator {
    fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    fn push(&mut self, delta: dss_llm::ToolCallDelta) -> Result<(), NativeToolProtocolError> {
        if !self.calls.contains_key(&delta.index) && self.calls.len() >= MAX_TOOL_CALLS_PER_TURN {
            return Err(NativeToolProtocolError::BoundaryExceeded);
        }

        let slot = self.calls.entry(delta.index).or_default();
        let mut made_progress = false;
        if let Some(id) = delta.id {
            if id.is_empty() || id.len() > MAX_TOOL_CALL_ID_BYTES {
                return Err(NativeToolProtocolError::BoundaryExceeded);
            }
            if slot.id.as_ref().is_some_and(|existing| existing != &id) {
                return Err(NativeToolProtocolError::IdMutation);
            }
            if slot.id.is_none() {
                slot.id = Some(id);
                made_progress = true;
            }
        }
        if let Some(name) = delta.name {
            if name.is_empty() || name.len() > MAX_TOOL_NAME_BYTES {
                return Err(NativeToolProtocolError::BoundaryExceeded);
            }
            if slot.name.as_ref().is_some_and(|existing| existing != &name) {
                return Err(NativeToolProtocolError::NameMutation);
            }
            if slot.name.is_none() {
                slot.name = Some(name);
                made_progress = true;
            }
        }
        if let Some(arguments) = delta.arguments {
            let call_bytes = slot
                .arguments
                .len()
                .checked_add(arguments.len())
                .filter(|bytes| *bytes <= MAX_TOOL_ARGUMENT_BYTES_PER_CALL)
                .ok_or(NativeToolProtocolError::BoundaryExceeded)?;
            self.total_argument_bytes = self
                .total_argument_bytes
                .checked_add(arguments.len())
                .filter(|bytes| *bytes <= MAX_TOOL_ARGUMENT_BYTES_TOTAL)
                .ok_or(NativeToolProtocolError::BoundaryExceeded)?;
            if !arguments.is_empty() {
                slot.arguments.reserve(call_bytes - slot.arguments.len());
                slot.arguments.push_str(&arguments);
                made_progress = true;
            }
        }
        if !made_progress {
            self.no_progress_delta_count = self
                .no_progress_delta_count
                .checked_add(1)
                .filter(|count| *count <= MAX_NATIVE_TOOL_NO_PROGRESS_DELTAS_PER_TURN)
                .ok_or(NativeToolProtocolError::BoundaryExceeded)?;
        }
        Ok(())
    }

    fn finalize(self) -> Result<Vec<ToolCall>, NativeToolProtocolError> {
        let mut ids = HashSet::with_capacity(self.calls.len());
        let mut finalized = Vec::with_capacity(self.calls.len());
        for (_, call) in self.calls {
            let id = call.id.ok_or(NativeToolProtocolError::Incomplete)?;
            let name = call.name.ok_or(NativeToolProtocolError::Incomplete)?;
            if !ids.insert(id.clone()) {
                return Err(NativeToolProtocolError::DuplicateId);
            }
            finalized.push(ToolCall::function(id, name, call.arguments));
        }
        Ok(finalized)
    }
}

/// A protocol or capability decision can invalidate text that was safe to
/// display provisionally but cannot enter canonical assistant history. Hide
/// that candidate before resetting the live draft, matching reviewer-reset
/// ordering, then report the terminal error through the ordinary failure path.
async fn fail_invalidated_candidate(
    session: &mut Session,
    tx: &mpsc::Sender<AgentEvent>,
    progress: &mut RunProgress,
    previous_text: String,
    reset_reason: &str,
    message: String,
) -> RunOutcome {
    let has_published_draft =
        !progress.published.thinking.is_empty() || !progress.published.text.is_empty();
    if has_published_draft {
        progress.hide_published(session);
        if !send(
            tx,
            AgentEvent::DraftReset {
                reason: reset_reason.to_string(),
            },
        )
        .await
        {
            return cancel(session, progress);
        }
    }
    fail(session, tx, progress, previous_text, message, None).await
}

/// LLM 失败路径：frame Failed + complete kind=error。
async fn fail(
    session: &mut Session,
    tx: &mpsc::Sender<AgentEvent>,
    progress: &mut RunProgress,
    previous_text: String,
    message: String,
    pending_ask: Option<PendingAsk>,
) -> RunOutcome {
    let final_text = if progress.published.text.is_empty() {
        previous_text
    } else {
        progress.published.text.clone()
    };
    let usage = progress.usage;
    let iterations = progress.iterations;
    let delivered = tx
        .send(AgentEvent::Complete {
            kind: CompleteKind::Error,
            final_text: final_text.clone(),
            awaiting: None,
            error: Some(message.clone()),
            usage,
            iterations,
            frame_status: FrameStatus::Failed,
            pending_ask: pending_ask.clone(),
            plan: session.plan.clone(),
        })
        .await
        .is_ok();
    if !delivered {
        return cancel(session, progress);
    }
    progress.commit_published(session);
    session.frame.set_status(FrameStatus::Failed);
    RunOutcome {
        kind: CompleteKind::Error,
        final_text,
        awaiting: None,
        pending_ask,
        error: Some(message),
        usage,
        iterations,
    }
}

/// 客户端在本轮 user prompt 已进入 canonical history 后断开。
///
/// 取消边界必须持久化在旧请求之后，让下一轮模型把后续 user 消息视为唯一
/// active request。边界是 harness notice，因此不会恢复成用户可见聊天内容。
fn cancel(session: &mut Session, progress: &mut RunProgress) -> RunOutcome {
    let final_text = progress.published.text.clone();
    progress.commit_cancelled(session);
    let already_marked = session.messages.last().is_some_and(|message| {
        message.role == "system"
            && message.harness_notice
            && message.content.as_deref() == Some(CANCELLED_REQUEST_BOUNDARY)
    });
    if !already_marked {
        session
            .messages
            .push(harness_notice(CANCELLED_REQUEST_BOUNDARY));
    }
    session.frame.set_status(FrameStatus::Cancelled);
    RunOutcome {
        kind: CompleteKind::Cancelled,
        final_text,
        awaiting: None,
        pending_ask: None,
        error: None,
        usage: progress.usage,
        iterations: progress.iterations,
    }
}

/// Start 投递失败时本轮 user prompt 尚未提交，不能污染既有历史。
fn cancel_before_prompt(session: &mut Session) -> RunOutcome {
    session.frame.set_status(FrameStatus::Cancelled);
    RunOutcome {
        kind: CompleteKind::Cancelled,
        final_text: String::new(),
        awaiting: None,
        pending_ask: None,
        error: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn native_delta(
        index: u32,
        id: Option<&str>,
        name: Option<&str>,
        arguments: Option<String>,
    ) -> dss_llm::ToolCallDelta {
        dss_llm::ToolCallDelta {
            index,
            id: id.map(str::to_string),
            name: name.map(str::to_string),
            arguments,
        }
    }

    #[test]
    fn native_tool_accumulator_is_ordered_and_accepts_stable_metadata() {
        let mut acc = ToolCallAccumulator::default();
        acc.push(native_delta(1, Some("b"), Some("two"), Some("{".into())))
            .unwrap();
        acc.push(native_delta(0, Some("a"), Some("one"), Some("{}".into())))
            .unwrap();
        acc.push(native_delta(1, Some("b"), Some("two"), Some("}".into())))
            .unwrap();
        let calls = acc.finalize().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "a");
        assert_eq!(calls[1].id, "b");
        assert_eq!(calls[1].function.arguments, "{}");
    }

    #[test]
    fn native_tool_accumulator_rejects_metadata_mutation_and_incomplete_calls() {
        let mut id_mutation = ToolCallAccumulator::default();
        id_mutation
            .push(native_delta(0, Some("a"), Some("tool"), None))
            .unwrap();
        assert_eq!(
            id_mutation.push(native_delta(0, Some("b"), None, None)),
            Err(NativeToolProtocolError::IdMutation)
        );

        let mut name_mutation = ToolCallAccumulator::default();
        name_mutation
            .push(native_delta(0, Some("a"), Some("one"), None))
            .unwrap();
        assert_eq!(
            name_mutation.push(native_delta(0, None, Some("two"), None)),
            Err(NativeToolProtocolError::NameMutation)
        );

        let mut incomplete = ToolCallAccumulator::default();
        incomplete
            .push(native_delta(0, Some("a"), None, Some("{}".into())))
            .unwrap();
        assert!(matches!(
            incomplete.finalize(),
            Err(NativeToolProtocolError::Incomplete)
        ));
    }

    #[test]
    fn native_tool_accumulator_accepts_byte_fragmented_write_file_arguments() {
        let content = "x".repeat(16 * 1024);
        let arguments = serde_json::json!({
            "path": "fast-reactor-ai-agenda.md",
            "content": content,
        })
        .to_string();
        assert!(arguments.len() > 4096);
        assert!((10 * 1024..=50 * 1024).contains(&arguments.len()));

        let mut acc = ToolCallAccumulator::default();
        acc.push(native_delta(
            0,
            Some("write-report"),
            Some("write_file"),
            None,
        ))
        .unwrap();
        for byte in arguments.bytes() {
            acc.push(native_delta(
                0,
                None,
                None,
                Some(char::from(byte).to_string()),
            ))
            .unwrap();
        }

        let calls = acc.finalize().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "write_file");
        assert_eq!(calls[0].function.arguments, arguments);
        let decoded: serde_json::Value =
            serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(decoded["content"].as_str(), Some(content.as_str()));
    }

    #[test]
    fn native_tool_accumulator_rejects_no_progress_and_replayed_metadata_spam() {
        let mut empty_spam = ToolCallAccumulator::default();
        for index in 0..MAX_NATIVE_TOOL_NO_PROGRESS_DELTAS_PER_TURN {
            empty_spam
                .push(native_delta(
                    0,
                    None,
                    None,
                    (index % 2 == 0).then(String::new),
                ))
                .unwrap();
        }
        assert_eq!(
            empty_spam.push(native_delta(0, None, None, None)),
            Err(NativeToolProtocolError::BoundaryExceeded)
        );

        let mut metadata_spam = ToolCallAccumulator::default();
        metadata_spam
            .push(native_delta(0, Some("stable"), Some("write_file"), None))
            .unwrap();
        for _ in 0..MAX_NATIVE_TOOL_NO_PROGRESS_DELTAS_PER_TURN {
            metadata_spam
                .push(native_delta(0, Some("stable"), Some("write_file"), None))
                .unwrap();
        }
        assert_eq!(
            metadata_spam.push(native_delta(0, Some("stable"), Some("write_file"), None,)),
            Err(NativeToolProtocolError::BoundaryExceeded)
        );
    }

    #[test]
    fn native_tool_accumulator_rejects_duplicate_ids_and_all_size_boundaries() {
        let mut duplicate = ToolCallAccumulator::default();
        duplicate
            .push(native_delta(0, Some("same"), Some("one"), None))
            .unwrap();
        duplicate
            .push(native_delta(1, Some("same"), Some("two"), None))
            .unwrap();
        assert!(matches!(
            duplicate.finalize(),
            Err(NativeToolProtocolError::DuplicateId)
        ));

        let mut too_many = ToolCallAccumulator::default();
        for index in 0..MAX_TOOL_CALLS_PER_TURN {
            too_many
                .push(native_delta(
                    index as u32,
                    Some(&format!("id-{index}")),
                    Some("tool"),
                    None,
                ))
                .unwrap();
        }
        assert_eq!(
            too_many.push(native_delta(99, Some("overflow"), Some("tool"), None)),
            Err(NativeToolProtocolError::BoundaryExceeded)
        );

        let mut oversized_id = ToolCallAccumulator::default();
        assert_eq!(
            oversized_id.push(native_delta(
                0,
                Some(&"i".repeat(MAX_TOOL_CALL_ID_BYTES + 1)),
                Some("tool"),
                None,
            )),
            Err(NativeToolProtocolError::BoundaryExceeded)
        );
        let mut oversized_name = ToolCallAccumulator::default();
        assert_eq!(
            oversized_name.push(native_delta(
                0,
                Some("id"),
                Some(&"n".repeat(MAX_TOOL_NAME_BYTES + 1)),
                None,
            )),
            Err(NativeToolProtocolError::BoundaryExceeded)
        );

        let mut per_call = ToolCallAccumulator::default();
        assert_eq!(
            per_call.push(native_delta(
                0,
                Some("large"),
                Some("tool"),
                Some("x".repeat(MAX_TOOL_ARGUMENT_BYTES_PER_CALL + 1)),
            )),
            Err(NativeToolProtocolError::BoundaryExceeded)
        );

        let mut total = ToolCallAccumulator::default();
        total
            .push(native_delta(
                0,
                Some("one"),
                Some("tool"),
                Some("x".repeat(MAX_TOOL_ARGUMENT_BYTES_PER_CALL)),
            ))
            .unwrap();
        total
            .push(native_delta(
                1,
                Some("two"),
                Some("tool"),
                Some("x".repeat(MAX_TOOL_ARGUMENT_BYTES_PER_CALL)),
            ))
            .unwrap();
        assert_eq!(
            total.push(native_delta(
                2,
                Some("three"),
                Some("tool"),
                Some("x".into()),
            )),
            Err(NativeToolProtocolError::BoundaryExceeded)
        );
    }

    #[test]
    fn repeated_cancel_only_appends_one_hidden_boundary() {
        let mut session = Session::new("cancel-dedupe", std::path::PathBuf::from("."));
        session.messages.push(ChatMessage::user("long task"));
        let mut progress = RunProgress::default();

        let _ = cancel(&mut session, &mut progress);
        let _ = cancel(&mut session, &mut progress);

        let boundaries: Vec<_> = session
            .messages
            .iter()
            .filter(|message| message.content.as_deref() == Some(CANCELLED_REQUEST_BOUNDARY))
            .collect();
        assert_eq!(boundaries.len(), 1);
        assert_eq!(boundaries[0].role, "system");
        assert!(boundaries[0].harness_notice);
    }

    #[test]
    fn approved_plan_requires_every_nonempty_step_to_be_done() {
        let plan = |statuses: &[&str]| dss_tools::PlanState {
            approved: true,
            research_question: None,
            steps: statuses
                .iter()
                .enumerate()
                .map(|(index, status)| dss_tools::PlanStep {
                    title: format!("step {index}"),
                    status: (*status).into(),
                })
                .collect(),
        };

        assert!(approved_plan_is_complete(Some(&plan(&["done", "DONE"]))));
        assert!(!approved_plan_is_complete(Some(&plan(&[
            "done", "pending"
        ]))));
        assert!(!approved_plan_is_complete(Some(&plan(&["failed"]))));
        assert!(!approved_plan_is_complete(Some(&plan(&[]))));
        let mut unapproved = plan(&["done"]);
        unapproved.approved = false;
        assert!(!approved_plan_is_complete(Some(&unapproved)));
        assert!(!approved_plan_is_complete(None));
    }

    #[test]
    fn explicit_iteration_limit_requires_nearby_limiting_language() {
        assert_eq!(MAX_ITERATIONS, 100);
        assert_eq!(
            parse_explicit_iteration_limit("Finish in ≤6 iterations and verify the report."),
            Some(6)
        );
        assert_eq!(
            parse_explicit_iteration_limit("最多 8 轮完成；如果够了就提前结束。"),
            Some(8)
        );
        assert_eq!(
            parse_explicit_iteration_limit("Use at most 4 agent iterations."),
            Some(4)
        );
        assert_eq!(
            parse_explicit_iteration_limit("Max 5 iterations for this run."),
            Some(5)
        );
        assert_eq!(
            parse_explicit_iteration_limit("不超过10个 agent iterations 完成。"),
            Some(10)
        );
        assert_eq!(
            parse_explicit_iteration_limit("最多 200 轮完成，完成后立即停止。"),
            Some(200)
        );
        assert_eq!(parse_explicit_iteration_limit("最多 0 轮完成。"), None);
        assert_eq!(parse_explicit_iteration_limit("最多 1001 轮完成。"), None);
        assert_eq!(parse_explicit_iteration_limit("此轮限 3 轮完成。"), Some(3));
        assert_eq!(
            parse_explicit_iteration_limit("There are 4 iterations in the input dataset."),
            None
        );
        assert_eq!(
            parse_explicit_iteration_limit("Compare 3 methods across 12 samples."),
            None
        );
        assert_eq!(
            parse_explicit_iteration_limit(
                "此轮最多 12 个 agent iterations（硬上限）\n\
                 1. 前 4 个 iteration：只用 fetch_url，不使用 web_search。\n\
                 2. 第 5 个 iteration：汇总已抓取证据。\n\
                 3. 第 6 个 iteration：更新三个文件。\n\
                 4. 第 7 个 iteration：逐一读回验证。\n\
                 5. iterations 1-4 属于抓取阶段。"
            ),
            Some(12)
        );
        assert_eq!(
            parse_explicit_iteration_limit(
                "前 4 个 iteration 只抓取；第 5 个 iteration 汇总；\
                 第 6 个 iteration 写作；第 7 个 iteration 验证；iterations 1-4 是阶段编号。"
            ),
            None
        );
        assert_eq!(
            parse_explicit_iteration_limit(
                "Use at most 9 agent iterations, and no more than 6 iterations if review is enabled."
            ),
            Some(6)
        );
        assert_eq!(
            parse_explicit_iteration_limit("The climax 4 iterations were discussed."),
            None
        );
    }

    #[test]
    fn explicit_iteration_limit_can_only_narrow_the_configured_budget() {
        assert_eq!(effective_iteration_limit(None, 160), (160, false));
        assert_eq!(effective_iteration_limit(Some(40), 160), (40, true));
        assert_eq!(effective_iteration_limit(Some(200), 160), (160, false));
        assert_eq!(effective_iteration_limit(Some(200), 640), (200, true));
    }

    #[test]
    fn science_policy_covers_observed_cross_domain_failures() {
        assert!(SCIENCE_EXECUTION_POLICY.contains("smallest cheap validation"));
        assert!(SCIENCE_EXECUTION_POLICY.contains("post-hoc/exploratory"));
        assert!(SCIENCE_EXECUTION_POLICY.contains("zero exceedances"));
        assert!(SCIENCE_EXECUTION_POLICY.contains("actual tool trace"));
        assert!(SCIENCE_EXECUTION_POLICY.contains("Respect explicit user limits"));
    }

    #[test]
    fn mcp_resource_reads_count_as_retrieval_but_agent_calls_do_not() {
        assert!(is_retrieval_tool("mcp_list_resources"));
        assert!(is_retrieval_tool("mcp_read_resource"));
        assert!(!is_retrieval_tool("call_agent"));
        assert!(!is_retrieval_tool("mcp__agent_registry__mutating_tool"));
    }
}
