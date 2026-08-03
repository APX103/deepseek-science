//! ask_user 工具：把问题挂起，Runner 检测后转 AwaitingUserResponse。
//!
//! 工具本身只把 PendingAsk 写进 ToolContext.pending_ask 并返回一条「已提问」
//! 的提示内容；Runner 在拿到 tool_results 后检查该字段，决定是否转 awaiting。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::context::{PendingAsk, PendingAskOption, ToolContext};
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

#[derive(Deserialize)]
struct AskArgs {
    question: String,
    #[serde(default)]
    options: Option<Vec<AskOptionArg>>,
    #[serde(default)]
    header: Option<String>,
}

#[derive(Deserialize)]
struct AskOptionArg {
    label: String,
    #[serde(default)]
    description: Option<String>,
}

pub struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn spec(&self) -> ToolSpec {
        ask_spec()
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: AskArgs = parse_args(&args)?;
        let options = a
            .options
            .unwrap_or_default()
            .into_iter()
            .map(|o| PendingAskOption {
                label: o.label,
                description: o.description,
            })
            .collect();
        let pending = PendingAsk {
            question: a.question.clone(),
            options,
            header: a.header.clone(),
        };
        // 挂起：Runner 会在 tool_results 后读这个字段转 AwaitingUserResponse。
        let mut guard = ctx.pending_ask.lock().await;
        *guard = Some(pending);
        drop(guard);

        // 返回一条占位内容（若 Runner 没转 awaiting，LLM 也能看到这条说明）。
        Ok(ToolOutput::ok(format!(
            "[asked user] {} — waiting for user response; this run will pause.",
            a.question
        )))
    }
}

// 复用 crate::error::ToolError 的解析错误（parse_args 会返回 InvalidArgs）。
use crate::error::ToolError;

fn ask_spec() -> ToolSpec {
    ToolSpec {
        name: "ask_user".into(),
        description: "Ask the user a question and pause the run until they respond. Use when you genuinely need a decision or clarification from the user. `options` optionally constrains the answer to a fixed set.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "Question to ask the user." },
                "options": {
                    "type": "array",
                    "description": "Optional fixed choices.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": { "type": "string" },
                            "description": { "type": "string" }
                        },
                        "required": ["label"]
                    }
                },
                "header": { "type": "string", "description": "Short title for the question (optional)." }
            },
            "required": ["question"]
        }),
    }
}
