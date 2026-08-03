# 整体架构

> **本文回答**：Deepseek Science 怎么分层？进程怎么跑？后端内部怎么组织？和前端/Tauri 的边界在哪？

> 状态：已定（三层：后端 / 前端 / Tauri 壳，全部从零实现）

---

## 分层总览

```
┌─────────────────────────────────────────────────────────────┐
│  Tauri 桌面壳 (全新实现, Rust)                                │
│  - 拉起/守护/清理后端二进制进程（原生 Rust 二进制）             │
│  - 注入后端端口到 webview                                       │
│  - 系统集成: Finder 定位、更新检查、traffic light              │
├─────────────────────────────────────────────────────────────┤
│  前端 (全新实现, React + TS + Vite, DeepSeek 风格)             │
│  - SSE 事件流 → 对话/工具/计划/工作区/日志 UI                   │
│  - 论文预览: TeX(KaTeX) + PDF(PDF.js) 双模                    │
│  - 设置: LLM Provider / MCP / 学术 API / 模板选择              │
├──────────────────── HTTP + SSE ──────────────────────────────┤
│  ★ 后端 (全新实现, Rust)                                       │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │  API 层 (axum)                                            │ │
│  │  /api/sessions/{sid}/stream-sse  流式运行                 │ │
│  │  /api/sessions/{sid}/compile      LaTeX→PDF              │ │
│  │  /api/templates /files /health /settings /projects /logs │ │
│  ├─────────────────────────────────────────────────────────┤ │
│  │  Agent 内核                                                │ │
│  │  Runner: 主循环 (调 LLM → 工具 → 验证门控)                │ │
│  │  Frames: 主/子 frame, 状态机                              │ │
│  │  Session: 工具注册 + 工作区 + 生命周期                    │ │
│  ├─────────────────────────────────────────────────────────┤ │
│  │  能力层                                                    │ │
│  │  tools/ skills/ memory/ compact/ verify/                 │ │
│  │  citations/ artifacts/ templates/ llm/ mcp/ observability │ │
│  ├─────────────────────────────────────────────────────────┤ │
│  │  存储 (SQLite via rusqlite + deadpool-sqlite)             │ │
│  │  会话/消息/记忆/artifact/验证/compaction archive/logs     │ │
│  └─────────────────────────────────────────────────────────┘ │
├─────────────────────────────────────────────────────────────┤
│  外部进程（按需拉起）                                          │
│  - Tectonic（LaTeX 编译，外部二进制）                          │
│  - Python/R 子进程（代码执行沙箱，方案待定）                    │
│  - MCP server（HTTP JSON-RPC）                                │
└─────────────────────────────────────────────────────────────┘
```

**设计说明**：本项目的三层拓扑——Tauri 壳守护原生后端进程、前端经 HTTP+SSE 与后端通信、后端内部按 API / Agent 内核 / 能力层 / 存储分层——是经过论证的合理结构，三层全部从零实现。

## 进程模型

### 三个进程层级

1. **Tauri 主进程**（全新 Rust）：桌面壳。职责——找空闲端口、spawn 后端、注入端口、关窗时杀进程组，全部从零编写。
2. **后端进程**（全新 Rust）：单一原生二进制，内含 HTTP 服务 + agent 内核 + 所有能力层。不打包 Python 运行时。
3. **外部子进程**（按需）：Tectonic、代码执行沙箱、（可选）Python/R runtime。后端用 `tokio::process` 拉起并管理生命周期。

### 启动时序

```
Tauri 启动
  → find_free_port(17896)                      // 端口探测
  → spawn "dss-backend serve --port <port>"
       env: DSS_DATA_DIR                       // 本项目环境变量
       setpgid + kill_on_drop                  // 进程组清理
  → inject_port_and_navigate()                 // localStorage + window.__BACKEND_PORT__
  → 前端轮询 /api/health，online 后加载
```

> 决策：**后端二进制名 `dss-backend`，CLI `dss serve`**。端口通过 webview 注入变量 `dss_backend_port` 暴露给前端。详见 Tauri 壳的进程拉起逻辑。

> 决策：**数据目录用 `~/.deepseek-science`**，符合独立工程原则。环境变量用本项目自有的 `DSS_DATA_DIR`。

### 关窗清理

实现 `killpg(SIGTERM) → 500ms grace → killpg(SIGKILL) → child.kill()`（进程组清理模式）。Rust 后端本身收到 SIGTERM 时应：中止进行中的 agent run、flush 日志、优雅关闭 DB。

## 后端内部分层（Rust workspace 设计）

> 状态：待定（crate 划分在 [02 技术栈](tech-stack.md#workspace-结构) 细化）

建议按「能力域」拆 crate，避免单 crate 膨胀，也方便单元测试与未来按需裁剪。初步划分：

```
deepseek-science/                  # workspace root
├─ Cargo.toml                      # [workspace]
├─ crates/
│  ├─ dss-core/                    # 类型定义、错误、配置、 trait 抽象（无重依赖）
│  │   # Message/Block/Frame/PlanState/ToolContext trait/Settings…
│  ├─ dss-llm/                     # LLMClient trait + OpenAI/Deepseek 实现 + 消息适配
│  ├─ dss-db/                      # SQLite schema + 仓储层 + 迁移
│  ├─ dss-tools/                   # 工具注册、路由、内置工具
│  ├─ dss-skills/                  # skill 发现、解析、BM25 检索
│  ├─ dss-mcp/                     # MCP streamable-HTTP 客户端 + 动态挂载
│  ├─ dss-memory/                  # 三层记忆 + BM25 召回 + 抽取
│  ├─ dss-compact/                 # Rolling Compact（chunk/summarizer/projection）
│  ├─ dss-verify/                  # reviewer 子系统、terminal barrier
│  ├─ dss-artifacts/               # 版本化产物存储
│  ├─ dss-agent/                   # Runner 主循环 + Frames + Session 组装
│  ├─ dss-api/                     # axum HTTP/SSE 路由 + SessionManager
│  ├─ dss-observability/           # 日志/trace 采集 + logs 表（见 11）
│  └─ dss-bin/                     # main.rs，CLI(clap) + 启动 axum
├─ frontend/                       # 全新 React 工程（DeepSeek 风格）
└─ src-tauri/                      # 全新 Tauri 壳
```

**依赖方向**：`dss-bin` → `dss-api` → `dss-agent` → `{tools, skills, mcp, memory, compact, verify, artifacts, observability}` → `{llm, db}` → `dss-core`。`dss-core` 是叶子，只放 trait + 类型，保证可被所有 crate 引用而不引入循环。

**设计说明**：本项目用 Rust workspace 拆 crate 以获得编译并行、测试隔离、明示依赖边界。`frontend/` 与 `src-tauri/` 是**本项目自有工程**（从零搭建）。

## 与前端/Tauri 的边界

### 后端 ↔ 前端：HTTP + SSE（本项目自定契约）

- **所有交互走 `/api/*` REST + `/api/sessions/{sid}/stream-sse` SSE**。前端不感知后端语言。
- 端点路径、请求/响应 JSON、SSE 事件类型与字段，**按本项目需要自行定义**。前后端都在本项目内，契约可协同演进。
- 契约设计详见 [04 API 契约](api-contract.md)。

### 后端 ↔ Tauri：仅「二进制路径 + 端口 + 信号」

- Tauri 只需要知道：后端二进制在哪、监听哪个端口、怎么杀。这三点由本项目壳自行实现。
- Tauri 的 invoke 命令（`restart_backend` / `check_update` / `open_in_file_manager` / `open_external_url`）在本项目自建壳里实现，`check_update` 指向本项目仓库。

### 后端 ↔ 外部进程

- **Tectonic**：`tokio::process::Command`，容错逻辑（解析 `.log`、包裹浮动环境 `\iffalse..\fi` 重编译）由本项目自行实现。
- **代码沙箱**：方案待定（见 [02](tech-stack.md#代码执行沙箱)）。无论哪种，都通过后端统一管理（拉起、超时、资源限制、回收）。
- **MCP server**：HTTP JSON-RPC 客户端（streamable HTTP + SSE，**无 stdio**），Rust 用 `reqwest` 实现。

## 并发模型（关键设计）

本项目用 **tokio 多线程 runtime**：

- **工具并行执行**：用 `tokio::task::JoinSet` + `tokio::time::timeout` 实现工具调用的真并行与超时控制。
- **SSE 流与 agent run 解耦**：agent run 是独立 task，事件经 channel 发给 SSE handler（`tokio::sync::mpsc`）。多个 session 可真并行运行。
- **阻塞操作 off-load**：文件 IO、子进程、SQLite（若用同步驱动）放 `tokio::task::spawn_blocking`，避免阻塞 reactor。

> 决策：**SQLite 驱动选型**影响并发模型——`sqlx`（异步）vs `rusqlite`（同步，需 `spawn_blocking`）。见 [02](tech-stack.md#sqlite-驱动)。

## 数据落点

| 数据 | 位置（本项目 `~/.deepseek-science`） |
|------|--------------------------------|
| 结构化数据 | `<data_dir>/dss.db`（SQLite，WAL，**全新 schema**，见 [05](data-model.md)） |
| 工作区文件 | `<data_dir>/workspaces/{sid}/` |
| 设置 | `<data_dir>/settings.json`（格式**自行设计**，对应 `Settings` 结构） |
| skills | 内置 + `<data_dir>/skills/` + `~/.claude/skills/` + 工作区 |
| templates | 内置（打包进二进制 via `include_dir!`） |
| 日志/trace | `<data_dir>/logs/`、`<data_dir>/trace/`（见 [11 日志系统](logging.md)） |

**SSD 软链**：本项目壳实现「若 `/Volumes/ssd/main_link/.deepseek-science` 存在则软链过去」的逻辑，便于开发机本地数据落 SSD。

## 错误与可观测性

- **错误**：用 `thiserror` 定义领域错误枚举，API 层转 HTTP 状态码 + JSON `{error}`。避免 anyhow 泄漏到 API 边界。
- **日志**：`tracing` + `tracing-subscriber`，结构化日志，级别可配；并经 `dss-observability` 落 `logs` 表供前端日志页消费（见 [11](logging.md)）。
- **trace**（可选）：记录 `llm_call`/`tool_call`/`tool_result` span，落 JSONL，默认关闭。Rust 用 `tracing` 的 span 天然适配。

---

下一步：读 [02 技术栈选型](tech-stack.md) 看 crate 级别的选型论证。
