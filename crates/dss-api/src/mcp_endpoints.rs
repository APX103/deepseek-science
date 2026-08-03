//! MCP 端点：GET /api/mcp/{name}/tools → {name, url, connected, tools, error?}。

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use serde_json::Value;

use crate::state::AppState;

#[derive(Serialize)]
pub struct McpToolsResp {
    name: String,
    url: String,
    enabled: bool,
    connected: bool,
    tools: Vec<McpToolItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct McpToolItem {
    name: String,
    description: String,
}

/// `GET /api/mcp/{name}/tools`。
pub async fn mcp_tools(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Json<Value> {
    match state.mcp.server_info(&name).await {
        Some(info) => Json(serde_json::json!({
            "name": info.name,
            "url": info.url,
            "enabled": true,
            "connected": info.connected,
            "tools": info.tools.into_iter().map(|(n, d)| serde_json::json!({"name": n, "description": d})).collect::<Vec<_>>(),
        })),
        None => Json(serde_json::json!({
            "name": name,
            "url": "",
            "enabled": false,
            "connected": false,
            "tools": [],
            "error": "MCP server not connected",
        })),
    }
}
