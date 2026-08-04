//! MCPServerManager：多 MCP server 管理（modules.md §6）。
//!
//! add_server（idempotent，失败不抛返 false）/ list_all_tools / call_tool / close_all。
//! 注意：MCPClient 不是 Send + Sync 友好的（持有 reqwest::Client 是 Send，但 connect 需 &mut）；
//! 这里用 tokio::sync::Mutex 包装连接态，连接后只读用。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::client::{MCPClient, McpError, McpTool};

/// 一个已管理的 server：连接后的 client + 缓存的工具列表。
struct ManagedServer {
    client: MCPClient,
    tools_cache: Vec<McpTool>,
    url: String,
}

pub struct MCPServerManager {
    servers: Mutex<HashMap<String, Arc<Mutex<ManagedServer>>>>,
}

impl MCPServerManager {
    pub fn new() -> Self {
        Self {
            servers: Mutex::new(HashMap::new()),
        }
    }

    /// 连接一个 MCP server。idempotent：已存在则返回 true；连接失败返回 false（不抛）。
    pub async fn add_server(&self, name: &str, url: &str) -> bool {
        let mut servers = self.servers.lock().await;
        if servers.contains_key(name) {
            return true;
        }
        let client = MCPClient::new(url);
        if let Err(e) = client.connect().await {
            tracing::warn!(server = %name, error = %e, "MCP connect failed");
            return false;
        }
        let tools = match client.list_tools().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(server = %name, error = %e, "MCP list_tools failed");
                Vec::new()
            }
        };
        servers.insert(
            name.to_string(),
            Arc::new(Mutex::new(ManagedServer {
                client,
                tools_cache: tools,
                url: url.to_string(),
            })),
        );
        tracing::info!(server = %name, "MCP server connected");
        true
    }

    pub async fn is_connected(&self, name: &str) -> bool {
        self.servers.lock().await.contains_key(name)
    }

    /// 列某 server 的工具。
    pub async fn list_tools(&self, name: &str) -> Option<Vec<McpTool>> {
        let servers = self.servers.lock().await;
        let srv = servers.get(name).cloned()?;
        let s = srv.lock().await;
        Some(s.tools_cache.clone())
    }

    /// 列全部 server 的工具（扁平为 (server_name, tool)）。
    pub async fn list_all_tools(&self) -> Vec<(String, McpTool)> {
        let servers = self.servers.lock().await;
        let mut out = Vec::new();
        for (name, srv) in servers.iter() {
            let s = srv.lock().await;
            for t in &s.tools_cache {
                out.push((name.clone(), t.clone()));
            }
        }
        out
    }

    /// 调用某 server 的某工具。
    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<String, McpError> {
        let srv = {
            let servers = self.servers.lock().await;
            servers
                .get(server)
                .cloned()
                .ok_or_else(|| McpError::Invalid(format!("server {server} not connected")))?
        };
        let s = srv.lock().await;
        s.client.call_tool(tool, args).await
    }

    /// server 元信息（端点用）。
    pub async fn server_info(&self, name: &str) -> Option<ServerInfo> {
        let servers = self.servers.lock().await;
        let srv = servers.get(name).cloned()?;
        let s = srv.lock().await;
        Some(ServerInfo {
            name: name.to_string(),
            url: s.url.clone(),
            connected: s.client.is_connected(),
            tools: s
                .tools_cache
                .iter()
                .map(|t| (t.name.clone(), t.description.clone()))
                .collect(),
        })
    }

    pub async fn list_servers(&self) -> Vec<String> {
        self.servers.lock().await.keys().cloned().collect()
    }

    pub async fn close_all(&self) {
        let mut servers = self.servers.lock().await;
        servers.clear();
    }
}

impl Default for MCPServerManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub name: String,
    pub url: String,
    pub connected: bool,
    pub tools: Vec<(String, String)>, // (name, description)
}
