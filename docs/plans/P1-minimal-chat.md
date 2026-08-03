# P1 — 最小对话（实施计划）

> 对应 [roadmap P1](../roadmap.md#p1--最小对话)。状态：**已完成**（2026-07-31）

## 目标

前端能发一句话、收 DeepSeek 流式回复，**无工具**。端到端：`POST /api/sessions` 建会话 → `POST /api/sessions/{sid}/stream-sse` 收 SSE 流（start/iteration/thinking/text/complete）。

**验收点**：
- `cargo build` 通过、无警告。
- curl 建会话 → stream-sse 收到按契约的 SSE 事件流，`complete` 含 `kind/usage/iterations/frame_status`。
- DeepSeek `reasoning_content` 增量映射为 `thinking` 事件。
- 无 API key 时返回明确错误（complete kind=error），进程不 panic。

## 行为基线（本阶段要稳定的行为）

- SSE 每行 `data: {json}\n\n`，事件 `type` 判别；`type=complete` 结束流。
- `complete.kind`：`natural|awaiting|max_iters|error|cancelled`（P1 只会出现 natural/error/cancelled）。
- Runner 只走 natural completion 路径：prompt → LLM 流式 → complete，单 iteration，无工具分支。
- agent run 是独立 tokio task，事件经 `tokio::sync::mpsc` 流向 SSE handler；客户端断开 → receiver drop → sender 失败 → run 中止（取消语义）。
- API key 优先级：`DEEPSEEK_API_KEY` env > settings.json `llm` 节；key 不进日志、不进 Debug 输出（手动 redact）。

## 任务清单（todo）

- [x] `dss-llm`：`LlmClient` trait（chat + chat_stream，dyn 兼容）+ `OpenAICompatClient`（reqwest，stream_options include_usage，手写 SSE 行解析，`[DONE]` 终止，`reasoning_content`→Thinking 增量）
- [x] `dss-agent`：FrameStatus/Frame/Session 骨架 + `AgentEvent`（serde tag=type）+ Runner natural completion
- [x] `dss-core`：Settings 增加 `llm` 节（base_url 默认 https://api.deepseek.com，model/api_key 可配，env 优先，Debug redact key）
- [x] `dss-api`：`POST /api/sessions`（sid=uuid4()[:12]，建 workspace，返回 {id,frame_id,model,workspace}）、`GET /api/sessions`、`POST /api/sessions/{sid}/stream-sse`、`GET /api/config`（{llm_configured,model,base_url}）；SessionManager 内存态
- [x] cargo build 无警告
- [x] 实测：key 从 `~/.deepseek/config.toml` 提取（未打印/未落盘），curl 全流程；model 实测 **deepseek-v4-pro 可用**（默认配置即该值）
- [x] 无 key 错误路径验证
- [x] 回填本文件「回顾」段

## 回归点

- `POST /api/sessions` → `{id, frame_id, model, workspace}`，且 `data_dir/workspaces/{sid}` 目录存在。
- `POST /api/sessions/{sid}/stream-sse` `{prompt}` → 事件序：`start` → `iteration{n:1}` → （可选 `thinking` 增量）→ `text` 增量 → `complete{kind:"natural", usage:{input_tokens,output_tokens}, iterations:1, frame_status:"completed"}`。
- 未知 sid → 404 JSON 错误。
- unset `DEEPSEEK_API_KEY` → stream-sse 返回 `complete{kind:"error"}` 且错误信息可读，服务继续存活。
- `GET /api/health` 仍 ok（P0 不回归）。

## 风险

- DeepSeek 实际可用 model 名不确定（config.toml 里 `default_text_model=deepseek-v4-pro` 可能不存在）→ 依次试 deepseek-v4-pro / deepseek-chat / deepseek-reasoner，以实测为准并记录。
- API key 泄露风险：全程不 echo、不写文件、不进日志；Settings Debug 手动 redact。
- reqwest 首次引入依赖树较大，编译慢属正常。
- axum SSE 需要 `tokio-stream` 的 ReceiverStream 适配（新依赖，稳定版）。

## 回顾

**实际做了什么**：
- `dss-llm`：`LlmClient` trait（async-trait dyn 兼容；`chat` + `chat_stream` 返回 `BoxStream<Result<StreamEvent>>`）+ `OpenAICompatClient`（reqwest/rustls，connect_timeout 15s，api_key 手动 Debug redact）。SSE 解析手写：按字节找 `\n` 切行（避免 UTF-8 字符跨 chunk 截断），`data:` 行解析，`[DONE]` 终止；`reasoning_content`→Thinking、`content`→Text、末包 usage→Usage。`chat()` 非流式也可用。
- `dss-agent`：FrameStatus 全量枚举（terminal 粘性守卫已就位）、Frame（root 构造）、Session（含 messages 历史，跨 run 累积）、`AgentEvent`（serde `tag="type"`，snake_case）、`Runner::run_natural`（start → iteration → thinking/text 增量 → complete；send 失败即客户端断开 → Cancelled）。
- `dss-core`：`LlmSettings`（base_url 默认 `https://api.deepseek.com`，model 默认 `deepseek-chat`）；env `DEEPSEEK_API_KEY`/`DSS_LLM_BASE_URL`/`DSS_LLM_MODEL` 覆盖配置文件；Debug redact key。
- `dss-api`：四个端点齐备。同会话并发 run 经 `try_lock_owned` 返 409；未知 sid 404；SSE keep-alive 15s；无 key 时 `start` + `complete{kind:"error"}` 明确报错。

**验证结果**（全部通过，验证端口 17897，见「偏离」）：
- `POST /api/sessions` → `{id:"aa897170efd5", frame_id, model, workspace}`，`workspaces/{sid}` 目录创建。
- stream-sse `{prompt:"你好，用一句话介绍你自己"}`：138 个事件，`start` → `iteration{n:1}` → 47 个 `thinking` 增量（deepseek-v4-pro 的 reasoning_content 生效）→ `text` 增量 → `complete{kind:"natural", final_text:"你好！我是DeepSeek…", usage:{input_tokens:10, output_tokens:137}, iterations:1, frame_status:"completed"}`。
- **可用 model：`deepseek-v4-pro`**（用户 `~/.deepseek/config.toml` 的 `default_text_model`，一次通过，未试 deepseek-chat/reasoner）。
- 同一 session 第二次 run `input_tokens=61` > 首次 10，确认会话历史跨 run 累积。
- 未知 sid → 404 `{error}`；run 进行中重复 stream-sse → 409。
- unset key 重启：`llm_configured=false` 警告日志，stream-sse 返回 `complete{kind:"error", error:"LLM not configured: set DEEPSEEK_API_KEY env or settings.json llm.api_key"}`，服务继续存活、health 正常。
- 全程未打印/落盘 API key；SIGTERM 优雅退出；验证后进程已杀、测试 workspace 已清理。

**偏离**：
- 验证用端口 **17897** 而非 17896：17896 被前端并行代理的 node dev server（PID 40446）占用，不动对方进程。默认端口仍是 17896。
- `complete` 事件未含契约全量字段 `plan?/artifacts`（P1 无 plan/artifact 概念，按 roadmap 只要求 start/iteration/thinking/text/complete；P2+ 补齐）。
- 取消语义用「send 失败即取消」实现（receiver drop 传导），未引入 CancellationToken——效果等价，依赖更少。

**遗留**：
- LLM 调用无重试（modules.md 要求 429/5xx backoff + 「已 yield 不重试」）→ P2 连同工具路径一起做。
- `GET /api/config` 只返回契约字段子集（llm_configured/model/base_url）；context_window/has_mcp 等随对应模块落地。
- SessionManager 无 LRU 驱逐（MAX_ACTIVE_SESSIONS=10）、无 DELETE /sessions/{sid} → P3 落库时一并。
- 消息历史是纯文本 ChatMessage（无 content blocks/message_adapter）→ P2 引入 ToolUse/ToolResult 时按 modules.md 改造。

---

## 前端接通补记（2026-07-31，主线验收）

前端 SSE 接通由子代理完成（`connectSSE` fetch+ReadableStream、流式 thinking 折叠块 + 打字机、usage 行、停止键、离线禁用；联调用 `frontend/scripts/mock-backend.mjs` 模拟后端先行验证）。

**端到端验收（真实后端 + 真实 DeepSeek，主线亲测）**：
- 起 `dss-backend serve --port 17896`（env 注入 key，未打印）+ `bun run dev`（5173）。
- 浏览器 +New → 真实 sid `bbc536ea4f85`；发「你好，用一句话介绍你自己」→ 流式收到回复「你好！我是DeepSeek，由深度求索公司创造的AI助手…」，complete 后 usage 行显示 `tokens: 10 in / 25 out · 1 iteration`，侧栏标题同步为消息文本。
- **P1 验收点（前端发一句、收 DeepSeek 流式回复）通过，P1 关闭。**
