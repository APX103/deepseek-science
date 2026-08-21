# P9 — Science Agent Harness Foundation

## 目标

把现有会话、运行和消息表从多个并列事实源，渐进收敛到一个可恢复、可审计、可扩展的 Agent Harness。迁移期间保留现有 projection 表，所有新事实双写到 typed append-only `session_events`。

## 不变量

1. 模型可见的会话事实必须有持久事件。
2. 事件与对应 projection 更新在同一个 SQLite 事务中提交。
3. durable facts 使用单调递增的 session-local sequence；SSE 只承担通知，不是事实源。
4. 旧数据库可原地升级，旧会话继续读取；新事件不伪造旧历史。
5. 审计读取有界分页，顺序稳定，payload 带 schema version。

## 当前完成

- [x] `session_events` schema、唯一顺序与查询索引。
- [x] Rust typed event API，未知 event type fail closed。
- [x] 会话创建、消息、工具 checkpoint、compaction 和 run 终态事务化双写。
- [x] run acceptance 保存 prompt、模型和完整工具 envelope。
- [x] `sessions.compaction_state` 在 checkpoint/终态原子保存并在冷恢复时加载。
- [x] `GET /api/sessions/{sid}/events?after_seq=&limit=` 审计游标 API。
- [x] 迁移、分页、重启恢复与事务回滚测试。
- [x] Agent Profile 与执行队列解耦；新任务写入通用 `agent_jobs`。
- [x] 旧 `bot_jobs` 单向迁移，旧 Bot API 保留为兼容别名。
- [x] job enqueue/claim/settle 进入 SessionEvent 审计流。
- [x] 新 `/agent-profiles`、`/sessions/{sid}/jobs`、`/jobs/{jid}` API，前端已切换。
- [x] 稳定 `Session -> Frame -> Run -> RunAttempt` 身份与 legacy Frame alias 迁移。
- [x] Attempt lease/fencing；过期 worker 的 checkpoint/终态写入 fail closed。
- [x] 工具 intent/result 审计与 effect class；未知外部副作用必须显式 reconciliation。
- [x] live timeout/panic 与结果提交不确定使用独立 `unknown` 语义；Run 终态事务禁止把它降级成普通失败。
- [x] 前端识别 `needs_reconciliation`，可列出调用并提交人工成功/失败观察结果。
- [x] 等待输入保持同一 Run；恢复创建新 Attempt，崩溃不再伪装 Run 已完成。
- [x] 持久化子 Frame：delegate/send/collect/stop/list，结果落库后 collect-only 交付。
- [x] active child 在尚无 mid-Run mailbox consumer 时明确拒绝 follow-up，不再返回虚假 queued 状态。

## 后续增量

- [ ] 把 plan approval/cancel/startup interruption 纳入事件流。
- [ ] 为 system prompt、运行设置和 tool catalog 增加稳定 fingerprint，同时保存可重建快照。
- [ ] 从事件投影 messages/runs，并加入 replay consistency checker；稳定后停止双写旧表。
- [ ] 把 JobRuntime 执行 ownership 从 `frontend-compat` 转给 backend worker，并加入 heartbeat/lease expiry。
- [ ] 后端启动时为可自动恢复的 child Run 建立 worker（当前 durable transcript/result 可恢复，执行需新消息显式唤起）。
- [ ] 实现 content-addressed ArtifactStore、版本与 lineage DAG。
- [ ] 增加 trace replay、citation fidelity、数值方法和长任务恢复 evals。

## 风险控制

- 事件 payload 目前保留完整 prompt 和工具 schema，只能通过受本地 API token 保护的接口读取；未来支持导出时必须增加显式脱敏策略。
- 旧会话在本阶段不会回填伪事件。投影器必须能识别“legacy baseline + event tail”。
- `CompactionState` 仍是索引 fold；持久化修复了重启丢失，但 message UUID/L2 compaction 仍是后续工作。
