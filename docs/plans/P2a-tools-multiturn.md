# P2a — 工具与多轮（最小闭环）

> 对应 [roadmap P2](../roadmap.md#p2--工具与多轮)。状态：**进行中**（2026-07-31）

## 背景

P1 已完整关闭（前端接通补记见 [P1 plan](P1-minimal-chat.md#前端接通补记2026-07-31主线验收)）。P2 全量交付物（全部内置工具 + 全部门控）体量很大，为避免一次铺开导致 token 耗尽（Kimi 即因此中断），把 P2 拆为：

- **P2a（本次）**：打通最小闭环——文件工具 + bash + 多轮循环 + ask_user，覆盖 roadmap 验收点。
- **P2b（后续）**：web_search/fetch_url、python 子进程、max_tokens 续传门、empty-retry 门、检索熔断。已登记 `docs/decisions.md` DEFER。

## 目标

agent 能多轮工具调用完成「写脚本算斐波那契并存文件」；ask_user 能阻塞等待用户输入。**端到端**：`POST /api/sessions` → `POST /api/sessions/{sid}/stream-sse`，SSE 流含 `tool_calls`/`tool_results` 事件，`complete` 在 ask_user 时带 `kind:"awaiting"` + `pending_ask`。

**验收点**：
1. `cargo build` 通过、无警告。
2. curl 让 agent 多轮工具调用完成「写个脚本算斐波那契并存文件」。
3. ask_user 工具阻塞：`complete{kind:"awaiting", awaiting:"user_response", pending_ask}`。
4. 前端 live 渲染工具卡片（tool_calls → tool_results 配对）+ ask_user 阻塞态。
5. P1 不回归：纯文本对话仍走 natural completion。

## 行为基线（本阶段要稳定的行为）

- SSE 事件新增 `tool_calls{calls:[{id,name,input}]}` / `tool_results{results:[{tool_use_id,content,is_error}]}`，字段名严格按 [api-contract](../api-contract.md#sse-事件格式)。
- Runner 主循环 `while iter < max_iterations`（默认 25），每轮：构 req（带 tools）→ 流式收 thinking/text/tool_call 增量 → 若有 tool_use 则执行并发回结果入历史 → continue；无 tool_use 则 natural completion 退出。
- 取消语义沿用 P1：事件 channel 关闭（send 失败）即中止，frame 置 Cancelled（不引 CancellationToken）。
- DeepSeek function-calling：流式 `delta.tool_calls[]` 按 `index` 累积 id/name/arguments 片段，Finish 时拼成完整 ToolCall。
- 工具并发执行：`JoinSet` + per-call `timeout(30s)`；异常 → `is_error=true`，错误详情进 content（modules.md §3）。
- 路径穿越防护：所有文件工具要求目标 `relative_to(workspace)` 成功，否则 `is_error`。
- 消息历史：assistant(tool_calls) 与 user(tool_result) 按 OpenAI 协议入历史（role=tool, tool_call_id 配对），跨 run 累积。
- API key 纪律不变：不打印/不落盘/不进 Debug。

## 任务清单（todo）

- [ ] 根 `Cargo.toml`：tokio 加 `process`/`fs`/`io-util` feature；workspace members 加 `crates/dss-tools`。
- [ ] `dss-llm`：`ChatMessage` 加 `tool_calls`/`tool_call_id`；`ChatRequest` 加 `tools`/`tool_choice`；`StreamEvent::ToolCallDelta`；`build_body`/`parse_sse_line` 解析 tool_calls；`LlmResponse.tool_calls`。
- [ ] 新建 `dss-tools`：`Tool` trait / `ToolRegistry` / `ToolRouter`（JoinSet+timeout）/ `ToolContext` / `ToolError`。
- [ ] `dss-tools` 内置工具：`read_file`/`write_file`/`edit_file`/`list_files`/`bash`/`ask_user`。
- [ ] `dss-agent`：`AgentEvent::ToolCalls`/`ToolResults` + Complete `pending_ask`；`Runner::run` 多轮循环。
- [ ] `dss-api`：`AppState.tools` + `build_state` 注册 6 工具 + `stream_sse` 接线。
- [ ] `cargo build` 无警告。
- [ ] curl 验收：斐波那契多轮 + ask_user 阻塞。
- [ ] 前端：store tool 累积 + WorkbenchPage 接 onToolCalls/onToolResults + ChatArea live 工具卡片 + ask_user 阻塞态。
- [ ] `bun run build` 绿；浏览器验收。
- [ ] 回填本文件「回顾」段；P2b 登记 decisions.md。

## 回归点

- `POST /api/sessions/{sid}/stream-sse` 纯文本 prompt → 仍 `start`→`iteration{n:1}`→`thinking`?→`text`→`complete{kind:"natural"}`（P1 行为不回归）。
- 工具 prompt（如「用 bash 写个算斐波那契的脚本并存成 fib.py」）→ 多个 `iteration`，期间 `tool_calls`/`tool_results` 配对，最后 `complete{kind:"natural", iterations:N}`。
- ask_user prompt → 某轮 `tool_calls` 含 `ask_user` → `tool_results` 后立即 `complete{kind:"awaiting", awaiting:"user_response", pending_ask:{...}}`。
- 路径穿越：`read_file{path:"../../etc/passwd"}` → `tool_results{is_error:true}`，不读出 workspace 外。
- 同 session 并发 run → 409；未知 sid → 404（P1 不回归）。
- 客户端断开 → run 取消，服务存活（P1 不回归）。

## 风险

- **DeepSeek tool 流式格式**：arguments 分片需按 index 累积；以真实返回调试。
- **bash 安全**：P2a 非沙箱，cwd 锁 workspace + 30s 超时 + 进程组 kill；无 venv 注入（留 P2b）。
- **历史 token 膨胀**：每轮 tool 消息累积，P2a 无 RC（P4 才做），MAX_ITERS=25 兜底。
- **token 预算**：后端先 curl 验收全部行为，前端接线放最后。

## 回顾

**实际做了什么**：
- 根 `Cargo.toml`：tokio 加 `process`/`fs`/`io-util`/`time` feature；workspace members 加 `crates/dss-tools`。
- `dss-llm`：`ChatMessage` 由纯文本扩展为支持 function-calling（`content`→`Option<String>` + `tool_calls`/`tool_call_id`/`name`，`skip_serializing_if` 抑制空字段保证 OpenAI 协议形态）；`ChatRequest` 加 `tools`/`tool_choice`；`StreamEvent::ToolCallDelta{index,id,name,arguments}`；`build_body` 注入 tools+tool_choice；`parse_sse_line` 按 `delta.tool_calls[].index` 解析增量；`LlmResponse.tool_calls` + 非流式 `parse_tool_calls`。
- 新建 `dss-tools`：`Tool` trait（`spec()` 返回 owned `ToolSpec`，因 `serde_json::Value` 非 const）、`ToolRegistry`、`ToolRouter`（`JoinSet` 并发 + `tokio::time::timeout(30s)` per call + 异常转 `is_error`）、`ToolContext`（workspace + `pending_ask`，`resolve_in_workspace` 路径穿越防护含 lexical fallback）、`ToolError`。内置工具：`read_file`/`write_file`（原子写）/`edit_file`（count=0/>1 报错）/`list_files`（同步 std::fs + spawn_blocking，避免 async 递归 Box::pin）/`bash`（sh -c，cwd=workspace，超时 kill，kill_on_drop）/`ask_user`（挂起 pending_ask）。
- `dss-agent`：`AgentEvent` 加 `ToolCalls`/`ToolResults` 变体 + Complete `pending_ask`（对齐 api-contract）；`Runner::run` 多轮循环（`while iter < MAX_ITERATIONS(25)`：流式累积 thinking/text/`tool_call_delta`(by index) → finalize → 有 tool_use 则执行并发入历史 continue，无则 natural completion 退出；ask_user 检测转 `AwaitingUserResponse` + `complete{kind:Awaiting}`；耗尽转 `MaxIters`）；取消语义沿用 send-fail→cancel。
- `dss-api`：`AppState` 加 `tools: Arc<ToolRegistry>`；`build_state` 注册 6 工具；`stream_sse` 接线 `Runner::run`（每 session 一个 `ToolContext`）。

**验证结果（后端，curl + 真实 DeepSeek，端口 17896，全部通过）**：
- 修了一个**死锁 bug**：初版 `stream_sse` 在 `try_lock_owned()` 拿到 owned lock 后又 `shared.lock().await` 二次锁同一 `tokio::Mutex`（不可重入），导致 handler 挂起、SSE 0 字节、curl 超时。改为从 owned guard 直接读 workspace。
- 斐波那契多轮（`write_file fib.py` → `bash python3 fib.py` → 报告）：3 iteration、2 对 `tool_calls`/`tool_results`、`complete{kind:"natural", usage:{input:1244,output:145}, iterations:3}`、workspace 实际生成 `fib.py`（150 bytes，内容正确，运行输出 `[0,1,1,2,3,5,8,13,21,34]`）。
- `tool_calls.calls[]`=`{id,name,input}`、`tool_results.results[]`=`{tool_use_id,content,is_error}`，字段名严格对齐契约。
- ask_user：`complete{kind:"awaiting", awaiting:"user_response", frame_status:"awaiting_user_response", pending_ask:{question,options:[机器学习,生物信息],header}}`。
- P1 回归：纯文本 → 单 iteration、无 tool_calls、`kind:"natural"`，未回归。
- 并发 run → 409；未知 sid → 404（P1 不回归）。
- `cargo build` 全 workspace 无警告；全程未打印/落盘 API key。

**前端（store/WorkbenchPage/ChatArea 接线 + 类型修正）**：
- `types.ts`：`pending_ask` 由 `string` 改为对象 `PendingAsk{question,options,header}`（对齐后端）；新增 `PendingAsk`/`PendingAskOption`。
- `store.ts`：`StreamBuffer` 加 `toolCalls`/`pendingAsk`/`kind`；新增 `appendStreamToolCall`（按 id 去重）/`appendStreamToolResult`（按 tool_use_id 配对回挂）；`commitStreamMessage` 输出 `tool_use`+`tool_result` blocks；`completeStream` 收 kind/pendingAsk，awaiting 时置会话 status=awaiting。
- `WorkbenchPage`：`handleSend` 接 `onToolCalls`/`onToolResults`，`onComplete` 传 `e.kind`/`e.pending_ask`。
- `ChatArea`：流式渲染区渲染 `ToolCallCard`（live 态，复用已有组件）；新增 `AskUserPanel`（awaiting 时展示问题+候选项）。`ToolCallCard.summarize` 扩展支持 `read_file/write_file/edit_file/list_files/bash/ask_user`。
- `bun run build`（tsc + vite）通过、无类型错误。

**偏离**：
- `ToolDef` 在 `dss-tools` 与 `dss-llm` 各有一份同构定义（两 crate 不互依），Runner 里 `to_llm_tool_defs` 做一次值转换。理由：避免 dss-tools 反向依赖 dss-llm。可接受，登记 decisions。
- 浏览器 GUI 端到端验收**未完成（IAB 自动化后端局限，非应用缺陷）**。已定位根因：ZCode IAB 的 `fill()` 直接写 DOM `.value` 但**不触发 React 受控组件的 `onChange`**，因此 `Composer` 的 `useState(value)` 不同步、保持 `''`；`submit()` 首行 `if (!value.trim() ...) return` 命中、提前返回（按钮 DOM `disabled=false` 但 React 读 state 为空）。而 IAB 的 `cua.type` / `press` / `dom_cua.keypress` 在本会话后期全部 broker-mismatch 失败，无法发可信按键。已交叉验证：`dom_cua.click` 对**普通 React 按钮**（设置/关闭，开/关 Settings 弹层）有效，证明应用本身对真实点击响应正常——只是 IAB 无法驱动受控输入框。**代码正确**（真实键盘输入会触发 onChange），后端 curl 全事件链已验证，`bun build`(tsc) 通过。建议用真实浏览器（或 cdp 后端）手动发一条工具 prompt 确认 live 工具卡片渲染。

**遗留（→ decisions.md DEFER，归入 P2b）**：
- web_search/fetch_url、python 子进程、max_tokens 续传门、empty-retry 门、检索熔断、plan 工具、delegate/submit_output、compile_pdf、记忆工具、artifacts ledger。
- 完整 content blocks（Anthropic 风格）模型：P2a 用 OpenAI 协议字段直挂 `ChatMessage`，P3 落库时再迁。
- 前端 ask_user 的「回复后继续 run」闭环（/approve 或 stream-sse 带 reply）——P2a 只做了展示与会话 awaiting 态；完整回复循环待 P3 session 恢复一起做。

