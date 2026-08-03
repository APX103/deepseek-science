//! P2b-gates 集成测试：流式 FakeLLM 驱动 Runner 走各决策门。

use dss_agent::{Runner, Session, MAX_ITERATIONS};
use dss_llm::{
    BoxedEventStream, ChatRequest, LlmClient, LlmError, StreamEvent, Usage,
};
use dss_tools::{builtin, ToolContext, ToolRegistry};
use futures::future::BoxFuture;
use futures::stream;
use std::path::PathBuf;
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
}

#[async_trait::async_trait]
impl LlmClient for StreamFakeLlm {
    async fn chat(&self, _req: ChatRequest) -> Result<dss_llm::LlmResponse, LlmError> {
        Err(LlmError::NotConfigured("use chat_stream".into()))
    }
    fn chat_stream(&self, _req: ChatRequest) -> BoxFuture<'_, Result<BoxedEventStream, LlmError>> {
        let mut turns = self.turns.lock().unwrap();
        let turn = turns
            .first()
            .cloned()
            .unwrap_or(ScriptedTurn { texts: vec![], finish_reason: None, tool_calls: vec![] });
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
        events.push(Ok(StreamEvent::Usage(Usage { input_tokens: 1, output_tokens: 1 })));
        events.push(Ok(StreamEvent::Finish { reason: turn.finish_reason }));

        let stream = Box::pin(stream::iter(events)) as BoxedEventStream;
        Box::pin(async move { Ok(stream) })
    }
    fn model(&self) -> &str {
        "fake-stream"
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
) -> (dss_agent::CompleteKind, u32) {
    let llm = StreamFakeLlm {
        turns: Arc::new(Mutex::new(turns)),
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
        false, // plan_mode
        &tx,
    )
    .await;
    // run 返回后丢弃 tx，使 rx 能收到 None 结束排空。
    drop(tx);
    while rx.recv().await.is_some() {}
    (outcome.kind, outcome.iterations)
}

#[tokio::test]
async fn natural_completion_with_content() {
    // 单轮：有 text 内容、无 tool、finish=stop → natural。
    let turns = vec![ScriptedTurn {
        texts: vec!["hello there".into()],
        finish_reason: Some("stop".into()),
        tool_calls: vec![],
    }];
    let (kind, iters) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::Natural);
    assert_eq!(iters, 1);
}

#[tokio::test]
async fn empty_retry_then_fails_after_cap() {
    // 连续 4 轮空响应（无 text、无 tool、finish=stop）→ 第 4 次超 cap(3) → error。
    let empty = ScriptedTurn { texts: vec![], finish_reason: Some("stop".into()), tool_calls: vec![] };
    let turns = vec![empty.clone(), empty.clone(), empty.clone(), empty.clone()];
    let (kind, _iters) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::Error);
}

#[tokio::test]
async fn empty_retry_recovers_when_content_arrives() {
    // 2 轮空 + 第 3 轮有内容 → natural（empty_retry 不超 cap）。
    let empty = ScriptedTurn { texts: vec![], finish_reason: Some("stop".into()), tool_calls: vec![] };
    let turns = vec![
        empty.clone(),
        empty.clone(),
        ScriptedTurn { texts: vec!["now I respond".into()], finish_reason: Some("stop".into()), tool_calls: vec![] },
    ];
    let (kind, iters) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::Natural);
    assert_eq!(iters, 3);
}

#[tokio::test]
async fn max_tokens_length_terminates_at_cap() {
    // 连续 5 轮 finish=length → 第 5 次终止（MaxIters）。
    let length = ScriptedTurn { texts: vec!["partial...".into()], finish_reason: Some("length".into()), tool_calls: vec![] };
    let turns = vec![length.clone(), length.clone(), length.clone(), length.clone(), length.clone()];
    let (kind, _iters) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::MaxIters);
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
    let (kind, iters) = run_agent(turns, None).await;
    assert_eq!(kind, dss_agent::CompleteKind::Natural);
    // 6 轮检索 + 1 轮写作 = 7 iteration。
    assert!(iters >= 7, "expected at least 7 iterations, got {iters}");
}
