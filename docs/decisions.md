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

## 当前决策日志（初始）

> 实现期持续追加。编号不重排。

### 待用户确认（QUESTION，高优先）

#### D-Q01 后端二进制名与 CLI（已定）
- **已定**：二进制名 `dss-backend`（`dss-bin` crate 的 `[[bin]]`），CLI 子命令 `dss-backend serve --port N`（默认 17896）。
- 影响：Tauri `resolve_backend_binary()`、打包脚本、文档示例。
- 相关：architecture.md。
- 状态：已落地（P0，见 [plans/P0-foundation.md](plans/P0-foundation.md)）。

#### D-Q02 data_dir 路径名（已定）
- **已定**：`~/.deepseek-science`，环境变量 `DSS_DATA_DIR`。见 D-001/D-006。
- 相关：architecture、data-model。

#### D-Q04 代码沙箱方案
- 方案 A（Python 子进程+JSON-RPC，倾向）/ B（PyO3）/ C（WASM）/ D（容器）/ E（可插拔）。
- 阻塞 P9 与方向 2.1。需调研。
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

#### D-T01 调研：代码沙箱方案定案
- 归属：P9 / 方向 2.1。产出 `research/sandbox.md`。

#### D-T02 调研：向量索引选型（文献知识库）
- 候选：hnsw / usearch / sqlite-vss。归属：P11 / 方向 3。

#### D-T03 调研：学科插件分发形态
- 编译期 feature / 动态加载 / Python 包。归属：P13+ / [07](domain-plugins.md)。

#### D-T04 调研：Deepseek 能力边界
- 并行 sampling、context caching、function calling 特化。归属：P10 / 方向 1。

#### D-T05 R 语言支持评估
- 生信（DESeq2/Seurat）依赖 R。归属：沙箱（D-T01）的子项。

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

#### D-F01 统一两套 BM25 实现
- 当前 skills（k1=1.2 + RRF）与 memory（k1=1.5 + CJK）各有一套实现。初期保持两套；统一为 DEFER。
- 来源：modules.md。

#### D-F02 agent 回调契约的类型化
- SSE 事件目前是序列化 JSON；Rust 内部可用强类型 enum。但为契约兼容，序列化仍走 JSON。DEFER：内部用 enum，边界序列化。

#### D-F03 config 双表示统一
- 当前有 Settings（toml/env）与 AppSettings（settings.json，前端设置面板的持久化格式）两套 + 转换胶水。Rust 可统一，但前端依赖 AppSettings 形态。DEFER：边界保持 AppSettings 形态，内部统一。

#### D-F04 P2b 范围（P2 拆分的延后项）
- P2（工具与多轮）已拆为 P2a（已完成）与 P2b。P2b 待做：web_search/fetch_url、python 子进程（最小方案，沙箱留 P9）、max_tokens 续传门、empty-retry 门、检索熔断、plan 工具、delegate/submit_output、compile_pdf、记忆工具、artifacts ledger。
- 门控阈值与顺序严格遵循 modules.md（不随意改动）。
- 来源：roadmap.md P2 / plans/P2a-tools-multiturn.md 回顾。

#### D-F05 ToolDef 双定义
- `dss-tools::ToolDef` 与 `dss-llm::ToolDef` 各有一份同构定义（两 crate 不互依）。Runner 里 `to_llm_tool_defs` 做值转换。
- DEFER：若后续 dss-tools 需要直接产出 LLM 请求，可让 dss-llm 依赖 dss-tools 或提取共享 crate。当前转换开销可忽略。

#### D-F06 前端 ask_user 回复闭环
- P2a 前端只做了 ask_user 的展示（`AskUserPanel`）与会话 awaiting 态；「用户回复后继续 run」的完整闭环（需 /approve 或 stream-sse 带 reply 参数）待 P3（session 恢复 + 多轮 awaiting 恢复）一起做。
- 来源：plans/P2a-tools-multiturn.md 回顾。

#### D-F07 python 沙箱化（P9 / 方向 2.1）
- P2b-tools 的 `python` 工具用最小子进程方案（`python3 -c`，每调用一个新进程，无 state 持久、无 venv）。
- DEFER 到 P9/方向 2.1：JSON-RPC 长进程沙箱 + host 注入、变量跨调用持久、venv/uv pip 管理、`install_packages` 工具。沙箱方案选型见 tech-stack.md（倾向方案 A）。
- 来源：plans/P2b-tools.md 回顾。

#### D-F08 web_search 搜索源
- P2b-tools 的 `web_search` 依赖 DuckDuckGo HTML 端点抓取。**本机出口 IP 被 DDG 反爬拦截**（返回 anomaly 页），实际不可用。
- DEFER：换可配 API 的搜索源（Brave Search API / SerpAPI / 自建 SearXNG），或经代理换出口。`parse_ddg` 的朴素解析逻辑届时按新源的 HTML/API 形态重写。
- 来源：plans/P2b-tools.md 回顾。

#### D-F09 frames 不落库（P3）
- P3 选 data-model 选项 A（frames 纯内存，session 恢复靠 `session_messages` + 重置 root frame 为 Completed）。理由：P3 无 verification/compaction，frames 表的 FK 依赖方都不存在；恢复靠消息历史即可重建可继续 run 的 Session。
- DEFER 到 P6（verification）/ P4（compaction）：那时 `verification_checks`/`compaction_archives` 引用 `frames.id`，需落 frames 表（选项 B），让 FK 有效 + 崩溃恢复 frame status。
- 来源：plans/P3-persistence.md 回顾、data-model.md「frames 是否落库」。

#### D-F10 run 中途取消丢未持久化消息
- P3 持久化在 run **结束**批量写 `session.messages[persisted_count..]`。客户端中途断开（cancel 语义）→ run 未到结束 → 本轮已 push 的部分消息不入库（下次恢复看不到这一轮）。
- 与 P1 cancel 语义一致（断开即中止）。DEFER：如需中途增量持久化，改成每轮末写库（与门控/P2b-gates 一起做时评估）。
- 来源：plans/P3-persistence.md 回顾。

#### D-F11 LRU 排序简化
- SessionManager 的 LRU 驱逐（MAX_ACTIVE_SESSIONS=10）用 `HashMap.keys().next()` 取一个非当前 session 驱逐，非真正「最久未用」。
- DEFER：改 `LinkedHashMap` 或维护 last_used 时间戳做真 LRU。本地桌面 10 上限很少触发，当前简化可接受。
- 来源：plans/P3-persistence.md 回顾。

#### D-F12 Rolling Compact 索引版 projection（P4a）
- modules.md 的 RC 用 `applied_summary_uuids` + 带 uuid/compact_boundary 的 Message。当前 `ChatMessage`（dss-llm）是 OpenAI 协议精简态、无 uuid。
- P4a 用**索引范围 fold**（`CompactionState.folds: Vec<Fold{start_idx,end_idx,summary}>`）实现 projection：把 fold 区间替换成 assistant summary。语义等价（append-only + projection，日志不 mutate）。
- DEFER 到 P4b：完整 uuid/compact_boundary Message 模型迁移 + L2 fold 实现（跨多个已 fold 区间的 head 段压缩）+ boundary 工具对齐 + compaction state 持久化。
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

至此，规划文档集完成。下一步是等你确认上面 **QUESTION（D-Q01、D-Q04）** 与 [06](enhancements.md#优先级建议) 的优先级，然后即可进入 P0 实施（写第一份 `plans/P0-*.md`）。
