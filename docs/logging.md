# 日志系统

> **本文回答**：「日志列表」具体是什么？后端怎么采集/存储/暴露日志？前端怎么呈现？

> 状态：方向已定（用户要求「加上日志，专门的日志列表」）；数据模型与端点设计为新增，待实现期细化。

---

## 需求理解

用户原话：「在新的 APP 里面肯定是要加上日志的，就是会有一个专门日志列表。」

动机与设计目标：
- 一个本地优先的科研工作台，后端有大量值得记录的事件：启动/关闭、DB 迁移、MCP 连接、LLM 调用、agent 执行过程、计划审批、验证裁决等。
- 如果 agent 执行过程只靠 SSE 事件（`tool_calls`/`tool_results`/`notice`）实时呈现，**刷新即逝、无历史可查**。
- 出问题时如果只能「开终端看后端 stdout」，对桌面 App 用户不友好。

**本系统要提供的**：一个结构化的、可按时间/session/级别检索的**统一日志视图**，把「系统运行日志」和「agent 执行记录」聚合到前端可浏览的列表里。

### 两类日志（统一到一个视图）

1. **系统日志（system）**：后端启动/关闭、DB 迁移、MCP 连接、LLM 调用成败、错误堆栈、配置加载等。来源：`tracing`。
2. **执行日志（agent/exec）**：每个 agent run 的 LLM 调用（含 token/耗时/模型）、工具调用（含输入摘要/结果摘要/耗时）、frame 状态变迁、计划审批、验证裁决、compaction 事件、记忆抽取等。来源：agent 事件的结构化落库。

> 决策：**两类同表存、用 `source`/`kind` 区分**，前端一个列表按维度过滤。不要做成两个独立面板。

---

## 数据模型

新增 `logs` 表（详见 [数据模型扩展](data-model.md#日志表新增)）：

```
logs
  id            INTEGER PK AUTOINCREMENT
  ts            TEXT NOT NULL          -- ISO8601 UTC，主排序键
  level         TEXT NOT NULL          -- debug|info|warn|error
  source        TEXT NOT NULL          -- system|agent
  kind          TEXT NOT NULL          -- 事件类型，见下表
  session_id    TEXT                   -- 关联会话（system 日志可空）
  frame_id      TEXT                   -- 关联 frame（agent 日志）
  iteration     INTEGER               -- agent 第几轮（agent 日志）
  message       TEXT NOT NULL          -- 人读摘要
  detail        TEXT                   -- JSON：结构化详情（tool 输入/结果、token、耗时、错误栈等）
  trace_id      TEXT                   -- 关联 trace span（可选）
```
索引：`(ts)`、`(session_id, ts)`、`(level)`、`(source, kind)`。

**保留策略**：默认保留 N 天（如 14），可配置；超量自动清理。避免无限膨胀。

### `kind` 枚举（事件类型）

| source | kind | message 示例 | detail |
|--------|------|-------------|--------|
| system | `startup` | 后端启动 v0.1.0 port 17896 | `{version, port, data_dir}` |
| system | `shutdown` | 后端关闭 | `{reason}` |
| system | `db_migrate` | 迁移 mem_layer_a 完成 | `{step, rows}` |
| system | `mcp_connect` | MCP server「搜索」已连接 | `{server, tools_count}` |
| system | `mcp_error` | MCP 连接失败 | `{server, error}` |
| system | `llm_error` | LLM 调用失败重试 | `{model, attempt, error}` |
| agent | `run_start` | 会话开始运行 | `{prompt_summary}` |
| agent | `llm_call` | 调用 deepseek-chat | `{model, input_tokens, output_tokens, ms, stop_reason}` |
| agent | `tool_call` | 调用 write_file | `{tool, input_summary, ms}` |
| agent | `tool_result` | write_file 完成 | `{tool, ok, result_summary}` |
| agent | `tool_error` | bash 执行失败 | `{tool, error}` |
| agent | `frame_status` | frame → AWAITING_PLAN_APPROVAL | `{from, to}` |
| agent | `plan` | 计划已批准（3 步） | `{steps_count, approved}` |
| agent | `compact` | Rolling Compact 折叠 1 段 | `{level, tokens_freed}` |
| agent | `verify` | reviewer 裁决：warn | `{verdict, findings_count}` |
| agent | `memory` | 抽取 3 条记忆 | `{appended, replaced, removed}` |
| agent | `run_end` | 会话运行结束 | `{kind, iterations, usage}` |
| system | `retention_sweep` | 保留策略清理完成（D-T07） | `{logs_by_age, logs_by_count, memory_expired, memory_demoted, memory_errors}` |

---

## 后端采集与存储（dss-api + dss-observability）

### 采集

- **system 日志**：`tracing` 的 subscriber 增加一个 layer，把 `INFO`+ 级别的事件写入 `logs` 表（`source=system`）。错误自动带堆栈到 `detail`。
- **agent 日志**：在 [模块](modules.md#11-dss-apihttpsse--sessionmanager) 的 SSE `WSCallbacks` 旁，加一个**持久化 callback**，把同样的事件（`on_tool_calls`/`on_tool_results`/`on_iteration`/`on_event`…）结构化落 `logs` 表（`source=agent`）。
  - 这样实时 SSE 与持久日志**共用同一事件源**，不会漂移。
- **写策略**：异步、批量、非阻塞（`mpsc` → 后台 flush task），不拖慢 agent 主循环。

### 新 crate：`dss-observability`

职责：
- `LogStore`：写/查 `logs` 表（按 session/level/kind/时间区间过滤，分页）。
- 持久化 callback（agent 事件 → logs）。
- tracing layer（系统事件 → logs）。
- 可选 trace JSONL（默认关，开则同时写 `trace/`）。

---

## API 端点（新增，见 [API 契约扩展](api-contract.md#日志端点新增)）

| Method | Path | 用途 |
|--------|------|------|
| GET | `/api/logs` | 查日志，query 过滤：`session_id`/`source`/`level`/`kind`/`since`/`until`/`limit`/`offset`。返回 `{logs:[...], total}` |
| GET | `/api/logs/{id}` | 单条详情（含完整 detail JSON） |
| DELETE | `/api/logs` | 清理（可按 `before` 时间批量删，或全清） |
| WS/SSE | `/api/logs/stream`（可选） | 实时日志推送（前端日志页 live tail） |

**响应项**：`{id, ts, level, source, kind, session_id, frame_id, iteration, message, detail, trace_id}`。`detail` 在列表接口可只给摘要、详情接口给全量（控制 payload）。

---

## 前端：日志列表（新组件）

**位置**：作为独立入口（如左侧栏新增「日志」tab，或顶栏入口），区别于会话/工作区。

**功能**：
- **列表**：按时间倒序，每行 `时间 | 级别图标 | source 标签 | kind | message`。级别用色（error 红/warn 黄/info 灰/debug 淡）。1px 细线分隔（DeepSeek 风格）。
- **过滤**：按 session / source(system/agent) / level / kind / 时间范围。
- **展开详情**：点一行展开 `detail` JSON（工具输入/输出、token、错误栈）。
- **跳转**：agent 日志可点「跳到会话」关联到对应 session。
- **（可选）实时 tail**：开着页面时新日志自动流入。

**样式**：遵循 [设计系统](design-system.md)（蓝色强调、1px 边框、平面、无毛玻璃）。代码/JSON 用 JetBrains Mono。

---

## 隐私与安全

- 日志可能含 LLM 输入/输出摘要、工具参数（含文件内容片段）。**默认不记 debug 级**（避免把用户文件写进日志库）。
- `detail` 里敏感字段（api_key、完整文件内容）**脱敏**后落库。
- 日志库是本地 SQLite，不外传（本地优先原则不变）。

> 决策：**默认 level ≥ info**；`tool_call` 的 `input_summary`/`result_summary` 截断长度（如各 500 字符），不存全量。全量留 trace JSONL（需显式开）。

---

## 对其他文档的影响

- [模块](modules.md)：新增 `dss-observability` crate；`dss-api` 加持久化 callback。
- [API 契约](api-contract.md)：新增日志端点（**新增**项，前端需配套新页面——不破坏既有契约）。
- [数据模型](data-model.md)：新增 `logs` 表。
- [路线图](roadmap.md)：新增「日志系统」阶段（建议靠后，如 P4 之后，待 agent 事件源稳定）。

---

## 待定（登记 [决策](decisions.md)）

- ~~日志保留策略默认值（14 天？按量？）。~~ ✅ 已定（D-T07）：按天 + 按量双限制，默认 14 天 / 10 万条；启动 + 每 6h sweep。
- 是否要 `/api/logs/stream` 实时推送（增加复杂度，可后置）。
- `kind` 枚举是否随增强方向扩展（如 P11 文献知识库加检索日志类型）。

---

至此，日志系统设计完成。回到 [README](../README.md) 看完整导航，或读 [决策记录](decisions.md) 看更新后的决策日志。
