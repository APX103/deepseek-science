//! 内置工具。
//!
//! 文件/bash/ask_user（P2a）+ web/python（P2b-tools）+ compile/skills（P5a）。

pub mod a2a;
pub mod agent_registry;
pub mod ask_user;
pub mod bash;
pub mod compile;
pub mod delegate;
pub mod files;
pub mod mcp;
pub mod memory;
pub mod openalex;
pub mod plan;
pub mod python;
pub mod skills;
pub mod web;

use std::sync::Arc;

use crate::ToolRegistry;

/// 注册全部内置工具到 registry。
pub fn register_all(registry: &mut ToolRegistry) {
    registry.register(Arc::new(files::ReadFileTool));
    registry.register(Arc::new(files::WriteFileTool));
    registry.register(Arc::new(files::EditFileTool));
    registry.register(Arc::new(files::ListFilesTool));
    registry.register(Arc::new(bash::BashTool));
    registry.register(Arc::new(ask_user::AskUserTool));
    registry.register(Arc::new(web::WebSearchTool));
    registry.register(Arc::new(web::FetchUrlTool));
    registry.register(Arc::new(python::PythonTool));
    registry.register(Arc::new(compile::CompilePdfTool));
    registry.register(Arc::new(skills::SearchSkillsTool));
    registry.register(Arc::new(skills::ListSkillsTool));
    registry.register(Arc::new(skills::SkillTool));
    registry.register(Arc::new(plan::GeneratePlanTool));
    registry.register(Arc::new(plan::UpdateStepStatusTool));
    registry.register(Arc::new(delegate::DelegateTool));
    registry.register(Arc::new(delegate::SubmitOutputTool));
    registry.register(Arc::new(memory::SearchMemoryTool));
    registry.register(Arc::new(memory::ReadMemoryTool));
    registry.register(Arc::new(openalex::SearchPapersTool));
    registry.register(Arc::new(openalex::FetchPaperTool));
}
