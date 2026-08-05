//! chunk 选择：L1/L2 触发判断 + pick_next_chunk。
//!
//! 常量与机制严格遵循 modules.md §8。没有显式 task_boundary 时，至少按完整 user
//! turn 对齐；这同时保证 assistant tool_calls + tool results 不会被拆开。

use crate::constants::{
    ABSOLUTE_TOKEN_CEILING, COMPACTION_TRIGGER_RATIO, HARD_WALL_RATIO, KA_FLOOR, KB_RATIO,
    L2_HEAD_TOKENS_FLOOR, L2_HEAD_TOKENS_KA_RATIO, L2_MIN_L1_SUMMARIES, MIN_CHUNK_TOKENS,
};
use crate::state::{align_fold_range, extend_end_to_turn_boundary, projection, CompactionState};
use crate::tokens::estimate_messages_tokens;
use dss_llm::ChatMessage;

/// kept-available 目标 token 数：max(KA_FLOOR, context_window * KB_RATIO)。
pub fn kept_available_target(context_window: usize) -> usize {
    (KA_FLOOR).max(((context_window as f64) * KB_RATIO) as usize)
}

/// 启动压缩的 token 阈值。
///
/// `ABSOLUTE_TOKEN_CEILING` 是所有模型窗口共享的绝对上限，因此大窗口也必须在
/// 300k 触发，而不能等到 `context_window * 0.75`。
pub fn compaction_trigger_tokens(context_window: usize) -> usize {
    ratio_tokens(context_window, COMPACTION_TRIGGER_RATIO).min(ABSOLUTE_TOKEN_CEILING)
}

/// 发给模型前允许的历史硬墙（不含 tool schema 的廉价估计）。
pub fn hard_wall_tokens(context_window: usize) -> usize {
    ratio_tokens(context_window, HARD_WALL_RATIO).min(ABSOLUTE_TOKEN_CEILING)
}

/// L1 希望把 projection 压回的目标。
///
/// 原设计里的“剩余 < ka*0.7”指的是窗口中剩余的**可用容量**，而不是未折叠
/// 消息量。换算后，已用目标是 `context_window - ka*0.7`；同时必须严格低于
/// 硬墙，给下一轮留出至少一个 token 的前向进展空间。
pub fn projection_token_target(context_window: usize) -> usize {
    let desired_available = ratio_tokens(kept_available_target(context_window), 0.7);
    let capacity_target = context_window.saturating_sub(desired_available.min(context_window));
    capacity_target.min(hard_wall_tokens(context_window).saturating_sub(1))
}

/// 当前是否到达压缩阈值。传入的 token 必须是当前 projection（加本轮保留上下文），
/// 不能是 append-only 原始日志，否则已有 fold 会在每轮被重复计算。
pub fn is_over_trigger(total_tokens: usize, context_window: usize) -> bool {
    let trigger = compaction_trigger_tokens(context_window);
    if trigger == 0 {
        total_tokens > 0
    } else {
        total_tokens >= trigger
    }
}

/// L1 触发：能选出一段 chunk（≥ MIN_CHUNK_TOKENS），且「折叠它之后剩余 < ka*0.7」
/// （即剩余的活跃上下文不足 kept-available 的 70%，需要继续压缩）。
///
/// 实现语义：从日志里找最早的一段未被折叠、且 token 数 ≥ MIN_CHUNK_TOKENS 的连续消息
/// 作为候选 chunk；若折叠它后仍超压（剩余 < ka*0.7），则「应触发」更多 L1。
pub fn should_trigger_l1(
    messages: &[ChatMessage],
    state: &CompactionState,
    context_window: usize,
) -> bool {
    should_trigger_l1_with_reserved(messages, state, context_window, 0)
}

/// 与 [`should_trigger_l1`] 相同，但把本轮不可折叠的 system/memory 上下文计入预算。
pub fn should_trigger_l1_with_reserved(
    messages: &[ChatMessage],
    state: &CompactionState,
    context_window: usize,
    reserved_tokens: usize,
) -> bool {
    // 找最早可折叠段（未被已有 fold 覆盖）。
    let Some(candidate) = pick_next_chunk(messages, state) else {
        return false;
    };
    let chunk_tokens = estimate_messages_tokens(&messages[candidate.0..candidate.1]);
    if chunk_tokens < MIN_CHUNK_TOKENS {
        return false;
    }
    let projected_tokens = estimate_messages_tokens(&projection(messages, state));
    projected_tokens.saturating_add(reserved_tokens) > projection_token_target(context_window)
}

/// L2 触发：已累计 ≥ L2_MIN_L1_SUMMARIES 个 L1 summary，且 head tokens（最近活跃段）
/// ≥ max(L2_HEAD_TOKENS_FLOOR, ka * L2_HEAD_TOKENS_KA_RATIO)。
pub fn should_trigger_l2(
    messages: &[ChatMessage],
    state: &CompactionState,
    context_window: usize,
) -> bool {
    if state.l1_summary_count < L2_MIN_L1_SUMMARIES {
        return false;
    }
    let ka = kept_available_target(context_window);
    let head_floor = L2_HEAD_TOKENS_FLOOR.max(((ka as f64) * L2_HEAD_TOKENS_KA_RATIO) as usize);
    // head = 最后一段未折叠消息的 token 数。
    let head = head_tokens(messages, state);
    head >= head_floor
}

/// 最近一段未折叠消息（head）的 token 数。
fn head_tokens(messages: &[ChatMessage], state: &CompactionState) -> usize {
    let last_fold_end = state
        .folds
        .iter()
        .map(|fold| align_fold_range(messages, fold.start_idx, fold.end_idx).1)
        .max()
        .unwrap_or(0);
    let start = last_fold_end.min(messages.len());
    estimate_messages_tokens(&messages[start..])
}

/// 选下一个待折叠 chunk：最早一段未被已有 fold 覆盖的连续消息，且累积到 ≥ MIN_CHUNK_TOKENS。
/// 返回 (start, end)，且 start/end 必须位于完整 user turn 的边界。
pub fn pick_next_chunk(
    messages: &[ChatMessage],
    state: &CompactionState,
) -> Option<(usize, usize)> {
    pick_next_chunk_with_min_tokens(messages, state, MIN_CHUNK_TOKENS)
}

/// 选下一个至少达到 `min_tokens` 的 chunk。
///
/// 每个 role=user 到下一条 role=user 之前是一个不可拆 turn，其中包含所有 assistant
/// `tool_calls`、role=tool 结果和最终回复。最新 user turn 是当前正在执行的请求，永不
/// 折叠；如果它自己超过硬墙，Runner 会 fail-closed，而不是摘要掉用户当前指令。
pub fn pick_next_chunk_with_min_tokens(
    messages: &[ChatMessage],
    state: &CompactionState,
    min_tokens: usize,
) -> Option<(usize, usize)> {
    let foldable_end = latest_user_turn_start(messages);
    // Select only within the earliest gap in the normalized fold union. Runtime folds are
    // contiguous, but this also keeps imported/legacy gaps and overlaps consistent with
    // projection instead of accidentally crossing an already-folded range.
    let (start, span_end) = first_unfolded_span(messages, state, foldable_end)?;
    if start >= span_end {
        return None;
    }
    // 累积直到达到调用方目标；MIN_CHUNK_TOKENS 始终是下限。
    let min_tokens = min_tokens.max(MIN_CHUNK_TOKENS);
    let mut acc = 0usize;
    let mut end = start;
    while end < span_end {
        acc += crate::tokens::estimate_message_tokens(&messages[end]);
        end += 1;
        if acc >= min_tokens {
            let aligned_end = extend_end_to_turn_boundary(messages, end).min(span_end);
            return Some((start, aligned_end));
        }
    }
    // 不到 MIN_CHUNK_TOKENS：P4a 仍返回这段（调用方按 chunk_tokens < MIN_CHUNK_TOKENS 不触发）。
    if end > start {
        Some((start, end))
    } else {
        None
    }
}

/// The newest user message begins the active turn and is the exclusive foldable-history end.
/// Histories without a user message (legacy/imported data) may fold through their full length.
fn latest_user_turn_start(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .rposition(|message| message.role == "user")
        .unwrap_or(messages.len())
}

#[cfg(test)]
fn has_tool_calls(message: &ChatMessage) -> bool {
    message.role == "assistant"
        && message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
}

fn ratio_tokens(tokens: usize, ratio: f64) -> usize {
    ((tokens as f64) * ratio).round() as usize
}

/// Earliest unfolded interval within `[0, foldable_end)`, after normalizing legacy ranges.
fn first_unfolded_span(
    messages: &[ChatMessage],
    state: &CompactionState,
    foldable_end: usize,
) -> Option<(usize, usize)> {
    if foldable_end == 0 {
        return None;
    }
    let mut ranges: Vec<(usize, usize)> = state
        .folds
        .iter()
        .map(|fold| align_fold_range(messages, fold.start_idx, fold.end_idx))
        .filter(|(start, end)| start < end)
        .collect();
    ranges.sort_by_key(|(start, end)| (*start, *end));

    let mut cursor = 0usize;
    for (start, end) in ranges {
        let start = start.min(foldable_end);
        let end = end.min(foldable_end);
        if start > cursor {
            return Some((cursor, start));
        }
        cursor = cursor.max(end);
        if cursor >= foldable_end {
            return None;
        }
    }
    Some((cursor, foldable_end)).filter(|(start, end)| start < end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Fold;
    use dss_llm::ToolCall;
    use std::collections::HashSet;

    fn big_msg(tokens: usize) -> ChatMessage {
        // tokens*4 字符。
        ChatMessage::user("x".repeat(tokens * 4))
    }

    #[test]
    fn trigger_l1_when_chunk_big_and_remaining_low() {
        // context_window = 10000；kept-available 目标大于窗口，所以应尽可能折叠旧历史。
        let cw = 10000;
        let msgs = vec![big_msg(5000), big_msg(5000)];
        let st = CompactionState::new();
        assert!(should_trigger_l1(&msgs, &st, cw));
    }

    #[test]
    fn default_window_uses_absolute_ceiling_for_trigger_and_hard_wall() {
        let cw = 500_000;
        assert_eq!(compaction_trigger_tokens(cw), 300_000);
        assert_eq!(hard_wall_tokens(cw), 300_000);
        assert_eq!(projection_token_target(cw), 255_000);
        assert!(!is_over_trigger(299_999, cw));
        assert!(is_over_trigger(300_000, cw));
    }

    #[test]
    fn no_trigger_l1_when_chunk_too_small() {
        let cw = 10000;
        let msgs = vec![big_msg(100), big_msg(100)]; // 远 < MIN_CHUNK_TOKENS
        let st = CompactionState::new();
        // pick_next_chunk 返回一段但 chunk_tokens=200 < 4096 → 不触发
        assert!(!should_trigger_l1(&msgs, &st, cw));
    }

    #[test]
    fn trigger_l2_when_enough_l1_and_head_big() {
        let cw = 200_000; // ka = max(50000, 140000) = 140000；head_floor = max(8192, 56000) = 56000
        let mut msgs = vec![];
        // 一段已折叠的旧消息（不影响 head）+ 一段大的 head。
        for _ in 0..3 {
            msgs.push(big_msg(5000));
        }
        let mut st = CompactionState::new();
        st.record_fold(Fold {
            start_idx: 0,
            end_idx: 3,
            summary: "s".into(),
            level: 1,
        });
        st.l1_summary_count = 3;
        // head：3 条 5000 token = 15000，但 head_floor=56000 → 不触发
        assert!(!should_trigger_l2(&msgs, &st, cw));
        // 加大到 head ≥ 56000：12 条 5000 = 60000
        for _ in 0..12 {
            msgs.push(big_msg(5000));
        }
        assert!(should_trigger_l2(&msgs, &st, cw));
    }

    #[test]
    fn pick_next_chunk_respects_existing_folds() {
        let msgs = vec![big_msg(5000), big_msg(5000), big_msg(5000)];
        let mut st = CompactionState::new();
        st.record_fold(Fold {
            start_idx: 0,
            end_idx: 1,
            summary: "s".into(),
            level: 1,
        });
        let (s, e) = pick_next_chunk(&msgs, &st).unwrap();
        assert_eq!(s, 1);
        assert!(e >= 2);
    }

    #[test]
    fn pick_next_chunk_never_splits_a_tool_transaction() {
        let calls = vec![
            ToolCall::function("call_1", "read_file", "x".repeat(800)),
            ToolCall::function("call_2", "write_file", "y".repeat(800)),
        ];
        let messages = vec![
            big_msg(4000),
            ChatMessage::assistant_tool_calls(calls),
            ChatMessage::tool("call_1", "a".repeat(1600), Some("read_file".into())),
            ChatMessage::tool("call_2", "b".repeat(1600), Some("write_file".into())),
            ChatMessage::user("next turn"),
        ];
        let state = CompactionState::new();

        // Force the nominal boundary after the assistant, the first tool result, and the
        // second tool result. Every cut must expand to the same complete transaction end.
        for nominal_end in 2..=4 {
            let requested = estimate_messages_tokens(&messages[..nominal_end]);
            let (start, end) =
                pick_next_chunk_with_min_tokens(&messages, &state, requested).unwrap();
            assert_eq!((start, end), (0, 4), "nominal end {nominal_end}");
        }

        let mut folded = CompactionState::new();
        folded.record_fold(Fold {
            start_idx: 0,
            end_idx: 4,
            summary: "tool transaction summary".into(),
            level: 1,
        });
        assert_valid_tool_protocol(&projection(&messages, &folded));
    }

    #[test]
    fn boundary_after_user_expands_through_complete_tool_turn() {
        let messages = vec![
            big_msg(5000),
            ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                "call_1",
                "read_file",
                "{}".into(),
            )]),
            ChatMessage::tool("call_1", "ok", Some("read_file".into())),
            ChatMessage::user("continue"),
        ];
        let state = CompactionState::new();
        let (start, end) = pick_next_chunk(&messages, &state).unwrap();
        assert_eq!((start, end), (0, 3));

        let mut folded = CompactionState::new();
        folded.record_fold(Fold {
            start_idx: start,
            end_idx: end,
            summary: "old context".into(),
            level: 1,
        });
        assert_valid_tool_protocol(&projection(&messages, &folded));
    }

    #[test]
    fn latest_active_user_turn_is_never_selected_for_compaction() {
        let messages = vec![
            big_msg(5000),
            ChatMessage::assistant("old answer".repeat(20_000)),
            big_msg(50_000),
        ];
        let state = CompactionState::new();

        let (start, end) = pick_next_chunk_with_min_tokens(&messages, &state, 100_000).unwrap();
        assert_eq!((start, end), (0, 2));
        assert_eq!(messages[end].role, "user");
        assert_eq!(end, latest_user_turn_start(&messages));

        let mut only_active = CompactionState::new();
        only_active.record_fold(Fold {
            start_idx: 0,
            end_idx: 2,
            summary: "old turn".into(),
            level: 1,
        });
        assert!(pick_next_chunk(&messages, &only_active).is_none());
    }

    #[test]
    fn next_chunk_starts_after_turn_aligned_legacy_overlap() {
        let messages = vec![
            ChatMessage::user("old request"),
            ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                "call_1",
                "read_file",
                "{}".into(),
            )]),
            ChatMessage::tool("call_1", "result", Some("read_file".into())),
            ChatMessage::assistant("old final"),
            big_msg(5000),
            ChatMessage::assistant("second answer"),
            ChatMessage::user("active request"),
        ];
        let mut state = CompactionState::new();
        state.record_fold(Fold {
            start_idx: 0,
            end_idx: 2,
            summary: "first legacy summary".into(),
            level: 1,
        });
        state.record_fold(Fold {
            start_idx: 2,
            end_idx: 3,
            summary: "overlap".into(),
            level: 1,
        });

        let (start, end) = pick_next_chunk(&messages, &state).unwrap();
        assert_eq!((start, end), (4, 6));
        assert_eq!(messages[start].role, "user");
        assert_eq!(messages[end].role, "user");
    }

    fn assert_valid_tool_protocol(messages: &[ChatMessage]) {
        let mut pending: Option<HashSet<String>> = None;
        for message in messages {
            if has_tool_calls(message) {
                assert!(pending.is_none(), "nested assistant tool transaction");
                pending = Some(
                    message
                        .tool_calls
                        .as_ref()
                        .unwrap()
                        .iter()
                        .map(|call| call.id.clone())
                        .collect(),
                );
                continue;
            }
            if message.role == "tool" {
                let ids = pending.as_mut().expect("orphan tool result");
                let id = message.tool_call_id.as_ref().expect("tool result id");
                assert!(ids.remove(id), "unexpected or duplicate tool result {id}");
                continue;
            }
            if let Some(ids) = pending.take() {
                assert!(ids.is_empty(), "assistant tool call missing result");
            }
        }
        if let Some(ids) = pending {
            assert!(
                ids.is_empty(),
                "assistant tool call missing terminal result"
            );
        }
    }
}
