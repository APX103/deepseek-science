//! CompactionState + projection（非破坏性：日志 append-only，projection 决定给 LLM 看的视图）。
//!
//! P4a 用索引范围 fold（modules.md 用 applied_summary_uuids + 带 uuid 的 Message；
//! 当前 ChatMessage 无 uuid，故索引版，登记 D-F12）。

use dss_llm::ChatMessage;

/// 一个被折叠的区间：日志的 `[start_idx, end_idx)` 被 summary 消息替代（在 projection 里）。
#[derive(Debug, Clone)]
pub struct Fold {
    /// 折叠区间起点（含），日志索引。
    pub start_idx: usize,
    /// 折叠区间终点（不含）。
    pub end_idx: usize,
    /// 该区间的 summary 文本（projection 时构造成 assistant 消息插入）。
    pub summary: String,
    /// 折叠层级：1=L1，2=L2。
    pub level: u8,
}

/// Session 上的压缩视图态。日志（session.messages）不被 mutate。
#[derive(Debug, Clone, Default)]
pub struct CompactionState {
    /// 已折叠的区间（按 start_idx 升序，互不重叠）。
    pub folds: Vec<Fold>,
    /// 已完成的 L1 summary 数（L2 触发判断用）。
    pub l1_summary_count: usize,
}

impl CompactionState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次 fold（保持升序、不重叠）。
    pub fn record_fold(&mut self, fold: Fold) {
        if fold.level == 1 {
            self.l1_summary_count += 1;
        }
        self.folds.push(fold);
        self.folds.sort_by_key(|f| f.start_idx);
    }

    /// 折叠区间覆盖的日志消息数。
    pub fn folded_message_count(&self) -> usize {
        self.folds.iter().map(|f| f.end_idx.saturating_sub(f.start_idx)).sum()
    }
}

/// 把日志投影成给 LLM 的视图：每个 fold 区间替换成一条 summary（assistant）消息。
/// 不在任一 fold 区间内的消息原样保留，顺序保持。
pub fn projection(messages: &[ChatMessage], state: &CompactionState) -> Vec<ChatMessage> {
    if state.folds.is_empty() {
        return messages.to_vec();
    }
    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    let mut cursor = 0usize;
    for fold in &state.folds {
        // fold 区间必须在日志范围内（end_idx 可等于 len）。
        let start = fold.start_idx.min(messages.len());
        let end = fold.end_idx.min(messages.len());
        if start < cursor {
            // 重叠（不应发生）——跳过。
            continue;
        }
        // 先追加 [cursor, start) 的原样消息。
        out.extend_from_slice(&messages[cursor..start]);
        // 再追加 summary 消息（assistant，harness 语义：RC summary）。
        out.push(ChatMessage::assistant(&fold.summary));
        cursor = end.max(cursor);
    }
    // 追加尾部剩余。
    if cursor < messages.len() {
        out.extend_from_slice(&messages[cursor..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(n: usize) -> Vec<ChatMessage> {
        (0..n).map(|i| ChatMessage::user(format!("msg{i}"))).collect()
    }

    #[test]
    fn projection_empty_state_is_identity() {
        let m = msgs(5);
        assert_eq!(projection(&m, &CompactionState::new()).len(), 5);
    }

    #[test]
    fn projection_folds_middle_chunk_into_summary() {
        let m = msgs(10); // 索引 0..10
        let mut st = CompactionState::new();
        st.record_fold(Fold { start_idx: 2, end_idx: 7, summary: "SUMMARY".into(), level: 1 });
        let view = projection(&m, &st);
        // [0,2) 2 条 + summary 1 条 + [7,10) 3 条 = 6
        assert_eq!(view.len(), 6);
        assert_eq!(view[2].content.as_deref(), Some("SUMMARY"));
        assert_eq!(view[0].content.as_deref(), Some("msg0"));
        assert_eq!(view[1].content.as_deref(), Some("msg1"));
        assert_eq!(view[3].content.as_deref(), Some("msg7"));
        // 日志 append-only：长度不变。
        assert_eq!(m.len(), 10);
    }

    #[test]
    fn projection_multiple_folds_preserve_order() {
        let m = msgs(10);
        let mut st = CompactionState::new();
        st.record_fold(Fold { start_idx: 1, end_idx: 3, summary: "S1".into(), level: 1 });
        st.record_fold(Fold { start_idx: 6, end_idx: 9, summary: "S2".into(), level: 1 });
        let view = projection(&m, &st);
        // msg0, S1, msg3,msg4,msg5, S2, msg9 = 7
        assert_eq!(view.len(), 7);
        assert_eq!(view[1].content.as_deref(), Some("S1"));
        assert_eq!(view[5].content.as_deref(), Some("S2"));
    }
}
