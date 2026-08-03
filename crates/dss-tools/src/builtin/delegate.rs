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
}

#[async_trait]
impl Tool for DelegateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delegate".into(),
            description: "Delegate a self-contained subtask to a sub-agent (single LLM call). Use for focused subtasks like drafting a section, analyzing data, or generating options. Depth limit: 2 levels.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "task": { "type": "string", "description": "The subtask to delegate (should be self-contained and specific)." },
                    "context_summary": { "type": "string", "description": "Brief context for the subtask (optional)." }
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
