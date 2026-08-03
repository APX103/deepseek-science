//! dss-mcp: MCP streamable-HTTP 客户端 + server 管理器。
//!
//! P7：connect/list_tools/call_tool + MCPServerManager + 动态工具挂载辅助。
//! agent-registry 注入 / mcp_search/mcp_call meta / mcp_read_resource DEFER。

pub mod client;
pub mod manager;

pub use client::{McpError, MCPClient, McpTool};
pub use manager::{MCPServerManager, ServerInfo};

/// 动态挂载的工具名：mcp__{server}__{tool}。
pub fn mcp_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}
