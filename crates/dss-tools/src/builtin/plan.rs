//! plan 工具：generate_plan / update_step_status（P6a）。
//!
//! generate_plan 把 plan 存进 ToolContext.plan（共享态），Runner 据此转 AWAITING_PLAN_APPROVAL。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::context::{PlanState, PlanStep, ToolContext};
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

pub struct GeneratePlanTool;
pub struct UpdateStepStatusTool;

#[derive(Deserialize)]
struct GeneratePlanArgs {
    steps: Vec<StepInput>,
    #[serde(default)]
    research_question: Option<String>,
}
#[derive(Deserialize)]
struct StepInput {
    title: String,
}

#[derive(Deserialize)]
struct UpdateStepArgs {
    step_id: usize, // 0-based 索引（P6a 简化）
    status: String, // pending|running|done|failed
}

#[async_trait]
impl Tool for GeneratePlanTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "generate_plan".into(),
            description: "Generate a step-by-step plan for the task. Use in plan mode. Each step has a title. After this, the run pauses for user approval before execution.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "steps": {
                        "type": "array",
                        "items": { "type": "object", "properties": { "title": { "type": "string" } }, "required": ["title"] }
                    },
                    "research_question": { "type": "string", "description": "Optional research question (enables stricter review)." }
                },
                "required": ["steps"]
            }),
        }
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: GeneratePlanArgs = parse_args(&args)?;
        if a.steps.is_empty() {
            return Ok(ToolOutput::err("plan must have at least one step"));
        }
        let plan = PlanState {
            steps: a.steps.into_iter().map(|s| PlanStep { title: s.title, status: "pending".into() }).collect(),
            approved: false,
            research_question: a.research_question,
        };
        let n = plan.steps.len();
        *ctx.plan.lock().await = Some(plan);
        Ok(ToolOutput::ok(format!(
            "plan generated ({n} steps); run will pause for user approval."
        )))
    }
}

#[async_trait]
impl Tool for UpdateStepStatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "update_step_status".into(),
            description: "Update the status of a plan step by its 0-based index. status: pending|running|done|failed.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "step_id": { "type": "integer", "description": "0-based step index." },
                    "status": { "type": "string", "enum": ["pending","running","done","failed"] }
                },
                "required": ["step_id", "status"]
            }),
        }
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: UpdateStepArgs = parse_args(&args)?;
        let mut guard = ctx.plan.lock().await;
        let Some(plan) = guard.as_mut() else {
            return Ok(ToolOutput::err("no plan; call generate_plan first"));
        };
        if a.step_id >= plan.steps.len() {
            return Ok(ToolOutput::err(format!("step_id {} out of range ({} steps)", a.step_id, plan.steps.len())));
        }
        plan.steps[a.step_id].status = a.status.clone();
        Ok(ToolOutput::ok(format!("step {} → {}", a.step_id, a.status)))
    }
}
