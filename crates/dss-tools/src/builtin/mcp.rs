//! MCP 动态工具：把外部 MCP server 的工具挂载为 mcp__{server}__{tool}。
//!
//! 每个 McpDynamicTool 持有 (server_name, tool_spec)；call 时经 ToolContext.mcp 转发。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::error::ToolError;
use crate::spec::{Tool, ToolOutput, ToolSpec};

/// 一个挂载的 MCP 工具（转发到 MCPServerManager.call_tool）。
pub struct McpDynamicTool {
    server: String,
    tool_name: String,
    description: String,
    input_schema: Value,
}

impl McpDynamicTool {
    pub fn new(server: impl Into<String>, tool: dss_mcp::McpTool) -> Self {
        Self {
            server: server.into(),
            tool_name: tool.name,
            description: tool.description,
            input_schema: tool.input_schema,
        }
    }
}

#[async_trait]
impl Tool for McpDynamicTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: dss_mcp::mcp_tool_name(&self.server, &self.tool_name),
            description: format!("[MCP:{}] {}", self.server, self.description),
            parameters: self.input_schema.clone(),
        }
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        match ctx.mcp.call_tool(&self.server, &self.tool_name, args).await {
            Ok(content) => Ok(ToolOutput::ok(content)),
            Err(e) => Ok(ToolOutput::err(format!("MCP call failed: {e}"))),
        }
    }
}

/// 给 registry 注册一组 MCP 工具（≤30 全量挂载为 mcp__{server}__{tool}）。
/// 返回新增工具数。
pub fn register_mcp_tools(
    registry: &mut crate::ToolRegistry,
    server: &str,
    tools: &[dss_mcp::McpTool],
) -> usize {
    let mut n = 0;
    for t in tools {
        registry.register(Arc::new(McpDynamicTool::new(server, t.clone())));
        n += 1;
    }
    n
}

// 避免 unused json import warning（保留以备 spec 扩展）。
#[allow(dead_code)]
fn _unused(_: Value) {
    let _ = json!({});
}
