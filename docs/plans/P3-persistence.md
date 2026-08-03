# P3 — 持久化与 session（实施计划）

> 对应 [roadmap P3](../roadmap.md#p3--持久化与-session)。状态：**进行中**（2026-08-03）

## 目标

刷新浏览器/重启后端，会话不丢；多 session 管理；projects CRUD。

**验收点**：
1. `cargo build` 无警告。
2. 发几轮对话（含工具）→ **重启后端** → `GET /api/sessions/{sid}` 恢复完整消息历史（含 tool_use/tool_result 块，顺序正确）。
3. projects 全套端点按 api-contract；默认项目 `proj_default` 启动时确保存在且不可删/不可 archive。
4. `DELETE /api/sessions/{sid}` 删会话（DB + workspace）。
5. `GET /api/sessions` 返回 `live: bool`。
6. harness_notice 显式列往返（顶层输出，P3 暂都 false）。
7. P2 不回归（新建会话仍能多轮工具调用）。

## 行为基线

- 存储：SQLite（rusqlite + deadpool-sqlite），`<data_dir>/dss.db`，WAL/FK/busy_timeout=5000。
- inline 迁移：presence-check → CREATE TABLE IF NOT EXISTS；失败 warn 不阻断启动。
- schema（P3 子集）：`projects` / `sessions` / `session_messages`。其余表留对应阶段。
- SessionManager：内存活跃 session（ActiveSession）+ DB 持久化；MAX_ACTIVE_SESSIONS=10 LRU（超限仅驱逐内存，DB 仍在）。
- 增量持久化：run 结束时把 session.messages 里 DB 还没有的新增消息批量写库（内存 `persisted_seq` 游标）。
- 恢复：`GET /sessions/{sid}` 内存无 → 从 DB 读 session_messages → 重建 `Vec<ChatMessage>` → 进内存 map。
- 消息序列化：存「OpenAI 协议形态」JSON，serde 直存/直取回 ChatMessage；tool_use_id 透传 LLM call id，与前端 loadFromState 两遍重建一致。

## 任务清单（todo）

- [ ] workspace 加 rusqlite/deadpool-sqlite/chrono；`dss-core` 加 `time.rs::now_rfc3339()`。
- [ ] 新建 `dss-db`：Pool（WAL/FK/busy_timeout）+ inline 迁移 + schema（projects/sessions/session_messages）+ 仓储层（projects CRUD / sessions CRUD / append_message / list_messages）+ DbError。
- [ ] `dss-api`：AppState 加 `db: Arc<Pool>`；build_state 启动迁移 + ensure_default_project。
- [ ] create_session 落 DB；stream_sse run 结束增量持久化；GET sessions/{sid} 恢复；DELETE sessions/{sid}；list_sessions 加 live 标记；LRU 驱逐。
- [ ] projects 全套端点。
- [ ] `cargo build` 无警告。
- [ ] curl 验收全点。
- [ ] 回填本文件「回顾」段；frames 落库决策登记 decisions。

## 回归点

- 重启后端 → GET sessions/{sid} → messages 完整（含 tool_use/tool_result），顺序正确，tool_use_id 一致。
- projects：建/改名/archive/unarchive/详情；默认项目 archive/delete → 400；非空项目 delete 非 force → 409。
- DELETE sessions/{sid} → 再 GET → 404；workspace 目录删除。
- list_sessions：活跃 session live=true，仅 DB 的 live=false。
- P2 回归：新建会话多轮工具调用仍正常。

## 风险

- 增量持久化边界：run 结束批量写；中途取消（客户端断开）丢已 push 部分（与 P1 cancel 一致，登记）。
- ChatMessage 往返：tool_calls/tool_call_id 无损往返（serde JSON 直存取）。
- rusqlite/deadpool-sqlite 首次编译稍慢，正常。

## 回顾

**实际做了什么**：
- 新建 `dss-db` crate：自定义用 `deadpool-sqlite` 的 Pool（Config.builder + post_create 钩子设 PRAGMA：WAL/foreign_keys/busy_timeout）；`run_migrations` 建 P3 子集表（projects/sessions/session_messages，CREATE IF NOT EXISTS 幂等）；仓储层（`repo.rs`，同步函数，经 `conn.interact` 调用）覆盖 projects/sessions/messages 的 CRUD + `append_message`（seq 自增）+ `list_messages`（按 seq）；`DbError`（Sqlite/Pool/BuildError/NotFound/Conflict/Other）。
- `dss-core` 加 chrono + `time.rs::now_rfc3339()`。
- `dss-api` 加 `db.rs`（异步封装，`conn.interact` 在 spawn_blocking 跑同步 repo）；`AppState` 加 `db: Arc<DbPool>` + `sessions: Arc<Mutex<HashMap<String, Arc<ActiveSession>>>>`；`build_state` 改 async（open pool → run_migrations → ensure_default_project）；`ActiveSession{session, persisted_count: AtomicUsize}`（游标并发安全）。
- session 持久化：create_session 落 DB；stream_sse run 结束批量增量写 `session.messages[persisted_count..]` 为 JSON（OpenAI 协议形态，serde 直存）；`GET /sessions/{sid}` 内存无则从 DB `list_messages` 反序列化回 `Vec<ChatMessage>` 恢复（强制以 DB role 为准）；`DELETE /sessions/{sid}`（DB cascade + workspace + 内存）；`list_sessions` 加 live 标记；LRU MAX_ACTIVE_SESSIONS=10（仅驱逐内存）。
- projects 全套端点（list/create/patch/archive/unarchive/delete?force/detail）；默认项目不可 archive/delete → 409；非空项目 delete 非 force → 409。
- `cargo build` 全 workspace 无警告。

**验证结果（curl + 真实 DeepSeek + 真实重启）**：
- **重启恢复（核心验收点）**：跑斐波那契多轮工具任务（3 iteration、2 对 tool_calls/tool_results）→ `pkill` 后端重启 → `GET /sessions/{sid}` 恢复出 6 条消息（user/assistant/tool/assistant/tool/assistant，顺序正确），assistant 消息带 `tool_calls:[write_file/bash]`、tool 消息带 `tool_call_id`（与前端 loadFromState 两遍重建一致），harness_notice 顶层 `false`。✅
- projects CRUD 全套：create(proj_8e7d7c0b)/patch 改名/archive/unarchive/list archived/delete；**默认项目 archive/delete → 409**；delete new → 204；get detail → 200。✅
- delete session → 204；GET deleted → 404；workspace 目录删除。✅
- **live flag 往返**：跑完后 live=True；重启后（未触达）live=False；GET 后恢复进内存。✅
- 增量持久化：run finished 日志 `persisted_new=4`（user+assistant+tool+assistant）；title 从 final_text 设置。✅
- P2 回归：新建会话 python 工具多轮（3**5=243，2 iteration）正常。✅

**偏离**：
- deadpool-sqlite 的 PRAGMA 经 `Config.builder().post_create(Hook::async_fn)` 注册（而非自定义 Manager）：rusqlite::Connection 非 Send，deadpool-sqlite 的 SyncWrapper + interact 是处理它的正确方式；post_create 钩子设 PRAGMA 是官方推荐路径。登记 decisions 无（属实现细节）。
- LRU 驱逐用 HashMap 取 `keys().next()`（P3 简化，非真正 LRU 顺序）；MAX_ACTIVE_SESSIONS=10 触发时驱逐一个非当前 session。真正 LRU（按 last-used 排序）留后续优化。

**遗留（→ decisions.md）**：
- frames 不落库（P3 靠 session_messages 恢复 root frame 状态；P6 verification/compaction FK 需要 frames 表时再落，data-model 选项 B）。
- run 中途取消（客户端断开）丢已 push 但未持久化的部分（与 P1 cancel 一致，因持久化在 run 结束批量写）。
- harness_notice 注入点（门控）留 P2b-gates，P3 该列暂都 false。
- plan_data 恢复留 P6（plan 工具）。
- 真正 LRU 排序留优化。
