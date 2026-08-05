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
        self.folds
            .iter()
            .map(|f| f.end_idx.saturating_sub(f.start_idx))
            .sum()
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
    // maybe_compact is invoked after Runner appends the current user prompt. Even a legacy fold
    // that used to run through the log tail must never hide that active turn.
    let active_turn_start = messages
        .iter()
        .rposition(|message| message.role == "user")
        .unwrap_or(messages.len());
    for fold in &state.folds {
        // fold 区间必须在日志范围内（end_idx 可等于 len）。
        let (start, end) = align_fold_range(messages, fold.start_idx, fold.end_idx);
        let end = end.min(active_turn_start);
        if start >= active_turn_start {
            continue;
        }
        if start < cursor {
            // Legacy folds may overlap after turn-boundary normalization. If this fold extends
            // the covered union, keep its summary and advance cursor; otherwise it is contained
            // entirely in an earlier fold and is redundant.
            if end > cursor {
                out.push(ChatMessage::assistant(&fold.summary));
                cursor = end;
            }
            continue;
        }
        if end <= start {
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

/// 把任意（包括旧版本留下的）fold 边界归一化到完整 user turn 之外。
///
/// 一个 turn 从 role=user 开始，到下一条 role=user 之前结束；它自然包含中间所有
/// assistant tool_calls、tool results、review/continuation notice 和最终 assistant。新的
/// chunk picker 本身不会拆 turn；projection 再做一次防御性归一化，确保运行中的旧
/// CompactionState 既不产生孤立 tool result，也不把请求与回答拆开。
pub(crate) fn align_fold_range(
    messages: &[ChatMessage],
    start: usize,
    end: usize,
) -> (usize, usize) {
    let mut aligned_start = start.min(messages.len());
    if aligned_start < messages.len() && messages[aligned_start].role != "user" {
        while aligned_start > 0 {
            aligned_start -= 1;
            if messages[aligned_start].role == "user" {
                break;
            }
        }
    }
    let aligned_end = extend_end_to_turn_boundary(messages, end.max(aligned_start));
    (aligned_start, aligned_end)
}

/// Extend an exclusive fold end to the next user-turn boundary.
pub(crate) fn extend_end_to_turn_boundary(messages: &[ChatMessage], end: usize) -> usize {
    let mut aligned_end = end.min(messages.len());
    if aligned_end == 0 || aligned_end == messages.len() || messages[aligned_end].role == "user" {
        return aligned_end;
    }
    while aligned_end < messages.len() && messages[aligned_end].role != "user" {
        aligned_end += 1;
    }
    aligned_end
}

#[cfg(test)]
mod tests {
    use super::*;
    use dss_llm::ToolCall;

    fn msgs(n: usize) -> Vec<ChatMessage> {
        (0..n)
            .map(|i| ChatMessage::user(format!("msg{i}")))
            .collect()
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
        st.record_fold(Fold {
            start_idx: 2,
            end_idx: 7,
            summary: "SUMMARY".into(),
            level: 1,
        });
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
        st.record_fold(Fold {
            start_idx: 1,
            end_idx: 3,
            summary: "S1".into(),
            level: 1,
        });
        st.record_fold(Fold {
            start_idx: 6,
            end_idx: 9,
            summary: "S2".into(),
            level: 1,
        });
        let view = projection(&m, &st);
        // msg0, S1, msg3,msg4,msg5, S2, msg9 = 7
        assert_eq!(view.len(), 7);
        assert_eq!(view[1].content.as_deref(), Some("S1"));
        assert_eq!(view[5].content.as_deref(), Some("S2"));
    }

    #[test]
    fn projection_repairs_legacy_fold_end_inside_tool_transaction() {
        let messages = vec![
            ChatMessage::user("old request"),
            ChatMessage::assistant_tool_calls(vec![
                ToolCall::function("call_1", "read_file", "{}".into()),
                ToolCall::function("call_2", "list_files", "{}".into()),
            ]),
            ChatMessage::tool("call_1", "first", Some("read_file".into())),
            ChatMessage::tool("call_2", "second", Some("list_files".into())),
            ChatMessage::user("new request"),
        ];
        let mut state = CompactionState::new();
        // Old picker could stop immediately after the assistant tool_calls message.
        state.record_fold(Fold {
            start_idx: 0,
            end_idx: 2,
            summary: "complete old transaction".into(),
            level: 1,
        });

        let view = projection(&messages, &state);
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].content.as_deref(), Some("complete old transaction"));
        assert_eq!(view[1].content.as_deref(), Some("new request"));
        assert!(view.iter().all(|message| message.role != "tool"));
    }

    #[test]
    fn projection_repairs_legacy_fold_start_inside_user_turn() {
        let messages = vec![
            ChatMessage::user("prefix"),
            ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                "call_1",
                "read_file",
                "{}".into(),
            )]),
            ChatMessage::tool("call_1", "result", Some("read_file".into())),
            ChatMessage::user("tail"),
        ];
        let mut state = CompactionState::new();
        state.record_fold(Fold {
            start_idx: 2,
            end_idx: 3,
            summary: "tool transaction".into(),
            level: 1,
        });

        let view = projection(&messages, &state);
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].content.as_deref(), Some("tool transaction"));
        assert_eq!(view[1].content.as_deref(), Some("tail"));
        assert!(view.iter().all(|message| message.role != "tool"));
    }

    #[test]
    fn projection_skips_legacy_folds_that_overlap_after_turn_alignment() {
        let messages = vec![
            ChatMessage::user("old request"),
            ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                "call_1",
                "read_file",
                "{}".into(),
            )]),
            ChatMessage::tool("call_1", "result", Some("read_file".into())),
            ChatMessage::assistant("old final"),
            ChatMessage::user("second request"),
            ChatMessage::assistant("second final"),
            ChatMessage::user("active request"),
        ];
        let mut state = CompactionState::new();
        state.record_fold(Fold {
            start_idx: 0,
            end_idx: 2,
            summary: "canonical summary".into(),
            level: 1,
        });
        state.record_fold(Fold {
            start_idx: 2,
            end_idx: 5,
            summary: "overlapping legacy summary".into(),
            level: 1,
        });

        let view = projection(&messages, &state);
        assert_eq!(view.len(), 3);
        assert_eq!(view[0].content.as_deref(), Some("canonical summary"));
        assert_eq!(
            view[1].content.as_deref(),
            Some("overlapping legacy summary")
        );
        assert_eq!(view[2].content.as_deref(), Some("active request"));
        assert!(view.iter().all(|message| message.role != "tool"));
    }

    #[test]
    fn projection_never_hides_latest_active_user_turn_from_legacy_tail_fold() {
        let messages = vec![
            ChatMessage::user("old request"),
            ChatMessage::assistant("old answer"),
            ChatMessage::user("active request"),
        ];
        let mut state = CompactionState::new();
        state.record_fold(Fold {
            start_idx: 0,
            end_idx: messages.len(),
            summary: "legacy tail summary".into(),
            level: 1,
        });

        let view = projection(&messages, &state);
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].content.as_deref(), Some("legacy tail summary"));
        assert_eq!(view[1].role, "user");
        assert_eq!(view[1].content.as_deref(), Some("active request"));
    }
}
