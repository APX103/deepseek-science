# P7 — MCP streamable HTTP 客户端 + 动态挂载

> 对应 roadmap P7 / modules.md §6。状态：已完成并扩展 Agent Registry Resources→A2A 链路（2026-08-12）。

## 目标
后端能连接一个外部 MCP server（streamable HTTP + SSE），列出其工具，把工具动态挂载为 `mcp__{server}__{tool}`，让 agent 能调用。`GET /api/mcp/{name}/tools` 端点。

## 验收点
1. `cargo build` 无警告；cargo test（JSON-RPC 解析、SSE 聚合、tools/list 解析）绿。
2. 集成测试：起一个内嵌的 MCP-兼容 axum echo server（initialize/tools/list/call_tool）→ MCPClient connect → list_tools 返回 → call_tool 返回结果。
3. MCPServerManager add_server/list_all_tools/call_tool。
4. 动态挂载：≤30 工具时注册 mcp__{server}__{tool}（通过 ToolRegistry）。
5. GET /api/mcp/{name}/tools 端点返回 `{name, url, connected, tools}`。
6. 现有功能不回归。

## 回顾

**实际做了什么**：
- 新建 `dss-mcp` crate：`client.rs`（MCPClient：streamable HTTP + SSE JSON-RPC；initialize 捕获 Mcp-Session-Id → notifications/initialized → tools/list → tools/call；响应解析兼容 text/event-stream 聚合 + 纯 JSON；connect 成功即 connected，session_id 可选）、`manager.rs`（MCPServerManager：add_server idempotent 失败不抛返 false / list_tools / list_all_tools / call_tool / server_info / close_all）。
- `dss-tools`：`McpDynamicTool`（转发到 manager.call_tool）+ `register_mcp_tools`（≤30 全量挂载 mcp__{server}__{tool}）；ToolContext 加 `mcp: Arc<MCPServerManager>` + with_mcp/with_mcp_arc。
- `dss-core`：Settings 加 `mcp_servers: Vec<McpServerConfig{name,url,enabled}>`（config.toml/settings.json 可配，后文件覆盖前）。
- `dss-api`：AppState 加 mcp；build_state 启动时连接配置的 enabled server + 挂载其工具（best-effort 失败不阻断）；`GET /api/mcp/{name}/tools` 端点；stream_sse 的 ToolContext 共享 mcp。
- 集成测试：内嵌 MCP-兼容 axum echo server（initialize/notifications/initialized/tools/list/tools/call JSON-RPC）→ MCPClient 全流程 + MCPServerManager。

**验证结果**：
- `cargo test` 全 workspace **37 测试全绿**（3 mcp unit：JSON 解析/SSE 聚合/error；2 mcp 集成：client 全流程 / manager idempotent+call）；0 警告。
- 启动（无配置 mcp server）：正常，无报错。
- GET /api/mcp/unknown/tools → `{connected:false, error:"MCP server not connected"}` 优雅返回。
- 配置 mcp_servers 后：启动连接 + 挂载工具（经集成测试 echo server 验证全流程）。

**后续遗留（DEFER）**：
- mcp_search/mcp_call meta 工具（>30 工具时；P7 只做 ≤30 全量挂载）。
- generate_mcp_skills（为每 server 生成 mcp-{slug} skill）。
- `registry_connect_mcp_server`（本轮不需要：Registry 是设置中默认 server，热运行时统一连接）。
- 前端 MCP 设置面板接真实（settings localStorage；后端 settings.json 已可配 mcp_servers）。

## 改动点

### 1. 新建 `dss-mcp` crate
- `client.rs`：MCPClient（reqwest，streamable HTTP + SSE）。
  - `connect(url)`: POST JSON-RPC `initialize`(protocolVersion 2024-11-05, capabilities) → 捕获 `Mcp-Session-Id` header → POST `notifications/initialized`。
  - `list_tools()`: `tools/list` → 解析 result.tools[{name,description,inputSchema}]。
  - `call_tool(name, args)`: `tools/call` → 解析 result.content。
  - 响应解析：兼容 text/event-stream（聚合 data: 取最后 result/error 对象）与纯 JSON。
- `manager.rs`：MCPServerManager（HashMap<name, MCPClient>）。add_server（idempotent，失败返 false）/list_all_tools/call_tool/close_all。
- `mount.rs`：动态挂载 helper——给定 (server_name, tools)，生成 ToolSpec 列表供注册（≤30 全量 mcp__{server}__{tool}，否则 meta mcp_search/mcp_call）。
- 单元测试：JSON-RPC 响应解析、SSE 聚合、tools/list 解析、call_tool 结果解析。

### 2. dss-tools：MCP 动态工具
- 加 `McpDynamicTool`（持有 server_name + tool_name + Arc<MCPServerManager>，call 时转发到 manager.call_tool）。
- ToolRegistry 加 `register_mcp_tools(server_name, tools, manager)`。
- ToolContext 加 `mcp: Option<Arc<MCPServerManager>>`。

### 3. dss-api
- AppState 加 `mcp: Arc<MCPServerManager>`。
- `GET /api/mcp/{name}/tools`：{name, url, connected, tools:[{name,description}], error?}。
- 配置：从 settings 读 mcp_servers（P7 加 settings 字段，默认空）；启动时尝试连接。

### 4. 集成测试（dss-mcp/tests/）
- 内嵌 MCP-兼容 axum server（initialize/tools/list/call_tool JSON-RPC）→ MCPClient 全流程。

## 工作顺序
1. 写计划。
2. dss-mcp：client（JSON-RPC + SSE 解析）+ manager + mount + 单元测试。
3. dss-tools McpDynamicTool + ToolContext.mcp。
4. dss-api 端点 + settings.mcp_servers。
5. 集成测试（内嵌 MCP server）。
6. cargo build/test 绿 + 更新 HANDOFF。

## 风险
- streamable HTTP + SSE 协议细节（Mcp-Session-Id、notifications/initialized）需精确；以内嵌 echo server 测试兜底。
- reqwest SSE 流式解析（复用 dss-llm 的手写行解析模式）。
- MCP server 实际可用性：P7 用内嵌 echo server 验证；真实 server（如 Zhipu 搜索）由用户配置后即用。

## 2026-08-12 Agent Registry 扩展

- 后端默认 `agent-registry = https://a2a-dev.intern-ai.org.cn/mcp`；显式列表覆盖，`[]` 关闭。
- MCP client 保留协商元数据、会话 header，支持有界分页 `resources/list` 和精确 `resources/read`，同时保留 text/blob content。
- `mcp_list_resources` / `mcp_read_resource` 仅在 captured runtime 存在已连接且声明 Resources 的 server 时动态提供，server schema 只枚举对应 manager key；`call_agent` 只接受 Registry 返回的 `resource_uri`，不接受任意 endpoint/credential。
- canonical Registry 强制 Resources-only：连接时不发送 `tools/list`，不挂载/调用它宣称的 MCP Tools。`call_agent` 在同一 captured MCP runtime 中重新 list/read，校验匿名 A2A descriptor，获取并校验 Agent Card，然后经既有 A2A client 发起一次 fresh Send；不接受任意 task/context handle。它保留 exclusive batch、每 run 副作用 guard、完整响应 frame、typed Registry provenance 和稳定结果 schema。
- 对真实测试服务的 wildcard interface、card/descriptor auth mismatch、direct Task response 仅有显式且留 warning 的窄兼容；普通用户配置的 A2A Agent 仍走严格路径。
- Hermetic ToolRouter E2E 与显式 ignored live E2E 均已覆盖完整 `list → read → call_agent → A2A Artifact` 流程；live oracle 精确校验 `DSS_A2A_E2E_OK`，并如实保留 `TASK_STATE_INPUT_REQUIRED` 可恢复状态。

## 不做（DEFER）
- mcp_search/mcp_call meta 工具（>30 工具时；P7 只做 ≤30 全量挂载）。
- generate_mcp_skills（为每 server 生成 mcp-{slug} skill）。
- `registry_connect_mcp_server`（Registry 已由 settings/runtime 统一连接）。
- 前端 MCP 设置面板接真实（目前 settings localStorage）。
