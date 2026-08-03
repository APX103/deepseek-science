//! 内置工具。
//!
//! 文件/bash/ask_user（P2a）+ web/python（P2b-tools）+ compile/skills（P5a）。

pub mod ask_user;
pub mod bash;
pub mod compile;
pub mod files;
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
}
