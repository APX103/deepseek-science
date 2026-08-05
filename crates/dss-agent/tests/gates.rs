//! P2b-gates 集成测试：流式 FakeLLM 驱动 Runner 走各决策门。

use dss_agent::{Runner, Session, MAX_ITERATIONS};
use dss_llm::{
    BoxedEventStream, ChatMessage, ChatRequest, LlmClient, LlmError, StreamEvent, Usage,
};
use dss_tools::{
    builtin, Tool, ToolBatchPolicy, ToolContext, ToolError, ToolOutput, ToolRegistry, ToolSpec,
};
use futures::future::BoxFuture;
use futures::stream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// 一次脚本化的流式响应：若干 text 增量 + 可选 finish_reason + 可选 tool_calls。
#[derive(Clone)]
struct ScriptedTurn {
    texts: Vec<String>,
    finish_reason: Option<String>,
    /// 工具调用（一次性给出，转成 ToolCallDelta 流）。
    tool_calls: Vec<(u32, String, String, String)>, // (index, id, name, arguments)
}

struct StreamFakeLlm {
    /// 按轮消费的脚本队列（每轮 LLM 调用 pop 一个）。
    turns: Arc<Mutex<Vec<ScriptedTurn>>>,
    seen_messages: Option<Arc<Mutex<Vec<Vec<ChatMessage>>>>>,
    seen_tool_names: Option<Arc<Mutex<Vec<Vec<String>>>>>,
}

#[async_trait::async_trait]
impl LlmClient for StreamFakeLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("use chat_stream".into()))
    }
    fn chat_stream(&self, req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        if let Some(seen) = &self.seen_messages {
            seen.lock().unwrap().push(req.messages.clone());
        }
        if let Some(seen) = &self.seen_tool_names {
            let names = req
                .tools
                .as_ref()
                .map(|tools| {
                    tools
                        .iter()
                        .map(|tool| tool.function.name.clone())
                        .collect()
                })
                .unwrap_or_default();
            seen.lock().unwrap().push(names);
        }
        let mut turns = self.turns.lock().unwrap();
        let turn = turns.first().cloned().unwrap_or(ScriptedTurn {
            texts: vec![],
            finish_reason: None,
            tool_calls: vec![],
        });
        // 注意：不 pop（多轮场景下每轮消费一个；这里用「peek + 调用方控制」）。
        // 为简单：实际 pop（每次 chat_stream 消费一个 turn）。
        if !turns.is_empty() {
            turns.remove(0);
        }
        drop(turns);

        let mut events: Vec<Result<StreamEvent, LlmError>> = Vec::new();
        for t in &turn.texts {
            events.push(Ok(StreamEvent::Text(t.clone())));
        }
        for (index, id, name, args) in &turn.tool_calls {
            events.push(Ok(StreamEvent::ToolCallDelta(dss_llm::ToolCallDelta {
                index: *index,
                id: Some(id.clone()),
                name: Some(name.clone()),
                arguments: Some(args.clone()),
            })));
        }
        events.push(Ok(StreamEvent::Usage(Usage {
            input_tokens: 1,
            output_tokens: 1,
        })));
        events.push(Ok(StreamEvent::Finish {
            reason: turn.finish_reason,
        }));

        let stream = Box::pin(stream::iter(events)) as BoxedEventStream;
        Box::pin(async move { Ok(stream) })
    }
    fn model(&self) -> &str {
        "fake-stream"
    }
}

struct TwoRunReviewLlm {
    stream_calls: AtomicUsize,
    review_calls: AtomicUsize,
}

struct TraceReviewLlm {
    stream_calls: AtomicUsize,
    review_requests: Arc<Mutex<Vec<ChatRequest>>>,
}

struct AlwaysFailReviewLlm {
    stream_calls: AtomicUsize,
    review_calls: AtomicUsize,
}

struct VetoThenUnavailableReviewLlm {
    stream_calls: AtomicUsize,
    review_calls: AtomicUsize,
}

struct ArtifactTextOnlyReviewLlm {
    stream_calls: AtomicUsize,
    review_calls: AtomicUsize,
}

struct ArtifactToolRepairReviewLlm {
    stream_calls: AtomicUsize,
    review_calls: AtomicUsize,
    stream_requests: Arc<Mutex<Vec<ChatRequest>>>,
}

struct HardWallFailLlm {
    summary_calls: AtomicUsize,
    stream_calls: AtomicUsize,
}

struct LargeSchemaTool;

struct ReviewEvidenceTool;

struct CountingTool {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

struct ExclusiveA2aProbeTool {
    calls: Arc<AtomicUsize>,
}

struct NeverCompletesProbeTool {
    calls: Arc<AtomicUsize>,
}

struct MixedPartialNativeLlm;

struct ProtocolEventsLlm {
    events: Mutex<Option<Vec<Result<StreamEvent, LlmError>>>>,
}

#[async_trait::async_trait]
impl Tool for LargeSchemaTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "large_schema_tool".into(),
            description: "d".repeat(80_000),
            parameters: serde_json::json!({ "type": "object", "properties": {} }),
        }
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::ok("unused"))
    }
}

#[async_trait::async_trait]
impl Tool for ReviewEvidenceTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_evidence_for_review".into(),
            description: "Return deterministic evidence for reviewer trace tests".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::ok("sampling rule: B=40"))
    }
}

#[async_trait::async_trait]
impl Tool for CountingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.into(),
            description: "record one deterministic execution".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "code": { "type": "string" }
                }
            }),
        }
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::ok(format!("executed: {args}")))
    }
}

#[async_trait::async_trait]
impl Tool for ExclusiveA2aProbeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "a2a_agent_fixture".into(),
            description: "test-only remote A2A delegation".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    fn batch_policy(&self) -> ToolBatchPolicy {
        ToolBatchPolicy::Exclusive
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::ok("complete remote transcript"))
    }
}

#[async_trait::async_trait]
impl Tool for NeverCompletesProbeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "slow_local_probe".into(),
            description: "test-only local tool that must never start".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        futures::future::pending::<Result<ToolOutput, ToolError>>().await
    }
}

#[async_trait::async_trait]
impl LlmClient for MixedPartialNativeLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("use chat_stream".into()))
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let events = vec![
            Ok(StreamEvent::Text(
                concat!(
                    "<｜DSML｜tool_calls><｜DSML｜invoke name=\"count_probe\">",
                    "</｜DSML｜invoke></｜DSML｜tool_calls>"
                )
                .into(),
            )),
            // A single incomplete native delta is still evidence that the
            // provider attempted both protocols and must block DSML execution.
            Ok(StreamEvent::ToolCallDelta(dss_llm::ToolCallDelta {
                index: 0,
                id: Some("partial-native".into()),
                name: None,
                arguments: None,
            })),
            Ok(StreamEvent::Finish {
                reason: Some("tool_calls".into()),
            }),
        ];
        Box::pin(async move { Ok(Box::pin(stream::iter(events)) as BoxedEventStream) })
    }

    fn model(&self) -> &str {
        "mixed-partial-native"
    }
}

#[async_trait::async_trait]
impl LlmClient for ProtocolEventsLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("review unavailable".into()))
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let events = self
            .events
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| vec![Err(LlmError::Stream("unexpected second stream".into()))]);
        Box::pin(async move { Ok(Box::pin(stream::iter(events)) as BoxedEventStream) })
    }

    fn model(&self) -> &str {
        "protocol-events"
    }
}

#[async_trait::async_trait]
impl LlmClient for HardWallFailLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        self.summary_calls.fetch_add(1, Ordering::SeqCst);
        Err(LlmError::NotConfigured("summary unavailable".into()))
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            Err(LlmError::NotConfigured(
                "must not send oversized request".into(),
            ))
        })
    }

    fn model(&self) -> &str {
        "hard-wall-fail"
    }
}

#[async_trait::async_trait]
impl LlmClient for TwoRunReviewLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        let review = self.review_calls.fetch_add(1, Ordering::SeqCst);
        let text = if review == 0 {
            r#"{"verdict":"fail","findings":["revise once"]}"#
        } else {
            r#"{"verdict":"pass","findings":[]}"#
        };
        Ok(dss_llm::LlmResponse {
            text: text.into(),
            ..Default::default()
        })
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let turn = self.stream_calls.fetch_add(1, Ordering::SeqCst);
        let text = match turn {
            0 => "first draft",
            1 => "revised first answer",
            _ => "second request answer",
        };
        let events = vec![
            Ok(StreamEvent::Text(text.into())),
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
        ];
        Box::pin(async move { Ok(Box::pin(stream::iter(events)) as BoxedEventStream) })
    }

    fn model(&self) -> &str {
        "two-run-review"
    }
}

#[async_trait::async_trait]
impl LlmClient for TraceReviewLlm {
    async fn chat(&self, req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        self.review_requests.lock().unwrap().push(req);
        Ok(dss_llm::LlmResponse {
            text: r#"{"verdict":"pass","findings":[]}"#.into(),
            ..Default::default()
        })
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let turn = self.stream_calls.fetch_add(1, Ordering::SeqCst);
        let events = if turn == 0 {
            vec![
                Ok(StreamEvent::ToolCallDelta(dss_llm::ToolCallDelta {
                    index: 0,
                    id: Some("call-read".into()),
                    name: Some("read_evidence_for_review".into()),
                    arguments: Some(r#"{"path":"README.md"}"#.into()),
                })),
                Ok(StreamEvent::Finish {
                    reason: Some("tool_calls".into()),
                }),
            ]
        } else {
            vec![
                Ok(StreamEvent::Text("bounded scientific answer".into())),
                Ok(StreamEvent::Finish {
                    reason: Some("stop".into()),
                }),
            ]
        };
        Box::pin(async move { Ok(Box::pin(stream::iter(events)) as BoxedEventStream) })
    }

    fn model(&self) -> &str {
        "trace-review"
    }
}

#[async_trait::async_trait]
impl LlmClient for AlwaysFailReviewLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        self.review_calls.fetch_add(1, Ordering::SeqCst);
        Ok(dss_llm::LlmResponse {
            text: r#"{"verdict":"fail","findings":["unsupported scientific claim"]}"#.into(),
            ..Default::default()
        })
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let attempt = self.stream_calls.fetch_add(1, Ordering::SeqCst) + 1;
        let events = vec![
            Ok(StreamEvent::Text(format!("unsupported draft {attempt}"))),
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
        ];
        Box::pin(async move { Ok(Box::pin(stream::iter(events)) as BoxedEventStream) })
    }

    fn model(&self) -> &str {
        "always-fail-review"
    }
}

#[async_trait::async_trait]
impl LlmClient for VetoThenUnavailableReviewLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        let review = self.review_calls.fetch_add(1, Ordering::SeqCst);
        let text = if review == 0 {
            r#"{"verdict":"fail","findings":["fix cited evidence"]}"#
        } else {
            "review transport returned non-JSON"
        };
        Ok(dss_llm::LlmResponse {
            text: text.into(),
            ..Default::default()
        })
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let attempt = self.stream_calls.fetch_add(1, Ordering::SeqCst) + 1;
        let events = vec![
            Ok(StreamEvent::Text(format!("draft {attempt}"))),
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
        ];
        Box::pin(async move { Ok(Box::pin(stream::iter(events)) as BoxedEventStream) })
    }

    fn model(&self) -> &str {
        "veto-then-unavailable-review"
    }
}

#[async_trait::async_trait]
impl LlmClient for ArtifactTextOnlyReviewLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        let review = self.review_calls.fetch_add(1, Ordering::SeqCst);
        let text = if review == 0 {
            r#"{"verdict":"fail","findings":["必须修改 report.md 并读回核验"],"repair_scope":"artifact","requires_tool_action":true}"#
        } else {
            r#"{"verdict":"pass","findings":[],"repair_scope":"response","requires_tool_action":false}"#
        };
        Ok(dss_llm::LlmResponse {
            text: text.into(),
            ..Default::default()
        })
    }

    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let attempt = self.stream_calls.fetch_add(1, Ordering::SeqCst) + 1;
        let events = vec![
            Ok(StreamEvent::Text(format!(
                "text-only artifact repair claim {attempt}"
            ))),
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
        ];
        Box::pin(async move { Ok(Box::pin(stream::iter(events)) as BoxedEventStream) })
    }

    fn model(&self) -> &str {
        "artifact-text-only-review"
    }
}

#[async_trait::async_trait]
impl LlmClient for ArtifactToolRepairReviewLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        let review = self.review_calls.fetch_add(1, Ordering::SeqCst);
        let text = if review == 0 {
            r#"{"verdict":"fail","findings":["必须修改 report.md 并读回核验"],"repair_scope":"artifact","requires_tool_action":true}"#
        } else {
            r#"{"verdict":"pass","findings":[],"repair_scope":"response","requires_tool_action":false}"#
        };
        Ok(dss_llm::LlmResponse {
            text: text.into(),
            ..Default::default()
        })
    }

    fn chat_stream(&self, req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        self.stream_requests.lock().unwrap().push(req);
        let turn = self.stream_calls.fetch_add(1, Ordering::SeqCst);
        let events = match turn {
            0 => vec![
                Ok(StreamEvent::Text("stale report claim".into())),
                Ok(StreamEvent::Finish {
                    reason: Some("stop".into()),
                }),
            ],
            1 => vec![
                Ok(StreamEvent::ToolCallDelta(dss_llm::ToolCallDelta {
                    index: 0,
                    id: Some("repair-write".into()),
                    name: Some("write_file".into()),
                    arguments: Some(
                        r#"{"path":"report.md","content":"corrected report\n"}"#.into(),
                    ),
                })),
                Ok(StreamEvent::Finish {
                    reason: Some("tool_calls".into()),
                }),
            ],
            2 => vec![
                Ok(StreamEvent::ToolCallDelta(dss_llm::ToolCallDelta {
                    index: 0,
                    id: Some("repair-read".into()),
                    name: Some("read_file".into()),
                    arguments: Some(r#"{"path":"report.md"}"#.into()),
                })),
                Ok(StreamEvent::Finish {
                    reason: Some("tool_calls".into()),
                }),
            ],
            _ => vec![
                Ok(StreamEvent::Text("corrected and verified report".into())),
                Ok(StreamEvent::Finish {
                    reason: Some("stop".into()),
                }),
            ],
        };
        Box::pin(async move { Ok(Box::pin(stream::iter(events)) as BoxedEventStream) })
    }

    fn model(&self) -> &str {
        "artifact-tool-repair-review"
    }
}

fn tmp_workspace() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dss-gatestest-{}", uuid::Uuid::new_v4().simple()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

async fn run_agent(
    turns: Vec<ScriptedTurn>,
    tools: Option<Arc<ToolRegistry>>,
) -> (dss_agent::CompleteKind, u32, dss_agent::FrameStatus) {
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(turns)),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("test", tmp_workspace());
    let registry = tools.unwrap_or_else(|| {
        let mut r = ToolRegistry::new();
        builtin::register_all(&mut r);
        Arc::new(r)
    });
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(1024);
    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "do the task",
        &registry,
        &ctx,
        MAX_ITERATIONS,
        500_000,
        None,
        None,
        &[],
        false, // plan_mode
        &tx,
    )
    .await;
    // run 返回后丢弃 tx，使 rx 能收到 None 结束排空。
    drop(tx);
    while rx.recv().await.is_some() {}
    (outcome.kind, outcome.iterations, session.frame.status)
}

async fn run_protocol_events(
    events: Vec<Result<StreamEvent, LlmError>>,
    registry: ToolRegistry,
) -> (dss_agent::RunOutcome, Session, Vec<dss_agent::AgentEvent>) {
    let llm = ProtocolEventsLlm {
        events: Mutex::new(Some(events)),
    };
    let mut session = Session::new("provider-protocol-test", tmp_workspace());
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(128);
    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "validate provider protocol",
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
    drop(tx);
    let mut emitted = Vec::new();
    while let Some(event) = rx.recv().await {
        emitted.push(event);
    }
    (outcome, session, emitted)
}

#[tokio::test]
async fn natural_completion_with_content() {
    // 单轮：有 text 内容、无 tool、finish=stop → natural。
    let turns = vec![ScriptedTurn {
        texts: vec!["hello there".into()],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    }];
    let (kind, iters, _) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::Natural);
    assert_eq!(iters, 1);
}

#[tokio::test]
async fn reasoning_channel_dsml_and_partial_prefixes_fail_without_publication() {
    for reasoning in [
        concat!(
            "<｜DSML｜tool_calls><｜DSML｜invoke name=\"count_probe\">",
            "</｜DSML｜invoke></｜DSML｜tool_calls>"
        ),
        "<||DSM",
    ] {
        let (outcome, session, events) = run_protocol_events(
            vec![
                Ok(StreamEvent::Thinking(reasoning.into())),
                Ok(StreamEvent::Text(
                    "safe text must remain quarantined".into(),
                )),
                Ok(StreamEvent::Finish {
                    reason: Some("stop".into()),
                }),
            ],
            ToolRegistry::new(),
        )
        .await;

        assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
        assert_eq!(outcome.final_text, "");
        assert!(events.iter().all(|event| {
            !matches!(
                event,
                dss_agent::AgentEvent::Thinking { .. }
                    | dss_agent::AgentEvent::Text { .. }
                    | dss_agent::AgentEvent::ToolCalls { .. }
                    | dss_agent::AgentEvent::ToolResults { .. }
            )
        }));
        assert!(session.messages.iter().all(|message| {
            message.content.as_deref().is_none_or(|content| {
                !content.contains("DSML")
                    && !content.contains("DSM")
                    && !content.contains("safe text must remain quarantined")
            })
        }));
    }
}

#[tokio::test]
async fn reasoning_and_text_share_one_bounded_quarantine() {
    let (outcome, session, events) = run_protocol_events(
        vec![
            Ok(StreamEvent::Thinking("r".repeat(1536 * 1024))),
            Ok(StreamEvent::Text("t".repeat(600 * 1024))),
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
        ],
        ToolRegistry::new(),
    )
    .await;
    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|error| error.contains("quarantine limit")));
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            dss_agent::AgentEvent::Thinking { .. } | dss_agent::AgentEvent::Text { .. }
        )
    }));
    assert!(session
        .messages
        .iter()
        .all(|message| message.role != "assistant"));
}

#[tokio::test]
async fn provider_stream_requires_one_terminal_finish_before_execution() {
    let textual_call = concat!(
        "<｜DSML｜tool_calls><｜DSML｜invoke name=\"count_probe\">",
        "</｜DSML｜invoke></｜DSML｜tool_calls>"
    );
    let scenarios = vec![
        vec![Ok(StreamEvent::Text(textual_call.into()))],
        vec![
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
            Ok(StreamEvent::Text("after finish".into())),
        ],
        vec![
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
            Ok(StreamEvent::Thinking("after finish".into())),
        ],
        vec![
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
            Ok(StreamEvent::ToolCallDelta(dss_llm::ToolCallDelta {
                index: 0,
                id: Some("after-finish".into()),
                name: Some("count_probe".into()),
                arguments: Some("{}".into()),
            })),
        ],
        vec![
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
        ],
        vec![
            Ok(StreamEvent::ToolCallDelta(dss_llm::ToolCallDelta {
                index: 0,
                id: Some("incomplete".into()),
                name: None,
                arguments: Some("{}".into()),
            })),
            Ok(StreamEvent::Finish {
                reason: Some("tool_calls".into()),
            }),
        ],
    ];

    for scenario in scenarios {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CountingTool {
            name: "count_probe",
            calls: calls.clone(),
        }));
        let (outcome, session, events) = run_protocol_events(scenario, registry).await;
        assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(events.iter().all(|event| {
            !matches!(
                event,
                dss_agent::AgentEvent::Thinking { .. }
                    | dss_agent::AgentEvent::Text { .. }
                    | dss_agent::AgentEvent::ToolCalls { .. }
                    | dss_agent::AgentEvent::ToolResults { .. }
            )
        }));
        assert!(session
            .messages
            .iter()
            .all(|message| message.tool_calls.is_none()));
    }
}

#[tokio::test]
async fn usage_is_the_only_event_allowed_after_finish() {
    let (outcome, _session, events) = run_protocol_events(
        vec![
            Ok(StreamEvent::Text("safe completed response".into())),
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
            Ok(StreamEvent::Usage(Usage {
                input_tokens: 3,
                output_tokens: 2,
            })),
        ],
        ToolRegistry::new(),
    )
    .await;
    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(outcome.usage.input_tokens, 3);
    assert_eq!(outcome.usage.output_tokens, 2);
    assert!(events.iter().any(|event| {
        matches!(event, dss_agent::AgentEvent::Text { text } if text == "safe completed response")
    }));
}

#[tokio::test]
async fn commonmark_indented_dsml_is_documentation_not_execution() {
    let code_span = serde_json::from_str::<serde_json::Value>(include_str!(
        "../../../test-fixtures/dsml-display-corpus.json"
    ))
    .unwrap()["plain"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let source = format!(
        "{}\n{}",
        concat!(
        "    <｜DSML｜tool_calls><｜DSML｜invoke name=\"count_probe\">\n",
        "    </｜DSML｜invoke></｜DSML｜tool_calls>\n",
        "> ```xml\n",
        "> <｜DSML｜tool_calls><｜DSML｜invoke name=\"count_probe\"></｜DSML｜invoke></｜DSML｜tool_calls>\n",
        "> ````\n",
        "- ~~~xml\n",
        "  <｜DSML｜tool_calls><｜DSML｜invoke name=\"count_probe\"></｜DSML｜invoke></｜DSML｜tool_calls>\n",
        "  ~~~~"
        ),
        code_span.replace("name=\"python\"", "name=\"count_probe\"")
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CountingTool {
        name: "count_probe",
        calls: calls.clone(),
    }));
    let (outcome, session, events) = run_protocol_events(
        vec![
            Ok(StreamEvent::Text(source.clone())),
            Ok(StreamEvent::Finish {
                reason: Some("stop".into()),
            }),
        ],
        registry,
    )
    .await;
    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(events
        .iter()
        .any(|event| matches!(event, dss_agent::AgentEvent::Text { text } if text == &source)));
    assert!(session
        .messages
        .iter()
        .all(|message| message.tool_calls.is_none()));
}

#[tokio::test]
async fn non_top_level_and_abandoned_container_dsml_fail_without_execution_or_leak() {
    let call = concat!(
        "<｜DSML｜tool_calls><｜DSML｜invoke name=\"count_probe\">",
        "<｜DSML｜parameter name=\"code\" string=true>CONTEXT_SECRET</｜DSML｜parameter>",
        "</｜DSML｜invoke></｜DSML｜tool_calls>"
    );
    let mut cases = vec![
        format!("- {call}"),
        format!("> {call}"),
        format!("<!-- {call} -->"),
        format!("<!--\n{call}\n-->"),
        format!("Intro\n    {call}"),
        format!("\\`{call}\\`"),
        format!("narrative before\n{call}"),
        format!("{call}\nnarrative after"),
        format!("> ```text\n> documentation\n{call}\n> ```"),
        format!("- ```text\n  documentation\n{call}\n  ```"),
    ];
    let corpus: serde_json::Value = serde_json::from_str(include_str!(
        "../../../test-fixtures/dsml-display-corpus.json"
    ))
    .unwrap();
    cases.extend(
        corpus["regressions"]
            .as_object()
            .unwrap()
            .values()
            .map(|source| {
                source
                    .as_str()
                    .unwrap()
                    .replace("name=\"python\"", "name=\"count_probe\"")
            }),
    );

    for source in cases {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CountingTool {
            name: "count_probe",
            calls: calls.clone(),
        }));
        let (outcome, session, events) = run_protocol_events(
            vec![
                Ok(StreamEvent::Text(source)),
                Ok(StreamEvent::Finish {
                    reason: Some("stop".into()),
                }),
            ],
            registry,
        )
        .await;

        assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
        assert_eq!(outcome.final_text, "");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(events.iter().all(|event| {
            !matches!(
                event,
                dss_agent::AgentEvent::Text { .. }
                    | dss_agent::AgentEvent::ToolCalls { .. }
                    | dss_agent::AgentEvent::ToolResults { .. }
            )
        }));
        assert!(session.messages.iter().all(|message| {
            message.content.as_deref().is_none_or(|content| {
                !content.contains("DSML") && !content.contains("CONTEXT_SECRET")
            }) && message.tool_calls.is_none()
        }));
    }
}

#[tokio::test]
async fn textual_dsml_is_canonicalized_without_publishing_parameter_body() {
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![
            ScriptedTurn {
                texts: vec![
                    " \n<｜DS".into(),
                    "ML｜tool_calls><｜DSML｜invoke name=\"count_probe\"><｜DSML｜parameter name=\"code\" string=\"true\"># 3. agenda checks\nprint('secret parameter')</｜DSML｜parameter>".into(),
                    "</｜DSML｜invoke></｜DSML｜tool_calls>".into(),
                ],
                finish_reason: Some("tool_calls".into()),
                tool_calls: vec![],
            },
            ScriptedTurn {
                texts: vec!["## Result\n\nCompleted.".into()],
                finish_reason: Some("stop".into()),
                tool_calls: vec![],
            },
        ])),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("textual-dsml-canonical", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CountingTool {
        name: "count_probe",
        calls: calls.clone(),
    }));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "run the probe",
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
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let texts: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            dss_agent::AgentEvent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["## Result\n\nCompleted."]);
    assert!(events.iter().all(|event| match event {
        dss_agent::AgentEvent::Text { text } => {
            !text.contains("DSML") && !text.contains("secret parameter")
        }
        _ => true,
    }));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, dss_agent::AgentEvent::ToolCalls { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, dss_agent::AgentEvent::ToolResults { .. }))
            .count(),
        1
    );

    let canonical = session
        .messages
        .iter()
        .find_map(|message| message.tool_calls.as_ref())
        .expect("canonical assistant tool call");
    assert_eq!(canonical.len(), 1);
    assert!(canonical[0].id.starts_with("dsml-"));
    assert_eq!(canonical[0].function.name, "count_probe");
    let args: serde_json::Value = serde_json::from_str(&canonical[0].function.arguments).unwrap();
    assert_eq!(
        args["code"],
        "# 3. agenda checks\nprint('secret parameter')"
    );
    assert!(session.messages.iter().all(|message| {
        message
            .content
            .as_deref()
            .is_none_or(|content| !content.contains("DSML"))
            && (message.role != "assistant"
                || message.tool_calls.is_some()
                || message
                    .content
                    .as_deref()
                    .is_none_or(|content| !content.contains("secret parameter")))
    }));
}

#[tokio::test]
async fn textual_dsml_zero_parameter_call_executes_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![
            ScriptedTurn {
                texts: vec![concat!(
                    "<｜DSML｜tool_calls><｜DSML｜invoke name=\"count_probe\">",
                    "</｜DSML｜invoke></｜DSML｜tool_calls>"
                )
                .into()],
                finish_reason: Some("tool_calls".into()),
                tool_calls: vec![],
            },
            ScriptedTurn {
                texts: vec!["done".into()],
                finish_reason: Some("stop".into()),
                tool_calls: vec![],
            },
        ])),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("textual-dsml-empty-args", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CountingTool {
        name: "count_probe",
        calls: calls.clone(),
    }));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "run no-argument probe",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let canonical = session
        .messages
        .iter()
        .find_map(|message| message.tool_calls.as_ref())
        .expect("canonical assistant tool call");
    assert_eq!(canonical[0].function.arguments, "{}");
}

#[tokio::test]
async fn native_tool_call_with_plain_narrative_remains_supported() {
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![
            ScriptedTurn {
                texts: vec!["Native preface.\n".into()],
                finish_reason: Some("tool_calls".into()),
                tool_calls: vec![(0, "native-count".into(), "count_probe".into(), "{}".into())],
            },
            ScriptedTurn {
                texts: vec!["native done".into()],
                finish_reason: Some("stop".into()),
                tool_calls: vec![],
            },
        ])),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("native-with-plain-text", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CountingTool {
        name: "count_probe",
        calls: calls.clone(),
    }));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "run native probe",
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
    drop(tx);
    let mut texts = Vec::new();
    while let Some(event) = rx.recv().await {
        if let dss_agent::AgentEvent::Text { text } = event {
            texts.push(text);
        }
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(texts, vec!["Native preface.\n", "native done"]);
}

#[tokio::test]
async fn partial_native_delta_plus_textual_dsml_fails_closed() {
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = MixedPartialNativeLlm;
    let mut session = Session::new("mixed-native-textual-dsml", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CountingTool {
        name: "count_probe",
        calls: calls.clone(),
    }));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "run mixed probe",
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
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|error| error.contains("both native and textual DSML")));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            dss_agent::AgentEvent::Text { .. }
                | dss_agent::AgentEvent::ToolCalls { .. }
                | dss_agent::AgentEvent::ToolResults { .. }
        )
    }));
    assert!(session.messages.iter().all(|message| {
        message
            .content
            .as_deref()
            .is_none_or(|content| !content.contains("DSML"))
            && message.tool_calls.is_none()
    }));
}

#[tokio::test]
async fn malformed_textual_dsml_fails_without_ui_history_or_execution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![ScriptedTurn {
            texts: vec![
                "<｜DSML｜tool_calls><｜DSML｜invoke name=\"count_probe\">".into(),
                "<｜DSML｜parameter name=\"code\" string=true>private malformed body".into(),
            ],
            finish_reason: Some("stop".into()),
            tool_calls: vec![],
        }])),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("malformed-textual-dsml", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(CountingTool {
        name: "count_probe",
        calls: calls.clone(),
    }));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "run malformed probe",
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
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(outcome.final_text, "");
    assert!(outcome.error.as_deref().is_some_and(|error| {
        error.contains("invalid textual DSML") && !error.contains("private malformed body")
    }));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            dss_agent::AgentEvent::Text { .. }
                | dss_agent::AgentEvent::ToolCalls { .. }
                | dss_agent::AgentEvent::ToolResults { .. }
        )
    }));
    assert!(session.messages.iter().all(|message| {
        message.content.as_deref().is_none_or(|content| {
            !content.contains("DSML") && !content.contains("private malformed body")
        })
    }));
}

#[tokio::test]
async fn unresolved_compaction_hard_wall_fails_before_stream_request() {
    let llm = HardWallFailLlm {
        summary_calls: AtomicUsize::new(0),
        stream_calls: AtomicUsize::new(0),
    };
    let mut session = Session::new("hard-wall-test", tmp_workspace());
    for _ in 0..40 {
        session
            .messages
            .push(ChatMessage::user("x".repeat(10_000 * 4)));
    }
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "latest request",
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
    drop(tx);
    let mut terminal_error = None;
    while let Some(event) = rx.recv().await {
        if let dss_agent::AgentEvent::Complete { error, .. } = event {
            terminal_error = error;
        }
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|error| error.contains("tokens 硬墙")));
    assert_eq!(session.frame.status, dss_agent::FrameStatus::Failed);
    assert_eq!(llm.summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(llm.stream_calls.load(Ordering::SeqCst), 0);
    assert!(terminal_error
        .as_deref()
        .is_some_and(|error| error.contains("tokens 硬墙")));
}

#[tokio::test]
async fn tool_schema_tokens_participate_in_runner_hard_wall() {
    let llm = HardWallFailLlm {
        summary_calls: AtomicUsize::new(0),
        stream_calls: AtomicUsize::new(0),
    };
    let mut session = Session::new("tool-schema-wall-test", tmp_workspace());
    // History is below 300k by itself. The 80k-character schema contributes about 20k tokens
    // and must push the real request through compaction/fail-closed instead of chat_stream.
    for _ in 0..29 {
        session
            .messages
            .push(ChatMessage::user("x".repeat(10_000 * 4)));
    }
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(LargeSchemaTool));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "latest request",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(llm.summary_calls.load(Ordering::SeqCst), 1);
    assert_eq!(llm.stream_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn decision_gate_budgets_reset_for_every_user_request() {
    let llm = TwoRunReviewLlm {
        stream_calls: AtomicUsize::new(0),
        review_calls: AtomicUsize::new(0),
    };
    let mut session = Session::new("two-run-gates", tmp_workspace());
    let registry = ToolRegistry::new();

    for (index, prompt) in ["first request", "second request"].into_iter().enumerate() {
        if index == 1 {
            assert_eq!(session.gate_state.veto_count, 1);
            session.gate_state.empty_retry_count = 3;
            session.gate_state.retrieval_streak = 5;
            session.gate_state.length_finish_count = 4;
            session.gate_state.plan_denial_count = 3;
        }
        let ctx = ToolContext::new(session.workspace.clone());
        let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
        let outcome = Runner::run(
            &mut session,
            &llm,
            llm.model(),
            prompt,
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
        drop(tx);
        while rx.recv().await.is_some() {}
        assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    }

    // The first failed draft and its corrected draft are both reviewed; the second
    // user request receives its own review after the per-request gate reset.
    assert_eq!(llm.review_calls.load(Ordering::SeqCst), 3);
    assert_eq!(session.gate_state.empty_retry_count, 0);
    assert_eq!(session.gate_state.retrieval_streak, 0);
    assert_eq!(session.gate_state.length_finish_count, 0);
    assert_eq!(session.gate_state.plan_denial_count, 0);
}

#[tokio::test]
async fn reviewer_veto_resets_stream_and_persists_rejected_draft_as_internal() {
    let llm = TwoRunReviewLlm {
        stream_calls: AtomicUsize::new(0),
        review_calls: AtomicUsize::new(0),
    };
    let mut session = Session::new("review-draft-test", tmp_workspace());
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "review this answer",
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
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(outcome.final_text, "revised first answer");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, dss_agent::AgentEvent::DraftReset { reason } if reason == "reviewer_veto"))
            .count(),
        1
    );
    let rejected = session
        .messages
        .iter()
        .find(|message| message.content.as_deref() == Some("first draft"))
        .expect("rejected draft retained for model history");
    assert!(rejected.harness_notice);
    let revised = session
        .messages
        .iter()
        .find(|message| message.content.as_deref() == Some("revised first answer"))
        .expect("revised final answer");
    assert!(!revised.harness_notice);
}

#[tokio::test]
async fn corrected_draft_that_still_fails_review_is_not_silently_accepted() {
    let llm = AlwaysFailReviewLlm {
        stream_calls: AtomicUsize::new(0),
        review_calls: AtomicUsize::new(0),
    };
    let mut session = Session::new("review-fail-closed-test", tmp_workspace());
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "make an unsupported scientific claim",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|error| error.contains("reviewer verification still failed")));
    assert_eq!(llm.review_calls.load(Ordering::SeqCst), 2);
    assert_eq!(llm.stream_calls.load(Ordering::SeqCst), 2);
    assert_eq!(session.frame.status, dss_agent::FrameStatus::Failed);
    let first = session
        .messages
        .iter()
        .find(|message| message.content.as_deref() == Some("unsupported draft 1"))
        .expect("first rejected draft retained");
    assert!(first.harness_notice);
    let second = session
        .messages
        .iter()
        .find(|message| message.content.as_deref() == Some("unsupported draft 2"))
        .expect("second failed draft retained for retry evidence");
    assert!(!second.harness_notice);
}

#[tokio::test]
async fn corrected_draft_with_unavailable_review_after_veto_fails_closed() {
    let llm = VetoThenUnavailableReviewLlm {
        stream_calls: AtomicUsize::new(0),
        review_calls: AtomicUsize::new(0),
    };
    let mut session = Session::new("review-none-fail-closed-test", tmp_workspace());
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "produce an evidence-backed answer",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    let error = outcome.error.as_deref().expect("fail-closed error");
    assert!(error.contains("reviewer verification was unavailable after a prior veto"));
    assert!(error.contains("fix cited evidence"));
    assert_eq!(llm.review_calls.load(Ordering::SeqCst), 2);
    assert_eq!(llm.stream_calls.load(Ordering::SeqCst), 2);
    assert_eq!(session.frame.status, dss_agent::FrameStatus::Failed);
}

#[tokio::test]
async fn artifact_repair_text_only_claim_is_not_sent_for_second_review() {
    let llm = ArtifactTextOnlyReviewLlm {
        stream_calls: AtomicUsize::new(0),
        review_calls: AtomicUsize::new(0),
    };
    let mut session = Session::new("artifact-text-only-gate", tmp_workspace());
    let mut registry = ToolRegistry::new();
    builtin::register_all(&mut registry);
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "repair report.md",
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
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert!(outcome.error.as_deref().is_some_and(|error| error.contains(
        "reviewer-required artifact repair was not completed after the corrective reminder"
    )));
    assert_eq!(llm.review_calls.load(Ordering::SeqCst), 1);
    assert_eq!(llm.stream_calls.load(Ordering::SeqCst), 3);
    assert_eq!(session.frame.status, dss_agent::FrameStatus::Failed);
    assert!(events.iter().any(|event| matches!(
        event,
        dss_agent::AgentEvent::DraftReset { reason }
            if reason == "reviewer_artifact_repair_required"
    )));
    assert!(session.messages.iter().any(|message| {
        message.harness_notice
            && message.content.as_deref().is_some_and(|content| {
                content.contains("必须先实际调用 write_file 或 edit_file")
                    && content.contains("不要只重复最终回复")
            })
    }));
}

#[tokio::test]
async fn artifact_repair_write_then_later_read_is_eligible_for_second_review() {
    let stream_requests = Arc::new(Mutex::new(Vec::new()));
    let llm = ArtifactToolRepairReviewLlm {
        stream_calls: AtomicUsize::new(0),
        review_calls: AtomicUsize::new(0),
        stream_requests: stream_requests.clone(),
    };
    let workspace = tmp_workspace();
    let mut session = Session::new("artifact-tool-repair-gate", workspace.clone());
    let mut registry = ToolRegistry::new();
    builtin::register_all(&mut registry);
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "repair report.md",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(outcome.final_text, "corrected and verified report");
    assert_eq!(llm.review_calls.load(Ordering::SeqCst), 2);
    assert_eq!(llm.stream_calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        std::fs::read_to_string(workspace.join("report.md")).unwrap(),
        "corrected report\n"
    );

    let requests = stream_requests.lock().unwrap();
    let corrective_request = requests.get(1).expect("corrective tool request");
    let tool_names = corrective_request
        .tools
        .as_ref()
        .expect("tools remain available during correction")
        .iter()
        .map(|tool| tool.function.name.as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"write_file"));
    assert!(tool_names.contains(&"read_file"));
    assert!(corrective_request.messages.iter().any(|message| {
        message.harness_notice
            && message.content.as_deref().is_some_and(|content| {
                content.contains("repair_scope=artifact")
                    && content.contains("工具仍然可用")
                    && content.contains("后续独立工具轮")
            })
    }));

    let write_pos = session
        .messages
        .iter()
        .position(|message| message.name.as_deref() == Some("write_file"))
        .expect("successful write result");
    let read_pos = session
        .messages
        .iter()
        .position(|message| message.name.as_deref() == Some("read_file"))
        .expect("successful read result");
    assert!(write_pos < read_pos);
    assert_eq!(session.messages[write_pos].is_error, Some(false));
    assert_eq!(session.messages[read_pos].is_error, Some(false));
}

#[tokio::test]
async fn terminal_reviewer_receives_only_the_active_ordered_tool_trace() {
    let review_requests = Arc::new(Mutex::new(Vec::new()));
    let llm = TraceReviewLlm {
        stream_calls: AtomicUsize::new(0),
        review_requests: review_requests.clone(),
    };
    let mut session = Session::new("review-trace-test", tmp_workspace());
    session
        .messages
        .push(ChatMessage::assistant("UNRELATED_PREVIOUS_RUN_MARKER"));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReviewEvidenceTool));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "audit this tool-backed analysis",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    let requests = review_requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let prompt = requests[0].messages[1].content.as_deref().unwrap();
    let user_pos = prompt.find("[1] role=user").unwrap();
    let call_pos = prompt
        .find("[2] role=assistant\ntool_call: id=call-read name=read_evidence_for_review")
        .unwrap();
    let result_pos = prompt.rfind("result: sampling rule: B=40").unwrap();
    let final_pos = prompt.find("Agent 的最终输出").unwrap();

    assert!(user_pos < call_pos);
    assert!(call_pos < result_pos);
    assert!(result_pos < final_pos);
    assert!(prompt.contains("call_id=call-read status=ok"));
    assert!(prompt.contains("bounded scientific answer"));
    assert!(!prompt.contains("UNRELATED_PREVIOUS_RUN_MARKER"));
}

#[tokio::test]
async fn run_context_is_sent_but_never_enters_canonical_history() {
    let turns = vec![ScriptedTurn {
        texts: vec!["context acknowledged".into()],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    }];
    let seen = Arc::new(Mutex::new(Vec::new()));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(turns)),
        seen_messages: Some(seen.clone()),
        seen_tool_names: None,
    };
    let mut session = Session::new("context-test", tmp_workspace());
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
    let mut context = ChatMessage::system("[Project Context]\nprivate instructions");
    context.harness_notice = true;

    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "do the task",
        &registry,
        &ctx,
        MAX_ITERATIONS,
        500_000,
        None,
        None,
        &[context],
        false,
        &tx,
    )
    .await;
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0][0].role, "system");
    assert!(requests[0][0]
        .content
        .as_deref()
        .is_some_and(|text| text.contains("evidence-first scientific research agent")));
    assert_eq!(requests[0][1].role, "system");
    assert!(requests[0][1]
        .content
        .as_deref()
        .is_some_and(|text| text.contains("private instructions")));
    assert!(session.messages.iter().all(|message| {
        !message
            .content
            .as_deref()
            .is_some_and(|text| text.contains("evidence-first scientific research agent"))
    }));
    assert!(session.messages.iter().all(|message| {
        message.role != "system"
            && !message.harness_notice
            && !message
                .content
                .as_deref()
                .is_some_and(|text| text.contains("private instructions"))
    }));
}

#[tokio::test]
async fn approved_plan_context_preserves_explicit_iteration_limit() {
    let mut turns = Vec::new();
    for index in 0..5 {
        turns.push(ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![(
                0,
                format!("read-{index}"),
                "read_evidence_for_review".into(),
                r#"{"path":"README.md"}"#.into(),
            )],
        });
    }
    turns.push(ScriptedTurn {
        texts: vec!["finished within the requested budget".into()],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    });

    let seen = Arc::new(Mutex::new(Vec::new()));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(turns)),
        seen_messages: Some(seen.clone()),
        seen_tool_names: None,
    };
    let mut session = Session::new("approved-budget-context", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReviewEvidenceTool));
    let ctx = ToolContext::new(session.workspace.clone());
    let mut original = ChatMessage::system(
        "[Original approved-plan request and constraints]\nFinish in ≤6 iterations.",
    );
    original.harness_notice = true;
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(128);

    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "execute the approved plan",
        &registry,
        &ctx,
        MAX_ITERATIONS,
        500_000,
        None,
        None,
        &[original],
        false,
        &tx,
    )
    .await;
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(outcome.iterations, 6);
    let requests = seen.lock().unwrap();
    assert_eq!(requests.len(), 6);
    assert!(requests[4].iter().all(|message| {
        !message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("最后一轮（6 轮）"))
    }));
    assert!(requests[5].iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("最后一轮（6 轮）"))
    }));
}

#[tokio::test]
async fn schedule_iteration_numbers_do_not_override_explicit_hard_limit() {
    let mut turns = Vec::new();
    for index in 1..=4 {
        turns.push(ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![(
                0,
                format!("scheduled-read-{index}"),
                "read_evidence_for_review".into(),
                r#"{"path":"README.md"}"#.into(),
            )],
        });
    }
    turns.push(ScriptedTurn {
        texts: vec!["schedule completed without lowering the hard limit".into()],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    });

    let seen_tool_names = Arc::new(Mutex::new(Vec::new()));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(turns)),
        seen_messages: None,
        seen_tool_names: Some(seen_tool_names.clone()),
    };
    let mut session = Session::new("scheduled-hard-iteration-budget", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReviewEvidenceTool));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(128);
    let prompt = "此轮最多 12 个 agent iterations（硬上限）\n\
                  1. 前 4 个 iteration：只用 fetch_url，不使用 web_search。\n\
                  2. 第 5 个 iteration：汇总已抓取证据。\n\
                  3. 第 6 个 iteration：更新三个文件。\n\
                  4. 第 7 个 iteration：逐一读回验证。\n\
                  5. iterations 1-4 属于抓取阶段。";

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        prompt,
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(outcome.iterations, 5);
    assert_eq!(
        seen_tool_names.lock().unwrap().len(),
        5,
        "the schedule's `前 4 个 iteration` must not disable tools on turn four"
    );
}

#[tokio::test]
async fn explicit_agent_iteration_budget_is_hard_and_final_turn_has_no_tools() {
    let turns = Arc::new(Mutex::new(vec![
        ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![(
                0,
                "read-1".into(),
                "read_evidence_for_review".into(),
                r#"{"path":"README.md"}"#.into(),
            )],
        },
        ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![(
                0,
                "read-2".into(),
                "read_evidence_for_review".into(),
                r#"{"path":"README.md"}"#.into(),
            )],
        },
        ScriptedTurn {
            texts: vec!["must never reach iteration three".into()],
            finish_reason: Some("stop".into()),
            tool_calls: vec![],
        },
    ]));
    let seen_tool_names = Arc::new(Mutex::new(Vec::new()));
    let llm = StreamFakeLlm {
        turns: turns.clone(),
        seen_messages: None,
        seen_tool_names: Some(seen_tool_names.clone()),
    };
    let mut session = Session::new("hard-user-iteration-budget", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReviewEvidenceTool));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "Use at most 2 agent iterations.",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(outcome.iterations, 2);
    assert!(outcome.error.as_deref().is_some_and(|error| {
        error.contains("agent iteration budget exhausted (2)")
            && error.contains("attempted tool call")
    }));
    assert_eq!(turns.lock().unwrap().len(), 1, "must not enter N+1");
    let requests = seen_tool_names.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], vec!["read_evidence_for_review"]);
    assert!(
        requests[1].is_empty(),
        "final request must advertise no tools"
    );
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == "tool")
            .count(),
        1,
        "the final attempted tool call must not execute"
    );
}

#[tokio::test]
async fn final_tools_disabled_turn_rejects_textual_dsml_without_ui_or_history_leak() {
    let seen_tool_names = Arc::new(Mutex::new(Vec::new()));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![ScriptedTurn {
            texts: vec![
                "<｜｜DS".into(),
                "ML｜｜tool_calls> <｜｜DSML｜｜invoke name=\"python\">".into(),
                "<｜｜DSML｜｜parameter name=\"code\" string=\"true\">print('must not execute or render')</｜｜DSML｜｜parameter>".into(),
                "</｜｜DSML｜｜invoke></｜｜DSML｜｜tool_calls>".into(),
            ],
            finish_reason: Some("stop".into()),
            tool_calls: vec![],
        }])),
        seen_messages: None,
        seen_tool_names: Some(seen_tool_names.clone()),
    };
    let mut session = Session::new("textual-dsml-final-turn", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReviewEvidenceTool));
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "Use at most 1 agent iteration.",
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
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(outcome.iterations, 1);
    assert_eq!(outcome.final_text, "");
    assert!(outcome.error.as_deref().is_some_and(|error| {
        error.contains("attempted tool call") && error.contains("tools were disabled")
    }));
    assert!(seen_tool_names.lock().unwrap()[0].is_empty());
    assert!(events
        .iter()
        .all(|event| !matches!(event, dss_agent::AgentEvent::Text { .. })));
    assert!(events.iter().all(|event| match event {
        dss_agent::AgentEvent::Complete { final_text, .. } => {
            !final_text.contains("DSML") && !final_text.contains("must not execute")
        }
        _ => true,
    }));
    assert!(session.messages.iter().all(|message| {
        message.content.as_deref().is_none_or(|content| {
            !content.contains("DSML") && !content.contains("must not execute")
        })
    }));
}

#[tokio::test]
async fn final_tools_disabled_turn_preserves_dsml_markers_in_markdown_code_examples() {
    let answer = concat!(
        "# Protocol example\n\n",
        "```text\n",
        "<｜｜DSML｜｜tool_calls> <｜｜DSML｜｜invoke name=\"python\">\n",
        "```\n\n",
        "The inline spelling `<||DSML||tool_calls>` is documentation, not a call."
    );
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![ScriptedTurn {
            texts: vec![answer.into()],
            finish_reason: Some("stop".into()),
            tool_calls: vec![],
        }])),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("documented-dsml-final-turn", tmp_workspace());
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "Use at most 1 agent iteration.",
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
    drop(tx);
    let mut streamed_text = String::new();
    while let Some(event) = rx.recv().await {
        if let dss_agent::AgentEvent::Text { text } = event {
            streamed_text.push_str(&text);
        }
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(outcome.final_text, answer);
    assert_eq!(streamed_text, answer);
    assert!(session.messages.iter().any(|message| {
        message.role == "assistant" && message.content.as_deref() == Some(answer)
    }));
}

#[tokio::test]
async fn reviewer_veto_on_final_budgeted_iteration_fails_with_findings() {
    let llm = TwoRunReviewLlm {
        stream_calls: AtomicUsize::new(0),
        review_calls: AtomicUsize::new(0),
    };
    let mut session = Session::new("review-veto-budget-test", tmp_workspace());
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        llm.model(),
        "Use at most 1 agent iteration.",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(outcome.iterations, 1);
    let error = outcome.error.as_deref().expect("iteration budget error");
    assert!(error.contains("agent iteration budget exhausted (1)"));
    assert!(error.contains("revise once"));
    assert_eq!(llm.stream_calls.load(Ordering::SeqCst), 1);
    assert_eq!(llm.review_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_tool_result_keeps_error_state_in_history() {
    let turns = vec![
        ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![(0, "missing-call".into(), "missing_tool".into(), "{}".into())],
        },
        ScriptedTurn {
            texts: vec!["recovered".into()],
            finish_reason: Some("stop".into()),
            tool_calls: vec![],
        },
    ];
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(turns)),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("tool-error-test", tmp_workspace());
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "use the unavailable tool",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    let tool_result = session
        .messages
        .iter()
        .find(|message| message.role == "tool")
        .expect("persisted tool result");
    assert_eq!(tool_result.is_error, Some(true));
}

#[tokio::test]
async fn a2a_mixed_batch_fails_closed_pairs_every_call_then_recovers_standalone() {
    let turns = vec![
        ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![
                (
                    0,
                    "mixed-a2a-1".into(),
                    "a2a_agent_fixture".into(),
                    r#"{"task":"first delegation"}"#.into(),
                ),
                (
                    1,
                    "mixed-slow".into(),
                    "slow_local_probe".into(),
                    "{}".into(),
                ),
                (
                    2,
                    "mixed-a2a-2".into(),
                    "a2a_agent_fixture".into(),
                    r#"{"task":"second delegation"}"#.into(),
                ),
            ],
        },
        ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![(
                0,
                "standalone-a2a".into(),
                "a2a_agent_fixture".into(),
                r#"{"task":"retry alone"}"#.into(),
            )],
        },
        ScriptedTurn {
            texts: vec!["used the restored remote transcript".into()],
            finish_reason: Some("stop".into()),
            tool_calls: vec![],
        },
    ];
    let seen_messages = Arc::new(Mutex::new(Vec::new()));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(turns)),
        seen_messages: Some(seen_messages.clone()),
        seen_tool_names: None,
    };
    let a2a_calls = Arc::new(AtomicUsize::new(0));
    let slow_calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExclusiveA2aProbeTool {
        calls: a2a_calls.clone(),
    }));
    registry.register(Arc::new(NeverCompletesProbeTool {
        calls: slow_calls.clone(),
    }));
    let mut session = Session::new("a2a-exclusive-batch-test", tmp_workspace());
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);

    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        Runner::run(
            &mut session,
            &llm,
            llm.model(),
            "delegate, then use one local tool",
            &registry,
            &ctx,
            MAX_ITERATIONS,
            500_000,
            None,
            None,
            &[],
            false,
            &tx,
        ),
    )
    .await
    .expect("mixed batch must be rejected before the never-completing local tool starts");
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert_eq!(outcome.iterations, 3);
    assert_eq!(a2a_calls.load(Ordering::SeqCst), 1);
    assert_eq!(slow_calls.load(Ordering::SeqCst), 0);

    let result_batches: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            dss_agent::AgentEvent::ToolResults { results } => Some(results),
            _ => None,
        })
        .collect();
    assert_eq!(result_batches.len(), 2);
    assert_eq!(result_batches[0].len(), 3);
    assert_eq!(
        result_batches[0]
            .iter()
            .map(|result| result.tool_use_id.as_str())
            .collect::<Vec<_>>(),
        ["mixed-a2a-1", "mixed-slow", "mixed-a2a-2"]
    );
    assert!(result_batches[0].iter().all(|result| {
        result.is_error
            && result
                .content
                .contains("call exactly one A2A tool by itself")
    }));
    assert_eq!(result_batches[1].len(), 1);
    assert_eq!(result_batches[1][0].tool_use_id, "standalone-a2a");
    assert!(!result_batches[1][0].is_error);

    let persisted_tool_ids = session
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect::<Vec<_>>();
    assert_eq!(
        persisted_tool_ids,
        ["mixed-a2a-1", "mixed-slow", "mixed-a2a-2", "standalone-a2a"]
    );

    let seen_messages = seen_messages.lock().unwrap();
    let recovery_request = seen_messages.get(1).expect("next-turn model request");
    assert!(recovery_request.iter().any(|message| {
        message.role == "tool"
            && message.tool_call_id.as_deref() == Some("mixed-a2a-1")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("call exactly one A2A tool by itself"))
    }));
}

#[tokio::test]
async fn empty_retry_then_fails_after_cap() {
    // 连续 4 轮空响应（无 text、无 tool、finish=stop）→ 第 4 次超 cap(3) → error。
    let empty = ScriptedTurn {
        texts: vec![],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    };
    let turns = vec![empty.clone(), empty.clone(), empty.clone(), empty.clone()];
    let (kind, _iters, _) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::Error);
}

#[tokio::test]
async fn empty_retry_recovers_when_content_arrives() {
    // 2 轮空 + 第 3 轮有内容 → natural（empty_retry 不超 cap）。
    let empty = ScriptedTurn {
        texts: vec![],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    };
    let turns = vec![
        empty.clone(),
        empty.clone(),
        ScriptedTurn {
            texts: vec!["now I respond".into()],
            finish_reason: Some("stop".into()),
            tool_calls: vec![],
        },
    ];
    let (kind, iters, _) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::Natural);
    assert_eq!(iters, 3);
}

#[tokio::test]
async fn max_tokens_length_terminates_at_cap() {
    // 连续 5 轮 finish=length → 第 5 次终止（MaxIters）。
    let length = ScriptedTurn {
        texts: vec!["partial...".into()],
        finish_reason: Some("length".into()),
        tool_calls: vec![],
    };
    let turns = vec![
        length.clone(),
        length.clone(),
        length.clone(),
        length.clone(),
        length.clone(),
    ];
    let (kind, _iters, status) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::MaxIters);
    assert_eq!(status, dss_agent::FrameStatus::Failed);
}

#[tokio::test]
async fn retrieval_circuit_breaker_injects_after_six_rounds() {
    // 连续 6 轮只调 web_search（检索类），第 7 轮有内容 → natural。
    // 需要工具注册表里有 web_search（builtin 有）。
    // 用 list_files（检索类、无网络、在空 workspace 上即时返回）驱动检索熔断。
    let search_turn = ScriptedTurn {
        texts: vec![],
        finish_reason: Some("tool_calls".into()),
        tool_calls: vec![(0, "call_1".into(), "list_files".into(), r#"{}"#.into())],
    };
    let mut turns: Vec<ScriptedTurn> = (0..6).map(|_| search_turn.clone()).collect();
    // 第 7 轮：有内容 → natural（检索熔断已在第 6 轮注入 notice，第 7 轮模型写作）。
    turns.push(ScriptedTurn {
        texts: vec!["final answer based on research".into()],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    });
    let (kind, iters, _) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::Natural);
    // 6 轮检索 + 1 轮写作 = 7 iteration。
    assert!(iters >= 7, "expected at least 7 iterations, got {iters}");
}

fn update_plan_turn() -> ScriptedTurn {
    ScriptedTurn {
        texts: vec![],
        finish_reason: Some("tool_calls".into()),
        tool_calls: vec![(
            0,
            "call_update".into(),
            "update_step_status".into(),
            r#"{"step_id":0,"status":"done"}"#.into(),
        )],
    }
}

struct PendingPlanMutationTool {
    started: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl Tool for PendingPlanMutationTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "update_step_status".into(),
            description: "test-only plan mutation that waits for cancellation".into(),
            parameters: serde_json::json!({ "type": "object" }),
        }
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let mut plan = ctx.plan.lock().await;
        plan.as_mut().expect("seeded plan").steps[0].status = "done".into();
        drop(plan);
        self.started.notify_one();
        futures::future::pending::<Result<ToolOutput, ToolError>>().await
    }
}

async fn run_with_approved_plan(
    turns: Vec<ScriptedTurn>,
    max_iterations: u32,
) -> (dss_agent::RunOutcome, Session, Vec<dss_agent::AgentEvent>) {
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(turns)),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("plan-test", tmp_workspace());
    let mut registry = ToolRegistry::new();
    builtin::register_all(&mut registry);
    let ctx = ToolContext::new(session.workspace.clone())
        .with_plan(Some(dss_tools::PlanState {
            steps: vec![dss_tools::PlanStep {
                title: "execute".into(),
                status: "pending".into(),
            }],
            approved: true,
            research_question: None,
        }))
        .await;
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(1024);

    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "execute the approved plan",
        &registry,
        &ctx,
        max_iterations,
        500_000,
        None,
        None,
        &[],
        false,
        &tx,
    )
    .await;
    drop(tx);

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    (outcome, session, events)
}

async fn run_preapproval_plan(
    turns: Vec<ScriptedTurn>,
    registry: ToolRegistry,
    max_iterations: u32,
    context_window: usize,
) -> (
    dss_agent::RunOutcome,
    Session,
    Vec<dss_agent::AgentEvent>,
    Vec<Vec<String>>,
    Option<dss_tools::PendingAsk>,
) {
    let seen_tool_names = Arc::new(Mutex::new(Vec::new()));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(turns)),
        seen_messages: None,
        seen_tool_names: Some(seen_tool_names.clone()),
    };
    let mut session = Session::new("preapproval-plan-test", tmp_workspace());
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(1024);

    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "plan the work",
        &registry,
        &ctx,
        max_iterations,
        context_window,
        None,
        None,
        &[],
        true,
        &tx,
    )
    .await;
    drop(tx);

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    let seen_tool_names = seen_tool_names.lock().unwrap().clone();
    let pending_ask = ctx.pending_ask.lock().await.clone();
    (outcome, session, events, seen_tool_names, pending_ask)
}

fn registry_with_builtins() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    builtin::register_all(&mut registry);
    registry
}

#[tokio::test]
async fn plan_tool_batch_publishes_snapshot_before_next_iteration() {
    let (outcome, session, events) = run_with_approved_plan(
        vec![
            update_plan_turn(),
            ScriptedTurn {
                texts: vec!["plan finished".into()],
                finish_reason: Some("stop".into()),
                tool_calls: vec![],
            },
        ],
        MAX_ITERATIONS,
    )
    .await;

    let tool_results_index = events
        .iter()
        .position(|event| matches!(event, dss_agent::AgentEvent::ToolResults { .. }))
        .expect("tool results event");
    let plan_update_index = events
        .iter()
        .position(|event| matches!(event, dss_agent::AgentEvent::PlanUpdate { .. }))
        .expect("mid-run plan update event");
    let next_iteration_index = events
        .iter()
        .position(|event| matches!(event, dss_agent::AgentEvent::Iteration { n: 2 }))
        .expect("next iteration event");
    let updated_plan = events
        .iter()
        .find_map(|event| match event {
            dss_agent::AgentEvent::PlanUpdate { plan } => Some(plan),
            _ => None,
        })
        .expect("published plan snapshot");

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert!(tool_results_index < plan_update_index);
    assert!(plan_update_index < next_iteration_index);
    assert_eq!(updated_plan.steps[0].status, "done");
    assert_eq!(
        session.plan.expect("session plan snapshot").steps[0].status,
        "done"
    );
}

#[tokio::test]
async fn approved_plan_cannot_complete_naturally_with_pending_steps() {
    let premature = ScriptedTurn {
        texts: vec!["premature final answer".into()],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    };
    let (outcome, session, events) = run_with_approved_plan(
        vec![
            premature.clone(),
            premature.clone(),
            premature.clone(),
            premature,
        ],
        MAX_ITERATIONS,
    )
    .await;

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Awaiting);
    assert_eq!(outcome.final_text, "");
    assert_eq!(outcome.awaiting.as_deref(), Some("plan_execution"));
    assert!(outcome.pending_ask.is_none());
    assert_eq!(outcome.iterations, 4);
    assert_eq!(
        session.frame.status,
        dss_agent::FrameStatus::AwaitingPlanExecution
    );
    let plan = session.plan.as_ref().expect("retryable approved plan");
    assert!(plan.approved);
    assert_eq!(plan.steps[0].status, "pending");

    let reset_count = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                dss_agent::AgentEvent::DraftReset { reason }
                    if reason == "plan_incomplete"
            )
        })
        .count();
    assert_eq!(reset_count, 4);
    let terminal = events.iter().find_map(|event| match event {
        dss_agent::AgentEvent::Complete {
            kind: dss_agent::CompleteKind::Awaiting,
            awaiting,
            frame_status,
            plan,
            ..
        } => Some((awaiting, frame_status, plan)),
        _ => None,
    });
    let (awaiting, frame_status, terminal_plan) = terminal.expect("awaiting terminal event");
    assert_eq!(awaiting.as_deref(), Some("plan_execution"));
    assert_eq!(*frame_status, dss_agent::FrameStatus::AwaitingPlanExecution);
    assert!(terminal_plan.as_ref().is_some_and(|plan| plan.approved));

    let rejected_drafts: Vec<_> = session
        .messages
        .iter()
        .filter(|message| message.content.as_deref() == Some("premature final answer"))
        .collect();
    assert_eq!(rejected_drafts.len(), 4);
    assert!(rejected_drafts.iter().all(|message| message.harness_notice));
}

#[tokio::test]
async fn generate_plan_batch_commits_snapshot_before_awaiting_approval() {
    let turns = vec![ScriptedTurn {
        texts: vec![],
        finish_reason: Some("tool_calls".into()),
        tool_calls: vec![(
            0,
            "call_generate".into(),
            "generate_plan".into(),
            r#"{"steps":[{"title":"collect evidence"},{"title":"write report"}]}"#.into(),
        )],
    }];
    let (outcome, session, events, seen_tool_names, _) =
        run_preapproval_plan(turns, registry_with_builtins(), MAX_ITERATIONS, 500_000).await;
    let tool_results_index = events
        .iter()
        .position(|event| matches!(event, dss_agent::AgentEvent::ToolResults { .. }))
        .expect("tool results event");
    let plan_update_index = events
        .iter()
        .position(|event| matches!(event, dss_agent::AgentEvent::PlanUpdate { .. }))
        .expect("plan update event");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, dss_agent::AgentEvent::PlanUpdate { .. }))
            .count(),
        1,
        "a generated plan must publish exactly one snapshot"
    );
    let complete_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                dss_agent::AgentEvent::Complete {
                    kind: dss_agent::CompleteKind::Awaiting,
                    ..
                }
            )
        })
        .expect("awaiting complete event");
    let terminal_plan = events.iter().find_map(|event| match event {
        dss_agent::AgentEvent::Complete {
            kind: dss_agent::CompleteKind::Awaiting,
            plan,
            ..
        } => plan.as_ref(),
        _ => None,
    });

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Awaiting);
    assert_eq!(outcome.awaiting.as_deref(), Some("plan_approval"));
    assert_eq!(outcome.iterations, 1);
    assert_eq!(
        session.frame.status,
        dss_agent::FrameStatus::AwaitingPlanApproval
    );
    assert!(tool_results_index < plan_update_index);
    assert!(plan_update_index < complete_index);
    assert_eq!(
        terminal_plan.expect("awaiting plan snapshot").steps.len(),
        2
    );
    assert_eq!(
        session
            .plan
            .as_ref()
            .expect("session plan snapshot")
            .steps
            .len(),
        2
    );
    assert!(std::fs::read_dir(&session.workspace)
        .expect("read workspace")
        .next()
        .is_none());

    let mut first_request_tools = seen_tool_names
        .first()
        .expect("first model request")
        .clone();
    first_request_tools.sort();
    assert_eq!(first_request_tools, ["ask_user", "generate_plan"]);
}

#[tokio::test]
async fn generate_plan_and_ask_user_batch_prefers_plan_approval() {
    let turns = vec![ScriptedTurn {
        texts: vec![],
        finish_reason: Some("tool_calls".into()),
        tool_calls: vec![
            (
                0,
                "call_generate".into(),
                "generate_plan".into(),
                r#"{"steps":[{"title":"collect evidence"},{"title":"test hypothesis"}]}"#
                    .into(),
            ),
            (
                1,
                "call_ask".into(),
                "ask_user".into(),
                r#"{"question":"Which assay should I use?","options":[{"label":"A"},{"label":"B"}]}"#
                    .into(),
            ),
        ],
    }];
    let (outcome, session, events, _, pending_ask) =
        run_preapproval_plan(turns, registry_with_builtins(), MAX_ITERATIONS, 500_000).await;

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Awaiting);
    assert_eq!(outcome.awaiting.as_deref(), Some("plan_approval"));
    assert!(outcome.pending_ask.is_none());
    assert!(pending_ask.is_none(), "superseded ask must be cleared");
    assert_eq!(outcome.iterations, 1);
    assert_eq!(
        session.frame.status,
        dss_agent::FrameStatus::AwaitingPlanApproval
    );
    assert!(session
        .plan
        .as_ref()
        .is_some_and(|plan| !plan.approved && plan.steps.len() == 2));

    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, dss_agent::AgentEvent::PlanUpdate { .. }))
            .count(),
        1,
        "the generated plan must publish exactly once"
    );

    let results = events
        .iter()
        .find_map(|event| match event {
            dss_agent::AgentEvent::ToolResults { results } => Some(results),
            _ => None,
        })
        .expect("both tool results remain in the audit stream");
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|result| !result.is_error));
    assert!(results
        .iter()
        .any(|result| result.tool_use_id == "call_generate"));
    let published_ask = results
        .iter()
        .find(|result| result.tool_use_id == "call_ask")
        .expect("published ask result");
    assert!(published_ask.content.contains("ask superseded"));
    assert!(published_ask
        .content
        .contains("no separate user response is pending"));
    assert!(published_ask.content.contains("awaiting plan approval"));
    assert!(!published_ask.content.contains("waiting for user response"));

    let persisted_ask = session
        .messages
        .iter()
        .find(|message| {
            message.role == "tool" && message.tool_call_id.as_deref() == Some("call_ask")
        })
        .expect("persisted ask result");
    assert_eq!(
        persisted_ask.content.as_deref(),
        Some(published_ask.content.as_str())
    );
    assert_eq!(persisted_ask.is_error, Some(false));

    let terminal = events.iter().find_map(|event| match event {
        dss_agent::AgentEvent::Complete {
            kind: dss_agent::CompleteKind::Awaiting,
            awaiting,
            frame_status,
            pending_ask,
            plan,
            ..
        } => Some((awaiting, frame_status, pending_ask, plan)),
        _ => None,
    });
    let (awaiting, frame_status, terminal_ask, terminal_plan) =
        terminal.expect("plan approval terminal event");
    assert_eq!(awaiting.as_deref(), Some("plan_approval"));
    assert_eq!(*frame_status, dss_agent::FrameStatus::AwaitingPlanApproval);
    assert!(terminal_ask.is_none());
    assert!(terminal_plan.as_ref().is_some_and(|plan| !plan.approved));
}

#[tokio::test]
async fn preapproval_ask_user_pauses_without_consuming_plan_denial() {
    let turns = vec![ScriptedTurn {
        texts: vec![],
        finish_reason: Some("tool_calls".into()),
        tool_calls: vec![(
            0,
            "call_ask".into(),
            "ask_user".into(),
            r#"{"question":"Which assay should I use?","options":[{"label":"A"},{"label":"B"}]}"#
                .into(),
        )],
    }];
    let (outcome, session, events, seen_tool_names, pending_ask) =
        run_preapproval_plan(turns, registry_with_builtins(), MAX_ITERATIONS, 500_000).await;

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Awaiting);
    assert_eq!(outcome.awaiting.as_deref(), Some("user_response"));
    assert_eq!(outcome.iterations, 1);
    assert_eq!(
        outcome
            .pending_ask
            .as_ref()
            .map(|ask| ask.question.as_str()),
        Some("Which assay should I use?")
    );
    assert_eq!(
        session.frame.status,
        dss_agent::FrameStatus::AwaitingUserResponse
    );
    assert_eq!(session.gate_state.plan_denial_count, 0);
    assert!(session.plan.is_none());
    assert!(
        pending_ask.is_none(),
        "the ephemeral ask is cleared after pause"
    );
    let published_ask = events
        .iter()
        .find_map(|event| match event {
            dss_agent::AgentEvent::ToolResults { results } => results
                .iter()
                .find(|result| result.tool_use_id == "call_ask"),
            _ => None,
        })
        .expect("ask-only result");
    assert!(!published_ask.is_error);
    assert!(published_ask.content.contains("[asked user]"));
    assert!(published_ask.content.contains("waiting for user response"));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            dss_agent::AgentEvent::Complete {
                kind: dss_agent::CompleteKind::Awaiting,
                awaiting,
                ..
            } if awaiting.as_deref() == Some("user_response")
        )
    }));
    let mut first_request_tools = seen_tool_names[0].clone();
    first_request_tools.sort();
    assert_eq!(first_request_tools, ["ask_user", "generate_plan"]);
}

#[tokio::test]
async fn prohibited_preapproval_writes_are_paired_rejected_and_bounded() {
    let turns = (0..4)
        .map(|index| ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![(
                0,
                format!("call_write_{index}"),
                "write_file".into(),
                r#"{"path":"forbidden.txt","content":"must not exist"}"#.into(),
            )],
        })
        .collect();
    let (outcome, session, events, _, _) =
        run_preapproval_plan(turns, registry_with_builtins(), MAX_ITERATIONS, 500_000).await;

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(outcome.iterations, 4);
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|error| error.contains("plan mode requires a plan")));
    assert_eq!(session.frame.status, dss_agent::FrameStatus::Failed);
    assert!(session.plan.is_none());
    assert!(!session.workspace.join("forbidden.txt").exists());

    let mut published_ids = Vec::new();
    for event in &events {
        if let dss_agent::AgentEvent::ToolResults { results } = event {
            assert_eq!(results.len(), 1);
            assert!(results[0].is_error);
            assert!(results[0].content.contains("Plan approval is required"));
            published_ids.push(results[0].tool_use_id.clone());
        }
    }
    assert_eq!(
        published_ids,
        [
            "call_write_0",
            "call_write_1",
            "call_write_2",
            "call_write_3"
        ]
    );
    let persisted_results: Vec<_> = session
        .messages
        .iter()
        .filter(|message| message.role == "tool")
        .collect();
    assert_eq!(persisted_results.len(), 4);
    assert!(persisted_results
        .iter()
        .all(|message| message.is_error == Some(true)));
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| {
                message.harness_notice
                    && message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains("plan 模式"))
            })
            .count(),
        3
    );
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            dss_agent::AgentEvent::Complete {
                kind: dss_agent::CompleteKind::MaxIters,
                ..
            }
        )
    }));
}

#[tokio::test]
async fn mixed_preapproval_batch_is_atomic_then_recovers_with_valid_plan() {
    let turns = vec![
        ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![
                (
                    0,
                    "mixed_plan".into(),
                    "generate_plan".into(),
                    r#"{"steps":[{"title":"must not commit"}]}"#.into(),
                ),
                (
                    1,
                    "mixed_write".into(),
                    "write_file".into(),
                    r#"{"path":"mixed.txt","content":"must not exist"}"#.into(),
                ),
            ],
        },
        ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![(
                0,
                "valid_plan".into(),
                "generate_plan".into(),
                r#"{"steps":[{"title":"safe plan"}]}"#.into(),
            )],
        },
    ];
    let (outcome, session, events, _, _) =
        run_preapproval_plan(turns, registry_with_builtins(), MAX_ITERATIONS, 500_000).await;

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Awaiting);
    assert_eq!(outcome.awaiting.as_deref(), Some("plan_approval"));
    assert_eq!(outcome.iterations, 2);
    assert_eq!(session.gate_state.plan_denial_count, 1);
    assert!(!session.workspace.join("mixed.txt").exists());
    let plan = session.plan.as_ref().expect("second-turn plan committed");
    assert_eq!(plan.steps.len(), 1);
    assert_eq!(plan.steps[0].title, "safe plan");

    let first_results = events
        .iter()
        .find_map(|event| match event {
            dss_agent::AgentEvent::ToolResults { results }
                if results
                    .iter()
                    .any(|result| result.tool_use_id == "mixed_plan") =>
            {
                Some(results)
            }
            _ => None,
        })
        .expect("mixed-batch results");
    assert_eq!(first_results.len(), 2);
    assert_eq!(first_results[0].tool_use_id, "mixed_plan");
    assert_eq!(first_results[1].tool_use_id, "mixed_write");
    assert!(first_results.iter().all(|result| result.is_error));
}

#[tokio::test]
async fn invalid_generate_plan_attempts_use_the_same_bounded_denial_budget() {
    let turns = (0..4)
        .map(|index| ScriptedTurn {
            texts: vec![],
            finish_reason: Some("tool_calls".into()),
            tool_calls: vec![(
                0,
                format!("invalid_plan_{index}"),
                "generate_plan".into(),
                r#"{"steps":[]}"#.into(),
            )],
        })
        .collect();
    let (outcome, session, events, _, _) =
        run_preapproval_plan(turns, registry_with_builtins(), MAX_ITERATIONS, 500_000).await;

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(outcome.iterations, 4);
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|error| error.contains("plan mode requires a plan")));
    assert!(session.plan.is_none());
    let results: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            dss_agent::AgentEvent::ToolResults { results } => results.first(),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 4);
    assert!(results.iter().all(|result| {
        result.is_error && result.content.contains("plan must have at least one step")
    }));
}

#[tokio::test]
async fn final_low_budget_text_only_plan_denial_is_plan_specific_not_max_iters() {
    let turns = vec![ScriptedTurn {
        texts: vec!["I will execute this without submitting a plan.".into()],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    }];
    let (outcome, session, events, _, _) =
        run_preapproval_plan(turns, registry_with_builtins(), 1, 500_000).await;

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(outcome.iterations, 1);
    assert_eq!(session.frame.status, dss_agent::FrameStatus::Failed);
    assert!(outcome.error.as_deref().is_some_and(|error| {
        error.contains("plan mode requires a plan") && error.contains("iteration budget")
    }));
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            dss_agent::AgentEvent::Complete {
                kind: dss_agent::CompleteKind::MaxIters,
                ..
            }
        )
    }));
}

#[tokio::test]
async fn final_low_budget_empty_plan_attempt_is_plan_specific_not_max_iters() {
    let turns = vec![ScriptedTurn {
        texts: vec![],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    }];
    let (outcome, session, events, _, _) =
        run_preapproval_plan(turns, registry_with_builtins(), 1, 500_000).await;

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(outcome.iterations, 1);
    assert_eq!(session.frame.status, dss_agent::FrameStatus::Failed);
    assert_eq!(session.gate_state.empty_retry_count, 1);
    assert_eq!(session.gate_state.plan_denial_count, 1);
    assert!(outcome.error.as_deref().is_some_and(|error| {
        error.contains("plan mode requires a plan") && error.contains("iteration budget")
    }));
    assert!(events.iter().all(|event| {
        !matches!(
            event,
            dss_agent::AgentEvent::Complete {
                kind: dss_agent::CompleteKind::MaxIters,
                ..
            }
        )
    }));
}

#[tokio::test]
async fn plan_schema_budget_excludes_large_execution_only_tool() {
    let turns = vec![ScriptedTurn {
        texts: vec![],
        finish_reason: Some("tool_calls".into()),
        tool_calls: vec![(
            0,
            "valid_plan".into(),
            "generate_plan".into(),
            r#"{"steps":[{"title":"bounded"}]}"#.into(),
        )],
    }];
    let mut registry = registry_with_builtins();
    registry.register(Arc::new(LargeSchemaTool));
    let (outcome, _session, _events, seen_tool_names, _) =
        run_preapproval_plan(turns, registry, MAX_ITERATIONS, 10_000).await;

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Awaiting);
    let mut first_request_tools = seen_tool_names
        .first()
        .expect("filtered request reached fake model")
        .clone();
    first_request_tools.sort();
    assert_eq!(first_request_tools, ["ask_user", "generate_plan"]);
    assert!(!first_request_tools
        .iter()
        .any(|name| name == "large_schema_tool"));
}

#[tokio::test]
async fn non_plan_requests_hide_update_step_status_without_an_active_plan() {
    let seen_tool_names = Arc::new(Mutex::new(Vec::new()));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![ScriptedTurn {
            texts: vec!["done".into()],
            finish_reason: Some("stop".into()),
            tool_calls: vec![],
        }])),
        seen_messages: None,
        seen_tool_names: Some(seen_tool_names.clone()),
    };
    let mut session = Session::new("non-plan-schema-test", tmp_workspace());
    let registry = registry_with_builtins();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "execute normally",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    let names = &seen_tool_names.lock().unwrap()[0];
    assert!(names.iter().any(|name| name == "write_file"));
    assert!(names.iter().any(|name| name == "bash"));
    assert!(!names.iter().any(|name| name == "update_step_status"));
}

#[tokio::test]
async fn active_plan_exposes_status_updates_then_hides_them_after_completion() {
    let seen_tool_names = Arc::new(Mutex::new(Vec::new()));
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![
            update_plan_turn(),
            ScriptedTurn {
                texts: vec!["plan finished".into()],
                finish_reason: Some("stop".into()),
                tool_calls: vec![],
            },
        ])),
        seen_messages: None,
        seen_tool_names: Some(seen_tool_names.clone()),
    };
    let mut session = Session::new("active-plan-schema-test", tmp_workspace());
    let registry = registry_with_builtins();
    let ctx = ToolContext::new(session.workspace.clone())
        .with_plan(Some(dss_tools::PlanState {
            steps: vec![dss_tools::PlanStep {
                title: "execute".into(),
                status: "pending".into(),
            }],
            approved: true,
            research_question: None,
        }))
        .await;
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "execute the approved plan",
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
    drop(tx);
    while rx.recv().await.is_some() {}

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    let requests = seen_tool_names.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].iter().any(|name| name == "update_step_status"));
    assert!(!requests[1].iter().any(|name| name == "update_step_status"));
}

#[tokio::test]
async fn hidden_inactive_plan_update_is_rejected_before_tool_execution() {
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![
            update_plan_turn(),
            ScriptedTurn {
                texts: vec!["recovered without a plan mutation".into()],
                finish_reason: Some("stop".into()),
                tool_calls: vec![],
            },
        ])),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("inactive-plan-update-rejection", tmp_workspace());
    let registry = registry_with_builtins();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, mut rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "execute normally",
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
    drop(tx);

    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Natural);
    assert!(ctx.plan.lock().await.is_none());
    let results = events
        .iter()
        .find_map(|event| match event {
            dss_agent::AgentEvent::ToolResults { results } => results.first(),
            _ => None,
        })
        .expect("rejected tool result");
    assert!(results.is_error);
    assert!(results
        .content
        .contains("available only while an approved plan"));
    assert!(!results
        .content
        .contains("no plan; call generate_plan first"));
}

#[tokio::test]
async fn cancelled_request_persists_hidden_boundary_for_next_run() {
    let started = Arc::new(tokio::sync::Notify::new());
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![update_plan_turn()])),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("cancel-plan-test", tmp_workspace());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PendingPlanMutationTool {
        started: started.clone(),
    }));
    let initial_plan = dss_tools::PlanState {
        steps: vec![dss_tools::PlanStep {
            title: "execute".into(),
            status: "pending".into(),
        }],
        approved: true,
        research_question: None,
    };
    session.plan = Some(initial_plan.clone());
    let ctx = ToolContext::new(session.workspace.clone())
        .with_plan(Some(initial_plan))
        .await;
    let (tx, rx) = mpsc::channel::<dss_agent::AgentEvent>(16);

    let task = tokio::spawn(async move {
        let outcome = Runner::run(
            &mut session,
            &llm,
            "fake-stream",
            "execute the approved plan",
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

    tokio::time::timeout(std::time::Duration::from_secs(1), started.notified())
        .await
        .expect("plan tool did not start");
    drop(rx);
    let (outcome, mut session) = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .expect("runner did not cancel")
        .expect("runner task panicked");

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Cancelled);
    let cancelled_plan = session
        .plan
        .as_ref()
        .expect("previously committed plan survives cancellation");
    assert_eq!(cancelled_plan.steps[0].status, "pending");
    assert!(cancelled_plan.approved);

    let cancel_boundaries: Vec<_> = session
        .messages
        .iter()
        .filter(|message| {
            message.role == "system"
                && message.harness_notice
                && message
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("上一条用户请求已由用户取消"))
        })
        .collect();
    assert_eq!(cancel_boundaries.len(), 1);
    assert!(cancel_boundaries[0]
        .content
        .as_deref()
        .is_some_and(|text| text.contains("下一轮只处理最新的用户请求")));

    let seen = Arc::new(Mutex::new(Vec::new()));
    let recovery_llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(vec![ScriptedTurn {
            texts: vec!["RECOVERY_OK".into()],
            finish_reason: Some("stop".into()),
            tool_calls: vec![],
        }])),
        seen_messages: Some(seen.clone()),
        seen_tool_names: None,
    };
    let recovery_registry = ToolRegistry::new();
    let recovery_ctx = ToolContext::new(session.workspace.clone());
    let (recovery_tx, mut recovery_rx) = mpsc::channel::<dss_agent::AgentEvent>(64);
    let recovery = Runner::run(
        &mut session,
        &recovery_llm,
        "fake-stream",
        "只回复 RECOVERY_OK",
        &recovery_registry,
        &recovery_ctx,
        MAX_ITERATIONS,
        500_000,
        None,
        None,
        &[],
        false,
        &recovery_tx,
    )
    .await;
    drop(recovery_tx);
    while recovery_rx.recv().await.is_some() {}

    assert_eq!(recovery.kind, dss_agent::CompleteKind::Natural);
    let requests = seen.lock().unwrap();
    let next_run_messages = requests.first().expect("recovery LLM request");
    let old_request_index = next_run_messages
        .iter()
        .position(|message| message.content.as_deref() == Some("execute the approved plan"))
        .expect("cancelled request retained as history");
    let boundary_index = next_run_messages
        .iter()
        .position(|message| {
            message.role == "system"
                && message.harness_notice
                && message
                    .content
                    .as_deref()
                    .is_some_and(|text| text.contains("上一条用户请求已由用户取消"))
        })
        .expect("persisted cancellation boundary");
    let latest_request_index = next_run_messages
        .iter()
        .rposition(|message| message.role == "user")
        .expect("latest user request");
    assert!(old_request_index < boundary_index);
    assert!(boundary_index < latest_request_index);
    assert_eq!(
        next_run_messages[latest_request_index].content.as_deref(),
        Some("只回复 RECOVERY_OK")
    );
}

#[tokio::test]
async fn disconnect_before_start_does_not_mark_existing_history_cancelled() {
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(Vec::new())),
        seen_messages: None,
        seen_tool_names: None,
    };
    let mut session = Session::new("uncommitted-cancel-test", tmp_workspace());
    session
        .messages
        .push(ChatMessage::user("already completed"));
    session.messages.push(ChatMessage::assistant("done"));
    let original_len = session.messages.len();
    let registry = ToolRegistry::new();
    let ctx = ToolContext::new(session.workspace.clone());
    let (tx, rx) = mpsc::channel::<dss_agent::AgentEvent>(1);
    drop(rx);

    let outcome = Runner::run(
        &mut session,
        &llm,
        "fake-stream",
        "must not enter history",
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

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Cancelled);
    assert_eq!(session.messages.len(), original_len);
    assert!(session
        .messages
        .iter()
        .all(|message| message.content.as_deref() != Some("must not enter history")));
    assert!(session
        .messages
        .iter()
        .all(|message| !message.harness_notice));
}

#[tokio::test]
async fn error_completion_keeps_latest_committed_plan() {
    let empty = ScriptedTurn {
        texts: vec![],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    };
    let (outcome, session, events) = run_with_approved_plan(
        vec![
            update_plan_turn(),
            empty.clone(),
            empty.clone(),
            empty.clone(),
            empty,
        ],
        MAX_ITERATIONS,
    )
    .await;

    let terminal_plan = events.iter().find_map(|event| match event {
        dss_agent::AgentEvent::Complete {
            kind: dss_agent::CompleteKind::Error,
            plan,
            ..
        } => plan.as_ref(),
        _ => None,
    });

    assert_eq!(outcome.kind, dss_agent::CompleteKind::Error);
    assert_eq!(
        terminal_plan.expect("error plan snapshot").steps[0].status,
        "done"
    );
    assert_eq!(
        session.plan.expect("session plan snapshot").steps[0].status,
        "done"
    );
}

#[tokio::test]
async fn max_iterations_completion_keeps_latest_committed_plan() {
    let (outcome, session, events) = run_with_approved_plan(vec![update_plan_turn()], 1).await;

    let plan_update_index = events
        .iter()
        .position(|event| matches!(event, dss_agent::AgentEvent::PlanUpdate { .. }))
        .expect("plan update event");
    let complete_index = events
        .iter()
        .position(|event| {
            matches!(
                event,
                dss_agent::AgentEvent::Complete {
                    kind: dss_agent::CompleteKind::MaxIters,
                    ..
                }
            )
        })
        .expect("max-iterations complete event");
    let terminal_plan = events.iter().find_map(|event| match event {
        dss_agent::AgentEvent::Complete {
            kind: dss_agent::CompleteKind::MaxIters,
            plan,
            ..
        } => plan.as_ref(),
        _ => None,
    });

    assert_eq!(outcome.kind, dss_agent::CompleteKind::MaxIters);
    assert!(plan_update_index < complete_index);
    assert_eq!(
        terminal_plan.expect("max-iterations plan snapshot").steps[0].status,
        "done"
    );
    assert_eq!(
        session.plan.expect("session plan snapshot").steps[0].status,
        "done"
    );
}
