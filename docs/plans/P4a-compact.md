# P4a — Rolling Compact（实施计划）

> 对应 [roadmap P4](../roadmap.md#p4--记忆与-compact)。状态：**进行中**（2026-08-03）。本阶段只做 RC，三层记忆留 P4b。

## 目标

长会话不爆 token：RC 在每轮 LLM 前 `maybe_compact`，通过 projection 决定给 LLM 看哪些消息（drop 旧 chunk + 插 summary），**绝不 mutate 消息日志**。超 context 时能压缩且不丢关键信息。

## 关键设计决策

- **常量全保留**（modules.md §8，一字不改）：

  | 常量 | 值 |
  |------|-----|
  | CHARS_PER_TOKEN | 4 |
  | KA_FLOOR | 50000 |
  | KB_RATIO | 0.7 |
  | MIN_CHUNK_TOKENS | 4096 |
  | OUTPUT_CEILING | 32000 |
  | COMPACTION_TRIGGER_RATIO | 0.75 |
  | HARD_WALL_RATIO | 0.9 |
  | MICROCOMPACT_RATIO | 0.65 |
  | ABSOLUTE_TOKEN_CEILING | 300000 |
  | PTL_RETRY_CAP | 32 |
  | COMPRESSION_GATE_DIVISOR | 3 |
  | DEFAULT_CONTEXT_CEILING | 500000 |
  | DEFAULT_KA_RATIO | 0.2 |

- **索引版 projection（P4a）**：modules.md 的 RC 用 `applied_summary_uuids` + 带 uuid 的 Message，但当前 ChatMessage（dss-llm）是 OpenAI 协议精简态、无 uuid。P4a 用**索引范围**做 projection：`CompactionState{ folded: Vec<Fold{start_idx,end_idx,summary} } }`，projection 时把每个 folded 区间替换成 summary 消息。语义等价于 append-only + projection（日志不动、只改给 LLM 的视图）。完整 uuid/compact_boundary Message 模型迁移留 P4b（登记 D-F12）。
- **非破坏性**：`session.messages` 只追加；summary 消息作为新 assistant 消息追加进日志；CompactionState 是 Session 上的视图态，projection 据此折叠。

## P4a 验收点

1. `cargo build` 无警告。
2. **单元测试**（纯逻辑，无 LLM）：
   - token 估算：`estimate_tokens("abcd")=1`、`estimate_messages_tokens` 累加。
   - `should_trigger_l1`：chunk ≥ MIN_CHUNK_TOKENS(4096) 且剩余 < ka*0.7 → 触发；否则不触发。
   - `should_trigger_l2`：≥3 个 L1 summary 且 head tokens ≥ max(8192, ka*0.4) → 触发。
   - projection：被 fold 的区间在 projection 里替换成 summary；日志长度不变（append-only）。
   - microcompact：硬墙压力下 >8000 字符的 tool result 截到 4000 + 提示。
3. **集成测试（FakeLLM）**：脚本化 FakeLLM 让 Runner 多轮大上下文触发 L1 fold + summary 注入，projection 给 LLM 的消息 token 数 < 全量，且 session.messages append-only 不被 mutate。
4. P3 不回归（正常短对话不触发 compact，行为不变）。

## 任务清单（todo）

- [ ] 新建 `dss-compact`：constants/tokens/state(projection)/chunk/microcompact + 单元测试。
- [ ] summarizer（门控）+ `maybe_compact` 主入口 + FakeLLM 集成测试。
- [ ] `dss-agent`：Session 加 `compaction` 字段；Runner 每轮用 projection 替 `session.messages.clone()`。
- [ ] `cargo build` 无警告；`cargo test` 全绿。
- [ ] curl 短对话不回归。
- [ ] 回填「回顾」段；D-F12 登记 decisions；更新 HANDOFF。

## 回归点

- 单元测试逐分支覆盖（见验收点 2）。
- 短对话（消息量 ≪ MIN_CHUNK_TOKENS）：projection == session.messages，不触发 compact，Runner 行为与 P3 一致。
- 大上下文（构造 > 触发阈值）：projection 折叠后 token 数显著下降，日志 append-only 长度只增不减。

## 风险

- **RC 行为漂移**（modules.md 最大风险）：靠单元测试逐分支 + FakeLLM 脚本化覆盖。常量一字不改。
- **索引 projection 的中途恢复**：P4a 不持久化 compaction state，重启后从空开始重新 fold（session_messages 仍在，行为正确，只是重新算）。登记。
- **summarizer 真实质量**：FakeLLM 测脚本；真实 DeepSeek 的 summary 质量在 P4b/增强再测。

## 回顾

**实际做了什么**：
- 新建 `dss-compact` crate：
  - `constants.rs`：全部 RC 常量（CHARS_PER_TOKEN=4 … DEFAULT_KA_RATIO=0.2 等，一字不改）+ 额外派生常量（MICROCOMPACT 阈值、L2 head floor、summarizer 重试/退化比）。
  - `tokens.rs`：`estimate_tokens`（len/4，按 char 计数）、`estimate_message_tokens`（content+tool_calls+固定开销）、`estimate_messages_tokens`。
  - `state.rs`：`CompactionState{folds, l1_summary_count}`、`Fold{start_idx,end_idx,summary,level}`、`projection`（把 fold 区间替换成 assistant summary 消息，append-only 保证）。
  - `chunk.rs`：`kept_available_target`、`is_over_trigger`、`should_trigger_l1`（chunk≥MIN_CHUNK_TOKENS 且剩余<ka*0.7）、`should_trigger_l2`（≥3 L1 summary 且 head≥max(8192,ka*0.4)）、`pick_next_chunk`（最早未折叠段，累积到 ≥MIN_CHUNK_TOKENS）。
  - `microcompact.rs`：硬墙截 tool result >8000→4000 + 提示（作用于 projection，不 mutate 日志）。
  - `summarizer.rs`：门控（目标=chunk_tokens/COMPRESSION_GATE_DIVISOR、≤3 重试、退化检测）、非流式 `chat` 调用做 summary。
  - `lib.rs::maybe_compact`：主入口（未过阈值直接返回 → 选 chunk → summarize → record_fold；循环 + PTL_RETRY_CAP 安全上限）。
- `dss-agent`：`Session` 加 `compaction: CompactionState`；`Runner::run` 加 `context_window` 参数，每轮 LLM 前 `maybe_compact` + `projection` + `microcompact` 构视图（替 `session.messages.clone()`）。
- `dss-api`：stream_sse 传 `DEFAULT_CONTEXT_CEILING(500000)`。
- 单元测试 12 个（tokens/projection/trigger_l1/trigger_l2/pick_next_chunk/microcompact 全分支）+ FakeLLM 集成测试 2 个（fold+压缩+append-only / 短对话不调 LLM）。**14 测试全绿。**

**验证结果**：
- 单元测试：`cargo test -p dss-compact` → 12 passed（tokens 估算、projection 折叠/多 fold 顺序、L1/L2 触发边界、pick_next_chunk 避让已有 fold、microcompact 截断/保留）。
- 集成测试：FakeLLM 驱动 `maybe_compact`（context_window=10000、4 条 5000 token 消息）→ 触发 fold、summarizer 被调用、projection token 数 < 全量、日志 append-only 长度不变、每个 fold 对应一条 SUMMARY。
- P3 回归：短对话（一句话）→ 单 iteration、natural、**不触发 compact**（日志确认无 "rolling compact"），行为与 P3 一致。
- `cargo build` 全 workspace 无警告；`cargo test` 14 测试全绿。

**偏离**：
- **索引版 projection**（D-F12）：modules.md 用 `applied_summary_uuids` + 带 uuid 的 Message；当前 ChatMessage 无 uuid，P4a 用索引范围 fold。语义等价（append-only + projection）。完整 uuid 模型迁移留 P4b。
- L2 fold 的「跨 L1 summary 再压缩」实现：`should_trigger_l2` 已实现判断，但 `maybe_compact` P4a 只循环 L1（L2 fold 的 chunk 选择跨多个已 fold 区间需更复杂的 head 段处理），留 P4b 配合 boundary 一起。L2 触发判断的单元测试已覆盖。

**遗留（→ decisions.md）**：
- 索引版 projection（D-F12）、L2 fold 实现 + boundary 对齐（P4b-gates）、compaction state 持久化（重启后从空重新 fold，session_messages 仍在行为正确，P4b）、三层记忆（P4b）。
- summarizer 真实质量（真实 DeepSeek）属增强，P4b/增强再测。
