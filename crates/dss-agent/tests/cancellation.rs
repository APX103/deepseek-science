//! Offline cancellation/error regressions for the SSE runner.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dss_agent::{AgentEvent, CompleteKind, FrameStatus, Runner, Session, MAX_ITERATIONS};
use dss_llm::{
    BoxedEventStream, ChatRequest, LlmClient, LlmError, LlmResponse, StreamEvent, ToolCallDelta,
    Usage,
};
use dss_tools::{Tool, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolSpec};
use futures::future::BoxFuture;
use futures::{stream, StreamExt};
use tokio::sync::{mpsc, Notify};

const PARTIAL_USAGE: Usage = Usage {
    input_tokens: 7,
    output_tokens: 3,

    cache_hit_tokens: 0,
    cache_miss_tokens: 0,
};

struct PendingRequestLlm;

#[async_trait::async_trait]
impl LlmClient for PendingRequestLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("not used".into()))
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        Box::pin(futures::future::pending())
    }

    fn model(&self) -> &str {
        "pending-request"
    }
}

struct PartialThenStalledLlm {
    buffered: Arc<Notify>,
}

#[async_trait::async_trait]
impl LlmClient for PartialThenStalledLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("not used".into()))
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        // Usage intentionally arrives before Text so receiving Text is also a
        // deterministic proof that the usage snapshot has been processed.
        let published = stream::iter(vec![
            Ok(StreamEvent::Thinking(
                "<｜DSML｜tool_calls>reasoning secret".into(),
            )),
            Ok(StreamEvent::Usage(PARTIAL_USAGE)),
            Ok(StreamEvent::Text(
                "<｜DSML｜tool_calls><｜DSML｜invoke name=\"python\"><｜DSML｜parameter name=\"code\" string=true>partial secret"
                    .into(),
            )),
        ]);
        let buffered = self.buffered.clone();
        let consumed = stream::once(async move {
            buffered.notify_one();
            Ok(StreamEvent::Usage(PARTIAL_USAGE))
        });
        let stalled = stream::pending::<Result<StreamEvent, LlmError>>();
        let events = Box::pin(published.chain(consumed).chain(stalled)) as BoxedEventStream;
        Box::pin(async move { Ok(events) })
    }

    fn model(&self) -> &str {
        "partial-then-stalled"
    }
}

struct PartialThenErrorLlm;

#[async_trait::async_trait]
impl LlmClient for PartialThenErrorLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("not used".into()))
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let events = Box::pin(stream::iter(vec![
            Ok(StreamEvent::Thinking(
                "<||DSML||tool_calls>error reasoning secret".into(),
            )),
            Ok(StreamEvent::Usage(PARTIAL_USAGE)),
            Ok(StreamEvent::Text(
                "<｜DSML｜tool_calls><｜DSML｜invoke name=\"python\"><｜DSML｜parameter name=\"code\" string=true>error secret"
                    .into(),
            )),
            Err(LlmError::Stream("synthetic stream failure".into())),
        ])) as BoxedEventStream;
        Box::pin(async move { Ok(events) })
    }

    fn model(&self) -> &str {
        "partial-then-error"
    }
}

#[derive(Clone, Copy)]
enum SecondTurnEnd {
    Stall,
    Error,
}

/// First iteration publishes and commits a real tool result. The second
/// iteration publishes partial assistant evidence, then stalls or errors.
struct ToolThenPartialLlm {
    calls: AtomicUsize,
    second_turn_end: SecondTurnEnd,
    second_buffered: Arc<Notify>,
}

#[async_trait::async_trait]
impl LlmClient for ToolThenPartialLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("not used".into()))
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let events: BoxedEventStream = if call == 0 {
            Box::pin(stream::iter(vec![
                Ok(StreamEvent::Usage(Usage {
                    input_tokens: 2,
                    output_tokens: 1,

                    cache_hit_tokens: 0,
                    cache_miss_tokens: 0,
                })),
                Ok(StreamEvent::ToolCallDelta(ToolCallDelta {
                    index: 0,
                    id: Some("call-visible".into()),
                    name: Some("evidence_probe".into()),
                    arguments: Some("{}".into()),
                })),
                Ok(StreamEvent::Finish {
                    reason: Some("tool_calls".into()),
                }),
            ]))
        } else {
            // Two snapshots in one iteration must replace, not double count,
            // the current iteration's contribution.
            let published = vec![
                Ok(StreamEvent::Usage(Usage {
                    input_tokens: 1,
                    output_tokens: 1,

                cache_hit_tokens: 0,
                cache_miss_tokens: 0,
                })),
                Ok(StreamEvent::Usage(Usage {
                    input_tokens: 3,
                    output_tokens: 2,

                cache_hit_tokens: 0,
                cache_miss_tokens: 0,
                })),
                Ok(StreamEvent::Thinking("second analysis".into())),
                Ok(StreamEvent::Text(
                    "<｜DSML｜tool_calls><｜DSML｜invoke name=\"python\"><｜DSML｜parameter name=\"code\" string=true>post-tool secret"
                        .into(),
                )),
            ];
            let second_buffered = self.second_buffered.clone();
            let consumed = stream::once(async move {
                second_buffered.notify_one();
                Ok(StreamEvent::Usage(Usage {
                    input_tokens: 3,
                    output_tokens: 2,

                    cache_hit_tokens: 0,
                    cache_miss_tokens: 0,
                }))
            });
            match self.second_turn_end {
                SecondTurnEnd::Stall => Box::pin(
                    stream::iter(published)
                        .chain(consumed)
                        .chain(stream::pending::<Result<StreamEvent, LlmError>>()),
                ),
                SecondTurnEnd::Error => Box::pin(stream::iter(published).chain(consumed).chain(
                    stream::once(async { Err(LlmError::Stream("second iteration failed".into())) }),
                )),
            }
        };
        Box::pin(async move { Ok(events) })
    }

    fn model(&self) -> &str {
        "tool-then-partial"
    }
}

struct PublishedToolThenPendingLlm;

#[async_trait::async_trait]
impl LlmClient for PublishedToolThenPendingLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("not used".into()))
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let events = Box::pin(stream::iter(vec![
            Ok(StreamEvent::Usage(Usage {
                input_tokens: 4,
                output_tokens: 1,

                cache_hit_tokens: 0,
                cache_miss_tokens: 0,
            })),
            Ok(StreamEvent::ToolCallDelta(ToolCallDelta {
                index: 0,
                id: Some("call-pending".into()),
                name: Some("pending_probe".into()),
                arguments: Some("{}".into()),
            })),
            Ok(StreamEvent::Finish {
                reason: Some("tool_calls".into()),
            }),
        ])) as BoxedEventStream;
        Box::pin(async move { Ok(events) })
    }

    fn model(&self) -> &str {
        "published-tool-then-pending"
    }
}

struct EvidenceProbe;

#[async_trait::async_trait]
impl Tool for EvidenceProbe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "evidence_probe".into(),
            description: "returns a stable result for cancellation tests".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::ok("tool-visible-result"))
    }
}

struct PendingProbe {
    started: Arc<Notify>,
}

#[async_trait::async_trait]
impl Tool for PendingProbe {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "pending_probe".into(),
            description: "waits until the receiver disconnects".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.started.notify_one();
        futures::future::pending::<Result<ToolOutput, ToolError>>().await
    }
}

fn tmp_workspace() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dss-cancellation-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).expect("create temporary workspace");
    dir
}

async fn spawn_run<L: LlmClient + 'static>(
    llm: L,
) -> (
    mpsc::Receiver<AgentEvent>,
    tokio::task::JoinHandle<(dss_agent::RunOutcome, Session)>,
) {
    spawn_run_with_registry(llm, ToolRegistry::new()).await
}

async fn spawn_run_with_registry<L: LlmClient + 'static>(
    llm: L,
    registry: ToolRegistry,
) -> (
    mpsc::Receiver<AgentEvent>,
    tokio::task::JoinHandle<(dss_agent::RunOutcome, Session)>,
) {
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        let mut session = Session::new("cancel-test", tmp_workspace());
        let ctx = ToolContext::new(session.workspace.clone());
        let outcome = Runner::run(
            &mut session,
            &llm,
            llm.model(),
            "cancel this request",
            &registry,
            &ctx,
            MAX_ITERATIONS,
            500_000,
            None,
            None,
            &[],
            false,
            &tx,
        )
        .await;
        (outcome, session)
    });
    (rx, task)
}

async fn recv_until(rx: &mut mpsc::Receiver<AgentEvent>, predicate: impl Fn(&AgentEvent) -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(event) = rx.recv().await {
            if predicate(&event) {
                return;
            }
        }
        panic!("event channel closed before target event");
    })
    .await
    .expect("timed out waiting for target event");
}

async fn await_run(
    task: tokio::task::JoinHandle<(dss_agent::RunOutcome, Session)>,
) -> (dss_agent::RunOutcome, Session) {
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("runner did not release the session")
        .expect("runner task panicked")
}

fn assert_cancelled_base(outcome: &dss_agent::RunOutcome, session: &Session) {
    assert_eq!(outcome.kind, CompleteKind::Cancelled);
    assert_eq!(session.frame.status, FrameStatus::Cancelled);
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == "user")
            .count(),
        1
    );
    let boundary = session.messages.last().expect("cancelled request boundary");
    assert_eq!(boundary.role, "system");
    assert!(boundary.harness_notice);
    assert!(boundary
        .content
        .as_deref()
        .is_some_and(|text| text.contains("上一条用户请求已由用户取消")));
}

fn assert_usage(usage: Usage, input_tokens: u32, output_tokens: u32) {
    assert_eq!(usage.input_tokens, input_tokens);
    assert_eq!(usage.output_tokens, output_tokens);
}

#[tokio::test]
async fn receiver_disconnect_cancels_pending_http_request_without_assistant_evidence() {
    let (mut rx, task) = spawn_run(PendingRequestLlm).await;
    recv_until(&mut rx, |event| {
        matches!(event, AgentEvent::Iteration { .. })
    })
    .await;

    drop(rx);
    let (outcome, session) = await_run(task).await;
    assert_cancelled_base(&outcome, &session);
    assert_eq!(outcome.iterations, 1);
    assert_usage(outcome.usage, 0, 0);
    assert!(session
        .messages
        .iter()
        .all(|message| message.role != "assistant"));
}

#[tokio::test]
async fn receiver_disconnect_discards_quarantined_text_and_thinking_but_preserves_usage() {
    let buffered = Arc::new(Notify::new());
    let (rx, task) = spawn_run(PartialThenStalledLlm {
        buffered: buffered.clone(),
    })
    .await;
    tokio::time::timeout(Duration::from_secs(1), buffered.notified())
        .await
        .expect("runner did not consume quarantined provider data");

    drop(rx);
    let (outcome, session) = await_run(task).await;
    assert_cancelled_base(&outcome, &session);
    assert_eq!(outcome.final_text, "");
    assert_eq!(outcome.iterations, 1);
    assert_usage(outcome.usage, 7, 3);

    assert!(session
        .messages
        .iter()
        .all(|message| message.role != "assistant"));
    assert!(session.messages.iter().all(|message| {
        message.content.as_deref().is_none_or(|content| {
            !content.contains("DSML")
                && !content.contains("reasoning secret")
                && !content.contains("partial secret")
        })
    }));
}

#[tokio::test]
async fn stream_error_discards_quarantined_text_and_thinking_but_preserves_usage() {
    let (mut rx, task) = spawn_run(PartialThenErrorLlm).await;
    let (outcome, session) = await_run(task).await;
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, CompleteKind::Error);
    assert_eq!(outcome.final_text, "");
    assert_eq!(outcome.iterations, 1);
    assert_usage(outcome.usage, 7, 3);
    assert_eq!(session.frame.status, FrameStatus::Failed);
    assert!(session
        .messages
        .iter()
        .all(|message| message.role != "assistant"));
    assert!(session.messages.iter().all(|message| {
        message.content.as_deref().is_none_or(|content| {
            !content.contains("DSML")
                && !content.contains("error secret")
                && !content.contains("error reasoning secret")
        })
    }));
}

#[tokio::test]
async fn second_iteration_cancel_keeps_real_tool_result_and_cumulative_usage() {
    let second_buffered = Arc::new(Notify::new());
    let llm = ToolThenPartialLlm {
        calls: AtomicUsize::new(0),
        second_turn_end: SecondTurnEnd::Stall,
        second_buffered: second_buffered.clone(),
    };
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EvidenceProbe));
    let (rx, task) = spawn_run_with_registry(llm, registry).await;
    tokio::time::timeout(Duration::from_secs(1), second_buffered.notified())
        .await
        .expect("runner did not consume the second quarantined turn");

    drop(rx);
    let (outcome, session) = await_run(task).await;
    assert_cancelled_base(&outcome, &session);
    assert_eq!(outcome.final_text, "");
    assert_eq!(outcome.iterations, 2);
    assert_usage(outcome.usage, 5, 3);

    let tool_calls = session
        .messages
        .iter()
        .find_map(|message| message.tool_calls.as_ref())
        .expect("published tool call retained");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call-visible");
    let tool_results: Vec<_> = session
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .collect();
    assert_eq!(tool_results.len(), 1);
    assert_eq!(
        tool_results[0].content.as_deref(),
        Some("tool-visible-result")
    );
    assert_eq!(tool_results[0].is_error, Some(false));

    assert!(session.messages.iter().all(|message| {
        message.reasoning_content.as_deref() != Some("second analysis")
            && message
                .content
                .as_deref()
                .is_none_or(|content| !content.contains("post-tool secret"))
    }));
}

#[tokio::test]
async fn second_iteration_error_keeps_real_tool_result_and_cumulative_usage() {
    let second_buffered = Arc::new(Notify::new());
    let llm = ToolThenPartialLlm {
        calls: AtomicUsize::new(0),
        second_turn_end: SecondTurnEnd::Error,
        second_buffered,
    };
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EvidenceProbe));
    let (mut rx, task) = spawn_run_with_registry(llm, registry).await;
    let (outcome, session) = await_run(task).await;
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, CompleteKind::Error);
    assert_eq!(outcome.final_text, "");
    assert_eq!(outcome.iterations, 2);
    assert_usage(outcome.usage, 5, 3);
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == "tool")
            .count(),
        1
    );
    assert!(session.messages.iter().all(|message| {
        message.reasoning_content.as_deref() != Some("second analysis")
            && message
                .content
                .as_deref()
                .is_none_or(|content| !content.contains("post-tool secret"))
    }));
    assert!(session.messages.iter().all(|message| {
        message.content.as_deref().is_none_or(|content| {
            !content.contains("DSML") && !content.contains("post-tool secret")
        })
    }));
}

#[tokio::test]
async fn cancel_after_tool_call_publication_pairs_unknown_result_without_side_effect() {
    let started = Arc::new(Notify::new());
    let llm = PublishedToolThenPendingLlm;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PendingProbe {
        started: started.clone(),
    }));
    let (mut rx, task) = spawn_run_with_registry(llm, registry).await;
    recv_until(&mut rx, |event| {
        matches!(event, AgentEvent::ToolCalls { .. })
    })
    .await;
    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .expect("pending tool did not start");

    drop(rx);
    let (outcome, session) = await_run(task).await;
    assert_cancelled_base(&outcome, &session);
    assert_eq!(outcome.iterations, 1);
    assert_usage(outcome.usage, 4, 1);

    let assistant = session
        .messages
        .iter()
        .find(|message| message.tool_calls.is_some())
        .expect("published tool call retained");
    let calls = assistant.tool_calls.as_ref().expect("tool calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call-pending");

    let result = session
        .messages
        .iter()
        .find(|message| message.role == "tool")
        .expect("canonical unknown-result pair");
    assert_eq!(result.tool_call_id.as_deref(), Some("call-pending"));
    assert_eq!(result.name.as_deref(), Some("pending_probe"));
    assert_eq!(result.is_error, Some(true));
    let content = result.content.as_deref().expect("unknown result message");
    assert!(content.contains("本轮被取消"));
    assert!(content.contains("没有持久化未发布的工具结果"));
    assert!(!content.contains("success"));
}
