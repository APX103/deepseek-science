# P6a — plan 工具 + plan 模式（AWAITING_PLAN_APPROVAL）

> 对应 roadmap P6 / modules.md §4。状态：进行中（2026-08-03）。
> verify + delegate（P6b）独立做；本阶段只做 plan 工具链（最独立、可测）。

## 目标
plan_mode 开启时：agent 调 `generate_plan` 生成计划 → Runner 转 AWAITING_PLAN_APPROVAL → 前端/用户批准 → 继续。plan denial 门（plan_mode 无 plan ≤3 次）。

## 验收点
1. `cargo build` 无警告；FakeLLM 测试覆盖 plan 流程（generate_plan → awaiting → approve → 继续）。
2. plan_mode + agent 调 generate_plan → `complete{kind:awaiting, awaiting:plan_approval}` + plan。
3. plan denial 门：plan_mode 但 agent 不出 plan（≤3 次注入提示）。
4. 非 plan_mode 不回归。

## 回顾

**实际做了什么**：
- `dss-tools`：`generate_plan`（steps→PlanState 写 ToolContext.plan，触发 awaiting）+ `update_step_status` 工具；ToolContext 加 `plan: Arc<Mutex<Option<PlanState>>>` + PlanState/PlanStep 类型。
- `dss-agent`：AgentEvent 加 `PlanUpdate{plan}` 变体 + Complete 加 `plan` 字段；Runner::run 加 `plan_mode: bool`；plan 检测（plan_mode 且 ctx.plan 有未批准 plan → 推 PlanUpdate + 转 AwaitingPlanApproval + complete awaiting=plan_approval）；plan denial 门（plan_mode 无 plan ≤3 次注入提示，超限 Failed）；GateState 加 plan_denial_count。
- `dss-api`：RunReq.plan_mode 传 Runner（默认 false）。

**验证结果（curl + 真实 DeepSeek）**：
- plan_mode=true + 让 agent「生成写作计划」→ 3 iteration、generate_plan 被调用（tool_calls/tool_results）、**plan_update 事件触发**、complete kind=awaiting。
- `cargo test` 37 测试全绿（P6a 复用既有 gates 测试，plan 流程经 curl 验证）；0 警告。
- 非 plan_mode 不回归（gates 5 测试全绿）。

**遗留（P6b / 后续）**：
- plan 审批后继续 run 的闭环（POST /api/sessions/{sid}/approve 端点 + 恢复时把 plan 标 approved 并继续执行）。P6a 只到「生成 plan → awaiting plan_approval」；恢复后当前会重新跑（未保持审批态），完整审批闭环留 P6b。
- verify（reviewer checkpoint + terminal barrier）。
- delegate / submit_output（子 agent，深度上限 2）。
- frames 表落库（verification/compaction FK 依赖）。

## 改动点
### 1. plan 工具（dss-tools）
- `generate_plan{steps:[{title}], research_question?}`：把 plan 存进 ToolContext（共享态），触发 awaiting。
- `update_step_status{step_id, status}`：更新 plan step（P6a 最小：更新共享 plan）。
- ToolContext 加 `plan: Arc<Mutex<Option<PlanState>>>`（PlanState{steps, approved, research_question}）。
### 2. Runner 接入
- run 接 `plan_mode: bool` 参数。
- 每轮末检查 plan：若 plan_mode 且 ctx.plan 有 plan 且未 approved → 转 AwaitingPlanApproval + complete awaiting=plan_approval。
- plan denial 门：plan_mode 且无 plan 且本轮自然完成 → ≤3 次注入提示，超限 Failed。
### 3. dss-api
- stream_sse 接受 RunReq.plan_mode（已存在字段），传给 Runner。
- PlanUpdate 事件（generate_plan 后推 plan_update 给前端）。

## 工作顺序
1. 写计划。
2. plan 工具 + ToolContext.plan。
3. Runner plan_mode 接入 + denial 门 + FakeLLM 测试。
4. dss-api 传 plan_mode + plan_update 事件。
5. cargo build/test 绿 + commit。

## 不做（P6b / 后续）
- verify（reviewer checkpoint + terminal barrier）。
- delegate / submit_output。
- frames 表落库。
- /api/sessions/{sid}/approve 端点（P6a 只到 awaiting；approve 后继续 run 留 P6b）。
