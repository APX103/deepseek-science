//! 集成测试：FakeLLM 驱动 maybe_compact，验证 L1 fold + projection 压缩 + append-only。

use dss_compact::{maybe_compact, projection, CompactionState};
use dss_llm::{BoxedEventStream, ChatMessage, ChatRequest, LlmClient, LlmError, LlmResponse};
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// FakeLlm：chat 返回脚本 summary；记录调用次数。
struct FakeLlm {
    calls: Arc<AtomicUsize>,
    summary_text: String,
}

#[async_trait::async_trait]
impl LlmClient for FakeLlm {
    async fn chat(&self, _: ChatRequest) -> Result<LlmResponse, LlmError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(LlmResponse {
            text: self.summary_text.clone(),
            thinking: None,
            usage: Default::default(),
            finish_reason: Some("stop".into()),
            tool_calls: Vec::new(),
        })
    }
    fn chat_stream(&self, _: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        Box::pin(async { Err(LlmError::NotConfigured("no stream in FakeLlm".into())) })
    }
    fn model(&self) -> &str {
        "fake-llm"
    }
}

fn big_msg(tokens: usize) -> ChatMessage {
    ChatMessage::user("x".repeat(tokens * 4))
}

#[tokio::test]
async fn maybe_compact_folds_l1_and_projection_shrinks() {
    // context_window = 10000 → trigger 阈值 = 7500 token。
    // 构造 4 条 5000 token 消息 = 20000 token（超阈值）。
    let cw = 10_000;
    let messages: Vec<ChatMessage> = (0..4).map(|_| big_msg(5000)).collect();
    let log_len_before = messages.len();

    let calls = Arc::new(AtomicUsize::new(0));
    let llm = FakeLlm { calls: calls.clone(), summary_text: "SUMMARY".into() };

    let mut state = CompactionState::new();
    let outcome = maybe_compact(&messages, &mut state, &llm, "fake-llm", cw).await;

    // 应触发至少 1 次 fold（调用了 summarize）。
    assert!(outcome.folded, "expected at least one L1 fold");
    assert!(calls.load(Ordering::Relaxed) >= 1, "summarizer should be called");

    // append-only：日志长度不变。
    assert_eq!(messages.len(), log_len_before);

    // projection 比全量短（fold 区间被 summary 替换）。
    let view = projection(&messages, &state);
    let view_tokens = dss_compact::tokens::estimate_messages_tokens(&view);
    let full_tokens = dss_compact::tokens::estimate_messages_tokens(&messages);
    assert!(
        view_tokens < full_tokens,
        "projection ({view_tokens}) should be smaller than full ({full_tokens})"
    );

    // 每个 fold 在 projection 里对应一条 "SUMMARY"。
    let summary_count = view
        .iter()
        .filter(|m| m.content.as_deref() == Some("SUMMARY"))
        .count();
    assert_eq!(summary_count, state.folds.len());
}

#[tokio::test]
async fn short_conversation_does_not_call_llm() {
    // 短对话：未过触发阈值 → 不调 LLM、不 fold。
    let messages = vec![ChatMessage::user("hi"), ChatMessage::assistant("hello")];
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = FakeLlm { calls: calls.clone(), summary_text: "S".into() };
    let mut state = CompactionState::new();
    let outcome = maybe_compact(&messages, &mut state, &llm, "fake-llm", 500_000).await;
    assert!(!outcome.folded);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(state.folds.is_empty());
}
