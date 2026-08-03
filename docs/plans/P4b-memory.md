# P4b — 三层记忆 + Runner 接入

> 对应 roadmap P4 / modules.md §7。状态：进行中（2026-08-03）。L2 fold/boundary/compaction 持久化仍 DEFER（独立小项）。

## 目标
agent 跨会话/跨轮有记忆：用户消息触发 recall（BM25 召回相关记忆，注入 `[Memory]` 块）；每轮末 LLM extract 抽取记忆（后台异步、fire-and-forget）。

## 验收点
1. `cargo build` 无警告；`cargo test`（memory BM25 召回、CJK 分词、extract 解析）全绿。
2. 插入若干记忆 → `GET /api/memories` 返回；BM25 recall 按相关性返回。
3. Runner：用户消息触发 recall 注入（FakeLLM 集成测试：session 有记忆 → recall 块进 LLM 视图）。
4. 每轮末 extract（FakeLLM：返回 emit_memories JSON → store 写入）。
5. 短对话/无记忆时不回归。

## 回顾

**实际做了什么**：
- `dss-db`：memories 表迁移（data-model §4 子集字段）+ repo（append_memory/list_memories[profile+project 隔离]/delete_memory/candidate_memories）。
- 新建 `dss-memory` crate：`bm25.rs`（recall BM25 **k1=1.5/b=0.75 Okapi IDF、CJK 每字成 token、中英停用词**，独立于 skills k1=1.2）、`store.rs`（MemoryStore 经 conn.interact 持久化）、`recall.rs`（recall top-N + render_recall_block 产 `[Memory]` 块）、`extract.rs`（单次 LLM 调用、解析 `emit_memories({append,...})`、≤5 条、跳过 tool_result/空内容）、`lib.rs`。
- `dss-agent` Runner::run 加 `memory: Option<&MemoryStore>` + `project_id` 参数；用户消息前 recall 注入（harness-notice system 消息）。
- `dss-api`：AppState 加 memory（MemoryStore，build_state 建）；stream_sse run 后 fire-and-forget spawn extract（错误只 warn、不阻塞）；新增 `GET /api/memories` / `DELETE /api/memories/{id}` 端点。

**验证结果（curl + 真实 DeepSeek）**：
- `cargo test` 全 workspace **32 测试全绿**（6 memory：CJK 分词/recall 排序/extract 解析×3 + 5 gates + 12 compact + 2 compact-integ + 7 skills）；0 警告。
- **extract**：发「记住:我用的编程语言是 Rust」→ 后台 extract → `GET /api/memories` 返回「用户的编程语言是 Rust」（project scope）。✅
- **recall 注入**：新会话问「我用什么编程语言」→ session 第一条消息是 system 角色 `[Memory]` 召回块（含 Rust 记忆）+ 第二条 user 消息。✅
- 端点：GET /api/memories?project_id=、DELETE /api/memories/{id} 工作。✅

**遗留（DEFER）**：
- L2 fold / boundary / compaction state 持久化（独立小项）。
- frame scope 记忆（P4b 只 profile/project 两层）。
- replace/remove 操作（P4b 只 append）。
- harness_notice 显式标记（recall/extract 注入目前是普通 system 消息）。
- 记忆的 confidence/evidence/origin 完整字段（用默认值）。

## 改动点

### 1. `dss-db`：memories 表 + repo
- 迁移加 `memories` 表（data-model §4，P4b 子集字段：id/entity/scope/entity_type/body/project_id/confidence/created_at/updated_at/last_surfaced_at）。
- repo：`append_memory`/`list_memories(scope,project_id)`/`delete_memory`/`search_memories`（BM25 在 dss-memory 里做，repo 只提供读取全部）。

### 2. 新建 `dss-memory` crate
- `store.rs`：MemoryStore（持有 Arc<DbPool>，async append/list/delete）。
- `bm25.rs`：recall BM25 **k1=1.5, b=0.75，Okapi IDF，CJK 每字成 token**（独立于 skills 的 k1=1.2）；中英文停用词；project 隔离（profile 永可见）。
- `recall.rs`：`recall(store, query, project_id) -> Vec<Memory>` + `render_recall_block -> String`（`[Memory] ...` 块）。
- `extract.rs`：`extract(llm, model, messages, project_id) -> MemOps{append,replace,remove}`；单次 LLM 调用，解析 `emit_memories({...})`；≤5 条；跳过 harness_notice/tool_result。
- `lib.rs`：高层 API。

### 3. Runner 接入
- Session 加 `project_id: Option<String>` + `memories_enabled: bool`。
- 用户消息 push 前：`recall` 召回，把 `[Memory]` 块作为 harness-notice system 消息注入（在 user 消息前）。
- 每轮末：`tokio::spawn` fire-and-forget 调 extract（不阻塞、不 await）。

### 4. `dss-api`
- AppState 加 `memory: Arc<MemoryStore>`。
- `GET /api/memories?entity=` / `DELETE /api/memories/{id}` 端点。
- stream_sse：session 带 project_id + memories_enabled（从 settings.memory.enabled，P4b 加该配置项，默认 true）。

## 工作顺序
1. 写计划（本文件）。
2. dss-db：memories 表迁移 + repo。
3. dss-memory：store/bm25(CJK)/recall/extract + 单元测试。
4. Runner 接入 recall 注入 + extract spawn。
5. dss-api：MemoryStore + 端点。
6. cargo build/test 绿 + curl 验收。
7. 回填回顾 + 更新 HANDOFF。

## 风险
- BM25 CJK 分词：每 CJK 字符单独成 token（与 skills 不同）。
- extract 后台 fire-and-forget：错误不阻塞主循环（tracing::warn）。
- LLM extract 输出格式：要求 `emit_memories({...})`；解析容错。

## 不做（DEFER）
- L2 fold / boundary / compaction state 持久化（独立小项，后续）。
- 三层记忆的 frame scope（P4b 只做 profile/project 两层）。
- 记忆的 confidence/evidence/origin 完整字段（P4b 用默认值）。
