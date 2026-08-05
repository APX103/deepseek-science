//! 集成测试：FakeLLM 驱动 maybe_compact，验证 L1 fold + projection 压缩 + append-only。

use dss_compact::{maybe_compact, maybe_compact_with_reserved_tokens, projection, CompactionState};
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
    let llm = FakeLlm {
        calls: calls.clone(),
        summary_text: "SUMMARY".into(),
    };

    let mut state = CompactionState::new();
    let outcome = maybe_compact(&messages, &mut state, &llm, "fake-llm", cw).await;

    // 应触发至少 1 次 fold（调用了 summarize）。
    assert!(outcome.folded, "expected at least one L1 fold");
    assert!(
        calls.load(Ordering::Relaxed) >= 1,
        "summarizer should be called"
    );

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
    let llm = FakeLlm {
        calls: calls.clone(),
        summary_text: "S".into(),
    };
    let mut state = CompactionState::new();
    let outcome = maybe_compact(&messages, &mut state, &llm, "fake-llm", 500_000).await;
    assert!(!outcome.folded);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert!(state.folds.is_empty());
}

#[tokio::test]
async fn default_500k_window_compacts_before_the_300k_absolute_wall() {
    // This is the production default. The old L1 predicate never fired here because it compared
    // raw remaining messages with kept-available instead of remaining window capacity.
    let cw = 500_000;
    let messages: Vec<ChatMessage> = (0..40).map(|_| big_msg(10_000)).collect();
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = FakeLlm {
        calls: calls.clone(),
        summary_text: "DEFAULT WINDOW SUMMARY".into(),
    };
    let mut state = CompactionState::new();

    let outcome = maybe_compact(&messages, &mut state, &llm, "fake-llm", cw).await;
    let projected_tokens =
        dss_compact::tokens::estimate_messages_tokens(&projection(&messages, &state));

    assert!(outcome.folded);
    assert!(calls.load(Ordering::Relaxed) >= 1);
    assert_eq!(outcome.projected_tokens, projected_tokens);
    assert!(
        projected_tokens < dss_compact::chunk::hard_wall_tokens(cw),
        "projection {projected_tokens} must be below the production hard wall"
    );
    assert!(!outcome.hard_wall_exceeded);
}

#[tokio::test]
async fn reserved_run_context_participates_in_the_hard_wall_budget() {
    // Session history alone is below 300k, but the per-run plan/memory context pushes the actual
    // request above it. Compaction must run instead of leaving Runner to send an oversized body.
    let cw = 500_000;
    let messages: Vec<ChatMessage> = (0..29).map(|_| big_msg(10_000)).collect();
    let reserved_tokens = 20_000;
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = FakeLlm {
        calls: calls.clone(),
        summary_text: "SUMMARY WITH RESERVED CONTEXT".into(),
    };
    let mut state = CompactionState::new();

    let outcome = maybe_compact_with_reserved_tokens(
        &messages,
        &mut state,
        &llm,
        "fake-llm",
        cw,
        reserved_tokens,
    )
    .await;

    assert!(outcome.folded);
    assert!(calls.load(Ordering::Relaxed) >= 1);
    assert!(outcome.projected_tokens < dss_compact::chunk::hard_wall_tokens(cw));
    assert!(!outcome.hard_wall_exceeded);
}

#[tokio::test]
async fn replayed_deepseek_reasoning_participates_in_compaction_budget() {
    let mut messages = Vec::new();
    for turn in 0..3 {
        messages.push(ChatMessage::user(format!("old request {turn}")));
        let mut assistant = ChatMessage::assistant(format!("answer {turn}"));
        assistant.reasoning_content = Some("r".repeat(400_000));
        messages.push(assistant);
    }
    messages.push(ChatMessage::user("active request"));
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = FakeLlm {
        calls: calls.clone(),
        summary_text: "REASONING HISTORY SUMMARY".into(),
    };
    let mut state = CompactionState::new();

    let outcome = maybe_compact(&messages, &mut state, &llm, "deepseek-v4-flash", 500_000).await;

    assert!(outcome.folded);
    assert!(calls.load(Ordering::Relaxed) >= 1);
    assert!(outcome.projected_tokens < dss_compact::chunk::hard_wall_tokens(500_000));
    let view = projection(&messages, &state);
    assert_eq!(
        view.last().and_then(|message| message.content.as_deref()),
        Some("active request")
    );
}

struct FailingLlm;

#[async_trait::async_trait]
impl LlmClient for FailingLlm {
    async fn chat(&self, _: ChatRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("summary unavailable".into()))
    }

    fn chat_stream(&self, _: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        Box::pin(async { Err(LlmError::NotConfigured("no stream".into())) })
    }

    fn model(&self) -> &str {
        "failing-llm"
    }
}

#[tokio::test]
async fn summary_failure_reports_an_unresolved_hard_wall() {
    let messages: Vec<ChatMessage> = (0..40).map(|_| big_msg(10_000)).collect();
    let mut state = CompactionState::new();

    let outcome = maybe_compact(&messages, &mut state, &FailingLlm, "fake-llm", 500_000).await;

    assert!(!outcome.folded);
    assert!(outcome.hard_wall_exceeded);
    assert!(outcome.projected_tokens > outcome.hard_wall_tokens);
    assert!(state.folds.is_empty());
}
