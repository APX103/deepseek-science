use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{Tool, ToolBatchPolicy, ToolDef, ToolOutput};

/// 单个待执行的工具调用（来自 LLM 的 tool_calls）。
#[derive(Debug, Clone)]
pub struct PendingToolCall {
    /// 与 assistant tool_call 的 id 一致（用于结果配对）。
    pub id: String,
    pub name: String,
    /// arguments（OpenAI 协议里是 JSON 字符串，这里解析为 Value 供工具消费）。
    pub input: Value,
}

/// 单个工具执行结果（OpenAI tool message content）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
    /// The call may have committed an external side effect, but no authoritative result was
    /// observed. The Runner must stop this Run at the reconciliation boundary.
    #[serde(default, skip_serializing_if = "is_false")]
    pub outcome_unknown: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// 工具注册表：name → Arc<dyn Tool>。
#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolRegistryError {
    #[error("tool name `{0}` is already registered")]
    Duplicate(String),
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name;
        debug!(%name, "registered tool");
        if self.tools.insert(name.clone(), tool).is_some() {
            warn!(%name, "replaced an already registered tool");
        }
    }

    /// Register a dynamic tool without silently shadowing a built-in, MCP, or another
    /// per-run tool. A2A overlays use this fail-closed path.
    pub fn register_checked(&mut self, tool: Arc<dyn Tool>) -> Result<(), ToolRegistryError> {
        let name = tool.spec().name;
        if self.tools.contains_key(&name) {
            return Err(ToolRegistryError::Duplicate(name));
        }
        debug!(%name, "registered checked dynamic tool");
        self.tools.insert(name, tool);
        Ok(())
    }

    /// Cheap immutable-run overlay base: cloned entries keep sharing each underlying Tool.
    pub fn snapshot(&self) -> Self {
        self.clone()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// Return the registered tool's model-batch policy without exposing its concrete type.
    /// Missing/undeclared tools remain non-exclusive here; if paired with an exclusive tool,
    /// the known exclusive call still makes the whole batch fail closed in the Runner.
    pub fn batch_policy(&self, name: &str) -> Option<ToolBatchPolicy> {
        self.tools.get(name).map(|tool| tool.batch_policy())
    }

    /// 给 LLM 的全部工具定义。
    pub fn definitions(&self) -> Vec<ToolDef> {
        let mut definitions: Vec<_> = self
            .tools
            .values()
            .map(|t| ToolDef::from(t.spec()))
            .collect();
        definitions.sort_by(|left, right| left.function.name.cmp(&right.function.name));
        definitions
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Execute a tool-call batch in the model-declared order.
///
/// Tool batches are not a dataflow graph: a model may emit write+run or run+read-output in one
/// response. Running the whole batch concurrently races those dependencies and also returns
/// results in completion order. Ordered execution is the safe default; models can still batch
/// independent reads without changing semantics, at the cost of a small amount of latency.
pub struct ToolRouter;

impl ToolRouter {
    pub const DEFAULT_PER_CALL_TIMEOUT: Duration = Duration::from_secs(30);

    /// Execute one batch sequentially. A single failure/timeout is paired with its original call
    /// and does not prevent later calls from running; results always match input order.
    pub async fn execute_tool_calls(
        registry: &ToolRegistry,
        ctx: &ToolContext,
        calls: Vec<PendingToolCall>,
    ) -> Vec<ToolResult> {
        if calls.is_empty() {
            return Vec::new();
        }

        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            let tool_opt = registry.get(&call.name);
            let ctx = ctx.clone();
            let recovery_ctx = ctx.clone();
            let call_id = call.id.clone();
            let call_name = call.name.clone();
            let effect_class = tool_opt.as_ref().map(|tool| tool.effect_class(&call.input));
            let mut set: JoinSet<ToolResult> = JoinSet::new();
            set.spawn(async move {
                let Some(tool) = tool_opt else {
                    return to_result(
                        &call.id,
                        Err(ToolError::NotFound(call.name.clone())),
                        &call.name,
                    );
                };
                let effect_class = tool.effect_class(&call.input);
                if let Err(error) = ctx
                    .audit_tool_started(&call.id, &call.name, effect_class, &call.input)
                    .await
                {
                    return ToolResult {
                        tool_use_id: call.id,
                        content: format!("tool `{}` was not executed because its durable intent could not be recorded: {error}", call.name),
                        is_error: true,
                        outcome_unknown: false,
                    };
                }
                let timeout = tool.timeout(&call.input);
                let fut = tool.call(&ctx, call.input.clone());
                let execution = tokio::time::timeout(timeout, fut).await;
                let uncertain_reason = match &execution {
                    Err(_) if effect_class == crate::spec::ToolEffectClass::ExternalSideEffect => {
                        Some(format!("tool timed out after {}s", timeout.as_secs()))
                    }
                    Ok(Err(ToolError::Timeout(seconds)))
                        if effect_class == crate::spec::ToolEffectClass::ExternalSideEffect =>
                    {
                        Some(format!("tool reported a timeout after {seconds}s"))
                    }
                    _ => None,
                };
                if let Some(reason) = uncertain_reason {
                    let audit_output = serde_json::json!({
                        "reason": reason,
                        "outcome": "unknown",
                    });
                    return match ctx
                        .audit_tool_uncertain(&call.id, &reason, &audit_output)
                        .await
                    {
                        Ok(()) => ToolResult {
                            tool_use_id: call.id,
                            content: format!(
                                "tool `{}` may have changed external state, but its outcome is unknown: {reason}. Human reconciliation is required before this Run can continue.",
                                call.name
                            ),
                            is_error: true,
                            outcome_unknown: true,
                        },
                        Err(error) => ToolResult {
                            tool_use_id: call.id,
                            content: format!(
                                "tool `{}` outcome is unknown and the reconciliation marker could not be committed: {error}",
                                call.name
                            ),
                            is_error: true,
                            outcome_unknown: false,
                        },
                    };
                }
                let result = match execution {
                    Ok(res) => to_result(&call.id, res, &call.name),
                    Err(_) => to_result(
                        &call.id,
                        Err(ToolError::Timeout(timeout.as_secs())),
                        &call.name,
                    ),
                };
                let audit_output = serde_json::json!({
                    "content": result.content,
                    "is_error": result.is_error,
                });
                if let Err(error) = ctx
                    .audit_tool_settled(&call.id, !result.is_error, &audit_output)
                    .await
                {
                    if effect_class == crate::spec::ToolEffectClass::ExternalSideEffect {
                        let reason = format!("durable tool result could not be committed: {error}");
                        let unknown = ctx
                            .audit_tool_uncertain(&call.id, &reason, &audit_output)
                            .await
                            .is_ok();
                        return ToolResult {
                            tool_use_id: call.id,
                            content: format!(
                                "tool `{}` returned but its durable result could not be committed: {error}. Treat the external outcome as unknown.",
                                call.name
                            ),
                            is_error: true,
                            outcome_unknown: unknown,
                        };
                    }
                    return ToolResult {
                        tool_use_id: call.id,
                        content: format!("tool `{}` returned but its durable result could not be committed: {error}. Treat the external outcome as unknown.", call.name),
                        is_error: true,
                        outcome_unknown: false,
                    };
                }
                result
            });
            let result = match set.join_next().await {
                Some(Ok(result)) => result,
                Some(Err(join_err)) => {
                    // A panic/cancellation is still paired with the exact call so the
                    // assistant tool-call history remains valid and auditable.
                    warn!(error = ?join_err, tool = %call_name, "tool task join failed");
                    let reason = format!("tool task crashed: {join_err}");
                    let outcome_unknown =
                        if effect_class == Some(crate::spec::ToolEffectClass::ExternalSideEffect) {
                            recovery_ctx
                                .audit_tool_uncertain(
                                    &call_id,
                                    &reason,
                                    &serde_json::json!({"reason": reason, "outcome": "unknown"}),
                                )
                                .await
                                .is_ok()
                        } else {
                            false
                        };
                    ToolResult {
                        tool_use_id: call_id,
                        content: format!("tool `{call_name}` task crashed: {join_err}"),
                        is_error: true,
                        outcome_unknown,
                    }
                }
                None => ToolResult {
                    tool_use_id: call_id,
                    content: format!("tool `{call_name}` task ended without a result"),
                    is_error: true,
                    outcome_unknown: false,
                },
            };
            results.push(result);
        }
        results
    }
}

fn to_result(tool_use_id: &str, res: Result<ToolOutput, ToolError>, name: &str) -> ToolResult {
    match res {
        Ok(out) => ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: out.content,
            is_error: out.is_error,
            outcome_unknown: false,
        },
        Err(e) => {
            let msg = format!("tool `{name}` failed: {e}");
            warn!(%name, error = %e, "tool call errored");
            ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: msg,
                is_error: true,
                outcome_unknown: false,
            }
        }
    }
}

/// 解析 OpenAI tool_call.function.arguments（JSON 字符串）成 Value。
pub fn parse_arguments(raw: &str) -> Value {
    if raw.is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(v) => v,
        Err(e) => {
            // 解析失败兜底成 {_raw: ...}，让工具自己报 invalid args。
            warn!(error = %e, "failed to parse tool arguments as JSON");
            serde_json::json!({ "_raw": raw })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ToolAuditSink;
    use crate::spec::{Tool, ToolOutput, ToolSpec};
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct WriteMarkerTool {
        marker: Arc<AtomicBool>,
    }

    struct ReadMarkerTool {
        marker: Arc<AtomicBool>,
    }

    struct SlowExternalTool;

    struct AuditProbe {
        uncertain: AtomicBool,
    }

    #[async_trait]
    impl ToolAuditSink for AuditProbe {
        async fn started(
            &self,
            _call_id: &str,
            _tool_name: &str,
            _effect_class: crate::spec::ToolEffectClass,
            _input: &Value,
        ) -> Result<(), String> {
            Ok(())
        }

        async fn settled(
            &self,
            _call_id: &str,
            _succeeded: bool,
            _output: &Value,
        ) -> Result<(), String> {
            Err("ambiguous calls must not be settled".into())
        }

        async fn uncertain(
            &self,
            _call_id: &str,
            _reason: &str,
            _output: &Value,
        ) -> Result<(), String> {
            self.uncertain.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait]
    impl Tool for SlowExternalTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "slow_external".into(),
                description: "test ambiguous timeout".into(),
                parameters: json!({"type":"object"}),
            }
        }

        fn timeout(&self, _args: &Value) -> Duration {
            Duration::from_millis(1)
        }

        async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
            tokio::time::sleep(Duration::from_millis(30)).await;
            Ok(ToolOutput::ok("late success"))
        }
    }

    #[async_trait]
    impl Tool for WriteMarkerTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "write_marker".into(),
                description: "test write".into(),
                parameters: json!({"type":"object"}),
            }
        }

        async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
            // A concurrent router lets the dependent read overtake this write.
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.marker.store(true, Ordering::SeqCst);
            Ok(ToolOutput::ok("written"))
        }
    }

    #[async_trait]
    impl Tool for ReadMarkerTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "read_marker".into(),
                description: "test read".into(),
                parameters: json!({"type":"object"}),
            }
        }

        async fn call(&self, _ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
            if self.marker.load(Ordering::SeqCst) {
                Ok(ToolOutput::ok("observed write"))
            } else {
                Ok(ToolOutput::err("read raced ahead of write"))
            }
        }
    }

    #[tokio::test]
    async fn dependent_batch_executes_and_returns_in_declared_order() {
        let marker = Arc::new(AtomicBool::new(false));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteMarkerTool {
            marker: marker.clone(),
        }));
        registry.register(Arc::new(ReadMarkerTool { marker }));
        let ctx = ToolContext::new(std::env::temp_dir());

        let results = ToolRouter::execute_tool_calls(
            &registry,
            &ctx,
            vec![
                PendingToolCall {
                    id: "write-call".into(),
                    name: "write_marker".into(),
                    input: json!({}),
                },
                PendingToolCall {
                    id: "read-call".into(),
                    name: "read_marker".into(),
                    input: json!({}),
                },
            ],
        )
        .await;

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].tool_use_id, "write-call");
        assert!(!results[0].is_error);
        assert_eq!(results[1].tool_use_id, "read-call");
        assert!(!results[1].is_error);
        assert_eq!(results[1].content, "observed write");
    }

    #[tokio::test]
    async fn external_timeout_is_unknown_instead_of_settled_failure() {
        let probe = Arc::new(AuditProbe {
            uncertain: AtomicBool::new(false),
        });
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(SlowExternalTool));
        let ctx = ToolContext::new(std::env::temp_dir()).with_tool_audit(probe.clone());

        let results = ToolRouter::execute_tool_calls(
            &registry,
            &ctx,
            vec![PendingToolCall {
                id: "ambiguous-call".into(),
                name: "slow_external".into(),
                input: json!({}),
            }],
        )
        .await;

        assert_eq!(results.len(), 1);
        assert!(results[0].is_error);
        assert!(results[0].outcome_unknown);
        assert!(probe.uncertain.load(Ordering::SeqCst));
    }

    #[test]
    fn snapshots_are_independent_and_checked_registration_never_shadows() {
        let marker = Arc::new(AtomicBool::new(false));
        let mut base = ToolRegistry::new();
        base.register(Arc::new(WriteMarkerTool {
            marker: marker.clone(),
        }));

        let mut overlay = base.snapshot();
        overlay
            .register_checked(Arc::new(ReadMarkerTool { marker }))
            .expect("new dynamic name");
        assert_eq!(base.names(), ["write_marker"]);
        assert_eq!(overlay.names(), ["read_marker", "write_marker"]);

        let duplicate =
            overlay.register_checked(overlay.get("write_marker").expect("existing shared tool"));
        assert_eq!(
            duplicate,
            Err(ToolRegistryError::Duplicate("write_marker".into()))
        );
        assert_eq!(
            overlay
                .definitions()
                .into_iter()
                .map(|definition| definition.function.name)
                .collect::<Vec<_>>(),
            ["read_marker", "write_marker"]
        );
    }
}

/// 工具的通用参数解析（带可选字段）。
#[derive(Debug, Clone, Deserialize)]
pub struct PathArgs {
    pub path: String,
}
