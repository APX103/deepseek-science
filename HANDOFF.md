# 交接说明（HANDOFF）

> 写给接手 Agent：本文是当前进度、环境事实、剩余 TODO 与工作规则的唯一入口。先读完本文，再按「下一步」行动。
> 更新日期：2026-08-04（P0–P8 + F2 + P5b 已实现；本轮修复 plan 持久化、harness_notice 落库、前端 tool_result 映射等）。

---

## 1. 项目是什么

**Deepseek Science**：本地优先的科研 AI 工作台。三层全部从零实现：Rust 后端（agent 内核 + HTTP/SSE）+ React 前端（DeepSeek 视觉风格）+ Tauri 桌面壳。以 DeepSeek 系列模型为主力推理引擎。

**先读文档（按序）**：
1. `README.md`（文档导航）
2. `docs/roadmap.md`（分阶段交付计划，一切行动按此）
3. `docs/architecture.md`（分层/crate 划分/进程模型）
4. `docs/api-contract.md`（HTTP/SSE 契约，前后端都以此为准）
5. `docs/design-system.md`（前端视觉规范：DeepSeek 蓝 #4D6BFE、1px 细边框、几乎无阴影、圆角克制）
6. 做某阶段前再读：`docs/modules.md`、`docs/data-model.md`、`docs/tech-stack.md`、`docs/logging.md` 对应章节

## 2. 当前进度（全部已实测验证）

| 阶段 | 状态 | 证据/说明 |
|------|------|----------|
| F1 前端 v1 | ✅ 完成 | `frontend/`：首页（项目列表+New Project 弹窗）、工作台三栏（侧栏可拖拽 200–360px / 右栏 360–760px、tab 可关完、artifact 开 tab 预览、Files 视图弹层预览、+New 空态会话）、⌘K 弹层、Skills 弹层（16 个 skill）、PDF 预览弹层。`bun run build` 通过，浏览器逐项自验过 |
| F1 收尾 | ✅ 完成 | 日志页 `/logs`（过滤/展开/跳转会话/清理）、Settings 弹层（LLM/MCP/General）、store localStorage 持久化（`dss_*` keys） |
| P0 后端地基 | ✅ 完成 | Cargo workspace：`crates/dss-core`（Settings/Error/paths）、`dss-api`（axum 0.8）、`dss-bin`（bin 名 `dss-backend`，clap `serve --port`）。`curl 127.0.0.1:17896/api/health` → `{"status":"ok","version":"0.1.0"}` 实测通过；SIGTERM 优雅退出。计划：`docs/plans/P0-foundation.md` |
| P1 后端最小对话 | ✅ 完成 | 新增 `dss-llm`（LlmClient trait + OpenAICompatClient，手写 SSE 解析，reasoning_content→thinking）、`dss-agent`（Frame/Session/Runner 最小 natural completion）。端点：`POST /api/sessions`、`GET /api/sessions`、`POST /api/sessions/{sid}/stream-sse`、`GET /api/config`。实测：`deepseek-v4-pro` 流式 138 事件（thinking 47 + text + complete 带 usage），无 key 时明确报错不 panic。计划：`docs/plans/P1-minimal-chat.md` |
| P1 前端接通 SSE | ✅ 完成 | `connectSSE`（fetch+ReadableStream）、ChatArea 流式渲染（thinking 折叠块+打字机+usage+停止键+离线禁用）。端到端亲测通过（真实 DeepSeek）。见 P1 plan「前端接通补记」。 |
| **P2a 工具与多轮（最小闭环）** | ✅ 完成 | 新增 `dss-tools`（Tool trait/Registry/Router+并发+30s 超时；read/write/edit/list/bash/ask_user，路径穿越防护）；`dss-llm` 接通 function-calling（tools/tool_choice/ToolCallDelta 流式）；`dss-agent` Runner 多轮循环（tool_use→执行→入历史→continue；ask_user 转 Awaiting；MAX_ITERATIONS=25）；`dss-api` 接线 ToolRegistry。**curl+真实 DeepSeek 实测全通过**：斐波那契多轮（write_file→bash，3 iteration，生成 fib.py）、ask_user 阻塞（complete kind=awaiting+pending_ask）、P1 纯文本不回归。前端接线完成（store tool 累积+ChatArea live ToolCallCard+AskUserPanel），`bun build` 通过；**前端 GUI 端到端未自动验收**（IAB 自动化无法驱动 React 受控输入，根因已确诊、代码正确，建议真实浏览器手动发一条工具 prompt 复核）。计划：`docs/plans/P2a-tools-multiturn.md` |
| **P2b-tools（web/python 工具）** | ✅ 完成 | `dss-tools` 加 web_search（DDG HTML 抓取+朴素解析）/fetch_url（reqwest+自写 html_to_text）/python（最小子进程）。**curl 实测**：python（2^10=1024）、fetch_url（example.com HTML→纯文本）全通过、P2a 回归通过。web_search 代码正确但 **DDG 在本机出口 IP 被反爬拦截**（返回 anomaly 页），换出口/搜索源可恢复（见 D-F08）。计划：`docs/plans/P2b-tools.md` |
| **P3 持久化与 session** | ✅ 完成 | 新增 `dss-db`（rusqlite+deadpool-sqlite，自定义 Pool post_create 设 WAL/FK/busy_timeout；inline 迁移；schema P3 子集 projects/sessions/session_messages；仓储层 CRUD）。`dss-api` 接 Pool：create/list/get/delete session、stream_sse 增量持久化、**从 DB 恢复（重启不丢）**、LRU(MAX_ACTIVE_SESSIONS=10)、projects 全套端点（默认项目不可 archive/delete）。**curl 实测全通过**：斐波那契多轮→重启后端→GET 恢复 6 条消息（含 tool_calls/tool_result 块、tool_use_id 一致、harness_notice 顶层字段）；projects CRUD 全套；delete session；live flag 往返；P2 回归。计划：`docs/plans/P3-persistence.md` |
| **P4a Rolling Compact** | ✅ 完成 | 新增 `dss-compact`（常量全保留一字不改；tokens 估算；CompactionState + projection 非破坏性；chunk L1/L2 触发判断 + pick_next_chunk；microcompact 截 tool result；summarizer 门控；`maybe_compact` 主入口）。`dss-agent` Session 加 `compaction` 字段、Runner 每轮 `maybe_compact`+projection+microcompact 构视图。**14 测试全绿**（12 单元 + 2 FakeLLM 集成：fold+压缩+append-only / 短对话不调 LLM）；P3 短对话不回归（不触发 compact）。计划：`docs/plans/P4a-compact.md` |
| **前端接 P3（wireup）** | ✅ 完成 | `api/client.ts` projects/sessions/health/config 全切真实 fetch + 后端行→前端类型映射；`store.ts` 去 mock、从后端 `loadFromBackend`/`loadMessages` 恢复；HomePage/WorkbenchPage 接真实数据；后端 SessionListItem 补时间字段。**浏览器实测**：Home 显示真实 proj_default+会话、进会话恢复历史消息、`NaNd ago` 修复。计划：`docs/plans/frontend-p3-wireup.md` |
| **P2b-gates（Runner 决策门）** | ✅ 完成 | Session 加 GateState；Runner 捕获 finish_reason；max_tokens 续传门（三档 ≥3 缩减/≥5 终止）、empty-retry 门（≤3）、检索熔断（连续纯检索 ≥6 注入写作 notice）。**FakeLLM 流式集成测试 5 个全绿**（natural/empty-fail/empty-recover/max_tokens-cap/retrieval-breaker）；短对话不回归。计划：`docs/plans/P2b-gates.md` |
| **P5a（skills + compile_pdf）** | ✅ 完成 | 新增 `dss-skills`（frontmatter 解析+只读顶层、BM25 k1=1.2+Jaccard+RRF 检索、builtin include_dir!+global+project 三源加载+首跑 seed、内置 paper-writing/lit-survey）；`dss-tools` 加 compile_pdf（Tectonic）+ search_skills/list_skills/skill 工具、ToolContext 加 skill_catalog；`dss-api` 加 POST /compile 端点 + AppState catalog。**curl 实测**：compile main.tex→main.pdf(8KB)、search_skills 返回 paper-writing/lit-survey。**26 测试全绿**（7 skills）。计划：`docs/plans/P5a-skills-compile.md` |
| **P4b（三层记忆）** | ✅ 完成 | 新增 `dss-memory`（BM25 recall k1=1.5+Okapi IDF+CJK 每字成 token、MemoryStore、render_recall_block、extract 解析 emit_memories）；`dss-db` 加 memories 表+repo；Runner 用户消息前 recall 注入；`dss-api` 后台 fire-and-forget extract + GET/DELETE /api/memories。**curl 实测**：发事实→后台 extract 存记忆；新会话触发 recall 注入 `[Memory]` 块。**32 测试全绿**（6 memory）。计划：`docs/plans/P4b-memory.md` |
| **F2（日志系统）** | ✅ 完成 | 新增 `dss-observability`（LogStore）；`dss-db` 加 logs 表+repo；`dss-api` build_state 写 system startup 日志、stream_sse 写 agent run_start/run_end 日志、GET/DELETE /api/logs + /api/logs/{id} 端点；前端 listLogs/getLog/clearLogs 切真实。**curl 实测**：startup + agent run_start/run_end 日志可见、按 session 过滤。计划：`docs/plans/F2-logging.md` |
| **P7（MCP）** | ✅ 完成 | 新增 `dss-mcp`（streamable HTTP+SSE JSON-RPC client：initialize/notifications/initialized/tools/list/tools/call，SSE 聚合解析；MCPServerManager add/list/call）；`dss-tools` McpDynamicTool + 动态挂载 mcp__{server}__{tool}；`dss-core` Settings 加 mcp_servers；`dss-api` 启动连接+挂载、GET /api/mcp/{name}/tools。**集成测试**：内嵌 MCP echo server 全流程；**37 测试全绿**（5 mcp）。计划：`docs/plans/P7-mcp.md` |
| **P6a（plan 工具）** | ✅ 部分 | `generate_plan`/`update_step_status` 工具 + ToolContext.plan + PlanState；Runner plan_mode 接入（plan 检测转 AwaitingPlanApproval + PlanUpdate 事件 + plan denial 门≤3）；dss-api 传 plan_mode。**curl 实测**：plan_mode 生成计划→plan_update→awaiting plan_approval。37 测试全绿。审批闭环（/approve）+ verify/delegate 留 P6b。计划：`docs/plans/P6a-plan.md` |
| **P6（完整）** | ✅ 完成（本轮加固） | P6a plan + P6b：plan 审批闭环（POST /approve）+ delegate/submit_output + verify（terminal barrier reviewer，veto ≤1）。**本轮修复**：plan 状态跨 run 持久化到 `sessions.plan_data`；approve 后写入 approved 标志并注入计划上下文。**curl 全验证**：approve→200→continue、delegate 子任务返回结构化结果、terminal barrier 正常对话 pass。**40 测试全绿**（3 verify）。 |
| **P8 Tauri 壳** | ✅ 完成 | `src-tauri/`（独立 workspace）：main.rs 找端口→spawn dss-backend→轮询 health→注入端口→关窗杀进程；tauri.conf.json + 图标。**cargo build 通过**；`cargo tauri build` 出 .app 待用户执行。计划：`docs/plans/P8-tauri.md` |
| **P5b 论文编排链** | ✅ 完成 | 充实 paper-writing skill（完整 6 步编排：clarify→survey→bib→tex→compile→report，含 LaTeX 模板+bibtex 格式+编译容错）。**端到端实测**：agent 找 skill→写 references.bib（10 条真实引用）→写 main.tex（完整论文结构）→Tectonic 编译→main.pdf（42KB）编译无错。 |
| **GUI 测试方案** | ✅ 文档 | `docs/plans/gui-test-guide.md`：10 个前端测试点（T1-T10），含操作/预期/截图检查/排查办法。供能读图的 agent（CodeX）或人工执行。 |
| skills/templates 真实端点 | ✅ 完成 | GET /api/skills、/api/templates、/api/templates/{id}（dss-api/meta.rs，include_dir!）；前端 listSkills/listTemplates/getTemplate 切真实。curl 实测真实数据。 |

**roadmap 主线已完成**：P0-P8 + F2 + P5b + P2b-gates + 前端接 P3。**40 测试全绿**。剩余收尾项：
- 前端仍有 Files/Artifacts/Compile/runOnce 走 mock（后端端点已存在，前端未接线）。
- Settings/MCP 列表前端仍用 localStorage（后端无对应管理端点）。
- `cargo tauri build` 出完整 .app/.dmg 需用户本地执行。
- CodeX/人工执行 GUI 测试（`docs/plans/gui-test-guide.md`）。

## 3. 环境事实（不要重新踩坑）

- **Rust 不在默认 PATH**：每次 shell 先 `export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`（cargo 1.96.0）。
- **前端**：bun 1.3.14（`cd frontend && bun run dev` → 5173，`/api` 已代理到 17896）；node v24 也有。
- **tectonic** 已在 `/opt/homebrew/bin/tectonic`（P5 用）。
- **DeepSeek API key**：在 `~/.deepseek/config.toml` 的 `api_key` 字段，`default_text_model = "deepseek-v4-pro"`。**纪律：严禁打印/落盘/写入项目文件**。后端读取方式：env `DEEPSEEK_API_KEY`，可用 `export DEEPSEEK_API_KEY=$(grep -E '^api_key' ~/.deepseek/config.toml | sed -E 's/.*= *"([^"]+)".*/\1/')` 注入（不要 echo）。
- **端口**：后端默认 17896；曾因被占用了 17897 验证——起服务前先 `lsof -i :17896` 或直接 curl 确认占用者是谁。
- **数据目录**：`~/.deepseek-science`（P0 首跑已建空骨架）。注意 `~/.deepseek` 是别的工具的目录，**不要动**。
- **已知无害项**：前端 console 有 React Router future-flag 警告（原有）；浏览器 localStorage 有测试残留的 `dss_*` keys（要纯净态就清掉）。
- **git**：项目已 `git init` 并有多条提交；是否 push 由用户决定。

## 4. 下一步（按优先级）

**主线已实现到 P8**；后续是收尾/加固，不再按 roadmap 顺序推进：

1. **前端 mock 清理**：Files/Artifacts/Compile/runOnce 接真实后端（`listFiles/readFile`、`/compile`、`stream-sse` 已存在，前端未调用）。
2. **Settings/MCP 管理端点**（可选）：若需要前端与后端共享 settings/MCP server 配置，需补后端 CRUD 端点并替换 localStorage。
3. **Tauri 打包验证**：执行 `cargo tauri build`，确认 .app/.dmg 能正常拉起后端+前端。
4. **GUI 人工测试**：按 `docs/plans/gui-test-guide.md` T1-T10 逐项验证，重点检查多轮工具、会话恢复、plan 审批、PDF 预览。
5. **P9+ 增强方向**：沙箱化 bash/python、Deepseek 深度集成、文献知识库、学科插件等，按 `docs/roadmap.md` 与 `docs/enhancements.md` 排期。

## 5. 工作规则（必须遵守）

1. **每阶段开工**：先读 roadmap 对应小节 + 相关设计文档；在 `docs/plans/Px-*.md` 写计划（目标/todo/回归点/风险）。
2. **每阶段收尾**：亲自跑验收点（不是「应该能跑」——要看到输出）；把结果与偏离填进 plan 文档的「回顾」段；遗留项登 `docs/decisions.md`。
3. **契约一致性**：前后端字段/事件名严格按 `docs/api-contract.md`；要改契约必须前后端同改并更新该文档。
4. **视觉一致性**：前端改动遵守 `docs/design-system.md`（1px 细边、无重阴影、无胶囊圆角、亮色默认+暗色可切）。
5. **最小改动**：不顺手重构、不引未论证的依赖（选型依据 `docs/tech-stack.md`）。
6. **不做 git 操作**（commit/init/push 等），除非用户明确要求。
7. **并行分工**：前端（frontend/）与后端（crates/）可并行委派子代理；同一目录不并行。子代理任务必须写清：目标、要读的文件路径、验收命令。
8. **安全纪律**：不打印 API key；不访问项目目录外的文件（`~/.deepseek/config.toml` 取 key 是唯一例外，且只提取不展示）；不起后台进程忘了杀。
9. **token 意识**：大任务拆阶段委派子代理执行，主线只做分派与验收。
