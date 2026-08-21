//! delegate 工具：把子任务委派给一次独立 LLM 调用（modules.md「简化为直接 LLM 调用」）。
//!
//! 深度上限 2（主 agent depth=0，子 depth=1，孙 depth=2；>2 拒绝）。
//! submit_output 暂为占位（P6b：子 agent 结构化返回，当前主 agent 不 spawn 真子 frame）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

/// delegate 深度上限（modules.md：深度上限 2）。
const MAX_DELEGATE_DEPTH: u32 = 2;

pub struct DelegateTool;

#[derive(Deserialize)]
struct DelegateArgs {
    task: String,
    #[serde(default)]
    context_summary: Option<String>,
    #[serde(default = "default_wait")]
    wait: bool,
}

fn default_wait() -> bool {
    true
}

#[async_trait]
impl Tool for DelegateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delegate".into(),
            description: "Delegate a self-contained subtask to a local sub-agent implemented as one configured-LLM call. This tool does not use MCP, the A2A protocol, SendMessage, GetTask, or any remote Agent Card; never report it as an A2A interaction. Use it for focused subtasks like drafting a section, analyzing data, or generating options. Depth limit: 2 levels.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "The subtask to delegate (should be self-contained and specific)." },
                    "context_summary": { "type": "string", "description": "Brief context for the subtask (optional)." }
                    ,"wait": { "type": "boolean", "description": "Wait for the result. Set false to dispatch and collect later.", "default": true }
                },
                "required": ["task"]
            }),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        // 深度检查。
        if ctx.delegate_depth >= MAX_DELEGATE_DEPTH {
            return Ok(ToolOutput::err(format!(
                "delegate depth limit reached ({MAX_DELEGATE_DEPTH}); cannot delegate further"
            )));
        }

        let a: DelegateArgs = parse_args(&args)?;
        if let Some(runtime) = ctx.subagents.as_ref() {
            return match runtime
                .delegate(&a.task, a.context_summary.as_deref(), a.wait)
                .await
            {
                Ok(value) => Ok(ToolOutput::ok(value.to_string())),
                Err(error) => Ok(ToolOutput::err(error)),
            };
        }
        let Some(llm) = ctx.llm.as_ref() else {
            return Ok(ToolOutput::err("LLM not available for delegation"));
        };

        let system = "你是一个被委派的子 agent。专注完成给定子任务，给出简洁、结构化的结果。不要闲聊，直接输出结果。";
        let prompt = if let Some(cs) = &a.context_summary {
            format!("上下文：{cs}\n\n子任务：{}", a.task)
        } else {
            format!("子任务：{}", a.task)
        };

        let req = dss_llm::ChatRequest::new(
            &ctx.model,
            vec![
                dss_llm::ChatMessage::system(system),
                dss_llm::ChatMessage::user(&prompt),
            ],
        );

        match llm.chat(req).await {
            Ok(resp) => {
                let text = if resp.text.is_empty() {
                    "(sub-agent returned empty response)".to_string()
                } else {
                    resp.text
                };
                Ok(ToolOutput::ok(text))
            }
            Err(e) => Ok(ToolOutput::err(format!("delegate LLM call failed: {e}"))),
        }
    }
}

pub struct CollectChildrenTool;

#[derive(Deserialize)]
struct CollectArgs {
    frame_ids: Vec<String>,
    #[serde(default = "default_collect_timeout")]
    timeout_seconds: u64,
}

fn default_collect_timeout() -> u64 {
    30
}

#[async_trait]
impl Tool for CollectChildrenTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "collect_children".into(),
            description: "Collect durable results from previously delegated child Frames. Waiting is bounded; a timeout never cancels the child.".into(),
            parameters: json!({"type":"object","properties":{"frame_ids":{"type":"array","items":{"type":"string"}},"timeout_seconds":{"type":"integer","minimum":0,"maximum":1800,"default":30}},"required":["frame_ids"]}),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: CollectArgs = parse_args(&args)?;
        let Some(runtime) = ctx.subagents.as_ref() else {
            return Ok(ToolOutput::err("durable subagent runtime is unavailable"));
        };
        match runtime
            .collect(&args.frame_ids, args.timeout_seconds.min(1800))
            .await
        {
            Ok(value) => Ok(ToolOutput::ok(value.to_string())),
            Err(error) => Ok(ToolOutput::err(error)),
        }
    }
}

pub struct SendChildMessageTool;

#[derive(Deserialize)]
struct SendArgs {
    frame_id: String,
    message: String,
}

#[async_trait]
impl Tool for SendChildMessageTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "send_child_message".into(),
            description: "Send a durable message to an idle direct child Frame. A busy child rejects the message instead of pretending it can consume mid-Run input; wait for its result or stop it first. A completed child resumes on a new Run while retaining its Frame transcript.".into(),
            parameters: json!({"type":"object","properties":{"frame_id":{"type":"string"},"message":{"type":"string"}},"required":["frame_id","message"]}),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: SendArgs = parse_args(&args)?;
        let Some(runtime) = ctx.subagents.as_ref() else {
            return Ok(ToolOutput::err("durable subagent runtime is unavailable"));
        };
        match runtime.send_message(&args.frame_id, &args.message).await {
            Ok(value) => Ok(ToolOutput::ok(value.to_string())),
            Err(error) => Ok(ToolOutput::err(error)),
        }
    }
}

pub struct StopChildTool;

#[derive(Deserialize)]
struct ChildIdArgs {
    frame_id: String,
}

#[async_trait]
impl Tool for StopChildTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "stop_child".into(),
            description: "Cancel the active Run of a direct child Frame without deleting its transcript or Frame identity.".into(),
            parameters: json!({"type":"object","properties":{"frame_id":{"type":"string"}},"required":["frame_id"]}),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: ChildIdArgs = parse_args(&args)?;
        let Some(runtime) = ctx.subagents.as_ref() else {
            return Ok(ToolOutput::err("durable subagent runtime is unavailable"));
        };
        match runtime.stop_child(&args.frame_id).await {
            Ok(value) => Ok(ToolOutput::ok(value.to_string())),
            Err(error) => Ok(ToolOutput::err(error)),
        }
    }
}

pub struct ListChildrenTool;

#[async_trait]
impl Tool for ListChildrenTool {
    fn effect_class(&self, _args: &Value) -> crate::spec::ToolEffectClass {
        crate::spec::ToolEffectClass::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_children".into(),
            description: "List durable direct child Frames and their current activity.".into(),
            parameters: json!({"type":"object","properties":{}}),
        }
    }

    async fn call(&self, ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        let Some(runtime) = ctx.subagents.as_ref() else {
            return Ok(ToolOutput::err("durable subagent runtime is unavailable"));
        };
        match runtime.children().await {
            Ok(value) => Ok(ToolOutput::ok(value.to_string())),
            Err(error) => Ok(ToolOutput::err(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegate_contract_cannot_be_mistaken_for_remote_a2a() {
        let spec = DelegateTool.spec();
        assert!(spec.description.contains("local sub-agent"));
        assert!(spec.description.contains("does not use MCP"));
        assert!(spec.description.contains("A2A protocol"));
        assert!(spec
            .description
            .contains("never report it as an A2A interaction"));
    }
}

/// submit_output：子 agent 结构化返回（P6b 占位；当前主 agent 不 spawn 真子 frame，
/// 此工具记录 output + completion bullets 到 frame context，供主 agent 参考）。
pub struct SubmitOutputTool;

#[derive(Deserialize)]
struct SubmitArgs {
    output: String,
    #[serde(default)]
    completion_bullets: Option<Vec<String>>,
}

#[async_trait]
impl Tool for SubmitOutputTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "submit_output".into(),
            description: "Submit structured output from a delegated subtask. Use after completing a delegated task to return results to the parent agent.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "output": { "type": "string", "description": "The result/output of the subtask." },
                    "completion_bullets": { "type": "array", "items": { "type": "string" }, "description": "Key completion points (optional)." }
                },
                "required": ["output"]
            }),
        }
    }

    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: SubmitArgs = parse_args(&args)?;
        let mut summary = a.output;
        if let Some(bullets) = a.completion_bullets {
            if !bullets.is_empty() {
                summary.push_str("\n\n完成要点：");
                for b in bullets {
                    summary.push_str(&format!("\n- {b}"));
                }
            }
        }
        Ok(ToolOutput::ok(summary))
    }
}
