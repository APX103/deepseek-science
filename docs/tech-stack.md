# 技术栈选型

> **本文回答**：后端 Rust crate、前端栈、Tauri 壳各怎么选？为什么？备选是什么？

> 状态：大部分已定，标注「待定/调研」的需要后续拍板

---

## 为什么后端用 Rust

本项目定位「全新工程」：后端、前端、Tauri 壳全部从零。后端选 Rust 的论证：

| 维度 | Python（通用参照） | Rust（本项目） |
|------|---------------------|---------------|
| 启动 | 解释器启动 + 打包解包，秒级 | 原生二进制，亚秒级 |
| 内存 | GC + 全套依赖，百 MB | 无 GC，目标 <50MB 常驻 |
| 并发 | asyncio 单线程 | tokio 多线程真并行 |
| 部署 | 解释器 + 打包 | 单静态二进制 |
| 内存/线程安全 | 运行时 | 编译期 |
| 生态（科研） | 极强（pandas/sklearn…） | 弱 → 需保留受控 Python 子进程 |

**代价**：科研生态弱，代码执行沙箱必须保留 Python 子进程（科研用户离不开）。这是 [00 灰区](overview.md#灰区待定登记追踪) 的核心权衡。

---

## Workspace 结构

```text
deepseek-science/             # 全新仓库
├─ Cargo.toml                 # [workspace], resolver = "2"
├─ crates/
│  ├─ dss-core/               # 类型 + trait + 配置（叶子，无重依赖）
│  ├─ dss-llm/                # LLMClient + OpenAI/Deepseek 适配
│  ├─ dss-db/                 # SQLite schema + 仓储 + 迁移
│  ├─ dss-tools/              # 工具注册 + 路由 + 内置工具
│  ├─ dss-skills/             # skill 发现/解析/BM25 检索
│  ├─ dss-mcp/                # MCP streamable-HTTP 客户端
│  ├─ dss-memory/             # 三层记忆 + 召回 + 抽取
│  ├─ dss-compact/            # Rolling Compact
│  ├─ dss-verify/             # reviewer + terminal barrier
│  ├─ dss-artifacts/          # 版本化产物
│  ├─ dss-agent/              # Runner + Frames + Session
│  ├─ dss-api/                # axum 路由 + SessionManager
│  ├─ dss-observability/      # 日志/trace 采集（见 11）
│  └─ dss-bin/                # CLI (clap) + 启动 axum
├─ frontend/                  # 全新 React 工程（DeepSeek 风格，见下）
└─ src-tauri/                 # 全新 Tauri 壳（见下）
```

依赖方向（无环）：`bin → api → agent → {tools,skills,mcp,memory,compact,verify,artifacts,observability} → {llm,db} → core`。

---

## 逐项选型

### 异步运行时：`tokio`

- **选**：`tokio`（full feature 或按需拆 `rt-multi-thread`/`macros`/`process`/`time`/`sync`/`io-util`）。
- **理由**：事实标准；本项目需要的并发原语（并行工具、超时、channel、子进程）都有直接对应（`JoinSet` / `timeout` / `mpsc` / `Command`）。
- **备选**：`async-std`（生态远不如 tokio，淘汰）。

### HTTP 框架：`axum`

- **选**：`axum`（基于 `hyper` + `tokio`）。
- **理由**：与 tokio/tower 生态无缝；类型安全的 extractor；SSE 一等支持（`axum::response::sse`）；社区活跃。
- **备选**：
  - `actix-web`（actor 模型，性能略高但 API 风格不同，生态略孤立）。
  - `poem`（简洁但生态小）。

### 序列化：`serde` + `serde_json`

- **选**：`serde`（derive `Serialize`/`Deserialize`）。
- **理由**：Rust 序列化事实标准；与 axum/reqwest/sqlx 无缝。
- **注意**：本项目消息结构需要承载「额外字段」（如 `_harness_notice` 等 harness 透传信息）。Rust 对应：消息结构体加显式字段（推荐，见 [03](modules.md#消息模型)），或用 `#[serde(flatten)] extra: HashMap<String, Value>` 保留兜底。

### SQLite 驱动

> 待定（影响并发模型）

- **候选 A：`sqlx`**（异步，编译期 SQL 校验）
  - 优：纯异步，与 tokio 融合；`query!` 宏编译期查 schema。
  - 劣：编译慢；SQL 写死在宏里，动态性弱；WAL/PRAGMA 配置需手动。
- **候选 B：`rusqlite` + `tokio::task::spawn_blocking`**（同步，轻量）
  - 优：轻、快、API 直观；连接池用 `r2d2` 或 `deadpool-sqlite`。
  - 劣：需手动包 `spawn_blocking`，但 SQLite 本就单写，影响不大。
- **候选 C：`sea-orm`**（ORM）
  - 优：DSL 像 SQLAlchemy。
  - 劣：抽象层厚，本项目大量手写 SQL，迁移成本高。
- **倾向**：**`rusqlite` + `deadpool-sqlite`**。本项目走「手写 SQL + inline 迁移」路线，rusqlite 贴近这种风格；编译快。
- **PRAGMA**：WAL/FK/busy_timeout 在连接初始化时设。

### 配置：`serde` + `toml` + 环境变量

- **选**：`toml` crate 解析 `config.toml`；`serde` 映射到 `Settings`；环境变量手动 `std::env`（量小）或 `figment`（支持 env + toml + 优先级合并）。
- **优先级**：`env (DSS_*) > settings.json > config.toml > defaults`。

### HTTP 客户端：`reqwest`

- **选**：`reqwest`（异步，`rustls` 或 native-tls）。
- **理由**：LLM 调用、MCP、web_search/fetch_url、OpenAlex/Crossref 全用它。SOCKS 代理用 `reqwest` + socks feature。
- **注意**：流式 SSE 解析（LLM 流式 + MCP SSE 响应）需手写 SSE 行解析（读 `data:` 行 + 识别 `[DONE]`）。

### CLI：`clap`

- **选**：`clap` v4（derive）。
- **用法**：`dss serve --port N`。

### 日志/trace：`tracing` + `tracing-subscriber`

- **选**：`tracing`（span/event）+ `tracing-subscriber`（fmt/json 输出）。
- **理由**：天然支持 span 嵌套，比自建 recorder 省事。
- **trace 落盘**（可选）：`tracing-appender` 写 `<data_dir>/trace/{session}/{trace}.jsonl`。

### 错误：`thiserror`（库内）+ `anyhow`（bin 内）

- **选**：各 crate 用 `thiserror` 定义 `Error` 枚举；`dss-bin` 边界可用 `anyhow` 聚合。

### 日期/时间：`chrono` 或 `time`

- **选**：`chrono`（生态熟）；UTC 存储，序列化 RFC3339。

### UUID / 随机：`uuid` + `rand`

- **选**：`uuid`（v4，session/frame/artifact id）。

### 异步 trait：原生 `async fn` in trait（Rust 1.75+）或 `async-trait`

- **选**：优先原生 `async fn in trait`（已稳定）；需要 dyn dispatch 时局部用 `async-trait`。
- **关键 trait**：`LLMClient`（`chat`/`chat_stream`/`count_tokens`）、`Tool`（`call`）、`Host`（暴露给沙箱代码）。

### LaTeX 编译：外部 `tectonic` 二进制

- **选**：不嵌 Rust LaTeX 引擎，沿用 Tectonic 外部二进制。
- **理由**：Tectonic 自包含、自动拉 CTAN 宏包，自己重写不现实。
- **容错**：本项目自行实现 `.log` 解析 + 浮动环境 `\iffalse..\fi` 重编译逻辑。

### 文件嵌入（模板/skills 随包）：`include_dir`

- **选**：`include_dir`（编译期把 `templates/`、内置 `skills/` 嵌进二进制）。
- **理由**：模板与内置 skills 随二进制分发，无需外部资源目录。

---

## 代码执行沙箱

> 调研中（最大不确定项，阻塞 [06 实验数据分析](enhancements.md#实验与数据分析) 的设计）

科研用户需要可执行的代码（Python 为主）。直接做进程内 `exec()` 的问题：无真正隔离、变量全局共享、`sys.path` 改动有副作用。我们希望引入真沙箱。

候选方案：

| 方案 | 隔离度 | 科研生态 | 复杂度 | 备注 |
|------|--------|---------|--------|------|
| **A. Python 子进程 + JSON-RPC** | 中（进程级） | 完整 | 中 | 拉起长期 Python 子进程，stdin/stdout JSON-RPC；`host` 作为 RPC 端点暴露。Jupyter kernel 协议可参考。 |
| **B. PyO3 嵌入 CPython** | 低（同进程） | 完整 | 中高 | Rust 进程内嵌 Python 解释器；隔离差但调用快。 |
| **C. WASM（pyodide / wasmtime）** | 高 | 受限（纯 Python 包） | 高 | 真沙箱，但 numpy/pandas/torch 等原生扩展支持差，**不适合科研**。 |
| **D. 容器（Docker / 沙箱执行器）** | 高 | 完整 | 高 | 最强隔离，但本地桌面依赖重（需 Docker），**不适合默认形态**。 |
| **E. 多方案可插拔** | — | — | 中高 | 定义 `Sandbox` trait，默认 A，高级用户可选 D。 |

**倾向**：**方案 A（Python 子进程 + JSON-RPC）为默认**，并定义 `Sandbox` trait 让 D/E 可扩展。理由：科研生态完整 + 进程级隔离 + 可控资源（CPU/内存/超时）+ `host` 注入语义自然映射（host 变成 RPC handler）。

**待调研项**（登记到 [research/](../research/)）：
- Jupyter kernel 协议 vs 自定义 JSON-RPC 的取舍。
- 子进程的 venv 管理（倾向 `uv venv`）。
- R 语言支持（部分学科需要）。
- 资源限制（cgroups / `setrlimit` / `nix`）。

---

## 备选/暂不引入

- **ORM（sea-orm/diesel）**：本项目手写 SQL 居多，引入 ORM 反增摩擦。`rusqlite` 足够。
- **消息队列 / Redis**：单进程单机，`tokio::sync::mpsc` 足够。
- **gRPC / tonic**：前端只认 HTTP+SSE，不需要。
- **模板引擎**：skill/模板是文件，不需 Rust 模板引擎。

---

## 前端技术栈（全新工程）

> 前端从零搭建，视觉用 DeepSeek 风格（见 [10 设计系统](design-system.md)）。

| 能力 | 选型 | 理由 |
|------|------|------|
| 框架 | **React 18 + TypeScript** | 生态成熟 |
| 构建 | **Vite** | 快 |
| 包管理/运行时 | **bun** | 快 |
| 路由 | **react-router** | 多页（对话/日志/设置）需要 |
| 样式 | **Tailwind CSS** + CSS 变量 | DeepSeek token 体系用 CSS 变量驱动（见 [10](design-system.md)），Tailwind 做原子样式 |
| Markdown | **react-markdown** + remark-gfm + remark-math + rehype-katex | 对话与论文正文渲染 |
| 数学 | **KaTeX** | LaTeX 公式；论文 TeX 预览 |
| PDF 渲染 | **pdfjs-dist** | 论文 PDF 模式（Tectonic 编译后渲染） |
| 3D 分子 | **3dmol** | 化学结构可视化（学科扩展） |
| 状态 | React 内置（useState/useReducer + Context） | 不引入 Redux/Zustand，保持轻量 |
| HTTP/SSE 客户端 | fetch + ReadableStream（手写 SSE 解析） | 无需额外库；SSE 按 `data:\n\n` 分帧 |
| Tauri 桥 | **@tauri-apps/api** v2 | invoke 命令、窗口控制 |
| 测试 | **Playwright** | e2e |

**设计原则**：组件结构覆盖对话流 / 工作区 / 计划 / 论文预览 / 设置 / 日志，代码独立编写、视觉完全 DeepSeek 风格。

---

## Tauri 壳技术栈（全新工程）

> 壳从零实现，职责为进程守护 / 端口注入 / Finder / 更新检查 / traffic light 等。

| 能力 | 选型 | 理由 |
|------|------|------|
| 框架 | **Tauri 2** | 跨平台桌面壳主流方案 |
| 异步 | tokio（rt-multi-thread/process/time/sync） | 进程管理 + 信号 |
| 子进程组清理 | nix（signal/process）+ `setpgid`/`killpg` | 进程组 SIGTERM→SIGKILL 清理 |
| 更新检查 | ureq（轻量 HTTP，查 GitHub release `latest.json`） | 查本项目仓库 |
| macOS 集成 | window-vibrancy + objc2（traffic light 定位/毛玻璃） | macOS 原生观感 |
| 序列化 | serde / serde_json | 配置/命令参数 |

**invoke 命令**（本项目自定）：`restart_backend` / `check_update` / `open_in_file_manager` / `open_external_url`。按需可加。

---

## 版本与工具链

- **Rust 版本**：稳定通道，最低支持版本（MSRV）随 tokio/axum 走，预计 1.75+（async fn in trait 稳定线）。
- **Edition 2021**。
- **Lint**：`clippy` + `rustfmt`，CI 强制。
- **测试**：`cargo test`（后端单元+集成）；前端 `Playwright`（e2e）。后端用 `FakeLLM` 模式做 agent 循环分支测试，保证主循环各分支可回归。

---

下一步：读 [03 模块详细设计](modules.md) 看每个 crate 的内部结构。
