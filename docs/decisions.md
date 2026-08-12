# 决策日志与跨阶段 TODO 记录机制

> **本文回答**：递进式开发中，决策、待办、改进想法、偏离怎么记录和追溯？

> 状态：已定（约定）

---

## 为什么需要这个

用户原话：「递进式地去开发……每次的 todo 可能需要记录，并且在实现了之后可能会有依次的修改。」

递进式开发的痛点：
1. **决策遗忘**：阶段 P2 做的取舍，到 P5 忘了为什么，容易误改。
2. **改进堆积**：实现核心基线时冒出的「这里可以更好」想法，无处安放，要么分散丢掉、要么当场改（污染稳定基线）。
3. **偏离追踪**：实际实现与当初方案的差异，不记录就成技术债。
4. **跨阶段依赖**：P4 发现的问题要 P9 才修，中间会忘。

**机制**：单一决策日志（本文）+ 阶段计划（`plans/`）+ 调研笔记（`research/`），三类文件各司其职。

---

## 三类文件

### 1. 决策日志（本文件 `decisions.md`）

**记录什么**：
- **DECISION**：已拍板的设计决策（带 `> 决策：…` 标记的，散在各文档里）。登记在此供追溯。
- **TODO**：待办，有明确归属阶段或模块。
- **DEFER**：增强/改进想法，暂不实现，攒着。**实现核心基线时冒出的「可以更好」一律先入 DEFER**，不立即改。
- **DEVIATION**：实际实现与方案的偏离（实现期才有）。
- **QUESTION**：待与用户确认的开放问题。

**格式**（每条一个条目，编号 `D-001` 起递增）：

```
### D-001 [DECISION] data_dir 路径
- 决定：~/.deepseek-science
- 理由：本地优先工作台的独立数据目录，环境变量 DSS_DATA_DIR 可覆盖
- 影响：Tauri ensure_data_dir、后端 data_dir 解析、迁移工具
- 相关：architecture.md 数据落点、data-model.md 迁移
- 状态：已定 / 待用户确认 / 已落地（P0）
```

### 2. 阶段计划（`plans/Px-*.md`）

每阶段一份。结构：
- **目标**：一句话 + 验收点。
- **行为基线**：该阶段要稳定下来的关键行为/分支。
- **任务清单**：todo（实现期勾选）。
- **回归测试**：脚本化 mock LLM 覆盖的分支。
- **风险**。
- **回顾**（阶段末填）：实际做了什么、偏离（链回本文件的 DEVIATION）、遗留（链回 TODO/DEFER）。

### 3. 调研笔记（`research/*.md`）

开放式调研（Rust crate 选型对比、Deepseek 能力、学科工具链）。结论成熟后摘要回填到对应设计文档，并在本文件登记 DECISION。

---

## 当前决策日志

> 实现期持续追加，编号不重排。状态统一使用：**已完成**（历史保留但不再是债务）、**部分完成**（已有可用基线，剩余范围明确）、**有效 TODO/DEFER**（仍未完成）。

### QUESTION 记录

#### D-Q01 后端二进制名与 CLI（已定）
- **已定**：二进制名 `dss-backend`（`dss-bin` crate 的 `[[bin]]`），CLI 子命令 `dss-backend serve --port N`（默认 17896）。
- 影响：Tauri `resolve_backend_binary()`、打包脚本、文档示例。
- 相关：architecture.md。
- 状态：已落地（P0，见 [plans/P0-foundation.md](plans/P0-foundation.md)）。

#### D-Q02 data_dir 路径名（已定）
- **已定**：`~/.deepseek-science`，环境变量 `DSS_DATA_DIR`。见 D-001/D-006。
- 相关：architecture、data-model。

#### D-Q04 代码沙箱方案（部分完成）
- **已完成基线**：bash/python 已采用受工作区约束的子进程执行；`python` 仍是每次调用新建进程的最小实现。
- **尚待定案**：方案 A（Python 长进程+JSON-RPC，倾向）/ B（PyO3）/ C（WASM）/ D（容器）/ E（可插拔），以及状态、venv/包管理和 host 注入边界。
- 状态：**部分完成，仍为 P9 有效决策项**。
- 相关：tech-stack.md、enhancements.md。

### 已记录的设计决策（DECISION，散见各文档）

#### D-001 [DECISION] data_dir 路径
- 决定：`~/.deepseek-science`，环境变量 `DSS_DATA_DIR` 可覆盖。
- 理由：本地优先工作台的独立数据目录，便于用户定位、备份、迁移。
- 来源：overview、architecture 数据落点。

#### D-002 [DECISION] 仅 SSE，WS 可不实现
- `connectStream`(WS) 不实现，只实现 `connectSSE`。WS 端点返回 410 或省略。
- 来源：api-contract.md。

#### D-003 [DECISION] harness_notice 升为显式 DB 列
- 不再折进 content JSON；API 输出形态保持兼容（顶层字段）。
- 来源：data-model.md。

#### D-004 [DECISION] Rolling Compact 常量已定型
- 所有常量/门控不随意改动；任何优化先 DEFER，攒够回归测试再议。
- 来源：modules.md。

#### D-005 [DECISION] agent 门控阈值与顺序已定型
- max_tokens 三档、empty-retry、plan denial、deep_review gate、检索熔断等，阈值与顺序严格遵循 [03 modules](modules.md)。
- 来源：modules.md。

#### D-006 [DECISION] SQLite 驱动用 rusqlite + deadpool-sqlite
- 非 sqlx/sea-orm；贴近手写 SQL 风格，编译快。
- 来源：tech-stack.md。

#### D-007 [DECISION] frames 落库（倾向）
- 因 verification_checks/compaction_archives 外键依赖，且长程研究需崩溃恢复。
- 来源：data-model.md。

#### D-008 [DECISION] 端口注入变量名用本项目自有命名
- 端口注入变量 `dss_backend_port` / `__BACKEND_PORT__`，Tauri 壳自建，命名自定。
- 来源：api-contract.md、architecture.md。

#### D-009 [DECISION] 本项目为全新原创工程
- 后端、前端、Tauri 壳全部从零设计与编写。架构、agent 循环、工具语义、SSE 事件流、API 契约、数据 schema、设计系统均为本项目自有设计。
- 视觉采用 DeepSeek 蓝（`#4D6BFE` 系）/简约/1px 细线条/平面（无毛玻璃重阴影），暗色默认。字体 Inter + JetBrains Mono。
- 来源：overview、architecture、tech-stack、design-system。

#### D-010 [DECISION] 新增日志列表功能
- 新增 `logs` 表 + `/api/logs` 端点 + 前端日志页。
- 统一两类日志：system（tracing）+ agent（复用 AgentCallbacks 事件源，与 SSE 同源不漂移）。
- 默认 level ≥ info，敏感字段脱敏，摘要截断。
- 来源：logging.md。

#### D-011 [DECISION] 前缀缓存计量 + 压缩顺序（借鉴 Reasonix）
- 背景：DeepSeek 上下文硬盘缓存自动开启，缓存命中输入价约为未命中的 1/120；agent 逐轮重发相似前缀，缓存命中率直接决定长会话成本。方案见 [research/prefix-cache-strategy.md](research/prefix-cache-strategy.md)。
- 决定：
  1. `Usage` 增加 `cache_hit_tokens`/`cache_miss_tokens`（dss-llm），`parse_usage` 解析 DeepSeek 顶层 `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens`，兼容 OpenAI `prompt_tokens_details.cached_tokens`；`complete.usage` 与前端 usage 行展示命中率。
  2. 记忆召回块从 `run_context`（system 前缀与历史之间）移到请求视图末尾：内容随查询变化，放中间会打断 DeepSeek 前缀缓存单元、让整段历史 miss；放末尾只影响本就未命中的尾部。副作用（有意）：记忆块不再进入 terminal barrier/reviewer 的 `run_context` 输入，审查更客观。
  3. 压缩顺序改为「免费减负先行、付费折叠兜底」：每轮先对视图做 microcompact（无 LLM 调用），仍超触发阈值才调 summarize 折叠；免费减负到触发线下时本轮不调 summarize。
- 未做（DEFER）：环境摘要 fingerprint 持久化进前缀、稳定记忆折进前缀、prefix-shape 缓存 miss 诊断。详见方案文档 §5 D4/D5。
- 来源：research/prefix-cache-strategy.md。

### 待办（TODO）

#### D-T01 调研：代码沙箱方案定案（部分完成）
- 已有最小子进程隔离基线；生产级方案仍未定案，尚需产出 `research/sandbox.md`。
- 状态：**部分完成，仍为有效 TODO**。归属：P9 / 方向 2.1；与 D-Q04、D-F07 联动。

#### D-T02 调研：向量索引选型（文献知识库）
- 候选：hnsw / usearch / sqlite-vss。归属：P11 / 方向 3。
- 状态：**有效 TODO（未完成）**。

#### D-T03 调研：学科插件分发形态
- 编译期 feature / 动态加载 / Python 包。归属：P13+ / [07](domain-plugins.md)。
- 状态：**有效 TODO（未完成）**。

#### D-T04 调研：DeepSeek 能力边界（部分完成）
- **已完成基线**：`reasoning_content` 流式展示、function calling、多轮 usage 与 prefix-cache 命中计量/请求前缀优化均已接入。
- **剩余范围**：并行 sampling、模型能力矩阵及更细的 function-calling 特化验证。
- 状态：**部分完成，仍为有效 TODO**。归属：P10 / 方向 1。

#### D-T05 R 语言支持评估
- 生信（DESeq2/Seurat）依赖 R。归属：沙箱（D-T01）的子项。
- 状态：**有效 TODO（未完成）**。

#### D-T06 校准：DeepSeek 设计 token 精确值 ✅ 已完成
- **产出**：2026-08-11 浏览器实测 chat.deepseek.com（深色登录页 `:root`/`.ds-input--border` computed style）+ deepseek.com（亮色首页 CTA 卡片）。回填 [design-system.md](design-system.md) 色彩 token，去掉全部 ⚠️ 标记。
- **关键校准**：亮色品牌蓝 `#4D6BFE` → `#3B82F6`；深色品牌蓝 `#5b7cff` → `#5686FE`；深色 surface `#1f1f23` → `#1B1B1C`；深色 border `rgba(255,255,255,0.08)` → `0.12`；亮色主文本 `#111827` → `#0F1115`、次文本 `#6B7280` → `#64748B`。
- **偏离说明**：实测发现 DeepSeek 官网实际用大圆角（按钮 pill/`4096px`、输入框 `28px`、卡片 `16px`），与本文档当初「不用大圆角胶囊」矛盾。本工作台**有意保留克制圆角**（更适合高密度工作台界面），在 design-system.md「线条与圆角」段注明偏离理由。仅改色彩 token，不动组件。
- 归属：前端主题阶段（已完成）。

#### D-T07 定：日志保留策略默认值 ✅ 已完成
- **已定**：按天 + 按量双限制。默认 14 天、10 万条，先到先清。
- **实现**（2026-08-11）：`dss-core::LogSettings`（retention_days/max_rows，默认 14/100_000）；`dss-db::prune_logs`（先按天 `DELETE ts < before`，再按量删最旧的到 max_rows）；`dss-observability::LogStore::prune`；`dss-api::state::spawn_retention_loop`（启动跑一次 + 每 6h 循环，顺带激活 memory retention sweep）；settings 端点 GET/POST 暴露 `log_retention_days`/`log_max_rows`；前端 SettingsModal General 加「日志保留」卡片。每次 sweep 写一条 `source=system, kind=retention_sweep` 日志。
- **未做（DEFER）**：`/api/logs/stream` 实时推送（复杂度，后置）；settings 热更新 retention loop 沿用启动快照（修改需重启后端生效，与 `log_level` 行为一致）。
- 归属：日志系统阶段（已完成）。来源：logging.md。

#### D-T08 调研：Orca（onorca.dev）竞品 ✅ 已完成
- 产出：[research/orca.md](research/orca.md)（2026-08-05 快照，17 节功能层面全梳理 + 借鉴分析）。
- 高优先级借鉴：编排五原语（Run/Task/Dispatch/Message/Gate + dispatch ID 作用域）→ P12/P5b；用量本地记账 + 阈值预警 → P10；轻量 checkpoint（状态评论非快照）→ P12。
- 中优先级：会话休眠/恢复分层回退 → P4/P12；段落级 AI 归属 → P5；定时自动化 + precheck → P12；agent 状态看板 → F2 延伸。
- 注意：Orca 活跃开发中（RC 构建 often daily），引用关键设计前回官网核对。

### 暂缓改进（DEFER）

> 实现核心基线时冒出的「可以更好」想法，先攒这儿，不立即改。

#### D-F01 统一两套 BM25 实现（有效 DEFER）
- 当前 skills（k1=1.2 + RRF）与 memory（k1=1.5 + CJK）各有一套实现。初期保持两套；统一为 DEFER。
- 状态：**有效债务（未完成）**。
- 来源：modules.md。

#### D-F02 agent 回调契约的类型化 ✅ 已完成
- **原始遗留**：Rust 内部回调使用松散 JSON，期望改为强类型、仅在边界序列化。
- **完成证据**：`dss-agent::AgentEvent` 已是带 serde tag 的强类型 enum，覆盖 start/iteration/thinking/text/tool/plan/complete/error；HTTP/SSE 边界继续序列化为既有 JSON 契约。
- 状态：**已完成，不再是 DEFER**。

#### D-F03 config 双表示统一（有效 DEFER）
- 当前有 Settings（toml/env）与 AppSettings（settings.json，前端设置面板的持久化格式）两套 + 转换胶水。Rust 可统一，但前端依赖 AppSettings 形态。DEFER：边界保持 AppSettings 形态，内部统一。
- 状态：**有效债务（未完成）**。

#### D-F04 artifacts provenance / ledger（原 P2b 延后范围，已收窄）
- **历史范围**：P2 拆分时曾包含 web/fetch、python、max_tokens/empty-retry/检索门、plan、delegate、compile、记忆和 artifacts ledger。
- **已完成**：除 artifacts provenance/ledger 外，上述工具与门控均已有实现；python 的生产级沙箱另由 D-F07 跟踪。
- **剩余范围**：定义并实现 artifacts 的来源、依赖与可追溯 ledger；完成前不对外宣称“版本化 artifacts”。
- 状态：**部分完成，债务仅保留 artifacts provenance/ledger**。门控阈值与顺序继续遵循 modules.md。
- 来源：roadmap.md P2 / plans/P2a-tools-multiturn.md 回顾。

#### D-F05 ToolDef 双定义（有效 DEFER）
- `dss-tools::ToolDef` 与 `dss-llm::ToolDef` 各有一份同构定义（两 crate 不互依）。Runner 里 `to_llm_tool_defs` 做值转换。
- DEFER：若后续 dss-tools 需要直接产出 LLM 请求，可让 dss-llm 依赖 dss-tools 或提取共享 crate。当前转换开销可忽略。
- 状态：**有效债务（未完成）**。

#### D-F06 前端 ask_user 回复闭环 ✅ 已完成
- **原始遗留**：P2a 只有 `AskUserPanel` 展示与 awaiting 态，缺少回答后继续 run。
- **完成证据**：前端按持久化 run 的 `awaiting=user_response` 识别等待原因，composer 将回答作为下一次 run 继续；若同时存在已批准未完成 plan，则保留并继续执行该 plan。`pending_ask` 与 plan 均可随 run checkpoint 恢复。
- 状态：**已完成，不再是 DEFER**。
- 来源：plans/P2a-tools-multiturn.md 回顾。

#### D-F07 python 沙箱化（部分完成；P9 / 方向 2.1）
- **已完成基线**：`python` 使用每次调用新建的子进程执行，具备最小进程隔离；无跨调用 state、无 venv。
- **剩余范围**：JSON-RPC 长进程沙箱、host 注入、变量持久、venv/uv 包管理。当前没有 `install_packages` 工具。
- 状态：**部分完成，仍为有效 DEFER**。沙箱方案见 D-Q04/D-T01。
- 来源：plans/P2b-tools.md 回顾。

#### D-F08 web_search 搜索源（部分完成）
- **原始问题**：DuckDuckGo HTML 会对本机出口返回 challenge/anomaly，单一搜索源不可用。
- **已完成基线**：DDG challenge、异常响应或请求失败时会回退到 Bing RSS，并对 feed 完整性和响应大小做校验。
- **剩余范围**：可配置的正式 API/自建搜索源与持续 live 可靠性验证；Bing RSS 只是降级路径。
- 状态：**部分完成，仍为有效 DEFER**。
- 来源：plans/P2b-tools.md 回顾。

#### D-F09 frames 不落库（有效 DEFER）
- P3 选 data-model 选项 A（frames 纯内存，session 恢复靠 `session_messages` + 重置 root frame 为 Completed）。理由：P3 无 verification/compaction，frames 表的 FK 依赖方都不存在；恢复靠消息历史即可重建可继续 run 的 Session。
- 当前虽已有 run/frame 元数据与历史 checkpoint，但仍没有完整 frames 树、verification/compaction archives 的持久化；需落实选项 B 才能崩溃恢复 frame status。
- 状态：**有效债务（未完成）**。
- 来源：plans/P3-persistence.md 回顾、data-model.md「frames 是否落库」。

#### D-F10 run 中途取消丢未持久化消息 ✅ 已完成
- **原始遗留**：P3 只在 run 结束批量写入，客户端中断可能丢失本轮已完成的消息。
- **完成证据**：Runner 在工具批次等安全边界发送 history checkpoint；API 通过 `append_history_checkpoint` 原子写消息、run/plan/pending_ask 状态，取消和终态再提交一致快照。已完成的工具证据可在取消/重载后恢复。
- 状态：**已完成，不再是 DEFER**。
- 来源：plans/P3-persistence.md 回顾。

#### D-F11 LRU 排序简化（有效 DEFER）
- SessionManager 的 LRU 驱逐（MAX_ACTIVE_SESSIONS=10）用 `HashMap.keys().next()` 取一个非当前 session 驱逐，非真正「最久未用」。
- DEFER：改 `LinkedHashMap` 或维护 last_used 时间戳做真 LRU。本地桌面 10 上限很少触发，当前简化可接受。
- 状态：**有效债务（未完成）**。
- 来源：plans/P3-persistence.md 回顾。

#### D-F12 Rolling Compact 索引版 projection（有效 DEFER）
- modules.md 的 RC 用 `applied_summary_uuids` + 带 uuid/compact_boundary 的 Message。当前 `ChatMessage`（dss-llm）是 OpenAI 协议精简态、无 uuid。
- P4a 用**索引范围 fold**（`CompactionState.folds: Vec<Fold{start_idx,end_idx,summary}>`）实现 projection：把 fold 区间替换成 assistant summary。语义等价（append-only + projection，日志不 mutate）。
- schema 已有 `sessions.compaction_state` 列，但当前没有读写路径。仍需完整 uuid/compact_boundary Message 模型、L2 fold、boundary 工具对齐及 compaction state 持久化/恢复。
- 状态：**有效债务（未完成）**。
- 来源：plans/P4a-compact.md 回顾、modules.md §8。

---

## 维护规则

1. **实现期每做一个决策/发现一个待办** → 立即在本文件追加条目（编号递增，不重排）。
2. **条目状态变化**（待定→已定→已落地）→ 更新该条目「状态」行，不删原条目。
3. **DEVIATION** 在阶段计划 `plans/Px-*.md` 的回顾段记录，并在本文件登记摘要（链回 plans）。
4. **DEFER 项的「激活」**：当某阶段计划要纳入一个 DEFER 项时，把它转为该阶段的 TODO，并在原 DEFER 条目标注「→ 激活于 Px」。
5. **定期回顾**：每阶段末通读本文件，确认无遗忘的 TODO/QUESTION。

---

## 与方案文档的交叉引用

各设计文档里的 `> 决策：…` 块都应在本文件有对应 DECISION 条目（双向链接）。实现期若改动设计文档，同步更新本文件。

---

当前实施已越过 P0；后续工作以各条目的最新状态和 `HANDOFF.md` 的优先级为准，不按历史章节位置推断完成度。
