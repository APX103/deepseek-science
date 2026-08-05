# 数据模型与存储

> **本文回答**：SQLite schema 长什么样？消息/会话/记忆/artifact 怎么建模？本项目内 schema 如何演进？

> 状态：schema 大部分已定；frames 落库与迁移细节待定

---

## 总则

- **存储引擎**：SQLite，WAL 模式，`foreign_keys=ON`，`busy_timeout=5000`。
- **位置**：`<data_dir>/dss.db`（data_dir 倾向 `~/.deepseek-science`，见 [architecture 数据落点](architecture.md#数据落点)）。
- **时间**：UTC，RFC3339 序列化。
- **ID 约定**：session `uuid4()[:12]`；frame/artifact `uuid4()`；memory `mem_<12hex>`；project `proj_<8hex>` 或 `proj_default`。
- **JSON 列**：存 `serde_json::Value` 文本。

---

## 表结构

本项目 schema 聚焦运行时实际读写的字段，**不保留当前未充分使用的列**（`token_class_usage`、`aux_*_tokens`、`aux_cost`、`specialists_used`、`mentioned_artifact_ids`、`compute_enabled`、`effort`、`status_description`、`fold_cue` 等）。

### 1. `projects`
```
id            TEXT PK                -- proj_default | proj_<8hex>
name          TEXT NOT NULL
description   TEXT
last_session_id TEXT
archived      INTEGER NOT NULL DEFAULT 0
created_at    TEXT NOT NULL
updated_at    TEXT NOT NULL
```
默认项目 `proj_default` 启动时确保存在。

### 2. `sessions`（用户视角的会话）
```
id            TEXT PK                -- uuid4()[:12]
title         TEXT
workspace     TEXT NOT NULL
model         TEXT
plan_mode     INTEGER NOT NULL DEFAULT 0
status        TEXT NOT NULL DEFAULT 'active'
discoverable  INTEGER NOT NULL DEFAULT 1  -- 0: 保留并可按 ID 恢复，但不出现在发现列表
project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL
plan_data     TEXT                    -- PlanState 序列化快照（恢复用）
created_at    TEXT NOT NULL
updated_at    TEXT NOT NULL
```
索引：`(status)`、`(discoverable)`、`(updated_at)`、`(project_id)`。

`discoverable=0` 只影响项目详情和全局会话列表；`GET /sessions/{sid}`、消息、run、workspace
及原项目上下文均保留。该字段用于可恢复地隔离开发/验收夹具，禁止按标题或模型输出字符串过滤。

### 3. `session_messages`
```
id            INTEGER PK AUTOINCREMENT
session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE
seq           INTEGER NOT NULL       -- 会话内单调递增
run_id        TEXT REFERENCES session_runs(run_id) ON DELETE SET NULL
role          TEXT NOT NULL          -- system|user|assistant|tool
content       TEXT NOT NULL          -- JSON: content blocks 数组
harness_notice INTEGER NOT NULL DEFAULT 0   -- ★ 显式列
created_at    TEXT NOT NULL
```
索引：`(session_id)`、`(session_id, seq)`。
> 决策：`harness_notice` 升为显式列，避免污染 content JSON。API 输出仍按契约输出顶层字段（见 [api-contract](api-contract.md#harness_notice-持久化与往返关键)）。

远端 A2A Agent 的完整可观察返回复用这张表：assistant 的 `tool_use` 与紧随其后的
`tool_result` 仍按调用 id 配对，`tool_result.content` 保存版本化
`dss.a2a.tool-result.v1` JSON（调用 action + 调用时 Agent Card + 每个完整 wire 响应 + 终态）。
`submit` 的非终态 Task handle 会随短调用立即落库；恢复后可用独立 `get_task` 工具调用只读查询，
不会重新发送原 Message。每次后续查询仍是一组正常配对的 `tool_use/tool_result`，所以整条跨重启
Task 时间线能按 session id 恢复，无需伪造 A2A 内部事件。响应超过显式上限时调用在接收下一项前失败，
已经接受的完整项仍保留；不允许静默截断后伪装成“全部返回”。
远端 `INPUT_REQUIRED`/`AUTH_REQUIRED` 保存为非错误的 `task_interrupted`，其中
`task_id/context_id/state` 是继续同一 Task 的稳定 handle；UI 不把它归类为完成或失败。旧会话中
曾以 `kind=task, success=false` 保存的相同标准 state 仍按可续接中断解释。
运行时工具实例还维护不落库的 per-run 副作用门：同一 user run 最多尝试一次 Send/Submit；查询或取消
已有 Task 后不得创建新 Task，只有相同 `task_id` 的可续接中断允许 follow-up。该门不替代数据库中的
Task handle，而是阻止模型纠错循环产生第二个远端副作用。

### 3.1 `session_runs`

`session_runs` 保存每轮的稳定 `run_id/ordinal/frame_id/status/kind/awaiting/pending_ask_json/plan_data`
及 `start_seq/end_seq`。首次完成工具批次时先创建 `completed_at=NULL` 的 provisional run，并在同一事务中
写入该批消息、会话 plan/status 和消息范围；后续工具批次更新同一行，终态事务再补齐 token、迭代数、
错误与 `completed_at`。应用启动时，未完成且仍为 `processing` 的 provisional run 标为
`interrupted`；已落到 plan/user awaiting 的 provisional run 保留可操作状态。这样崩溃恢复不会出现
“工具轨迹存在但计划/提问状态丢失”，也不会为一个 run 重复分配 ordinal。

### 4. `memories`（三层记忆）
```
id            TEXT PK                -- mem_<12hex>
entity        TEXT NOT NULL DEFAULT 'project'   -- 兼容旧字段（= scope）
scope         TEXT                   -- profile|project|frame
entity_type   TEXT NOT NULL DEFAULT 'note'      -- claim|evidence|citation|tool_use|note
body          TEXT NOT NULL          -- ≤1000 字符
evidence      TEXT NOT NULL DEFAULT 'stated'
origin        TEXT NOT NULL DEFAULT 'user_stated'
frame_id      TEXT
session_id    TEXT
project_id    TEXT REFERENCES projects(id) ON DELETE SET NULL  -- profile scope 强制 NULL
confidence    REAL NOT NULL DEFAULT 0.5
meta          TEXT                   -- JSON
created_at    TEXT NOT NULL
updated_at    TEXT NOT NULL
last_surfaced_at TEXT
```
索引：`(entity)`、`(frame_id)`、`(entity_type)`、`(session_id)`、`(project_id)`。

### 5. `artifacts`（逻辑产物，多版本）
```
id            TEXT PK
project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE
root_frame_id TEXT NOT NULL
frame_id      TEXT
filename      TEXT NOT NULL
latest_version_id TEXT REFERENCES artifact_versions(id) ON DELETE SET NULL
is_user_upload INTEGER NOT NULL DEFAULT 0
is_branch_mint   INTEGER NOT NULL DEFAULT 0
is_ephemeral     INTEGER NOT NULL DEFAULT 0
consumed_at   TEXT
folder_id     TEXT
sort_order    INTEGER NOT NULL DEFAULT 0
priority      TEXT NOT NULL DEFAULT 'unknown'
superseded_by_artifact_id TEXT
created_at    TEXT NOT NULL
```

### 6. `artifact_versions`（物理内容 + 谱系）
```
id            TEXT PK
artifact_id   TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE
version_number INTEGER NOT NULL
frame_id      TEXT
content_type  TEXT NOT NULL
size_bytes    INTEGER NOT NULL
checksum      TEXT NOT NULL           -- sha256
storage_path  TEXT NOT NULL
created_at    TEXT NOT NULL
extracted_code TEXT
code_description TEXT
lineage_messages TEXT                 -- JSON
agent_name    TEXT
language      TEXT
is_intermediate INTEGER NOT NULL DEFAULT 0
parent_version_id TEXT
is_checkpoint INTEGER NOT NULL DEFAULT 0
```
唯一约束：`(artifact_id, version_number)`。

### 7. `artifact_dependencies`（DAG 边）
```
id                   TEXT PK
version_id           TEXT NOT NULL REFERENCES artifact_versions(id) ON DELETE CASCADE
depends_on_version_id TEXT NOT NULL REFERENCES artifact_versions(id) ON DELETE CASCADE
ref_name             TEXT NOT NULL DEFAULT ''
created_at           TEXT NOT NULL
```
唯一约束：`(version_id, depends_on_version_id, ref_name)`。

### 8. `verification_checks`（reviewer 裁决）
```
id            TEXT PK
root_frame_id TEXT NOT NULL REFERENCES frames(id) ON DELETE CASCADE
artifact_version_id TEXT
claim_id      TEXT
claim         TEXT
verdict       TEXT NOT NULL          -- pass|warn|fail|inconclusive
severity      TEXT
evidence      TEXT
rebuttal      TEXT
reviewer_idx  INTEGER
reviewer_model TEXT
reviewer_frame_id TEXT
reviewer_kind TEXT
source_ref    TEXT NOT NULL
status        TEXT NOT NULL DEFAULT 'open'   -- open|resolved|unaddressed
reflag_count  INTEGER
created_at    TEXT NOT NULL
```
索引：`(root_frame_id)`、`(root_frame_id, status)`、`(claim_id)`。

### 9. `compaction_archives`（Rolling Compact 归档）
```
id            TEXT PK
frame_id      TEXT NOT NULL REFERENCES frames(id) ON DELETE CASCADE
compaction_index INTEGER NOT NULL
fold_kind     TEXT NOT NULL          -- rc_fold_l1|rc_fold_l2|compact_destructive
summary       TEXT NOT NULL
archived_messages TEXT NOT NULL      -- JSON: 被折叠的原始消息
created_at    TEXT NOT NULL
```
索引：`(frame_id)`。

### 10. `frames`

> 待定（见下）

```
-- 候选 schema（若决定落库）
id            TEXT PK
parent_frame_id TEXT REFERENCES frames(id) ON DELETE SET NULL
root_frame_id  TEXT REFERENCES frames(id) ON DELETE SET NULL
agent_name    TEXT NOT NULL          -- MAIN|SUBAGENT|REVIEWER|BOOKMARKER
delegate_name TEXT
status        TEXT NOT NULL DEFAULT 'processing'
model         TEXT
input_tokens  INTEGER
output_tokens INTEGER
cache_read_tokens INTEGER
cache_write_tokens INTEGER
project_id    TEXT REFERENCES projects(id) ON DELETE CASCADE
name          TEXT
conversation_type TEXT NOT NULL DEFAULT 'agent'
is_hidden     INTEGER NOT NULL DEFAULT 0
task_summary  TEXT
created_at    TEXT NOT NULL
updated_at    TEXT NOT NULL
completed_at  TEXT
last_user_message_at TEXT
```

### 日志表（本项目新增）

本项目新增 `logs` 表承载日志列表功能，完整定义见 [logging 日志系统](logging.md#数据模型)。字段：`id/ts/level/source/system|agent/kind/session_id/frame_id/iteration/message/detail(JSON)/trace_id`，索引 `(ts)`/`(session_id,ts)`/`(level)`/`(source,kind)`。默认保留 N 天自动清理。

### frames 是否落库

> 待定

**运行时**：frame 树存内存（`FrameService._frames` dict），会话恢复靠 `session_messages` 重建。

**选项**：
- **A. 不落库**：frame 树纯内存，session 恢复靠 `session_messages` + `plan_data` 重建 root frame。简单，但进程崩溃丢 frame 运行时态（消息已持久，只是 frame status 丢失）。
- **B. 落库**（改进）：frame 状态持久化，崩溃可恢复 frame status。但 `verification_checks`/`compaction_archives` 有 FK→frames，需 frames 存在。

**倾向**：**B（落库）**。因为 `verification_checks` 和 `compaction_archives` 已引用 `frames.id`，落库让外键有效；且崩溃恢复是长程自主研究（增强方向）的需求。

---

## 迁移

### 本项目内 schema 演进

**inline 迁移 runner**（结构化设计）：

```rust
// 伪码
struct Migration { id: &'static str, up: fn(&Connection) -> Result<()> }
let migrations = [ /* 编号化步骤 */ ];
for m in migrations {
    if !applied(m.id)? {           // presence check（列/索引/表存在性）
        m.up(conn)?;               // ALTER / CREATE
        mark_applied(m.id)?;
    }
}
```

每步 presence check（`PRAGMA table_info` / `sqlite_master`），失败不阻断启动、下次重试。失败记 `tracing::warn!`。

> 决策：**不引入 Alembic 式版本化迁移工具**。项目早期 inline 够用；若 schema 频繁变更再评估 `refinery` / `sqlx migrate`。

---

## 数据落点（对照 [architecture](architecture.md#数据落点)）

```
<data_dir>/                          # 倾向 ~/.deepseek-science（待定）
├─ dss.db                            # 主库
├─ settings.json                     # AppSettings
├─ workspaces/{sid}/                 # 会话工作区
│  ├─ main.tex / references.bib / …
│  └─ .venv/                         # 工具 install_packages 建的 venv
├─ skills/                           # 用户 skill（首跑复制 builtin）
├─ logs/
└─ trace/{session}/{trace}.jsonl     # 可选 trace
```

---

## 并发与写入

- SQLite 单写：所有写经 `deadpool-sqlite` 单连接或 `spawn_blocking` 串行化。WAL 允许并发读。
- `busy_timeout=5000` 应对偶发锁竞争。
- 长 agent run 中消息持久化是**增量**的：每个已交付工具批次写一个消息+run 状态检查点，终态只追加
  其后的文本并更新同一 run。`(session_id, seq)` 唯一且连续；启动迁移会修复旧库中的重复或间断序号。

---

下一步：读 [enhancements 增强方向设计预留](enhancements.md)。
