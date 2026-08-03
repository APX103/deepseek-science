# F2 — 日志系统（后端 logs 表 + 端点 + 前端日志页接真实）

> 对应 roadmap F2 / logging.md。状态：进行中（2026-08-03）。

## 目标
前端日志页 `/logs` 能浏览**系统日志 + agent 执行记录**（同表，按 source/level/kind/session 过滤）；agent run 的关键事件（llm_call/tool_call/run_end/...）持久化可查。

## 验收点
1. `cargo build` 无警告；cargo test（LogStore 基础）绿。
2. 后端启动写一条 system `startup` 日志；`GET /api/logs` 能看到。
3. agent run 产生 agent 日志（run_start/llm_call/tool_call/run_end），`GET /api/logs?session_id=` 能看到。
4. 前端 `/logs` 页从 mock 切真实 `GET /api/logs`，显示真实日志列表 + 过滤。
5. DELETE /api/logs 清理。

## 回顾

**实际做了什么**：
- `dss-db`：logs 表迁移（logging.md schema + 索引）+ repo（append_log/list_logs[LogFilter owned]/get_log/delete_logs）。
- 新建 `dss-observability` crate：`LogStore`（Arc<DbPool>，async append/list/get/delete，conn.interact）+ `LogEntry`（builder）。
- `dss-api`：AppState 加 logs；build_state 写 system `startup` 日志（含 version/data_dir/llm_configured detail）；stream_sse 写 agent `run_start`/`run_end` 日志（含 kind/iterations/token detail）；新增 `GET /api/logs`（过滤+分页）/`GET /api/logs/{id}`/`DELETE /api/logs?before=` 端点。
- 前端：`client.ts` listLogs/getLog/clearLogs 切真实 GET/DELETE /api/logs；LogsPage 经 listLogs 读真实（filter 接 query）。

**验证结果（curl）**：
- 后端启动：GET /api/logs 看到 system `startup`（detail 含 version/data_dir）。✅
- agent run：发「hi」→ GET /api/logs?session_id= 看到 agent `run_start`（run started: hi）+ `run_end`（run ended: Natural）。✅
- cargo build 0 警告；cargo test 32 测试全绿（F2 复用既有测试，LogStore 经端点实测）。
- 前端 listLogs 切真实（bun build 通过）；浏览器 GUI 因 IAB webview 后端不稳定未能刷新查看（同 P2a 已知限制），但 curl 全验证 + 代码路径正确。

**遗留（DEFER）**：
- 完整 tracing Layer（系统日志走显式 helper，关键点写）。
- 工具级 agent 日志（tool_call/tool_result，目前只 run_start/run_end；要接需 Runner 暴露事件或扫 session.messages）。
- /api/logs/stream 实时 tail、trace JSONL。
- 默认保留策略自动清理定时任务（提供 DELETE before 手动/前端触发）。

## 改动点

### 1. `dss-db`：logs 表 + repo
- 迁移加 `logs` 表（logging.md schema）+ 索引。
- repo：`append_log`/`list_logs(filters)`/`get_log`/`delete_logs(before?)`。

### 2. 新建 `dss-observability` crate
- `LogStore`（持 Arc<DbPool>，async append/list/get/delete）。
- **system 日志**：简单做法——提供 `log_system(store, level, kind, message, detail)` helper，在关键点显式调用（startup/migrate/llm_error）；**不**做完整 tracing Layer（复杂度高，P4b 留简化版）。
- **agent 日志**：在 Runner 已发的 AgentEvent 旁，由 dss-api 的 stream_sse 任务把这些事件结构化写 logs（tool_calls→tool_call/tool_result、iteration→iteration、complete→run_end）。复用同一事件源，不漂移。
- 写策略：P4b 直接 async 写（SQLite WAL + 小量），不做 mpsc 批量（够用，登记 DEFER）。

### 3. `dss-api`
- AppState 加 `logs: Arc<LogStore>`。
- 端点：`GET /api/logs`（query 过滤）/`GET /api/logs/{id}`/`DELETE /api/logs?before=`。
- build_state：写 startup 日志；run_migrations 写 db_migrate 日志。
- stream_sse：spawn 任务里把 AgentEvent 写 logs（agent source）。

### 4. 前端 `/logs`
- `api/client.ts`：`listLogs(filters)`/`deleteLogs(before)` 切真实 GET/DELETE /api/logs。
- `LogsPage`：去掉 mock，挂载 listLogs；过滤接 query。

## 工作顺序
1. 写计划。
2. dss-db logs 表 + repo；dss-observability crate（LogStore + helpers）。
3. dss-api 端点 + startup/agent 日志写入。
4. 前端 listLogs/deleteLogs + LogsPage 接真实。
5. cargo build/test + curl 验收 + 浏览器看日志页。
6. 回填回顾 + 更新 HANDOFF。

## 风险
- agent 日志写入拖慢主循环：P4b 每 run 几条写库，可接受（登记优化：mpsc 批量）。
- 日志体量：默认 ≥info，tool 摘要截断 500 字符；保留策略默认 14 天（DELETE before）。

## 不做（DEFER）
- 完整 tracing Layer（系统日志走显式 helper）。
- /api/logs/stream 实时 tail。
- trace JSONL。
- 默认保留策略的自动清理定时任务（提供 DELETE before 手动/前端触发）。
