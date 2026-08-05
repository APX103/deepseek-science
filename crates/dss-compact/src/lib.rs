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
    /// 完成后 projection + 本轮保留上下文的估算 token 数。
    pub projected_tokens: usize,
    /// 当前模型窗口对应的硬墙。
    pub hard_wall_tokens: usize,
    /// summarizer 失败或没有可折叠消息时，结果是否仍超过硬墙。
    pub hard_wall_exceeded: bool,
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
    maybe_compact_with_reserved_tokens(messages, state, llm, model, context_window, 0).await
}

/// 与 [`maybe_compact`] 相同，但把本轮不可折叠的 system/memory/tool-schema token
/// 纳入预算。
///
/// Runner 应传入这些消息的 token 数；否则 290k 历史 + 20k 本轮上下文会因为只看
/// 历史而错过 300k 绝对硬墙。
pub async fn maybe_compact_with_reserved_tokens(
    messages: &[ChatMessage],
    state: &mut CompactionState,
    llm: &dyn LlmClient,
    model: &str,
    context_window: usize,
    reserved_tokens: usize,
) -> CompactionOutcome {
    let hard_wall_tokens = chunk::hard_wall_tokens(context_window);
    let mut projected_tokens = projection_tokens(messages, state, reserved_tokens);
    if !chunk::is_over_trigger(projected_tokens, context_window) {
        return outcome(0, projected_tokens, hard_wall_tokens);
    }

    let mut folds_added = 0usize;
    // 循环触发 L1，直到不再需要或无候选。
    // 安全上限：避免极端情况无限折叠（与 PTL_RETRY_CAP 同量级）。
    let safety = constants::PTL_RETRY_CAP;
    let mut guard = 0usize;
    while guard < safety {
        guard += 1;
        if !chunk::should_trigger_l1_with_reserved(messages, state, context_window, reserved_tokens)
        {
            break;
        }
        // Select enough raw history to get below the target even when the summary reaches its
        // enforced output ceiling. Message boundaries and tool transactions may make it larger.
        let reduction_needed =
            projected_tokens.saturating_sub(chunk::projection_token_target(context_window));
        let max_summary_tokens = constants::OUTPUT_CEILING / constants::CHARS_PER_TOKEN + 2;
        let requested_chunk_tokens = reduction_needed
            .saturating_add(max_summary_tokens)
            .max(constants::MIN_CHUNK_TOKENS);
        let Some((start, end)) =
            chunk::pick_next_chunk_with_min_tokens(messages, state, requested_chunk_tokens)
        else {
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
        // A provider is free to ignore the requested summary size. Never commit a fold that
        // grows (or preserves) the projection, otherwise the retry cap merely hides the fact
        // that the hard wall was not reduced.
        let summary_tokens = tokens::estimate_message_tokens(&ChatMessage::assistant(&summary));
        if summary_tokens >= chunk_tokens {
            break;
        }
        let mut next_state = state.clone();
        next_state.record_fold(Fold {
            start_idx: start,
            end_idx: end,
            summary,
            level: 1,
        });
        let next_projected_tokens = projection_tokens(messages, &next_state, reserved_tokens);
        if next_projected_tokens >= projected_tokens {
            break;
        }
        *state = next_state;
        folds_added += 1;
        projected_tokens = next_projected_tokens;
    }

    outcome(folds_added, projected_tokens, hard_wall_tokens)
}

fn projection_tokens(
    messages: &[ChatMessage],
    state: &CompactionState,
    reserved_tokens: usize,
) -> usize {
    tokens::estimate_messages_tokens(&projection(messages, state)).saturating_add(reserved_tokens)
}

fn outcome(
    folds_added: usize,
    projected_tokens: usize,
    hard_wall_tokens: usize,
) -> CompactionOutcome {
    CompactionOutcome {
        folded: folds_added > 0,
        folds_added,
        projected_tokens,
        hard_wall_tokens,
        hard_wall_exceeded: projected_tokens > hard_wall_tokens,
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
        async fn chat(
            &self,
            _: dss_llm::ChatRequest,
        ) -> Result<dss_llm::LlmResponse, dss_llm::LlmError> {
            Err(dss_llm::LlmError::NotConfigured(
                "NoLlm should not be called".into(),
            ))
        }
        fn chat_stream(
            &self,
            _: dss_llm::ChatRequest,
        ) -> futures::future::BoxFuture<'_, Result<dss_llm::BoxedEventStream, dss_llm::LlmError>>
        {
            Box::pin(async { Err(dss_llm::LlmError::NotConfigured("NoLlm no stream".into())) })
        }
        fn model(&self) -> &str {
            "no-llm"
        }
    }
}
