# 模块详细设计

> **本文回答**：每个能力模块在 Rust 里怎么组织？关键 trait / 结构体 / 算法是什么？

> 状态：核心结构已定；标注「待定」处需后续细化

本文是篇幅最大的一份。每个模块按「职责 → 关键类型 → 实现注意」展开，作为本项目各 crate 的行为规范。

---

## 0. dss-core（类型与 trait 基座）

**职责**：承载所有跨 crate 共享的类型与 trait，无重依赖（叶子 crate）。

### 消息模型

本项目采用 Anthropic 风格的 content blocks，对外通过 `message_adapter` 转 OpenAI 格式。模型定义如下：

```rust
pub enum Role { System, User, Assistant }

pub enum ContentBlock {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: ToolResultContent, is_error: bool },
    Thinking { thinking: String },           // DeepSeek reasoning_content
    // 预留：Image / Document
}

pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<serde_json::Value>),
}

pub struct Message {
    pub role: Role,
    pub content: MessageContent,             // String 或 Vec<ContentBlock>，normalize 后统一为 Blocks
    // —— RC / 持久化附加字段（本项目显式字段）——
    pub uuid: Option<String>,
    pub rolling_summary: Option<RollingSummaryMeta>,
    pub server_input_tokens: Option<u32>,
    pub server_output_tokens: Option<u32>,
    pub compact_boundary: Option<bool>,
    pub task_boundary: Option<TaskBoundary>,
    pub harness_notice: bool,                // ★ 一等字段
}
```

**设计说明**：
- 本项目的 `Message` 把 `_harness_notice`/`uuid`/`rolling_summary` 等全部提升为**显式字段**，避免散落各处的 `getattr(m, "_harness_notice", False)` 式读取（这类隐式读取在记忆抽取、API 序列化、前端会话恢复中各需一份重复逻辑）。
- `harness_notice` 的语义：**LLM 可见、用户不可见**的内部调度提示。主循环在多处注入它，且 `prepare_messages_for_llm` 不过滤此类消息。

### 关键 trait

```rust
#[async_trait]  // 或原生 async fn in trait
pub trait LlmClient: Send + Sync {
    async fn chat(&self, req: ChatRequest) -> Result<LlmResponse, LlmError>;
    async fn chat_stream(&self, req: ChatRequest)
        -> Result<BoxStream<'static, StreamEvent>, LlmError>;  // 默认 Err(NotImplemented)?
    fn count_tokens(&self, text: &str) -> usize;
}

pub trait Tool: Send + Sync {
    fn spec(&self) -> &ToolSpec;             // {name, description, parameters(JSON Schema)}
    async fn call(&self, ctx: &ToolContext, args: serde_json::Value)
        -> Result<ToolOutput, ToolError>;
}

pub trait Host: Send + Sync {                // 暴露给沙箱代码
    async fn llm(&self, req: HostLlmRequest) -> Result<HostLlmResponse, HostError>;
    async fn mcp(&self, server: &str, tool: &str, args: serde_json::Value) -> Result<serde_json::Value, HostError>;
    fn artifact_path(&self, version_id: &str) -> Option<PathBuf>;
    // …lineage / query(sql) / artifacts / current_model
}
```

**实现注意**：`LlmClient` 的流式能力探测——用 trait 方法返回 `Result<BoxStream, NotImplemented>` 或单独的 `supports_streaming()` 探测，语义明确，不依赖「子类是否 override」这种隐式约定。

### 配置类型

`Settings`：`host`/`port`/`data_dir`/`models`/`rolling_compact`/`memory`/`verification`/`mcp`/`registry`/`a2a`/`api_keys`/`mcp_servers`。`AppSettings` 为前端设置面板的持久化格式。

---

## 1. dss-llm（LLM 客户端 + 消息适配）

**职责**：OpenAI 兼容协议调用、Deepseek 特化、流式聚合、消息格式互转。

### OpenAICompatClient

- `chat()`：POST `/chat/completions`，payload `{model, messages, max_tokens, temperature?, tools?, tool_choice:"auto"}`。
- 重试：429/502/503/504、连接/超时/网络错误；backoff 取 `Retry-After` 或 `backoff * 2^attempt`，封顶 60s。
- `chat_stream()`：`stream:true` + `stream_options:{include_usage:true}`，SSE 行解析，`[DONE]` 终止。**已 yield 内容后不重试**。
- `StreamAggregator`：按 `delta.tool_calls[].index` 累积工具调用参数；`finalize()` 按 Thinking → Text → ToolUse 顺序产出 blocks。

### Deepseek 特化（增强方向，见 [enhancements](enhancements.md#deepseek-深度集成)）

- **reasoning_content 流式**：把 `reasoning_content` 映射成 `ThinkingBlock` 并通过 `on_assistant_thinking` 推到前端。本项目保留此设计，并探索：reasoning 作为独立「思考流」UI、reasoning 参与记忆抽取的策略、长 context 下 reasoning-only 空响应的处理（本项目的 empty-retry 门已覆盖此场景）。
- **长上下文策略**：Deepseek 长上下文 + Rolling Compact 的协同（可能放宽 RC 触发阈值）。
- **（待调研）多 agent / 并行推理**：Deepseek 若支持并行 sampling 或多 agent 协议，在 `delegate` / `verify` 里利用。

### message_adapter

- `messages_to_openai`：system 前置；user 的 ToolResult 拆成独立 `role:"tool"` 消息；assistant 的 ToolUse 转 `tool_calls`；**Thinking 丢弃**（OpenAI 协议无标准字段）。
- `response_from_openai`：`reasoning_content`→Thinking，`content`→Text，`tool_calls`→ToolUse（arguments JSON 解析失败兜底 `{"_raw": orig}`），finish_reason 映射。

### token_counter

`estimate_tokens(text) = len(text)/4`（`CHARS_PER_TOKEN=4`）。Rolling Compact 用此廉价估计；可选接入精确 tokenizer（Rust 用 `tiktoken-rs`）。

---

## 2. dss-db（存储 + 迁移）

**职责**：SQLite schema、连接池、仓储层、inline 迁移。

### schema

本项目自有的 schema，聚焦运行时实际读写的字段，不保留当前未充分使用的列（如 token_class_usage、aux_*、specialists_used 等）。详见 [data-model 数据模型](data-model.md)。

Harness 核心表除 projection 外，还包括 `execution_frames` / `run_attempts` /
`tool_call_attempts` / `frame_mailbox` / `child_results`。Frame 树、执行 lease 与子结果均持久化；
内存 Session 只是 root Frame 的热 projection。详见 [data-model](data-model.md#10-execution_frames)
和 [ADR-001](adr-001-durable-agent-frames.md)。

### 连接与 PRAGMA

`deadpool-sqlite` 池；连接初始化设 `PRAGMA foreign_keys=ON; journal_mode=WAL; busy_timeout=5000`。写操作走 `spawn_blocking`。

### 迁移

**inline 迁移 runner**（结构化设计）：编号化的迁移步骤，每步先 `presence check`（列/索引是否存在）再 `ALTER`，失败不阻断启动、下次重试。详见 [data-model 迁移](data-model.md#迁移)。

---

## 3. dss-tools（工具系统）

**职责**：工具注册、并发执行、内置工具实现。

### 注册与路由

```rust
pub struct ToolRegistry { tools: HashMap<String, Arc<dyn Tool>> }
impl ToolRegistry {
    pub fn register(&mut self, tool: Arc<dyn Tool>);
    pub fn definitions(&self, allowed: Option<&[&str]>) -> Vec<ToolDefinition>;  // 给 LLM 的 schema 列表
}

pub struct ToolRouter { /* 持 registry + 配置 */ }
impl ToolRouter {
    pub async fn execute_tool_calls(&self, calls: Vec<ToolUseBlock>) -> Vec<ToolResultBlock>;
    // JoinSet 并发 + per-call timeout(30s) + 错误转 is_error=true
}
```

**实现注意**：并发执行用 `JoinSet`；per-call 超时用 `tokio::time::timeout(30s)`；异常→`is_error` 并把错误详情（`anyhow::Error` 的 `{:?}`）塞 content。

### 内置工具清单（本项目完整工具集）

下表列全。标 ★ 的是 Rust 实现需特别关注（涉及子进程/外部服务）：

| 工具 | 输入 | 作用 | 注意 |
|------|------|------|------|
| `python` ★ | `code` | 执行 Python | **沙箱方案待定**（[tech-stack](tech-stack.md#代码执行沙箱)）；保留「变量跨调用持久」语义 |
| `bash` ★ | `command, timeout=30` | 执行 shell | `tokio::process::Command`，cwd=workspace，PATH 注入 venv |
| `read_file` | `path, offset, limit` | 读文件；PDF 走文本抽取 | 路径穿越防护（`relative_to(workspace)`） |
| `write_file` | `path, content` | 写文件，记 artifacts ledger | — |
| `edit_file` | `path, old_string, new_string, replace_all` | 精确替换；count=0/>1 报错 | 原子写 |
| `list_files` | `path` | 列目录 | — |
| `compile_pdf` ★ | `path, out_name` | Tectonic 编译 + 容错 | 复用 `.log` 解析 + 浮动环境 `\iffalse` 重编译 |
| `search_papers` ★ | `query, max_results, year_from, sort` | OpenAlex | Bearer auth（OPENALEX_API_KEY） |
| `fetch_paper` ★ | `doi` | Crossref/OpenAlex 元数据 | — |
| `web_search` ★ | `query, max_results` | DuckDuckGo HTML 抓取 | 自建实现（非 Anthropic 服务端工具） |
| `fetch_url` ★ | `url, max_chars` | 抓取转纯文本 | Chrome UA + HTML→text |
| `read_memory`/`write_memory`/`search_memory` | entity/ops/query | 三层记忆 | 见 dss-memory |
| `generate_plan`/`update_step_status` | steps/step_id,status | 计划工具；触发 AWAITING_PLAN_APPROVAL | — |
| `ask_user` | question, options, header | 触发 AWAITING_USER_RESPONSE | 设 `pending_ask` |
| `delegate` ★ | task, context_summary, output_schema, … | 子 agent | 深度上限 2，子工具集裁剪 |
| `submit_output` | output, completion_bullets | 子 agent 结构化返回 | 设 frame context 标志 |
| `call_agent` ★ | resource_uri, task, skill_id?, timeout_seconds? | A2A | 仅经已配置的 `agent-registry` Resource；只允许一次 fresh Send，不接受 endpoint/credential/任意 task handle |
| `save_artifacts`/`get_artifact`/`list_artifacts` | files/version_id/— | 版本化产物 | 见 dss-artifacts |
| `search_skills`/`list_skills`/`skill` | query/source/name | skill 发现/加载 | 见 dss-skills |
| `install_packages` ★ | packages | venv + uv pip | workspace/.venv |
| `boundary` | label | 注入 task_boundary harness-notice | chunk 边界对齐 |
| `summary_query` | summary, question | 查折叠摘要原始消息 | 单次 LLM Q&A |
| `mcp_list_resources` | server | MCP 资源发现 | 仅在 captured runtime 有已连接且声明 Resources 的 server 时提供；server 枚举只含这些 manager key，不接受 URL |
| `mcp_read_resource` | server, uri | MCP 资源读取 | 与 list 共用 captured server 枚举，精确读取 list 返回的 URI |
| `mcp__{server}__{tool}` 或 `mcp_search`/`mcp_call` | — | 动态挂载 | 阈值 30 切换 |

### ToolContext（运行时共享态）

`ToolContext` 包含：`frame`/`frame_service`/`workspace`/`plan`/`pending_ask`/`artifacts ledger`/`venv_python`/`api_keys`/`artifact_store`/`skill_catalog`/`host`/`memory_store`/`memory_index`/`context_window`/`rolling_compact_config`/`llm`/`registry`/`mcp_manager`/`session_id`/`project_id`。

Rust 里 `ToolContext` 多为 `Arc<...>` 共享只读 + 内部可变（`Mutex`/`RwLock`）。注意 frame 状态变更需同步到 `FrameService`。

---

## 4. dss-agent（内核：Runner + Frames + Session）

**职责**：主循环、状态机、会话组装。这是整个项目的行为核心，其主循环逻辑定义了 agent 的所有决策门。

### Frame 状态机

```rust
pub enum FrameStatus {
    Processing, Completed, Failed, Success, Replaced, Cancelled,
    AwaitingPlanApproval, AwaitingUserResponse,
}
// TERMINAL = {Completed, Failed, Success, Replaced, Cancelled}
// SUCCESSFUL = {Completed, Success}
// NEEDS_INPUT = {AwaitingPlanApproval, AwaitingUserResponse}
// RUNNING = {Processing, AwaitingUserResponse}   ← 注意 AwaitingUserResponse 同时在 NEEDS_INPUT 和 RUNNING
```

**实现注意**：保留 `AwaitingUserResponse` 既在 NEEDS_INPUT 又在 RUNNING 的状态集合设计（这是有意为之的 quirk）；terminal 状态粘性，只能经 `reopen()` 逃出（`FrameService::update_status` 设守卫）。

### Runner 主循环

```
while iter < max_iterations {
    // 哨兵
    if status == Cancelled { return Cancelled }
    if status ∈ TERMINAL { return Error }
    if status == AwaitingPlanApproval {
        if deep_review { 自动批准; continue } else { return Awaiting("plan_approval") }
    }
    if status == AwaitingUserResponse { return Awaiting("user_response") }

    iter += 1; on_iteration(iter);

    // Rolling Compact（每轮 LLM 前）
    maybe_compact().await;

    // 构 prompt + 调 LLM
    let system = build_system_prompt(ctx, plan_mode, deep_review);
    let tools = registry.definitions();
    let llm_msgs = prepare_messages_for_llm();   // RC projection
    let resp = call_llm(llm_msgs, system, tools).await;
    frame.add_usage(resp.usage);

    let (should_exit, result) = process_llm_response(resp).await;
    if should_exit { return result }
}
// 循环耗尽
frame.status = Completed; return MaxIters
```

`process_llm_response` 的决策门（顺序与阈值是 agent 行为的「物理常数」，严格遵循本规范）：

1. **Refusal 门**：stop_reason=Refusal → Failed。
2. 分流 content：tool_uses / text_blocks / thinking_blocks。
3. 追加 assistant 消息；若已流式则不重复回调。
4. **max_tokens 续传**（三档：≥5 终止、≥3 大幅缩减、否则分块继续）。
5. 无 tool_use 且非 max_tokens → `_handle_natural_completion`：
   - **empty-retry 门**（thinking-only 也算空，≤3 次）。
   - **plan denial 门**（plan_mode 且无 plan，≤3 次）。
   - **deep_review output 门**（未写 .tex，≤3 次）。
   - **terminal barrier**（reviewer 最终审查，veto 则再修一轮）。
   - **clean completion**。
6. tool_use 路径：并发执行 → 结果入历史 → **检索熔断**（连续纯检索 ≥6 轮强制写作）→ submit_output 退出检查 → awaiting 检查 → reviewer checkpoint。

> 决策：**所有阈值与门顺序已定型，实现时严格遵循本文档定义，不随意改动**。这些是 agent 行为的「物理常数」，改动会引发难以定位的行为回归。任何调整（如改阈值）须单独立项 + 回归测试，并在 [decisions](decisions.md) 登记。

### harness-notice 注入点

记忆召回块、max_tokens 三档提示、检索熔断「停止搜索开始写作」、empty-retry 提示、plan denial、deep_review no-output denial、terminal barrier findings、`[boundary]` 标记——全部保留。统一用 `Message { harness_notice: true, .. }` 构造。

### Frames

- `Frame`：id / parent_frame_id / root_frame_id / agent_name(MAIN/SUBAGENT/REVIEWER/BOOKMARKER) / status / messages / context / token 计数 / task_summary。
- `FrameService`：in-memory `HashMap<String, Frame>`，`create_root_frame` / `create_child_frame` / `update_status`（terminal 守卫）/ `reopen` / `get_tree`。
- **REVIEWER/BOOKMARKER 子 frame**：本项目简化为「直接 LLM 调用」而非真 spawn 子 frame。保留 frame 类型枚举以备未来恢复原设计。

### Session 组装

`prepare()` 序列：建 workspace → 建 root frame → 建 ToolContext → ArtifactStore（+ 从 DB load 恢复）→ 加载 skill catalog（5 源：builtin→global→claude→project→custom，后覆盖前）→ 记忆系统（按 settings.memory.enabled 门控）→ Host 对象 → `register_all` → 回填 ctx → MCP 动态挂载（阈值 30 切全量/meta）。

---

## 5. dss-skills（skill 发现 + BM25 检索）

**职责**：扫描 SKILL.md、解析 frontmatter、BM25+Jaccard+RRF 检索。

- **格式**：`SKILL.md` = YAML frontmatter + markdown body。约束 `SKILL_MAX_BYTES=65536`、`DESCRIPTION_MAX=1024`、`NAME_RE=^[a-z0-9\-/]+$`。frontmatter 解析**只读顶层**（跳过缩进行，避免 `metadata:` 块遮蔽 `name`）。
- **5 源加载**：builtin（`include_dir!` 嵌入）→ global(`{data_dir}/skills`) → claude(`~/.claude/skills`) → project(`{workspace}/.deepseek-science/skills`) → custom。首跑复制 builtin 到 global（不覆盖）。
- **检索**：BM25(k1=1.2,b=0.75) + Jaccard，RRF(k=60, threshold=0.029) 融合。
- **paper-writing 链**：随包携带（lit-survey/paper-structure/academic-figures/experiment-design/peer-review + paper-writing 编排器 + 长程自主研究 skill）。
- **kernel sidecar**：`skills/*/kernel.py` 这类 sidecar **本项目不加载**，实际靠 `exec` 注入 `host` 对象。`host` 经沙箱 RPC 注入。

> 决策：**skills 的 BM25(k1=1.2) 与 memory 的 BM25(k1=1.5) 是两套独立常量，不要统一**。两者服务不同检索场景，参数各自调优。

---

## 6. dss-mcp（MCP 客户端 + 动态挂载）

**职责**：streamable HTTP JSON-RPC 客户端、服务器管理、Resources 发现/读取、工具动态注册。

- **协议**：**仅 streamable HTTP + SSE**（无 stdio）。`initialize`（protocolVersion `2024-11-05`）→ 捕获 `Mcp-Session-Id` → `notifications/initialized` → `tools/list`。
- **`MCPClient`**：`connect`/`call_tool`/`list_resources`/`read_resource`。响应解析兼容 `text/event-stream`（聚合 `data:` 取最后 result/error）与纯 JSON。
- **`MCPServerManager`**：`add_server`（idempotent，失败不抛返回 false）/ `try_add_server`（保留详细错误）/ `list_all_tools` / `call_tool` / `list_resources` / `read_resource` / `close_all`。
- **动态挂载**：普通显式 MCP server 最多挂载 30 个经过名称/schema/总量校验的工具；名称按 authority tuple 稳定化，碰撞不会覆盖既有工具。未知或可能有副作用的远端工具按独占、单次尝试处理。
- **agent-registry 注入**：未配置 `mcp_servers` 时默认注入名为 `agent-registry`、地址为 `https://a2a-dev.intern-ai.org.cn/mcp` 的 server；显式列表整体覆盖，`[]` 可关闭。该 canonical server 强制为 Resources-only，连接时不请求/挂载其 MCP Tools。运行时只有它已连接且声明 Resources 时才注册 run-local `call_agent`。工具会重新 list/read 精确 `resource_uri`，严格解析匿名 A2A descriptor，再经 `A2aClient` 与 Agent Card 交互；只允许一次 fresh Send，不接受任意 task handle，也不会调用或转发 credential endpoint。

**实现注意**：Rust 的 SSE 解析用 `reqwest::streaming` + `eventsource-stream` 或手写行解析。

---

## 7. dss-memory（三层记忆 + 召回 + 抽取）

**职责**：profile/project/frame 三层记忆，BM25 召回，LLM 抽取。

- **store**：`mem_<12hex>` id；body ≤1000 字符；profile scope 强制 `project_id=None`（跨项目共享）；其他 scope 用 project_id 隔离。
- **recall BM25**：k1=1.5, b=0.75，Okapi IDF；**CJK 每字成 token**；英文+中文停用词；project 隔离（profile 永可见）。`render_recall_block` 产 `[Memory]` 块注入。
- **extract**：每轮末单次 LLM 调用，`emit_memories({append,replace,remove})`，≤5 条/轮；跳过 harness_notice 与 tool_result；项目隔离。后台异步执行（fire-and-forget），不阻塞主循环。

> 决策：**memory 的 BM25(k1=1.5) 与 skills 的 BM25(k1=1.2) 是两套独立常量，不要统一**。

---

## 8. dss-compact（Rolling Compact）

**职责**：非破坏性上下文压缩。这是最精巧的模块，其常量已定型，实现时严格遵循本文档定义，不随意改动。

- **常量**（全部保留）：`CHARS_PER_TOKEN=4`、`KA_FLOOR=50000`、`KB_RATIO=0.7`、`MIN_CHUNK_TOKENS=4096`、`OUTPUT_CEILING=32000`、`COMPACTION_TRIGGER_RATIO=0.75`、`HARD_WALL_RATIO=0.9`、`MICROCOMPACT_RATIO=0.65`、`ABSOLUTE_TOKEN_CEILING=300000`、`PTL_RETRY_CAP=32`、`COMPRESSION_GATE_DIVISOR=3`、`DEFAULT_CONTEXT_CEILING=500000`、`DEFAULT_KA_RATIO=0.2` 等。
- **核心机制**：append-only 消息 + `applied_summary_uuids` 视图状态。**绝不 mutate 消息日志**，靠 projection 决定 drop/repositioned。
- **L1/L2 chunk**：`should_trigger_l1`（chunk ≥ MIN_CHUNK_TOKENS 且剩余 < ka*0.7）、`should_trigger_l2`（≥3 个 L1 summary 且 head tokens ≥ max(8192, ka*0.4)）、`pick_next_chunk`（失败 ≥3 次升级 L2）。
- **task_boundary 对齐**：`boundary` 工具注入的 `task_boundary` 让 chunk 边界对齐任务之间。
- **summarizer 门控**：目标 = chunk_tokens/3；≤3 次重试；退化检测（final < best*0.25 且 < degenerate_min 则回退）。
- **microcompact**：硬墙压力下截断 >8000 字符的 ToolResult 到 4000 + 提示，无 LLM 调用。

> 决策：**Rolling Compact 的常量与机制已定型，实现时严格遵循本文档定义，不随意改动**。任何「优化」想法先登记到 [decisions](decisions.md)，积累足够回归测试后再议。

---

## 9. dss-verify（reviewer + terminal barrier）

**职责**：阈值触发 checkpoint、terminal barrier 最终审查。

- 本项目采用简化设计：直接 LLM 调用，不 spawn 子 frame。
- **收敛规则**：若 `plan.research_question` 非空，verifier 强制启用（即使 config.enabled=false）。
- **reviewer 模型解析顺序**：`verification.reviewer_model` → `models.reviewer` → 主模型。
- **checkpoint**：`maybe_checkpoint` 阈值触发 → `checkpoint()` → 有 actionable findings 则推 `reviewer_checkpoint` 事件。
- **terminal barrier**：自然完成时最终审查，发现可修复问题则 veto、强制再修一轮。

---

## 10. dss-artifacts（版本化产物）

**职责**：把工作区文件提升为带版本的 artifact，多版本历史 + 依赖 DAG。

`save_artifacts` 读文件 bytes → 存 `artifact_versions`（content_type/size/checksum/storage_path）→ 更新 `artifacts.latest_version_id` → 依赖边入 `artifact_dependencies`。从 DB 恢复（resume）失败则降级为空 in-memory（非致命）。

---

## 11. dss-api（HTTP/SSE + SessionManager）

**职责**：axum 路由、SSE 流、session 生命周期。

详见 [api-contract API 契约](api-contract.md)。核心：

- **SessionManager**：`MAX_ACTIVE_SESSIONS=10` + LRU 驱逐；`ActiveSession` 跨 run 复用，每 `send` 重建 Agent，只持久化增量消息。
- **SSE**：`POST /api/sessions/{sid}/stream-sse` → `StreamingResponse`，每行 `data: {json}\n\n`，`type=complete` 结束。事件经 `tokio::sync::mpsc` 从 agent callbacks 流向 SSE handler。客户端取消 → 取消 run task。
- **provider hot-swap**：检测 settings 变更，热替换 LLM client（除非外部注入，如测试 FakeLLM）。
- **CORS**：`*`（本地桌面）。

---

## 12. dss-bin（CLI + 启动）

- `clap` 子命令：`serve --port N`（默认）、可能 `config`（打印生效配置）。
- `main`：解析配置 → 初始化 tracing → 建 DB 池 → 建 SessionManager → 启动 axum → 注册 SIGTERM 优雅关闭。

---

## 测试策略

- **FakeLLM**：按脚本返回响应，精确驱动 agent 循环每个分支（自然完成/工具调用/max_tokens 续传/空响应重试/max iters/ask_user 恢复/boundary/compile 容错/RC）。这是本项目测试 agent 行为的核心手法。
- **单元**：每个 crate 的纯逻辑（BM25、chunk 选择、message_adapter、frontmatter 解析、路径穿越防护）。
- **集成**：起 axum + 内存 DB + FakeLLM，跑完整 session。

---

下一步：读 [api-contract API 契约](api-contract.md)。
