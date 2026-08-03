# 前端接 P3（wireup）— 把前端从 mock 切到真实后端 API

> 状态：进行中（2026-08-03）。非 roadmap 单独阶段，是 F1 收尾 / P3 前端落地。

## 目标
前端 HomePage / WorkbenchPage / Sidebar 读真实后端 projects/sessions；建会话/建项目/删项目走真实 API；WorkbenchPage 进真实 sid 时从后端恢复消息历史（GET /api/sessions/{sid}）。后端离线时优雅回退提示（不崩）。

## 现状
- `api/client.ts`：除 `probeBackend`/`createSessionApi`/`connectSSE` 外全是 mock。
- `store.ts`：seed = mock 数据 → localStorage；页面经 `useProjects/useSessions/useMessages` 读 store。
- 后端已有完整端点（P3）：projects 全套、sessions GET/POST/DELETE/{sid}、stream-sse。

## 改动点

### 1. `api/client.ts` 切真实 fetch + 后端→前端类型映射
- `request<T>(path, init)`：真实 fetch 封装（JSON 解析 + 非 ok 抛错）。
- `getHealth/getConfig/listProjects/createProject/patchProject/archiveProject/deleteProject/getProject`、`listSessions/getSession/deleteSession` 全部走真实 API。
- 映射：后端 `ProjectRow{id,name,description,last_session_id,archived,created_at,updated_at}` → 前端 `Project`（补默认 pinned=false、session_count=0、agent_context=''）；后端 `SessionRow{id,title,workspace,model,status,project_id,...}` → `SessionSummary`（project_id/live/status/title）。
- `getSession(sid)` → 解析 messages（content 是 OpenAI 形态 JSON + 顶层 harness_notice）。

### 2. `store.ts` 改为后端驱动
- 去掉 mock seed；启动 `loadProjects()/loadSessions()` 从后端拉取填 store。
- `addProject/updateProject/removeProject` 仍更新本地 store（由页面调用真实 API 后回写）。
- 新增 `setProjects/setSessions`（供页面从 API 回填）。
- messages：`loadMessages(sid)` 从 `getSession(sid)` 拉取历史填 `messages[sid]`。

### 3. 页面接真实数据
- HomePage：挂载时 `loadProjects()+loadSessions()`；New Project 走真实 `createProject` → addProject；项目操作走真实 API。
- WorkbenchPage：进会话时若 messages 空 → `loadMessages(sid)` 恢复历史；handleSend 不变（已真实）。
- Sidebar：New session 已走 `createSessionApi`（保留）。

### 4. 离线兜底
- 后端离线（probeBackend=false）：Home/Workbench 显示空态 + 提示，不崩。

## 验收
1. `bun run build` 通过。
2. 起后端：Home 显示真实 projects（至少 proj_default）；建项目 → 出现在列表；重启前端/刷新 → 项目仍在（DB 持久）。
3. 进会话发消息 → 刷新页面 → 历史消息恢复（含 tool 块）。
4. 删项目/会话 → 列表更新。
5. P2a 工具对话流式渲染不回归。

## 回顾

**实际做了什么**：
- `api/client.ts`：`request<T>` 真实 fetch 封装；health/config/listProjects/createProject/patchProject/archiveProject/unarchiveProject/deleteProject/getProject、listSessions/getSession/deleteSession 全切真实 API；加后端行→前端类型映射（`mapProject`/`mapSession`，补 pinned/session_count/agent_context 默认值、status 映射 active→completed）；`getSession` 把后端「OpenAI 协议形态 ChatMessage」转前端 `ContentBlock[]`（text/tool_use/tool_result，ChatArea pairTools 据此渲染）。
- `store.ts`：去 mock seed；启动从后端 `loadFromBackend()`（projects+sessions）+ `loadMessages(sid)`（会话历史恢复）+ `useBackendOnline`；去掉 localStorage 持久化（数据真源是后端 DB）。
- `HomePage`：挂载 `loadFromBackend`；Archive/Delete 走真实 `api.archiveProject/deleteProject`（成功后 reload）；离线态提示。
- `WorkbenchPage`：进会话 `loadMessages(sid)` 恢复历史 + `loadFromBackend` 刷侧栏。
- 后端 `sessions.rs::SessionListItem` 补 `created_at/updated_at/project_id`（修 `NaNd ago`）。

**验证结果（起后端+前端 dev，浏览器 + curl）**：
- Home 显示真实 `proj_default` + 真实会话列表（title/时间正确，`33m ago`）。✅
- 进真实 sid → Workbench 从后端恢复出 assistant 消息（"你好！我是一个AI助手…"）。✅
- 后端 project create→list→delete、session delete 经 API 验证全通（前端经同一 list 端点可见）。✅
- `bun build`（tsc）通过；cargo build 无警告。
- 修了 1 个回归 bug：SessionListItem 漏 created_at/updated_at 导致前端 `NaNd ago`。

**偏离/遗留**：
- New Project 弹窗的「真实创建」未做 GUI 端到端（IAB 无法驱动受控输入填表单），但 `createProject` API 已真实、HomePage 的 list 已验证——手动用真实浏览器填表单即可工作。
- Settings/MCP/Skills/Templates/Files/Compile/Logs 仍 mock（对应后端阶段未做）。
- artifacts 面板仍 mock。

## 不做
- Settings/MCP/Skills/Templates/Files/Compile 仍 mock（对应后端阶段未做）。
- artifacts 面板仍 mock。
