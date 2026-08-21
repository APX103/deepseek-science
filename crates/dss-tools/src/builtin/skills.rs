//! skills 工具：search_skills / list_skills / skill（让 agent 查找与读取 skill）。

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{parse_args, Tool, ToolOutput, ToolSpec};

pub struct SearchSkillsTool;
pub struct ListSkillsTool;
pub struct SkillTool;

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
}
#[derive(Deserialize)]
struct SkillArgs {
    name: String,
}

#[async_trait]
impl Tool for SearchSkillsTool {
    fn effect_class(&self, _args: &Value) -> crate::spec::ToolEffectClass {
        crate::spec::ToolEffectClass::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "search_skills".into(),
            description: "Search available skills by query. Returns matching skill names and descriptions, ranked by relevance. Use to find a skill for the current task.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "query": { "type": "string", "description": "What you want to do." } },
                "required": ["query"]
            }),
        }
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: SearchArgs = parse_args(&args)?;
        let hits = ctx.skill_catalog.search(&a.query);
        if hits.is_empty() {
            return Ok(ToolOutput::ok("no matching skills".to_string()));
        }
        let out = hits
            .iter()
            .map(|h| format!("- {} ({}): {}", h.name, h.source, h.description))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok(out))
    }
}

#[async_trait]
impl Tool for ListSkillsTool {
    fn effect_class(&self, _args: &Value) -> crate::spec::ToolEffectClass {
        crate::spec::ToolEffectClass::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_skills".into(),
            description: "List all available skills with their descriptions.".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn call(&self, ctx: &ToolContext, _args: Value) -> Result<ToolOutput, ToolError> {
        let skills = ctx.skill_catalog.list();
        if skills.is_empty() {
            return Ok(ToolOutput::ok("no skills available".to_string()));
        }
        let out = skills
            .iter()
            .map(|s| format!("- {} ({}): {}", s.name, s.source, s.description))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok(out))
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn effect_class(&self, _args: &Value) -> crate::spec::ToolEffectClass {
        crate::spec::ToolEffectClass::ReadOnly
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill".into(),
            description: "Read the full body/instructions of a skill by name (from search_skills/list_skills). Returns the skill's markdown content to follow.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "name": { "type": "string", "description": "Skill name." } },
                "required": ["name"]
            }),
        }
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let a: SkillArgs = parse_args(&args)?;
        match ctx.skill_catalog.get(&a.name) {
            Some(s) => Ok(ToolOutput::ok(dss_skills::render_skill_body(s))),
            None => Ok(ToolOutput::err(format!("skill not found: {}", a.name))),
        }
    }
}
