# 增强方向设计预留

> **本文回答**：除了稳固的核心基线，本项目在四大方向上想强化什么？每个方向的设计空间、关键技术问题、对核心架构的预留要求是什么？

> 状态：方向已定，具体方案多在「调研/待定」。本文是**设计预留**——确保核心架构不被锁死，后续可平滑叠加。

四大方向（用户确认）：
1. **Deepseek 深度集成**
2. **实验与数据分析**
3. **文献与知识图谱**
4. **长程自主研究**

外加用户特别强调的**跨学科特殊数据处理与可视化**，单列 [07](domain-plugins.md)。

---

## 总原则：先稳固基线，再叠加增强

每个增强方向的实现，**前提是先把对应核心能力打磨到稳定并通过回归测试**。增强是在稳固基线上的叠加，不是绕过基线。所有增强想法登记到 [决策日志](decisions.md)，避免提前侵入核心、污染基线。

---

## 1. Deepseek 深度集成

### 基线能力
- Deepseek 作为 OpenAI 兼容 provider 之一接入（`base_url` + `api_key` + `model`）。
- `reasoning_content` 已映射为 `ThinkingBlock` 并流式推前端。
- empty-retry 门已处理「reasoning-only 空响应」bug（长 context 下模型只输出 reasoning 就停）。

### 增强空间

#### 1.1 reasoning_content 的深度利用
- **独立思考流 UI**：把 reasoning 与正文分离呈现（前端可能需小改，或通过现有 `thinking` 事件已够）。调研 Deepseek 不同模型的 reasoning 行为差异。
- **reasoning 参与决策**：是否让 reasoning 内容进入记忆抽取（当前记忆抽取跳过 thinking）。可能从 reasoning 里提取「为什么这么做」的元信息。
- **reasoning 与 Rolling Compact**：thinking 块占 token，RC 压缩时如何对待（保留最近、折叠旧的）。

#### 1.2 长上下文策略
- Deepseek 长上下文（128k/256k+）下，Rolling Compact 的触发阈值可动态调整（`context_window` 配置已支持，但 RC 常量是固定的，见 [03](modules.md#8-dss-compactrolling-compact)）。
- **调研**：长 context 下模型注意力衰减，是否仍需较早压缩以保质量（而非只为省 token）。

#### 1.3 多模型/多 agent 编排（待调研 Deepseek 能力）
- Deepseek 是否支持并行 sampling、内部多 agent、或特定的 tool-use 优化。
- 若支持，在 `delegate`（子 agent）和 `verify`（reviewer）里利用——例如 reviewer 用更强模型、worker 并行多路探索。
- **预留**：`LLMClient` trait 已抽象，多 provider/多模型路由天然支持（`models.large/small/medium/reviewer` tier 已是雏形）。

#### 1.4 Deepseek 专属能力适配（待调研）
- Deepseek 的 function calling 格式是否有特化（与 OpenAI 标准的差异）。
- 缓存（prompt caching）：Deepseek 若支持 context caching，在 system prompt + 长历史的 prefix cache 上优化成本/延迟。`TokenUsage` 已有 `cache_read/write_tokens` 字段。
- **预留**：`LlmResponse.usage` 保留 cache 字段；`LlmClient` 可扩展 `cached_prefix` 提示。

### 对核心架构的预留要求
- ✅ `LLMClient` trait 抽象，Deepseek 作为特化实现。
- ✅ `TokenUsage` 含 cache 字段。
- ✅ ThinkingBlock 一等公民。
- 待定：多 agent 编排需要 `delegate`/`Session` 支持更灵活的模型路由（当前子 agent 已能指定 `model`）。

---

## 2. 实验与数据分析

### 基线能力
- `python`/`bash` 工具（进程内 exec，沙箱弱）、`install_packages`（uv venv）、`save_artifacts`（版本化产物）、artifact DAG。

### 增强空间

#### 2.1 真沙箱代码执行（最高优先，阻塞其他）
- 进程内 `exec()` 共享命名空间是明显缺口（[02](tech-stack.md#代码执行沙箱)）。
- **方向**：`Sandbox` trait，默认 Python 子进程 + JSON-RPC（方案 A），`host` 对象作为 RPC 端点。
- **调研项**（登记 [research/](../research/)）：
  - Jupyter kernel 协议 vs 自定义 JSON-RPC。
  - 变量跨调用持久（进程内命名空间语义）如何在子进程映射 → 子进程长存 + 命名空间持久。
  - 资源限制：CPU 时间、内存、磁盘、网络（`setrlimit` / `nix` / cgroups）。
  - R 语言支持（统计/生信学科需要）。

#### 2.2 数据可视化产物
- agent 生成的图表（matplotlib/plotly）作为 artifact 版本化存储，前端预览。
- **现状**：前端 `ArtifactPreview` 已支持 png/jpg/csv/json，规划中的分子可视化组件支持 3D 结构。
- **增强**：交互式图表（plotly/vega）的渲染（前端可能需加组件）；图表与数据的谱系关联（artifact_dependencies 已支持 DAG）。

#### 2.3 可复现实验记录
- 把「数据 + 代码 + 参数 + 环境 + 结果」绑定为可复现的实验单元。
- **现状**：`artifact_versions` 有 `environment_snapshot`/`lineage_messages`/`dependency_mappings` 列（精简 schema 时**保留这几列**）。
- **增强**：自动捕获执行环境（pip freeze / 要求文件）、参数化重跑、结果对比。
- 关联长程自主研究（方向 4）：实验循环的状态文件化。

#### 2.4 数据处理工具链
- 常用数据操作的内置工具或 skill：CSV 探索、统计摘要、清洗、拟合。
- 调研：哪些是高频到值得做成一等工具（而非靠 agent 写 Python）。

### 对核心架构的预留要求
- ✅ `Sandbox` trait 抽象，`ToolContext.venv_python` / `host` 已预留。
- ✅ artifact schema 保留 `environment_snapshot`/`lineage_messages`/`dependency_mappings`。
- 待定：沙箱方案定案前，`python`/`bash` 工具的接口可能调整（但对外 tool schema 保持兼容）。

---

## 3. 文献与知识图谱

### 基线能力
- `search_papers`（OpenAlex）、`fetch_paper`（DOI）、引用图扩展工具、DOI 校验、文献综述 skill（LQS 分级评分）、`references.bib` 产出。

### 增强空间

#### 3.1 本地知识库与 RAG
- 把检索到的论文（元数据 + 摘要 + 全文）沉淀到本地，支持语义检索（向量）+ 关键词（BM25）混合召回。
- **现状**：记忆系统是 BM25，无向量索引。
- **增强**：
  - 向量索引（embedding）：选型 `hnsw` 纯 Rust / `usearch` / sqlite-vss 扩展。embedding 来源：Deepseek embedding API 或本地模型。
  - 混合召回：BM25 + 向量 RRF 融合（skill 检索已是 BM25+Jaccard+RRF，可推广）。
- **新表**：`papers`（doi/title/abstract/...）、`paper_embeddings`（向量）、`citations`（图边）。见 [05](data-model.md) 可扩展。

#### 3.2 引用网络分析
- 从种子论文出发扩展引用图（向前/向后），识别关键节点、聚类、演进脉络。
- **现状**：有引用扩展工具但图谱不持久。
- **增强**：`citations` 表持久化图；图算法（中心性、社区检测）——纯 Rust `petgraph` 库。

#### 3.3 知识沉淀与跨会话复用
- 检索过的论文、提取的 claim、建立的关联，进入跨会话知识库（区别于通用记忆）。
- 与方向 4（长程研究）联动：长任务积累的文献成为可复用资产。

#### 3.4 多源检索
- 除 OpenAlex/Crossref 外：Semantic Scholar、arXiv、Google Scholar、PubMed（生信）、DBLP（CS）。
- 调研：各源 API 的配额、字段差异、去重策略。

### 对核心架构的预留要求
- ✅ `search_papers`/`fetch_paper` 工具已抽象。
- 待定：向量索引选型（影响是否引入 native 依赖 / sqlite 扩展）。
- 待定：新表（papers/embeddings/citations）加入 schema——建议在 [05](data-model.md) 基础上预留扩展区。

---

## 4. 长程自主研究

### 基线能力
- 长程自主研究 skill（无人值守研究循环协议）。
- 三类失败应对：认知循环、卡顿、运行时脆弱。
- 三层守护：文件化状态（`findings.jsonl`/`progress.json`）、stall 检测 + 结构性 pivot、guardian/worker 分离。

### 增强空间

#### 4.1 更稳健的状态持久化
- 长任务跨 context compaction、跨进程重启存活。
- **现状**：状态写在工作区文件（session 级），compaction 后靠 `[boundary]` + summary 保留。
- **增强**：把长任务的「研究状态」提升为一等 DB 实体（`research_runs` 表？），含目标、进度、findings、决策日志。进程崩溃后可从 DB 恢复继续。
- 关联 [05 frames 落库](data-model.md#frames-是否落库)：frame 状态持久化是前提。

#### 4.2 故障恢复与检查点
- 定期 checkpoint：把当前 frame 状态、最近 findings、未完成任务持久化。
- 崩溃后 `resume`：重建 frame、重放未完成步骤。
- 与 artifact checkpoint（`artifact_versions.is_checkpoint`）联动。

#### 4.3 可观测的自主循环
- 长任务需要「发生了什么」的可视化：决策点、pivot 时刻、finding 累积曲线。
- **现状**：trace JSONL + `notice` 事件。
- **增强**：长任务专用的进度视图（前端可能需新组件，或复用 plan panel）。

#### 4.4 资源与成本治理
- 长任务跑数小时，LLM 成本/时间需治理：预算上限、token 累计、自动暂停/续跑。
- **预留**：`TokenUsage` 累积、`max_iterations` 已是软上限；增加「研究级」预算门。

### 对核心架构的预留要求
- ✅ `delegate`（guardian/worker）、`boundary`、文件化状态模式已有。
- 待定：`research_runs` 表 / frames 落库。
- 待定：resume 机制需要 Session/Agent 支持「从持久化状态重建」。

---

## 跨方向的预留：插件化

四个方向的某些能力（学科专用检索、特定数据格式、专用可视化）最好以**插件**形式存在，而非全堆进核心。详见 [07 学科扩展插件体系](domain-plugins.md)。

---

## 优先级建议

> 待与用户确认（登记 [决策日志](decisions.md)）

按「解锁价值 / 阻塞关系」排序的初步建议：

1. **代码沙箱（方向 2.1）**——阻塞实验分析，是引入真沙箱、清掉早期技术债的契机。优先级最高。
2. **Deepseek 集成（方向 1）**——项目立身之本，主力模型必须跑顺。reasoning 利用可渐进。
3. **文献知识库（方向 3）**——科研核心场景，RAG 提升明显。
4. **长程研究（方向 4）**——依赖 1/2/3 就绪 + frames 落库，放后期。
5. **学科插件（[07](domain-plugins.md)）**——需调研驱动，按学科需求拉动。

但这个排序会在 [08 路线图](roadmap.md) 里结合核心基线的交付节奏重新组织。

---

下一步：读 [07 学科扩展插件体系](domain-plugins.md)。
