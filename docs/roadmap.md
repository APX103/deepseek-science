# 递进式开发路线图

> **本文回答**：怎么分阶段交付？每个阶段交付什么、能独立验收什么？阶段之间怎么衔接？

> 状态：框架已定，每阶段工时/具体任务在 [plans/](plans/) 细化

---

## 总策略

用户明确：**递进式开发，分好几次实现，每次 todo 记录，实现后可能依次修改**。因此路线图遵循：

1. **纵向切片**：每阶段交付一条「能跑通」的端到端路径，而非横向铺所有模块。先能对话，再补能力。
2. **先稳固核心基线，再加增强**：每阶段优先把对应核心能力打磨稳定，增强方向（[06](enhancements.md)）叠加在稳固基线上。
3. **可独立验收**：每阶段有明确的「能做什么」验收点，可暂停、可回退。
4. **不追求等量**：阶段大小按「能否独立验收」切，不强行均分。

---

## 阶段总览

| 阶段 | 目标 | 验收点 | 依赖 |
|------|------|--------|------|
| **P0 地基** | workspace + 能起后端 | `dss serve` 起来，`GET /api/health` 返回 ok | 无 |
| **P1 最小对话** | LLM 单轮对话 + SSE | 前端能发一句、收流式回复（无工具） | P0 |
| **P2 工具与多轮** | 文件工具 + 多轮 + 基础工具循环 | agent 能读写文件、bash、web_search，多轮工具调用 | P1 |
| **P3 持久化与 session** | DB + session 恢复 | 刷新会话不丢，多 session 管理，projects | P2 |
| **P4 记忆与 Compact** | 三层记忆 + Rolling Compact | 长会话不爆 token，跨会话记忆召回 | P3 |
| **P5 skills 与论文链** | skill 体系 + LaTeX 编译 + paper-writing | 跑通「写综述→PDF」全链 | P2,P3 |
| **P6 verify 与子 agent** | reviewer + terminal barrier + delegate | 多角色评审、子任务委派 | P4 |
| **P7 MCP 与 A2A** | MCP 动态挂载 + agent-registry | 挂载外部 MCP server、A2A 调用 | P2 |
| **P8 打包与整合** | Tauri 壳 + 前端整合 | 单 .app 跑通全流程 | P3+, F1 |
| **F1 前端工程** | React 前端（DeepSeek 风格） | 视觉对齐 DeepSeek，核心功能可用 | 可与后端并行，P1 后任意时点 |
| **F2 日志系统** | 日志表 + 端点 + 前端日志页 | 前端可浏览系统+agent 日志 | P4 后（agent 事件源稳定） |
| **P9+ 增强方向** | Deepseek / 沙箱 / 知识图谱 / 长程研究 / 学科插件 | 见 [06](enhancements.md)/[07](domain-plugins.md) | 各阶段 |

每个阶段的详细计划写在 [`plans/Px-*.md`](plans/)（实现期创建）。

---

## P0 — 地基

**目标**：全新仓库 + Rust workspace 骨架 + 能起一个空后端。

**交付**：
- **全新 git 仓库** `deepseek-science/`。
- Cargo workspace（[02](tech-stack.md#workspace-结构) 的 crate 划分，先建空壳 + `dss-core`/`dss-bin`/`dss-api`）。
- `clap` CLI `dss serve --port N`。
- axum 起服务，`GET /api/health` → `{status:"ok", version}`。
- 配置加载（config.toml + settings.json + env 优先级，[02](tech-stack.md#配置-serde--toml--环境变量)）。
- `tracing` 日志。
- data_dir 解析（`~/.deepseek-science`，含 SSD 软链）。

**验收**：`dss serve` 起来；`curl /api/health` ok。

**待定（本阶段登记）**：后端二进制名（[决策日志](decisions.md) D-Q01）。

## P1 — 最小对话

**目标**：前端能发一句话、收 Deepseek 流式回复，**无工具**。

**交付**：
- `dss-llm`：`LlmClient` trait + OpenAICompatClient（`chat` + `chat_stream`）+ message_adapter。
- `dss-agent` 最小版：Frame/FrameStatus/Session 骨架，Runner 主循环（**只走 natural completion 路径**，无工具分支）。
- `dss-api`：`POST /api/sessions`、`POST /api/sessions/{sid}/stream-sse`，SSE 事件 `start`/`iteration`/`thinking`/`text`/`complete`。
- 端到端：前端输入 → Deepseek 流式回复。

**验收**：在前端（指向新后端）发「你好」，看到流式回复；reasoning_content 走 thinking 流。

**行为基线**：Runner 的 natural completion 路径 + empty-retry 门（先不含 plan/verify 门）。

## P2 — 工具与多轮

**目标**：工具系统 + 多轮工具调用循环。

**交付**：
- `dss-tools`：`Tool` trait、`ToolRegistry`、`ToolRouter`（JoinSet 并发 + timeout）。
- 内置工具：`read_file`/`write_file`/`edit_file`/`list_files`、`bash`、`web_search`/`fetch_url`、`ask_user`、`boundary`、`summary_query`（占位）。
- `python` 工具：**先用最小子进程方案**（非沙箱，仅跑通），沙箱留 P9 改进。
- Runner 完整工具路径：tool_use → 执行 → 结果入历史 → 检索熔断 → submit_output 退出 → awaiting 检查。
- max_tokens 续传门、empty-retry 门、检索熔断（门控阈值与顺序严格遵循 [03](modules.md)）。

**验收**：让 agent「写个脚本算斐波那契并存文件」能多轮工具调用完成；ask_user 能阻塞等待。

## P3 — 持久化与 session

**目标**：DB + session 恢复 + projects。

**交付**：
- `dss-db`：schema（[05](data-model.md) 全表）、连接池、inline 迁移 runner。
- `SessionManager`：`MAX_ACTIVE_SESSIONS=10` LRU、`create`/`restore`/`run`、增量消息持久化、provider hot-swap。
- sessions/projects 全套端点。
- harness_notice 显式列 + API 输出兼容。
- frames 落库（按 [05](data-model.md#frames-是否落库) 决策）。

**验收**：刷新浏览器会话不丢；多 session 切换；projects CRUD；前端 `loadFromState` 两遍重建正确。

## P4 — 记忆与 Compact

**目标**：三层记忆 + Rolling Compact。

**交付**：
- `dss-memory`：store + BM25 recall + LLM extract（后台异步）。
- `dss-compact`：常量已定型、不随意改动；chunk 选择、summarizer 门控、projection、microcompact。
- Runner 接入：每轮前 `maybe_compact`、每轮末记忆 extract、用户消息触发 recall 注入。

**验收**：长会话（模拟超 context）能压缩且不丢关键信息；跨会话记忆能召回注入；compact 不 mutate 消息日志。

**风险**：Rolling Compact 最易出行为漂移，需用脚本化 mock LLM 驱动 agent 循环分支测试覆盖各分支。

## P5 — skills 与论文链

**目标**：skill 体系 + LaTeX 编译 + paper-writing 全链。

**交付**：
- `dss-skills`：5 源加载、frontmatter 解析、BM25+Jaccard+RRF 检索；内置 skills 随包（`include_dir!`）。
- `compile_pdf` 工具（Tectonic + 容错）+ `POST /compile` 端点。
- templates（4 套）。
- paper-writing 链 + 长程自主研究 skill。

**验收**：跑「写一份关于 X 的综述」，产出 main.tex + references.bib + PDF。

## P6 — verify 与子 agent

**目标**：reviewer + terminal barrier + delegate。

**交付**：
- `dss-verify`：verifier（阈值 checkpoint + terminal barrier）、收敛规则。
- `delegate`/`submit_output` 工具（深度上限 2，子工具裁剪）。
- plan 工具（`generate_plan`/`update_step_status`）+ plan denial 门 + AWAITING_PLAN_APPROVAL。

**验收**：deep_review 模式触发 reviewer；plan 模式生成计划等批准；delegate 子任务返回结构化结果。

## P7 — MCP 与 A2A

**目标**：MCP 动态挂载 + agent-registry + A2A。

**交付**：
- `dss-mcp`：streamable HTTP 客户端、MCPServerManager、动态挂载（阈值 30 切换）、mcp_skills 生成。
- `call_agent`、`registry_connect_mcp_server`、`mcp_read_resource`。
- `/api/mcp/{name}/tools` 端点。

**验收**：挂载一个外部 MCP server（如 Zhipu 搜索），agent 能调用其工具。

## P8 — 打包与整合

**目标**：Tauri 壳 + 前后端整合 + 单 .app。

**交付**：
- 全新 `src-tauri/` 壳（[02 Tauri 栈](tech-stack.md#tauri-壳技术栈全新工程)）：进程守护/端口注入/系统集成从零实现，拉起本项目 Rust 二进制。
- 构建脚本：`cargo build` 后端 + `bun run build` 前端 + `tauri build` 出 .app/.dmg。
- 可选：数据导入工具（[05 迁移](data-model.md#迁移)），支持从其它工作台的历史数据导入，非首发必需。

**验收**：`build` 出 .app，双击运行全流程（壳拉起后端、前端加载、对话可用）。

## F1 — 前端工程（可与后端并行）

**目标**：搭建 React 前端工程，DeepSeek 蓝/简约/细线条风格，核心功能可用。

**交付**（见 [10 设计系统](design-system.md)、[02 前端栈](tech-stack.md#前端技术栈全新工程)）：
- 先做 D-T06：用 devtools 实测 chat.deepseek.com 精确 token，回填 [10] 的 ⚠️ 值。
- `frontend/` 工程：Vite + React 18 + TS + Tailwind，建立 DeepSeek 设计 token（蓝色色阶、1px 边框、平面、无毛玻璃/渐变/光环），暗色默认。
- 核心组件从零编写：对话流、工作区、计划面板、设置、论文预览（TeX+PDF）、日志页（配合 F2）。
- SSE 客户端（fetch + ReadableStream）、API 客户端、类型定义。

**验收**：整体观感对齐 DeepSeek（蓝、留白、细线、平面）；明暗双模式达标；能对接后端跑通最小对话。

**特点**：纯前端工作，**可与后端 P1–P8 主线并行推进**，只需一个能跑的后端（哪怕 P1 的最小对话）做联调。

## F2 — 日志系统（前端 + 后端）

**目标**：新增前端可浏览的统一日志视图。

**交付**（见 [11 日志系统](logging.md)）：
- 后端 `dss-observability` crate：`logs` 表、`LogStore`、tracing layer（system 日志）、持久化 callback（agent 日志，与 SSE 同源）。
- `/api/logs` 端点（查/详情/删/可选 stream）。
- 前端日志页组件（列表/过滤/展开详情/跳转会话）。
- 定 D-T07：保留策略默认值。

**验收**：能浏览系统日志与 agent 执行记录；按 session/level/kind 过滤；agent 日志与 SSE 实时事件一致。

**依赖**：P4 之后（agent 事件源稳定后再持久化，避免返工）。

## P9+ — 增强方向

按 [06](enhancements.md#优先级建议) 优先级，每个增强方向独立小阶段：

- **P9 沙箱**（方向 2.1）：`Sandbox` trait，Python 子进程 + JSON-RPC，替换 P2 的临时 python 工具。
- **P10 Deepseek 深化**（方向 1）：reasoning 利用、长 context 策略、缓存。
- **P11 文献知识库**（方向 3）：向量索引、papers/citations 表、混合召回。
- **P12 长程研究**（方向 4）：research_runs 表、resume、检查点。
- **P13+ 学科插件**（[07](domain-plugins.md)）：按调研结果，每学科一小阶段。

> 这些是「增强」，可在 P8 后任意插入，也可与主线并行（若人力允许）。

---

## 阶段间衔接约定

- **每阶段开始**：在 `plans/Px-*.md` 写明：目标、任务清单（todo）、关键行为的回归点、风险。
- **每阶段结束**：更新 `plans/Px-*.md` 的「回顾」段（做了什么、偏离了什么、遗留什么）；遗留项登入 [决策日志](decisions.md)。
- **增强想法**：随时可提，但不立即实现——登记到 [决策日志](decisions.md)，在路线图排期时评估。

---

下一步：读 [09 决策日志与 TODO 记录](decisions.md)。
