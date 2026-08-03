//! chunk 选择：L1/L2 触发判断 + pick_next_chunk。
//!
//! 常量与机制严格遵循 modules.md §8。P4a 暂无 boundary 工具，chunk 按消息边界 +
//! MIN_CHUNK_TOKENS 选；留 task_boundary 对齐接入点（P4b-gates）。

use crate::constants::{
    COMPACTION_TRIGGER_RATIO, KA_FLOOR, KB_RATIO, L2_HEAD_TOKENS_FLOOR, L2_HEAD_TOKENS_KA_RATIO,
    L2_MIN_L1_SUMMARIES, MIN_CHUNK_TOKENS,
};
use crate::state::CompactionState;
use crate::tokens::estimate_messages_tokens;
use dss_llm::ChatMessage;

/// kept-available 目标 token 数：max(KA_FLOOR, context_window * KB_RATIO)。
pub fn kept_available_target(context_window: usize) -> usize {
    (KA_FLOOR).max(((context_window as f64) * KB_RATIO) as usize)
}

/// 当前是否到达「触发压缩」阈值：已用 token ≥ context_window * COMPACTION_TRIGGER_RATIO。
pub fn is_over_trigger(total_tokens: usize, context_window: usize) -> bool {
    let trigger = ((context_window as f64) * COMPACTION_TRIGGER_RATIO) as usize;
    total_tokens >= trigger
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
    // 找最早可折叠段（未被已有 fold 覆盖）。
    let Some(candidate) = pick_next_chunk(messages, state) else {
        return false;
    };
    let chunk_tokens = estimate_messages_tokens(&messages[candidate.0..candidate.1]);
    if chunk_tokens < MIN_CHUNK_TOKENS {
        return false;
    }
    let ka = kept_available_target(context_window);
    // 「剩余」= 总 token - 该 chunk token（折叠后约略量）。
    let total = estimate_messages_tokens(messages);
    let remaining = total.saturating_sub(chunk_tokens);
    let threshold = ((ka as f64) * 0.7) as usize;
    remaining < threshold
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
    let last_fold_end = state.folds.last().map(|f| f.end_idx).unwrap_or(0);
    let start = last_fold_end.min(messages.len());
    estimate_messages_tokens(&messages[start..])
}

/// 选下一个待折叠 chunk：最早一段未被已有 fold 覆盖的连续消息，且累积到 ≥ MIN_CHUNK_TOKENS。
/// 返回 (start, end)（end 可能 == messages.len() 或达到 token 下限的最早位置）。
///
/// P4a 不做 task_boundary 对齐（无 boundary 工具）；按 MIN_CHUNK_TOKENS 累积到刚好达标。
pub fn pick_next_chunk(
    messages: &[ChatMessage],
    state: &CompactionState,
) -> Option<(usize, usize)> {
    // 起点：第一个未被 fold 覆盖的消息。
    let start = first_unfolded_idx(messages.len(), state)?;
    if start >= messages.len() {
        return None;
    }
    // 累积直到 ≥ MIN_CHUNK_TOKENS。
    let mut acc = 0usize;
    let mut end = start;
    while end < messages.len() {
        acc += crate::tokens::estimate_message_tokens(&messages[end]);
        end += 1;
        if acc >= MIN_CHUNK_TOKENS {
            return Some((start, end));
        }
    }
    // 不到 MIN_CHUNK_TOKENS：P4a 仍返回这段（调用方按 chunk_tokens < MIN_CHUNK_TOKENS 不触发）。
    if end > start { Some((start, end)) } else { None }
}

/// 第一个未被任何 fold 覆盖的日志索引。
fn first_unfolded_idx(len: usize, state: &CompactionState) -> Option<usize> {
    if state.folds.is_empty() {
        return if len == 0 { None } else { Some(0) };
    }
    // folds 升序；最后一个 fold 的 end 之后即首个未折叠点。
    let last_end = state.folds.last()?.end_idx;
    if last_end < len {
        Some(last_end)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Fold;

    fn big_msg(tokens: usize) -> ChatMessage {
        // tokens*4 字符。
        ChatMessage::user("x".repeat(tokens * 4))
    }

    #[test]
    fn trigger_l1_when_chunk_big_and_remaining_low() {
        // context_window = 10000；ka = max(50000, 7000) = 50000；ka*0.7=35000
        // 构造：2 条 5000 token 的消息（总 10000）。
        let cw = 10000;
        let msgs = vec![big_msg(5000), big_msg(5000)];
        let st = CompactionState::new();
        // candidate = [0, 1)（5000 token ≥ 4096）；remaining = 10000-5000 = 5000 < 35000 → 触发
        assert!(should_trigger_l1(&msgs, &st, cw));
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
        st.record_fold(Fold { start_idx: 0, end_idx: 3, summary: "s".into(), level: 1 });
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
        st.record_fold(Fold { start_idx: 0, end_idx: 1, summary: "s".into(), level: 1 });
        let (s, e) = pick_next_chunk(&msgs, &st).unwrap();
        assert_eq!(s, 1);
        assert!(e >= 2);
    }
}
