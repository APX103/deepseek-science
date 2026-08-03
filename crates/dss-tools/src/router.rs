use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::task::JoinSet;
use tracing::{debug, warn};

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{Tool, ToolDef, ToolOutput};

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
}

/// 工具注册表：name → Arc<dyn Tool>。
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: HashMap::new() }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name;
        debug!(%name, "registered tool");
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    /// 给 LLM 的全部工具定义。
    pub fn definitions(&self) -> Vec<ToolDef> {
        self.tools.values().map(|t| ToolDef::from(t.spec())).collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// 并发执行工具调用（JoinSet + per-call 30s timeout）。
pub struct ToolRouter;

impl ToolRouter {
    pub const PER_CALL_TIMEOUT: Duration = Duration::from_secs(30);

    /// 并发执行一批工具调用。单个失败/超时不影响其它；统一转成 ToolResult。
    pub async fn execute_tool_calls(
        registry: &ToolRegistry,
        ctx: &ToolContext,
        calls: Vec<PendingToolCall>,
    ) -> Vec<ToolResult> {
        if calls.is_empty() {
            return Vec::new();
        }

        let mut set: JoinSet<ToolResult> = JoinSet::new();
        for call in calls {
            let tool_opt = registry.get(&call.name);
            let ctx = ctx.clone();
            set.spawn(async move {
                let Some(tool) = tool_opt else {
                    return to_result(&call.id, Err(ToolError::NotFound(call.name.clone())), &call.name);
                };
                let fut = tool.call(&ctx, call.input.clone());
                match tokio::time::timeout(Self::PER_CALL_TIMEOUT, fut).await {
                    Ok(res) => to_result(&call.id, res, &call.name),
                    Err(_) => to_result(&call.id, Err(ToolError::Timeout(30)), &call.name),
                }
            });
        }

        let mut results = Vec::with_capacity(set.len());
        while let Some(res) = set.join_next().await {
            match res {
                Ok(r) => results.push(r),
                Err(join_err) => {
                    // 任务 panic / cancel——兜底一条错误结果。
                    warn!(error = ?join_err, "tool task join failed");
                    results.push(ToolResult {
                        tool_use_id: String::new(),
                        content: format!("tool task crashed: {join_err}"),
                        is_error: true,
                    });
                }
            }
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
        },
        Err(e) => {
            let msg = format!("tool `{name}` failed: {e}");
            warn!(%name, error = %e, "tool call errored");
            ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: msg,
                is_error: true,
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

/// 工具的通用参数解析（带可选字段）。
#[derive(Debug, Clone, Deserialize)]
pub struct PathArgs {
    pub path: String,
}
