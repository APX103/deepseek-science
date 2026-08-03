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
| POST | `/api/settings` | 合并保存 AppSettings（需 ≥1 启用的 provider） |

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
| GET | `/api/projects/{pid}` | 项目详情 + 会话列表 |

### Sessions
| GET | `/api/sessions` | 会话列表（带 `live: bool`） |
| POST | `/api/sessions` | 建会话（sid=`uuid4()[:12]`，复制模板 `template.tex`→`main.tex`，返回 `{id, frame_id, mcp_tools, model, workspace}`） |
| GET | `/api/sessions/{sid}` | 会话状态（live 走内存，否则从 DB 恢复） |
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
