# Science Agent Harness 验收测试报告

- 日期：2026-08-21（Asia/Shanghai）
- 分支：`codex/science-agent-harness`
- 被测提交：`e93b521 feat: make agent frames durable and recoverable`
- 结论：**通过**

## 1. 验收范围

本轮重新验证以下已实现能力：

1. 稳定的 `Session -> Frame -> Run -> RunAttempt` 身份模型。
2. root Frame 在多轮 Run 之间保持同一 ID；旧 Frame ID 通过 alias 迁移。
3. awaiting 与进程崩溃后继续同一 Run、创建新 Attempt。
4. Attempt lease/fencing：失去所有权的 worker 不能 checkpoint 或提交终态。
5. 工具调用 intent/result 审计与 `read_only/idempotent/external_side_effect` 分类。
6. 未知外部副作用进入 `needs_reconciliation`，显式对账后才能恢复。
7. 持久化子 Frame 的 delegate、result landing、collect-only、send/resume、stop/list。
8. Frame tree 与 session event 审计 API。
9. 前端 TypeScript 与生产 bundle 构建。
10. 真实 backend 进程被强杀后的数据库恢复和 SSE 续跑。

## 2. 静态门禁与全量回归

执行命令：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked -q
cd frontend && bun run build
```

结果：

| 检查 | 结果 | 证据摘要 |
|---|---|---|
| Rust 格式 | 通过 | `cargo fmt --check` 退出码 0 |
| Rust lint | 通过 | clippy warnings 作为 error，退出码 0 |
| Rust 全 workspace | 通过 | 518 passed，0 failed，2 ignored |
| 前端类型检查 | 通过 | `tsc -b` 成功 |
| 前端生产构建 | 通过 | Vite 370 modules transformed |

两个 ignored 测试均在源码中显式标为真实外部网络/副作用测试：

- `public_repository_structure_round_trip`：真实匿名 DeepWiki MCP 请求。
- `live_registry_resource_invokes_a2a_and_validates_artifact`：真实 Agent Registry/A2A 网络副作用。

它们不是失败，也没有在本地验收中伪造为通过。前端构建有一个现存的 bundle 大于 500 kB 提示；它不影响构建成功，但可作为后续 code-splitting 优化项。

## 3. 持久化子 Frame 集成测试

执行命令：

```bash
cargo test -p dss-api --lib \
  subagents::tests::durable_child_lands_collects_and_resumes_on_the_same_frame \
  -- --exact
```

结果：**通过**。

该测试使用真实 SQLite pool、迁移、`DurableSubagentRuntime` 和 fake LLM，验证：

- delegate 创建一个持久化 child Frame 和第一个 child Run/Attempt；
- child 结果先写入 `child_results`，父级 collect 后第二次 collect 为空；
- `send_message` 在同一 child Frame 上创建第二个 Run；
- 最终数据库为 1 个 child Frame、2 个 child Runs；Frame ID 没有变化。

## 4. 外部副作用对账集成测试

执行命令：

```bash
cargo test -p dss-db --lib \
  harness::tests::external_side_effect_requires_explicit_result_before_same_run_can_resume \
  -- --exact
```

结果：**通过**。

验证内容：

- 未结算的 `external_side_effect` 不会自动重放；
- Run 保持 `needs_reconciliation`；
- 显式写入观察到的成功/失败结果后，生成可审计 harness notice；
- 同一 Run 可以创建 attempt #2 恢复；
- 没有伪造缺少 assistant tool-call 前半段的 OpenAI `tool` 消息。

## 5. 真实进程 kill/restart E2E

测试拓扑：

```text
curl/SSE client -> real dss-backend -> local HTTP mock LLM
                         |
                         +-> real SQLite database
```

过程：

1. 启动真实 `target/debug/dss-backend`。
2. 通过 `POST /api/sessions` 创建真实 Session/root Frame。
3. 通过 `POST /api/sessions/{sid}/stream-sse` 接受 Run。
4. mock LLM 的第一次响应故意阻塞 8 秒。
5. 在 Run 已持久化为 `processing` 后，对 backend 执行 `kill -9`。
6. 使用同一个 data directory 重启 backend，触发 startup reconciliation。
7. 验证旧 Run 为 `interrupted` 且 `completed_at=NULL`。
8. 使用新的 client transport ID 再次调用 SSE；服务端恢复原 durable Run，并创建 attempt #2。
9. 验证 SSE 返回 `durably resumed`，Run 正常完成。
10. 查询 `/events` 与 `/frames` 验证审计链和 Frame projection。

机器断言结果：

```json
{
  "crash_projection": "interrupted|NULL",
  "final_state": "1|2|completed|1",
  "events": [
    "attempt_started",
    "attempt_lease_expired",
    "run_interrupted",
    "run_resumed",
    "attempt_started",
    "attempt_settled"
  ],
  "frame_activity": "idle",
  "sse_contains_resumed_answer": true
}
```

`final_state` 依次表示：

- 1 个 durable Run；
- 2 个 RunAttempts；
- Run 最终为 `completed`；
- 1 个稳定 root Frame。

因此该 E2E 证明：SSE/client run ID 只是传输控制 ID，进程恢复不会错误创建第二个逻辑 Run，也不会把崩溃中的 Run 伪装成已完成。

## 6. Feature 验收矩阵

| Feature | 状态 | 主要证据 |
|---|---|---|
| 稳定 root Frame ID | 通过 | Frame 单测、process E2E 最终仅 1 Frame |
| 同 Run / 新 Attempt 恢复 | 通过 | process E2E：1 Run / 2 Attempts |
| awaiting Run 恢复 | 通过 | `parked_run_resumes_on_a_new_attempt_without_changing_frame_or_run_identity` |
| lease/fencing | 通过 | stale attempt checkpoint/settle 回归测试 |
| 启动恢复 | 通过 | `attempt_lease_expired/run_interrupted/run_resumed` 审计链 |
| 外部副作用 fail closed | 通过 | reconciliation 专项测试 |
| 子 Frame 持久化与复用 | 通过 | DurableSubagentRuntime 集成测试 |
| result collect exactly once | 通过 | child result collection 回归测试 |
| Frame tree API | 通过 | process E2E 查询 `/frames` |
| typed session events | 通过 | process E2E 查询 `/events` |
| 旧 schema 原地升级 | 通过 | dss-db migration tests |
| 前端生产构建 | 通过 | `tsc -b && vite build` |

## 7. 完成边界与后续项

本次获批的 Frame Harness 核心闭环已经完成并通过上述验收。

以下能力在 P9 文档中仍明确属于后续增量，不能在本报告中宣称已完成：

- backend 启动后自动为 cold child Run 建 worker；当前 child transcript/result 可恢复，但执行需要新消息显式唤起；
- content-addressed ArtifactStore 与 lineage DAG；
- 依赖真实 DeepWiki/Agent Registry 的 live-network 测试；
- macOS Tauri 窗口的 GUI computer-use 自动化。本报告的端到端层级是“真实 backend 进程 + HTTP/SSE + SQLite + mock LLM”，不是截图驱动的 UI 测试。

Bot 数据表和旧 API 仍作为 Agent Profile 兼容层保留，但不再承担 Harness 的核心身份或执行语义。
