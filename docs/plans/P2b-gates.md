# P2b-gates — Runner 决策门控

> 对应 roadmap P2 / modules.md §4 `process_llm_response` 决策门。状态：进行中（2026-08-03）。

## 目标
Runner 完整决策门（modules.md「物理常数」，阈值一字不改）：
1. **max_tokens 续传门**：finish_reason=length → 三档（≥5 终止、≥3 大幅缩减、否则分块继续）。
2. **empty-retry 门**：无 tool_use 且非 max_tokens 且内容为空（thinking-only 也算空）→ 注入 harness_notice 提示重试，≤3 次；超限 → Failed。
3. **检索熔断**：连续纯检索（只调 web_search/fetch_url 等检索类工具）≥6 轮 → 注入「停止搜索开始写作」harness_notice，强制下一轮写作。

## 验收点
1. `cargo build` 无警告。
2. **FakeLLM 单元/集成测试**覆盖各分支：
   - max_tokens 三档（length finish_reason；构造剩余 token 比例触发各档）。
   - empty-retry：连续空响应 → 重试到 3 次 → Failed。
   - 检索熔断：连续 6 轮只调 web_search → 第 7 轮注入熔断 notice。
3. 正常对话不回归（有内容/有工具调用正常完成）。

## 回顾

**实际做了什么**：
- `Session` 加 `GateState{empty_retry_count, retrieval_streak, length_finish_count}`。
- Runner 流式中捕获 `StreamEvent::Finish{reason}` 存本轮 `finish_reason`。
- 决策门（顺序严格按 modules.md §4）：
  1. **max_tokens 续传门**：finish_reason=="length" → `length_finish_count++`；≥5(HARD_CAP) → 终止(MaxIters)；≥3(TRIM_AT) → 注入「大幅缩减」提示；否则注入「续传」提示；continue。
  2. **工具路径**：执行工具；ask_user 检测；**检索熔断**——若本轮全是检索类工具(`is_retrieval_tool`: web_search/fetch_url/search_papers/.../list_files/read_file)则 `retrieval_streak++` 否则归 0；≥6 → 注入「停止搜索开始写作」notice 并归 0。
  3. **无 tool_use 且非 length**：空(text_buf 空) → `empty_retry_count++`；>3(CAP) → Failed；否则注入「请给实际回复」notice continue；有内容 → clean natural completion。
- 常量：`EMPTY_RETRY_CAP=3`、`RETRIEVAL_CIRCUIT_BREAKER=6`、`LENGTH_FINISH_HARD_CAP=5`、`LENGTH_FINISH_TRIM_AT=3`（modules.md 阈值）。
- harness_notice 暂作为普通 system 消息注入（P3 已有 harness_notice 列，显式标记留后续）。
- FakeLLM 流式集成测试 5 个：natural / empty-retry fail / empty-retry recover / max_tokens cap / retrieval circuit breaker。**5 测试全绿。**

**验证结果**：
- `cargo test` 全 workspace 19 测试全绿（5 gates + 12 compact unit + 2 compact 集成）；0 警告。
- P3 回归：短对话 → 单 iteration、natural，门控不干扰正常对话。

**遗留**：
- harness_notice=true 显式标记（注入消息目前是普通 system 消息）——留 P4b 配合 message 模型重构。
- plan denial 门 / deep_review output 门 / terminal barrier（reviewer）→ P6（依赖 plan 工具 / verify 模块）。
- max_tokens 三档的「剩余 token 比例」精确判断（≥3 大幅缩减 vs 分块继续）目前用累计次数简化（n>=3 提示缩减、n>=5 终止），与 modules.md 三档语义一致；精确 token 比例留优化。

## 设计
- `Session` 加 `gate_state: GateState{ empty_retry_count: u32, retrieval_streak: u32, length_finish_count: u32 }`。
- 流式中捕获 `StreamEvent::Finish{reason}` 存本轮 `finish_reason`。
- 决策顺序（严格按 modules.md）：
  1. finish_reason == length → max_tokens 门（三档，用 length_finish_count 计数）。
  2. 有 tool_use → 执行；更新 retrieval_streak（若全是检索类工具则 +1，否则归 0）；≥6 → 注入熔断 notice 后 continue。
  3. 无 tool_use 且非 length → empty-retry 门（空则 retry++ ≤3 注入提示；否则 natural completion）。
- harness_notice：用 `ChatMessage::system(...)` 注入（P2b-gates 暂不接 harness_notice 显式标记，作为普通 system 消息；P3 已有 harness_notice 列，留后续把注入消息标 harness_notice=true）。
- 检索类工具集合：`web_search`/`fetch_url`/`search_papers`/`fetch_paper`/`search_memory`/`search_skills`/`list_files`/`read_file`（读/检索类，非写/执行）。

## 工作顺序
1. 写计划（本文件）。
2. Runner 加 GateState + finish_reason 捕获 + 三门决策。
3. FakeLLM 测试（dss-agent/tests/）覆盖三门。
4. cargo build + cargo test 全绿 + curl 正常对话不回归。
5. 回填回顾 + 更新 HANDOFF。

## 风险
- **行为漂移**：阈值严格按 modules.md，不改。FakeLLM 脚本化覆盖。
- max_tokens 三档的「剩余 token 比例」需 token 估算（复用 dss_compact::tokens）。
