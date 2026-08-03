//! dss-compact: Rolling Compact（非破坏性上下文压缩）。
//!
//! 常量与机制严格遵循 modules.md §8。P4a：索引版 projection + L1 fold（L2/boundary 留 P4b-gates）。
//! 三层记忆留 P4b。

pub mod chunk;
pub mod constants;
pub mod microcompact;
pub mod state;
pub mod summarizer;
pub mod tokens;

use dss_llm::{ChatMessage, LlmClient};

pub use state::{projection, CompactionState, Fold};

/// maybe_compact 的结果。
#[derive(Debug, Clone, Default)]
pub struct CompactionOutcome {
    /// 本次是否触发了折叠（L1）。
    pub folded: bool,
    /// 本次产生的 fold 数。
    pub folds_added: usize,
}

/// 主入口：在每轮 LLM 前调用。判断是否触发 L1 → 选 chunk → summarize → 记 fold。
///
/// - 不 mutate `messages`（日志 append-only）；fold 记进 `state`。
/// - 调用方随后用 `projection(messages, state)` 得到给 LLM 的视图。
/// - 短对话（未过触发阈值 / chunk 不足）直接返回，不动任何东西。
pub async fn maybe_compact(
    messages: &[ChatMessage],
    state: &mut CompactionState,
    llm: &dyn LlmClient,
    model: &str,
    context_window: usize,
) -> CompactionOutcome {
    let total = tokens::estimate_messages_tokens(messages);
    if !chunk::is_over_trigger(total, context_window) {
        return CompactionOutcome::default();
    }

    let mut folds_added = 0usize;
    // 循环触发 L1，直到不再需要或无候选。
    // 安全上限：避免极端情况无限折叠（与 PTL_RETRY_CAP 同量级）。
    let safety = constants::PTL_RETRY_CAP;
    let mut guard = 0usize;
    while guard < safety {
        guard += 1;
        if !chunk::should_trigger_l1(messages, state, context_window) {
            break;
        }
        let Some((start, end)) = chunk::pick_next_chunk(messages, state) else {
            break;
        };
        let chunk_tokens = tokens::estimate_messages_tokens(&messages[start..end]);
        if chunk_tokens < constants::MIN_CHUNK_TOKENS {
            break;
        }
        // summarize（失败则中止本轮，不记 fold）。
        let summary = match summarizer::summarize_chunk(llm, model, &messages[start..end]).await {
            Ok(s) if !s.trim().is_empty() => s,
            _ => break,
        };
        state.record_fold(Fold { start_idx: start, end_idx: end, summary, level: 1 });
        folds_added += 1;
    }

    CompactionOutcome {
        folded: folds_added > 0,
        folds_added,
    }
}

#[cfg(test)]
mod lib_tests {
    use super::*;

    #[test]
    fn short_conversation_no_compact() {
        // 无 LLM 也能跑：未过触发阈值 → 直接返回。
        let mut st = CompactionState::new();
        let msgs = vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")];
        // 用一个阻塞 wrapper 跑 async（这里因未触发，不会调 LLM）。
        let rt = tokio::runtime::Runtime::new().unwrap();
        let out = rt.block_on(maybe_compact(&msgs, &mut st, &NoLlm, "m", 500_000));
        assert!(!out.folded);
        assert!(st.folds.is_empty());
    }

    // 一个永不实际被调用的 LLM（短对话测试里 maybe_compact 提前返回）。
    struct NoLlm;
    #[async_trait::async_trait]
    impl LlmClient for NoLlm {
        async fn chat(&self, _: dss_llm::ChatRequest) -> Result<dss_llm::LlmResponse, dss_llm::LlmError> {
            Err(dss_llm::LlmError::NotConfigured("NoLlm should not be called".into()))
        }
        fn chat_stream(
            &self,
            _: dss_llm::ChatRequest,
        ) -> futures::future::BoxFuture<'_, Result<dss_llm::BoxedEventStream, dss_llm::LlmError>> {
            Box::pin(async { Err(dss_llm::LlmError::NotConfigured("NoLlm no stream".into())) })
        }
        fn model(&self) -> &str {
            "no-llm"
        }
    }
}
