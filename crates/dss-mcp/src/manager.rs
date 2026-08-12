//! MCPServerManager：多 MCP server 管理（modules.md §6）。
//!
//! add_server（idempotent，失败不抛返 false）/ list_all_tools / call_tool / close_all。
//! 已连接的 client 通过 `Arc` 共享；网络 I/O 不持有 server map 锁，避免慢请求阻塞状态查询。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::client::{
    MCPClient, McpError, McpResource, McpResourceContent, McpRouteOptions, McpTool,
};

/// 一个已管理的 server：连接后的 client + 缓存的工具列表。
struct ManagedServer {
    client: MCPClient,
    tools_cache: Vec<McpTool>,
    tools_enabled: bool,
    url: String,
}

pub struct MCPServerManager {
    servers: Mutex<HashMap<String, Arc<ManagedServer>>>,
}

impl MCPServerManager {
    pub fn new() -> Self {
        Self {
            servers: Mutex::new(HashMap::new()),
        }
    }

    /// 连接一个 MCP server。idempotent：已存在则返回 true；连接失败返回 false（不抛）。
    pub async fn add_server(&self, name: &str, url: &str) -> bool {
        if let Err(e) = self.try_add_server(name, url).await {
            tracing::warn!(server = %name, error = %e, "MCP connect failed");
            false
        } else {
            true
        }
    }

    /// Connect a server for Resources discovery only. Unlike [`Self::add_server`], this never
    /// sends `tools/list` and rejects later `tools/call` attempts through the manager.
    pub async fn add_server_resources_only(&self, name: &str, url: &str) -> bool {
        if let Err(e) = self.try_add_server_resources_only(name, url).await {
            tracing::warn!(server = %name, error = %e, "MCP Resources-only connect failed");
            false
        } else {
            true
        }
    }

    /// Detailed connection API used by diagnostics and live tests.
    pub async fn try_add_server(&self, name: &str, url: &str) -> Result<(), McpError> {
        self.try_add_server_with_mode(name, url, McpRouteOptions::default(), true)
            .await
    }

    /// Detailed Resources-only connection API used by Registry integrations and tests.
    pub async fn try_add_server_resources_only(
        &self,
        name: &str,
        url: &str,
    ) -> Result<(), McpError> {
        self.try_add_server_with_mode(name, url, McpRouteOptions::default(), false)
            .await
    }

    /// Connect with runtime-only route overrides. Network I/O happens outside the map lock.
    pub async fn try_add_server_with_route_options(
        &self,
        name: &str,
        url: &str,
        route: McpRouteOptions,
    ) -> Result<(), McpError> {
        self.try_add_server_with_mode(name, url, route, true).await
    }

    /// Resources-only counterpart to [`Self::try_add_server_with_route_options`].
    pub async fn try_add_server_resources_only_with_route_options(
        &self,
        name: &str,
        url: &str,
        route: McpRouteOptions,
    ) -> Result<(), McpError> {
        self.try_add_server_with_mode(name, url, route, false).await
    }

    async fn try_add_server_with_mode(
        &self,
        name: &str,
        url: &str,
        route: McpRouteOptions,
        tools_enabled: bool,
    ) -> Result<(), McpError> {
        if let Some(server) = self.servers.lock().await.get(name).cloned() {
            return ensure_connection_mode(name, &server, tools_enabled);
        }
        let client = MCPClient::try_new_with_route_options(url, route)?;
        client.connect().await?;
        let tools = if tools_enabled {
            match client.list_tools().await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(server = %name, error = %e, "MCP list_tools failed");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let mut servers = self.servers.lock().await;
        if let Some(server) = servers.get(name) {
            return ensure_connection_mode(name, server, tools_enabled);
        }
        servers.insert(
            name.to_string(),
            Arc::new(ManagedServer {
                client,
                tools_cache: tools,
                tools_enabled,
                url: url.to_string(),
            }),
        );
        tracing::info!(server = %name, tools_enabled, "MCP server connected");
        Ok(())
    }

    pub async fn is_connected(&self, name: &str) -> bool {
        self.servers.lock().await.contains_key(name)
    }

    /// 列某 server 的工具。
    pub async fn list_tools(&self, name: &str) -> Option<Vec<McpTool>> {
        let servers = self.servers.lock().await;
        let srv = servers.get(name).cloned()?;
        Some(srv.tools_cache.clone())
    }

    /// 列全部 server 的工具（扁平为 (server_name, tool)）。
    pub async fn list_all_tools(&self) -> Vec<(String, McpTool)> {
        let servers = self.servers.lock().await;
        let mut out = Vec::new();
        for (name, srv) in servers.iter() {
            for t in &srv.tools_cache {
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
        if !srv.tools_enabled {
            return Err(McpError::Invalid(format!(
                "server {server} is connected for Resources only"
            )));
        }
        srv.client.call_tool(tool, args).await
    }

    pub async fn list_resources(&self, server: &str) -> Result<Vec<McpResource>, McpError> {
        let srv = self.managed(server).await?;
        srv.client.list_resources().await
    }

    pub async fn read_resource(
        &self,
        server: &str,
        uri: &str,
    ) -> Result<Vec<McpResourceContent>, McpError> {
        let srv = self.managed(server).await?;
        srv.client.read_resource(uri).await
    }

    async fn managed(&self, server: &str) -> Result<Arc<ManagedServer>, McpError> {
        self.servers
            .lock()
            .await
            .get(server)
            .cloned()
            .ok_or_else(|| McpError::Invalid(format!("server {server} not connected")))
    }

    /// server 元信息（端点用）。
    pub async fn server_info(&self, name: &str) -> Option<ServerInfo> {
        let servers = self.servers.lock().await;
        let srv = servers.get(name).cloned()?;
        Some(ServerInfo {
            name: name.to_string(),
            url: srv.url.clone(),
            connected: srv.client.is_connected(),
            resources: srv
                .client
                .metadata()
                .is_some_and(|metadata| metadata.capabilities.resources),
            tools: srv
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

fn ensure_connection_mode(
    name: &str,
    server: &ManagedServer,
    tools_enabled: bool,
) -> Result<(), McpError> {
    if server.tools_enabled == tools_enabled {
        Ok(())
    } else {
        Err(McpError::Invalid(format!(
            "server {name} is already connected with a different capability mode"
        )))
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
    pub resources: bool,
}
