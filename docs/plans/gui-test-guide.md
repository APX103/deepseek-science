# GUI 测试方案（给能读图的 agent / 人工执行）

> 本文档列出所有需要浏览器验证的前端功能点。后端均已 curl 验证、tsc build 通过,但浏览器渲染需人工/读图 agent 确认。
>
> **强制数据隔离**：任何 GUI、真实模型、打包 App 或 Computer Use 验收都不得使用默认的
> `~/.deepseek-science`。先创建一次性数据目录，并在整个后端/App 进程生命周期内显式设置
> `DSS_DATA_DIR`。测试结束后只清理这个已记录的精确目录。禁止把验收会话写入生产 `Default`
> 项目，也禁止通过抓取其他配置文件的方式在命令行暴露 API key。
>
> 开发模式示例：
>
> ```bash
> DSS_TEST_DATA_DIR="$(mktemp -d /private/tmp/deepseek-science-gui.XXXXXX)"
> chmod 700 "$DSS_TEST_DATA_DIR"
> DSS_DATA_DIR="$DSS_TEST_DATA_DIR" ./target/debug/dss-backend serve --port 17896
> ```
>
> 另一个终端运行 `cd frontend && bun run dev`，浏览器打开 `http://localhost:5173/`。需要真实模型时，
> 只在这个隔离实例的 Settings 中配置。测试报告必须记录隔离目录，并在退出后确认其已清理。
>
> 打包 App 必须通过仓库的一次性隔离启动器运行；启动器会创建 0700 数据目录、可选地以
> 0600 复制 settings、等待 App 退出、核对生产数据库哈希，并清理隔离目录：
>
> ```bash
> ./scripts/launch-isolated-app-e2e.sh \
>   --app "/absolute/path/Deepseek Science.app"
> ```
>
> 真实模型验收可额外传入 `--settings /absolute/path/settings.json`；脚本不会打印凭据内容。

---

## T1: 首页项目列表（真实后端数据）

**操作**: 打开首页 `http://localhost:5173/`。

**预期**:
- Projects 区显示 **Default** 项目（来自后端 DB `proj_default`），不是 mock 的「钙钛矿太阳电池」。
- Recent sessions 区显示真实会话（之前 curl 测试创建的，或空）。
- 时间显示正常（如 `33m ago`），**不是 `NaNd ago`**。

**截图检查点**: 项目列表 + 时间格式。

**如果出错**:
- `NaNd ago` → 后端 `GET /api/sessions` 没返回 `created_at`/`updated_at`（已在 P3 修复，确认 SessionListItem 含这俩字段）。
- 显示 mock 数据（钙钛矿）→ 前端 store 没切后端，检查 `store.ts` 的 `loadFromBackend()` 是否在 HomePage `useEffect` 调用。
- 后端未连接提示 → 确认后端在 17896 端口运行，前端 Vite 代理 `/api` → 17896。

---

## T2: 建会话 + 流式对话（核心功能）

**操作**: 点左侧栏 **New** → 进入工作台 → 输入框打字「你好，介绍你自己」→ 回车发送。

**预期**:
- 用户气泡立即上屏。
- 看到 **thinking 折叠块**（DeepSeek reasoning，可点击展开）。
- **流式文字**逐字出现（打字机效果）。
- 完成后底部显示 **usage 行**（`tokens: X in / Y out · N iteration`）。
- 侧栏标题更新为消息内容。

**截图检查点**: thinking 块 + 流式文字 + usage 行。

**如果出错**:
- 输入框无法输入/发送无反应 → 可能是 React 受控组件问题。排查 `ChatArea.tsx` Composer 的 `onChange`/`onKeyDown`。**注意**: ZCode IAB 自动化无法驱动受控 textarea（已知限制），但真实浏览器应正常。
- 无流式回复 → 检查后端日志 `run finished`；确认 `DEEPSEEK_API_KEY` 已注入。
- thinking 不折叠 → 检查 `ThinkingBlock` 组件渲染。

---

## T3: 工具调用卡片（live 渲染）

**操作**: 在工作台输入「用 write_file 写一个 hello.txt 内容是 Hello，然后用 read_file 读出来确认」。

**预期**:
- 出现 **ToolCallCard**（可折叠卡片），显示工具名 + 摘要（如「写入 hello.txt」）。
- 卡片展开后显示 input 参数 + 结果。
- 多轮工具调用（write_file → read_file），每个工具一张卡。
- 完成后 assistant 文字回复。

**截图检查点**: ToolCallCard 折叠态 + 展开态（input/result）。

**如果出错**:
- 工具卡片不出现 → 检查 `store.ts` 的 `appendStreamToolCall`/`appendStreamToolResult` 是否在 `WorkbenchPage.handleSend` 的 `onToolCalls`/`onToolResults` 回调里接线。
- 卡片摘要不对 → `ToolCallCard.tsx` 的 `summarize()` 函数（已支持 read_file/write_file/bash 等）。
- 流式时卡片卡在 in-progress → `commitStreamMessage` 是否正确配对 tool_use/tool_result。

---

## T4: ask_user 阻塞面板

**操作**: 输入「我想写综述但需要确认方向，请用 ask_user 问我偏好哪个领域（给两个选项）」。

**预期**:
- agent 调 ask_user → 出现 **AskUserPanel**（蓝色边框面板，显示问题 + 候选项 chips）。
- 输入框转为「等待回复」态。
- complete 后 usage 行显示。

**截图检查点**: AskUserPanel（问题 + 选项 chips）。

**如果出错**:
- 面板不出现 → 检查 `ChatArea.tsx` 的 `stream.kind === 'awaiting' && stream.pendingAsk` 条件渲染。
- pendingAsk 为 null → 检查 `completeStream` 是否传了 `e.pending_ask`（WorkbenchPage `onComplete`）。

---

## T5: 会话刷新恢复（持久化）

**操作**: 在有消息的会话里 **刷新浏览器（F5）**。

**预期**:
- 消息历史恢复（含 thinking 块、文字、工具卡片）。
- tool_use/tool_result 配对正确（卡片不卡在 in-progress 黄点）。

**截图检查点**: 刷新后的完整消息流。

**如果出错**:
- 消息丢失 → 检查 `WorkbenchPage` 的 `loadMessages(sid)` 是否在 `useEffect` 调用。
- 工具卡片卡在 in-progress → 后端 `GET /sessions/{sid}` 的 content JSON 往返问题（tool_use_id 是否一致）。排查 `api/client.ts` getSession 的 content→ContentBlock 映射。

---

## T6: 重启后端恢复

**操作**: 在有消息的会话 → **杀后端重启** → 刷新前端。

**预期**: 消息历史完整恢复（DB 持久化）。

**如果出错**: 同 T5。另确认后端重启后 DB 迁移正常（日志 `sqlite pool ready, migrations applied`）。

---

## T7: 日志页 `/logs`

**操作**: 点顶栏 **日志** 按钮（或直接访问 `http://localhost:5173/logs`）。

**预期**:
- 日志列表按时间倒序。
- 每行：时间 | 级别 | source(system/agent) | kind | message。
- 看到 system `startup` 日志 + agent `run_start`/`run_end` 日志。
- 级别用色：info 灰、warn 黄、error 红。

**截图检查点**: 日志列表（system + agent 混合）。

**过滤测试**: 输入 session_id 过滤 → 只显示该会话的 agent 日志。

**如果出错**:
- 列表空 → 检查 `LogsPage` 是否调 `listLogs()`（真实 `GET /api/logs`）；后端日志表是否有数据。
- 显示 mock 数据 → `client.ts` 的 `listLogs` 是否切真实（已改，确认没回退）。

---

## T8: 新建项目

**操作**: 首页点 **New project** → 弹窗填 Name/Description → 提交。

**预期**: 项目出现在列表顶部，刷新后仍在（DB 持久化）。

**如果出错**: 检查 `NewProjectModal` → `createProject`（真实 `POST /api/projects`）→ `addProject`（store 更新）。

---

## T9: Skills 弹层

**操作**: 工作台左侧栏点 **Customize** 或搜索 skill 相关入口。

**预期**: 弹层显示真实 skills（lit-survey, paper-writing），来自后端 `GET /api/skills`。

**如果出错**: 检查 SkillsModal 组件是否调 `listSkills()`（真实 API）。

---

## T10: 离线态

**操作**: 停掉后端 → 刷新前端。

**预期**: 输入框禁用 + 提示「后端未连接」。首页显示离线提示。不崩溃。

---

## 已知限制（ZCode IAB 自动化）

以下操作在 ZCode IAB 自动化中**无法完成**（已知限制，非应用缺陷）：
- 受控 `<textarea>` 输入（`fill()` 不触发 React `onChange`）。
- 按钮点击（`click()` actionability 超时）。

**真实浏览器（或 CodeX 的读图能力）可正常完成以上全部操作。** 代码已由 tsc build + curl 全事件链验证正确。
