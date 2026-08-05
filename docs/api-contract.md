# API 契约

> **本文回答**：后端暴露哪些 HTTP/SSE 端点？请求/响应/SSE 事件的字段定义是什么？

> 状态：已定（本项目自有契约）

**核心原则**：前后端都是本项目自有工程，API 契约由本项目自行定义、可协同演进。端点路径与事件字段即本文档定义的契约；偏离需在 [decisions](decisions.md) 登记。

---

## API 基址

- **Tauri 环境**：`http://127.0.0.1:{localStorage['dss_backend_port'] 或 window.__BACKEND_PORT__}/api`
- **浏览器开发**：`/api`（Vite 代理到后端）

> 决策：端口注入变量名用本项目自有的 `dss_backend_port` / `__BACKEND_PORT__`。Tauri 壳为本项目自建，`inject_port_and_navigate()` 在本项目壳里实现。

---

## 端点清单（method + path + 用途）

★ = 涉及流式/子进程，实现重点。

### 系统 / 配置
| Method | Path | 用途 |
|--------|------|------|
| GET | `/api/health` | `{status:"ok", version}` |
| GET | `/api/config` | 当前生效配置（llm_configured/model/base_url/context_window/has_mcp/mcp_count/api_keys_configured/default_workspace/host/port） |
| GET | `/api/settings` | AppSettings（API key 已脱敏） |
| POST | `/api/settings` | 以 `revision` 做 CAS 后合并保存 AppSettings；陈旧快照返回 409 |

`AppSettings.a2a_agents` 是最多 16 项的数组。每项可编辑字段为
`id/name/endpoint/enabled/timeout_seconds/bearer_token?/clear_bearer_token?`；GET 只返回
`bearer_token_masked`，绝不返回明文凭据。诊断字段为
`status/last_error/last_refreshed_at/tool_name/card_summary`。保存使用与 LLM 相同的私密原子
`settings.json` 事务并立即替换一个 LLM+A2A 运行时快照；已经开始的 run 继续使用旧快照。
空 token 默认保留同 endpoint 的既有凭据，只有显式 `clear_bearer_token=true` 才清除；修改
endpoint 时旧凭据绝不自动转发。GET 返回的 `revision` 必须原样带回 POST，防止两个设置窗口互相覆盖。

### A2A client

- 本应用只实现 client，不暴露 inbound A2A server。
- 每个启用项在后续 run 中映射为一个稳定的 `a2a_agent_*` 工具；Plan mode 只看到能力目录，审批前仍由硬门禁止执行。
- 保存时 best-effort 拉取 Agent Card；每次工具调用前必须再次对配置 origin 的
  `/.well-known/agent-card.json` 做条件 GET。刷新失败时本次调用 fail closed。
- 支持 A2A v1.0/v0.3 的 JSON-RPC 与 HTTP+JSON。动态工具显式区分：默认 `send`
  （`SendMessage` 后在本次预算内轮询）、`submit`（只发送一次并立即返回/检查点远端非终态
  Task handle）、`get_task`（只执行幂等 `GetTask`，绝不重放 Message）以及 `cancel_task`
  （显式请求取消）。因此长任务可在另一轮、App 重启或按 session id 恢复后继续查询。
  `INPUT_REQUIRED`/`AUTH_REQUIRED` 是可续接的 `task_interrupted`，不是完成或失败：本地工具结果
  `is_error=false` 并保留 `task_id/context_id/state`；满足输入/认证要求后，用同一工具发送携带该
  Task handle 的 follow-up Message 续接，单纯查看状态仍使用 `get_task`。旧版已落库的
  `kind=task, success=false` 中断 envelope 会按标准 state 兼容识别。
  每个 user run 最多允许一次发送 Message 的副作用；一旦该 run 已执行 `get_task/cancel_task`，只允许
  对刚观察到的 `INPUT_REQUIRED/AUTH_REQUIRED` 使用相同 `task_id` 续接，禁止纠错循环另起新 Task。
  网络结果不确定时同样不自动重试 Send，因为新的 message id 无法保证远端幂等。
  工具结果只承诺保存客户端实际收到的全部完整响应，不声称重建轮询间隔内的服务端瞬时事件。
- 工具结果 `content` 是 schema 为 `dss.a2a.tool-result.v1` 的 JSON：包括调用时 Card
  快照、action、稳定 message/request id、按 `sequence` 排列的全部已接收 wire body、终态和警告。
  这个 schema 是 Deepseek Science 的本地显示/持久化 envelope，不是 A2A wire 扩展，也不会发送给
  远端。A2A v1 没有标准 thinking/tool-call 字段；客户端只展示远端实际返回的标准
  Message/Task/TaskStatus/Artifact/Part 与完整原始 frame。
- 远端 Card/输出是非可信外部数据；URL part 不自动抓取，Markdown 不执行 HTML。请求不跟随重定向，Card 声明的调用 URL 必须同源；响应受单项/总量和超时上限约束。
- 一个工具批次交付后，assistant tool-call、全部配对结果、当前 plan/pending-ask 与 provisional run
  元数据先在同一 SQLite 事务中增量检查点，再继续下一次 LLM 请求；最终状态在原 run 行上原子收口。
  因此最终回答尚未生成时退出也可按 session id 恢复已经完成的 A2A 工具卡。长任务应优先
  `submit`，让 Task handle 在第一次短调用返回时尽快落库；后续每个 `get_task` 结果继续作为独立工具卡
  落库。单次 `send/get_task` 调用内部尚未返回的中间轮询帧不单独建事件流。

### 记忆
| GET | `/api/memories?entity=` | 列记忆 |
| DELETE | `/api/memories/{mem_id}` | 删记忆 |

### Skills / 模板
| GET | `/api/skills` | `[{name, description, source, enabled}]` |
| GET | `/api/templates` | `[{id, name, description, documentclass, columns}]` |
| GET | `/api/templates/{template_id}` | 模板 `.tex` 纯文本 |

### MCP
| GET | `/api/mcp/{server_name}/tools` | `{name, url, enabled, connected, tools:[{name,description}], error?}` |

### Projects
| GET | `/api/projects?archived=false` | 项目列表（默认项目置顶） |
| POST | `/api/projects` | 建项目 `proj_<8hex>` |
| PATCH | `/api/projects/{pid}` | 改名/描述/last_session_id |
| POST | `/api/projects/{pid}/archive` | 软删（默认项目 400） |
| POST | `/api/projects/{pid}/unarchive` | 恢复 |
| DELETE | `/api/projects/{pid}?force=false` | 永久删（有 session 且非 force → 409） |
| GET | `/api/projects/{pid}` | 项目详情 + `discoverable=1` 的会话列表 |

### Sessions
| GET | `/api/sessions` | 可发现会话列表（带 `live: bool`；排除 archived project 和 `discoverable=0`） |
| POST | `/api/sessions` | 建会话（sid=`uuid4()[:12]`，复制模板 `template.tex`→`main.tex`，返回 `{id, frame_id, mcp_tools, model, workspace}`） |
| GET | `/api/sessions/{sid}` | 按精确 ID 取会话状态（含 `discoverable=0`；live 走内存，否则从 DB 恢复） |
| DELETE | `/api/sessions/{sid}` | 删会话（DB + workspace + MCP 清理） |

### Files
| GET | `/api/sessions/{sid}/files` | `{files:[{path,size,name}]}`（递归，排除 .venv/__pycache__/.git） |
| GET | `/api/sessions/{sid}/files/{path:path}` | 文件内容；.pdf inline；二进制 attachment；`download=true` 强制下载；**路径穿越防护**（`relative_to(workspace)` 否则 403） |
| DELETE | `/api/sessions/{sid}/files/{path:path}` | 删文件 + 清 artifact 记录 |

### Run / Approve / Compile
| POST | `/api/sessions/{sid}/run` ★ | 非流式 run，返回 `{kind, final_text, awaiting, pending_ask?, error, usage, iterations}` |
| POST | `/api/sessions/{sid}/approve` | 批准计划，返回 `{approved:true, steps}` |
| POST | `/api/sessions/{sid}/compile` ★ | `CompileReq{path, out_name?}` → Tectonic 编译，返回 `{success, pdf_path, size_kb, message, errors, log_excerpt[-3000:]}` |

### 流式
| POST | `/api/sessions/{sid}/stream-sse` ★ | `RunReq{prompt, plan_mode?, deep_review?}` → `text/event-stream`，每行 `data: {json}\n\n`，`type=complete` 结束 |
| WS | `/api/sessions/{sid}/stream` | WebSocket（可选；前端走 SSE，本项目可只实现 SSE；若实现 WS 则契约同） |

> 决策：**优先只实现 SSE**（前端 `connectSSE` 是现役路径）。WS 端点可返回 410 Gone 或暂不实现。

---

## SSE 事件格式

每个事件是一行 `data: <json>\n\n`。`<json>` 的 `type` 字段判别。字段名即本项目定义。

| `type` | 字段 | 来源 callback | 前端处理 |
|--------|------|--------------|---------|
| `start` | `frame_id, task_summary` | `on_start` | status=running |
| `iteration` | `n` | `on_iteration` | 设迭代号，重置 curRef 占位 |
| `thinking` | `text` | `on_assistant_thinking` | 累加到 curRef.thinking |
| `text` | `text` | `on_assistant_text` | 累加到 curRef.text（打字机） |
| `tool_calls` | `calls:[{id,name,input}]` | `on_tool_calls` | 按 call.id 去重追加 |
| `tool_results` | `results:[{tool_use_id,content,is_error}]` | `on_tool_results` | 挂到最近 assistant 消息 |
| `plan_update` | `plan:{steps,approved}` | `on_plan_update` | setPlan |
| `notice` | `event, detail` | `on_event` | 作为 system 消息气泡 |
| `complete` | `kind, final_text, awaiting?, pending_ask?, error?, usage, iterations, frame_status, plan?, artifacts` | manager `emit_complete` | 设 usage/plan/artifacts/awaiting；status 由 kind 推导 |
| `error` | `message` | 传输层 | status=error |

**`complete.kind` 取值**：`natural | awaiting | max_iters | error | cancelled`。
**`complete.awaiting`**：`"user_response" | "plan_approval" | null`。
**`complete.usage`**：`{input_tokens, output_tokens, …}`。

### 实现注意（易错点）

1. **流式 vs 批量文本去重**：若已发 `text` 增量事件，则 `complete` 里不要再触发批量文本回调（`_text_streamed_this_turn` 守卫）。否则前端会重复渲染。
2. **`complete` 携带全量快照**：plan + artifacts 由 manager（非 agent）在 `emit_complete` 时拼装，保证 UI 最终态一致。
3. **queue 满策略**：callbacks 的内部 queue 在满（1000）时丢弃增量事件，但**强制驱逐最旧以必达 `complete`**。Rust 的 `mpsc` 需复刻此语义（或用足够大 buffer + 优先保证 complete）。
4. **客户端取消**：SSE 连接断开 → 取消 run task（Rust 用 `CancellationToken` 或 `tokio::select!` 监听连接关闭）。

---

## 会话状态序列化（`GET /sessions/{sid}`）

```jsonc
{
  "id": "sid",
  "frame_id": "...",
  "status": "processing|completed|...",
  "task_summary": "...",
  "plan_mode": false,
  "plan": { "steps": [...], "approved": false } | null,
  "artifacts": { "path": { "path":..., "size":..., "frame_id":... } },
  "messages": [
    {
      "role": "user|assistant",
      "content": "..." | [ {block} ],   // 见下
      "harness_notice": true | null      // ★ null 表示无此键（前端按 falsy 处理）
    }
  ]
}
```

**content block 形态**（`_serialize_content`）：
- `{type:"thinking", thinking}`
- `{type:"text", text}`
- `{type:"tool_use", id, name, input}`
- `{type:"tool_result", tool_use_id, content, is_error}`

A2A 不另建一条不可恢复的 UI 数据通道。其版本化 JSON 原样存放在上述
`tool_result.content`，所以实时 SSE 和按 session id 恢复使用同一份 canonical 数据；前端只做专用呈现，未知 schema 退回通用工具卡。

### harness_notice 持久化与往返（关键）

**本项目设计**：`session_messages` 表加显式 `harness_notice BOOLEAN` 列（见 [data-model](data-model.md)），序列化时直接输出该字段，避免 content 被污染。**API 输出形态**：顶层 `harness_notice: true|null`（前端按 falsy 处理 null）。

### 前端 `loadFromState` 的两遍重建（必须兼容）

前端从 DB 恢复会话时做两遍：
1. 遍历收集所有 `tool_result` 块到 `tool_use_id → result` map。
2. 重建 UIMessage，把 result 回挂到对应 assistant 的 tool call（修复刷新后 tool call 卡在 in-progress 黄点的问题）。

**含义**：后端返回的 `tool_result` 块的 `tool_use_id` 必须与对应 `tool_use` 的 `id` 严格一致；消息顺序必须保证 assistant(tool_use) 在前、user(tool_result) 在后（或同消息内）。Rust 序列化不得打乱。

### 日志端点（本项目新增）

本项目新增日志端点。完整定义见 [logging 日志系统](logging.md#api-端点)：

| Method | Path | 用途 |
|--------|------|------|
| GET | `/api/logs` | 查日志（query: `session_id`/`source`/`level`/`kind`/`since`/`until`/`limit`/`offset`）→ `{logs:[...], total}` |
| GET | `/api/logs/{id}` | 单条详情（含完整 detail JSON） |
| DELETE | `/api/logs` | 清理（`before` 批量删 / 全清） |
| GET(WS/SSE) | `/api/logs/stream` | （可选）实时日志推送 |

---

## 版本字段

`GET /api/health` 的 `version`：本项目从 `0.1.0` 起（独立版本号）。前端不依赖此值做行为分支（仅展示）。

---

## 不在契约内的（后端内部）

- frame 树的内部表示、RC 的 projection 状态、token 计数细节——这些不暴露给前端，只在 `complete`/`usage` 聚合后体现。

---

下一步：读 [data-model 数据模型与存储](data-model.md)。
