# 前缀缓存 + 上下文压缩 集成方案（独立文档）

> **状态**：核心已实现（阶段 A 缓存计量、阶段 B 记忆块位置、阶段 C 压缩顺序已于 2026-08-05 落地；阶段 D 与 DEFER 项见 §5/§7）。
>
> **本文是一份独立的集成方案**：不依赖任何既有路线图/计划文档，读者仅凭本文即可理解背景、借鉴对象、现状差距、方案设计与风险。
>
> **借鉴对象**：`/Users/apx103/work/DeepSeek-Reasonix`（一个围绕 DeepSeek 前缀缓存做 token 成本优化的 agent 工具，Go 实现）。

---

## 1. 背景：为什么值得做

本项目（Deepseek Science）是本地优先的科研 AI 工作台，以 DeepSeek 系列模型为主力推理引擎。agent 的工作模式是**同一会话内逐轮向模型重发几乎相同的内容**：system 提示词、工具 schema、以及不断增长的历史消息。

DeepSeek API 官方提供**自动的上下文硬盘缓存**（默认开启，无需改代码）：如果本次请求的前缀与历史请求存在完整重复，重复部分按「缓存命中」计费。两者的价差极其悬殊（2026-08 官方定价，deepseek-v4-pro）：

| 计费项 | 价格（元/百万 token） | 相对成本 |
|---|---|---|
| 输入 · 缓存命中 | 0.025 | **1×** |
| 输入 · 缓存未命中 | 3 | **120×** |
| 输出 | 6 | — |

也就是说：**长会话中如果 system + 工具 schema + 历史前缀能保持字节稳定、持续命中缓存，输入成本可降到约 1/120**；反之如果每轮都让前缀漂移（例如工具定义顺序变化、system 内容逐轮微调、历史中段被随意改写），则会以全价支付每一轮的全部输入 token。

现有 agent 内核（Runner 主循环 + Rolling Compact 压缩）**没有缓存意识**：不解析缓存统计、不刻意固化前缀、压缩时随意折叠历史中段。本方案的目的是把「缓存命中省 token + 上下文压缩」作为一个整体能力集成进来。

---

## 2. DeepSeek 缓存机制事实（官方文档）

来源：<https://api-docs.deepseek.com/zh-cn/guides/kv_cache>（2026-08 抓取）。

1. **自动开启**，每个请求都会触发缓存构建，无需应用侧任何配置。
2. **前缀完整单元匹配**：每条缓存前缀是一个独立的完整单元，后续请求**只有完整匹配该单元**才命中；中途任何字节变化，该单元及其后的所有单元全部 miss。
3. **三种落盘时机**：
   - 请求结束位置落盘（用户输入结束位置 / 模型输出结束位置）；
   - **公共前缀检测落盘**：系统检测到多次请求之间的公共前缀，会单独落盘为独立单元（例如请求 A+B 与 A+C 互相不命中，但公共前缀 A 会被落盘，第三轮 A+D 可以命中 A）；
   - 按固定 token 间隔落盘（长输入/输出防止"迟迟不结束导致完全无法缓存"）。
4. **usage 返回两个字段**：`prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`（响应顶层）。
5. 缓存"尽力而为"：构建秒级；闲置数小时至数天自动清空，不保证 100% 命中。

**由此得出的三条设计推论**（本方案全部设计都建立在这三条上）：

- **越靠前的字节越值钱**：system 提示词、工具 schema、早期历史是天然的公共前缀，必须保持字节稳定；
- **中段改写会腰斩缓存**：压缩（折叠/修剪）发生的当次请求从改写处开始 miss；但改写完成后新前缀会重新落盘、后续请求继续命中——所以压缩要低频、聚合，并且**保住最前面的稳定段**；
- **可变尾部天然 miss**：最新用户消息、每轮新产生的工具结果是无法避免的增量成本，不属于优化对象，也不应被优化手段误伤。

---

## 3. 借鉴对象：Reasonix 的核心思想

调研对象：`/Users/apx103/work/DeepSeek-Reasonix`。它的产品定位就是"围绕 DeepSeek 前缀缓存调优，长会话把 token 成本压低"。其核心思想可提炼为两句话：

> **把一切可复用的知识固化成一个字节稳定的前缀，把所有可变内容留在尾部。**
> **历史 append-only 重放，窗口快满时才做低频重置；先用免费手段减负，再用付费摘要兜底。**

### 3.1 缓存稳定前缀（省 token 的主体）

- **系统提示词一次性组装、逐轮字节不变**：启动时组装 base prompt → 输出风格 → 决策/语言策略 → 工作区信息 → **环境摘要** → **记忆** → **技能索引**（`internal/boot/boot.go:389-475`）；运行期 `Agent.systemPrompt()` 只原样返回（`internal/agent/agent.go:3117-3129`）。
- **环境摘要持久化、跨重启稳定**：探测结果按 fingerprint 缓存到磁盘（TTL 24h），临时失败用旧快照合并（`internal/environment/snapshot.go:37-44,121-141`）。注释明言：环境区块在 provider-cached 前缀内，重观测会导致 "10x miss pricing"。
- **工具 schema 规范化 + 排序**：注册时做 `CanonicalizeSchema`（递归稳定 JSON Schema：`required` 数组排序、删非法字段、空 schema 归一，`internal/provider/schema_canonicalize.go:10-35`）；导出时**按键排序**（`internal/tool/tool.go:523-544`）。
- **记忆折叠进前缀**：`memory.Compose` 把记忆全量折进前缀，空记忆时返回 base 原样（identity 不变式，`internal/memory/memory.go:179-188`）。**mid-session 的记忆变更绝不改前缀**——走"transient tail"注入（在消息尾部追加一条内部指令，`internal/control/input.go:181-193`），下次会话才折进前缀。
- **技能索引只放名称+描述**："Bodies never enter the prefix"（`boot.go:452-475`）。

### 3.2 延迟压缩（免费优先、付费兜底）

- **阈值分级**（`internal/agent/compact.go:87-95`）：soft=窗口 50%（只发提示不动）、snip=60%、high=80%（触发压缩）、force=90%（强制）。
- **压缩前先免费减负**（`compact.go:113-143`）：60% 先裁剪（snip）旧工具输出，80% 先修剪（prune）旧工具输出。stale 判定 = 位于保护尾之前 + 超过 1024 字节的 tool 消息；保留错误消息与用户标记 `[[keep]]` 的消息；剪前先归档原件。**若修剪后总量已低于触发线，直接跳过付费的摘要调用**（`compact.go:133-142`）。
- **保留前缀 + verbatim 尾**（`compact.go:440-481`）：压缩时保留 system + 首条小用户消息 + 历史 digest 的固定前缀段（永不重新摘要、永不被折叠），尾部保留默认 16384 token 预算、对齐工具边界的原样消息段；只有中间段被折叠成摘要。摘要累积，不重复折叠已折叠内容。
- **摘要生成**：同 provider、无工具调用、固定 7 段 system prompt、90s 超时 + 一次重试、失败时机械折叠（`compact.go:687-760`）。

### 3.3 计量与诊断（让优化可量化）

- **usage 解析**：解析 DeepSeek 顶层 `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens`，并兼容 OpenAI 的 `prompt_tokens_details.cached_tokens` 与 Anthropic 缓存计数（`internal/provider/openai.go:1141-1189,1359-1360`）。
- **miss 诊断**：每轮对请求前缀做哈希（`capturePrefixShape` → `CompareShape`，`internal/agent/cache_shape.go:67-94`），当命中率异常时用哈希对比定位是 system 漂移、工具集变化还是正常尾部增长。
- **验证**：mock DeepSeek 模拟前缀缓存语义做 e2e 测试（`internal/agent/cachehit_e2e_test.go`）；真实 API 的 seed → 闲置过 TTL → resume 对照 benchmark（`benchmarks/context-maintenance-e2e/main.go`）。

---

## 4. 本项目现状与差距（以代码为准）

| 维度 | 理想状态（Reasonix） | 本项目现状 | 差距 |
|---|---|---|---|
| 缓存 token 计量 | 解析 hit/miss 并展示 | `Usage` 只有 input/output（`crates/dss-llm/src/types.rs:188-193`）；`parse_usage` 直接丢弃缓存字段（`crates/dss-llm/src/client.rs:147-150`） | **完全缺失** |
| system 前缀稳定性 | 启动一次性组装、字节不变 | 无独立前缀概念；system 混在消息历史里逐轮重发，内容未刻意固化 | **缺失** |
| 工具 schema | 规范化 + 排序导出 | `ToolRegistry.definitions()` 直接遍历 `HashMap`（`crates/dss-tools/src/router.rs:55-60`），顺序依赖注册顺序与哈希实现，跨进程不稳定 | 需补齐 |
| 记忆进前缀 | 稳定段折进前缀、变更走 transient tail | 记忆召回块作为 harness-notice **追加到历史末尾**（`crates/dss-agent/src/runner.rs:111-123`）——不破坏前缀但不固化、内容随查询变化 | 需重构 |
| 环境摘要 | fingerprint 持久化、字节稳定 | 无 | 需新增 |
| 压缩策略 | 60% 免费 prune → 80% 才付费折叠；保留前缀 + verbatim 尾 | dss-compact 已有 append-only + projection 地基（`crates/dss-compact/src/state.rs:55-80`），但**无免费 prune**（仅硬墙 microcompact）、**无前缀/尾部保护**、触发阈值固定 0.75/0.9 | 需增强 |
| miss 诊断 | 前缀哈希对比 | 无 | 可选 |

**结论：完全可行。** 现有地基（append-only 消息 + projection 视图、LlmClient 抽象、ToolRegistry、FakeLLM 测试基建）正好够用，缺的是"缓存意识"这一层：计量、前缀固化、schema 排序、免费减负、前缀保护。

---

## 5. 方案设计

四个独立阶段，顺序按「先计量、再固化、后压缩、最后验证」排列。每个阶段可单独合入、单独验收。

### 阶段 A：缓存计量（地基）

**目标**：每一轮都能看到 cache hit/miss，用真实数据量化收益、驱动后续决策。

1. `dss-llm`：
   - `Usage` 增加 `cache_hit_tokens: u32` / `cache_miss_tokens: u32`（默认 0）；
   - `parse_usage`（`client.rs:147`）解析 DeepSeek 顶层 `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens`，并兼容 `prompt_tokens_details.cached_tokens`（面向非 DeepSeek 的 OpenAI 兼容端点）；流式末包复用同一解析函数（`client.rs:273-274`），天然覆盖。
2. `dss-agent`：`RunOutcome` 与 `complete` 事件携带缓存字段；Session 聚合累计 hit/miss，供前端展示会话级命中率。
3. `dss-api` / 前端：`complete.usage` 扩展字段，usage 行显示 `hit/miss` 与命中率。

**验收**：同 session 真实 DeepSeek 连续两轮，第二轮 `cache_hit_tokens > 0`；单元测试覆盖三种响应形态（顶层字段 / `cached_tokens` 兼容 / 字段缺失默认 0）。

### 阶段 B：缓存稳定前缀（收益主体）

**目标**：让「system 前缀 + 工具 schema」成为跨轮、跨重启字节稳定的公共前缀。

1. **新增前缀组装模块**（独立 crate `dss-prompt`，理由见决策点 D1）：
   - `StablePrefix` 组装器：基础角色规则 + 环境摘要 + 记忆摘要 + 技能索引（仅名称+描述），会话创建时**一次性组装**，运行期只读、逐轮字节不变；
   - 确定性序列化：固定段顺序、固定分隔符；
   - **环境摘要**：探测结果按 fingerprint 持久化（`{data_dir}/env-probes-<hash>.json`，TTL 24h），跨重启稳定，失败合并旧快照；
   - **记忆折叠**：稳定记忆（profile 层）折进前缀，空记忆 identity 不变式；mid-session 记忆/策略变更走 transient tail（追加到历史末尾的内部消息），下次会话才折进前缀。
2. **工具 schema 规范化 + 排序**（`dss-tools`）：
   - `definitions()` 改为按名字排序输出；
   - 新增 `CanonicalizeSchema`：递归稳定 JSON Schema（`required` 排序、删非法字段、空 schema 归一为 `{"type":"object","properties":{}}`）。
3. **消息流改造**：前缀作为每轮请求的第一条 system 消息（字节不变即可命中，无需只发一次）；可变 system 消息明确移出前缀区。

**验收**：同 session 连续两轮产出的 system 消息逐字节相等；乱序注册工具仍得到相同 schema 输出；集成测试用「缓存模拟 FakeLLM」断言前缀稳定时命中率攀升；既有测试全回归（重点：持久化恢复、compact、plan 上下文注入）。

### 阶段 C：压缩增强（免费优先 + 前缀保护）

**目标**：压缩不再盲目破坏前缀；免费减负先行，付费折叠兜底。

1. **免费 prune**（`dss-compact` 新增，与硬墙 microcompact 独立）：
   - stale tool result 判定：位于保护尾之前 + ≥1024 字节的 `role=tool` 消息；
   - prune 后若整体低于触发线，**跳过付费 summarize**；
   - 剪前归档到 `compaction_archives`（表已存在）。
2. **前缀保护**：`projection` 增加保护区间——最前面的 system 前缀 + 首条用户消息永不折叠；尾部保留 verbatim 预算并对齐工具边界；fold 只发生在中间区。
3. **触发分级**：把「免费减负」放到「付费折叠」之前（60% 先剪、80% 才折叠的量级），替代当前单一触发阈值。

**验收**：FakeLLM 大上下文测试——prune 先行且修剪后不调 summarize；projection 不触碰保护区间；既有 compact 单元测试全绿。

### 阶段 D：诊断与验证（可选）

- **prefix shape 诊断**：对请求前缀做哈希，命中率异常时对比上一轮哈希，区分 system 漂移 / 工具集变化 / 正常尾部增长。
- **缓存模拟测试**：在测试基建里加"完整单元匹配"语义的 FakeLLM，验证前缀稳定时命中率爬升、压缩后重新落盘。
- **真实 API 对照**：seed → 闲置过缓存 TTL → resume，对比冷启动与持续会话的 miss 成本（脚本化，不进 CI）。
- **日志**：`llm_call` 日志带 hit/miss 字段，便于离线分析。

---

## 6. 风险与权衡

| 风险 | 说明 | 缓解 |
|---|---|---|
| 消息流改造回归 | 阶段 B 改变 system 消息组织方式，波及持久化恢复、plan 注入、前端渲染 | 阶段 B 单独小步合入；每步跑全测试 + 真实 DeepSeek 冒烟 |
| 压缩破坏缓存 | fold/prune 当次请求从改写处开始 miss（完整单元匹配） | 阶段 C 的前缀保护区间；低频聚合压缩；免费 prune 优先 |
| 缓存"尽力而为" | TTL 数小时~数天、不保证 100% 命中 | 阶段 A 计量先行，用真实命中率校准预期 |
| 环境/记忆内容变化节奏 | 环境或记忆摘要变动会 invalidate 前缀 | fingerprint 持久化 + TTL；只把稳定内容放进前缀，易变内容走 transient tail |
| 工具 schema 顺序变化 | 影响依赖注册顺序的既有测试 | 排序 + 规范化后更新断言 |
| 压缩阈值改动 | 现有压缩常量被文档声明为"已定型" | 走决策记录流程，先小步试、量化对比 |

---

## 7. 决策点（实现前需明确）

> 以下决策已按「倾向」落地（2026-08-05），登记于 `docs/decisions.md` D-011。

- **D1 前缀模块放哪**：独立 crate `dss-prompt` vs 并入 `dss-agent`。**已定：不新增 crate**。最新代码已把稳定 system 前缀（`SCIENCE_EXECUTION_POLICY` + 项目上下文）放在 `session.messages` 之外、每轮以 `run_context` 前置，fold 索引天然不触碰前缀；无需为前缀新建 crate，改动面最小。
- **D2 记忆折叠策略**：全量折进前缀 vs 每轮尾部召回。**已定：记忆召回块移到请求视图末尾**（历史之后），内容随查询变化，放末尾只影响未命中尾部、历史前缀稳定命中；稳定记忆折进前缀 DEFER。
- **D3 压缩触发分级**：沿用单阈值 vs 免费 prune 先行。**已定：免费减负先行**——每轮先 microcompact（无 LLM 调用），仍超触发阈值才付费折叠；免费减负到触发线下时不调 summarize。折叠常量未改（遵守 modules.md §8 已定型约束）。
- **D4 环境摘要是否进前缀**：**DEFER**。fingerprint 持久化 + TTL 的收益对本场景（Tauri 桌面、单会话内前缀已稳定）不如记忆块位置修正直接，登记为后续项。
- **D5（新增）prefix-shape miss 诊断**：DEFER。计量（阶段 A）已落地，命中率异常时再补哈希诊断。

---

## 8. 工作量估算

| 阶段 | 内容 | 估时 |
|---|---|---|
| A | 缓存计量 | 0.5 天 |
| B | 缓存稳定前缀 | 1.5 天 |
| C | 压缩增强（prune + 前缀保护） | 1 天 |
| D | 诊断与验证（可选） | 1 天 |

主线约 3 天（不含 D）。

---

## 9. 参考来源

- Reasonix 源码：`/Users/apx103/work/DeepSeek-Reasonix`（核心文件：`internal/boot/boot.go`、`internal/agent/compact.go`、`internal/agent/prune.go`、`internal/provider/schema_canonicalize.go`、`internal/agent/cache_shape.go`、`internal/agent/cachehit_e2e_test.go`、`benchmarks/context-maintenance-e2e/main.go`）。
- DeepSeek 官方文档：上下文硬盘缓存 <https://api-docs.deepseek.com/zh-cn/guides/kv_cache>、模型与价格 <https://api-docs.deepseek.com/zh-cn/quick_start/pricing>（2026-08 抓取）。
- 本项目代码位置：`crates/dss-llm/src/client.rs`、`crates/dss-llm/src/types.rs`、`crates/dss-tools/src/router.rs`、`crates/dss-agent/src/runner.rs`、`crates/dss-compact/src/{state.rs,lib.rs}`。
