# Orca（onorca.dev）竞品调研报告

> **本文回答**：Orca 是什么？它的产品/功能模型长什么样？有哪些设计可以借鉴到 Deepseek Science？
>
> **调研日期**：2026-08-05。来源：[https://www.onorca.dev/docs](https://www.onorca.dev/docs)（当日快照）。
> **注意**：Orca 处于**活跃开发中**（RC 构建 often daily），功能与文档持续演进，本文是时间点快照；引用关键设计前建议回官网核对最新语义。

---

## 0. 摘要（一页看懂）

Orca 是一个 **Y Combinator 投资的桌面 IDE（GitHub 37.8k star，Stably 出品）**，定位 "The worktree IDE for AI coding agents"——**在桌面上并行运行多个 AI 编码代理**（自带 Claude Code / Codex / Cursor CLI / GLM 等 30+ 种 CLI agent），核心卖点是：

1. **隔离**：每个任务一个真实 `git worktree`，并行 agent 互不踩文件；
2. **编排**：Run/Task/Dispatch/Message/Decision gate 五原语的多 agent 编排，支持依赖、并行、人工决策门；
3. **审查**：diff 查看器 + 逐行 AI 内容标注 + 行级归属溯源（哪些代码是 AI 写的）；
4. **恢复**：守护进程托管 agent 进程，关应用不杀任务，重开热重连；空闲休眠、会话历史恢复；
5. **可见性**：agent 状态（工作/等输入/完成/阻塞）处处可见，通知收件箱 + 动态流 + 移动端遥控；
6. **远程**：SSH worktree / 自托管 Orca Server / 云 VM recipe 三种远程执行，本地只留编辑与审查。

**它不是**：模型（自带各家订阅）、git 替代品（全是真 git worktree）、托管 VPS（远程用你自己的机器）。

与 Deepseek Science 的本质差异：Orca 是"**编排外部 CLI agent 的 IDE**"，不运行推理、不自研 agent kernel；DSS 是"**自研 agent kernel + Deepseek 推理引擎**"。因此可借鉴的不是其技术实现（PTY/OSC 检测），而是**交互模型与状态管理设计**。

**借鉴优先级速览**（详见 §16）：

| 优先级 | 借鉴点 | 对应 DSS |
|--------|--------|----------|
| 高 | 编排五原语 + dispatch ID 作用域 | P12 长程研究 / P5b 论文链 |
| 高 | 用量/限速本地记账 + 阈值预警 | P10 Deepseek 深化 |
| 高 | 轻量 checkpoint（状态评论而非快照） | P12 research_runs |
| 中 | 会话休眠/恢复的分层回退语义 | P4 compact / P12 |
| 中 | 段落级 AI 归属溯源 | P5 论文链 |
| 中 | 定时自动化 + precheck 探针 | P12 |
| 中 | Agent 状态可见性（Needs You 看板） | F2 日志页延伸 |
| 低 | worktree 目录隔离哲学 | P9 沙箱对照 |
| 低 | 记忆格式生态兼容（AGENTS.md 导入） | P4 记忆 |

---

## 1. 产品定位

- **一句话**：桌面 IDE，用隔离 + 编排 + 审查 + 恢复，让"同时跑多个 AI 编码代理"这件事可控。
- **公司**：Stably / Lovecast Inc.，YC 投资，开源（GitHub 37.8k star），桌面应用免费，自带各家 agent 的订阅（BYO model）。
- **目标用户**：以写代码为生、愿意把 AI 当杠杆（"not as a replacement"）的开发者。
- **官方强调"不是什么"**：
  - 不是模型——"bring your own Claude, Codex, or OpenCode subscription"；
  - 不是 git 替代品——每个 worktree 都是真实 git worktree，可随时 `cd` 进去用普通 git，外部 `git worktree remove` 也会被识别；
  - 不是托管 VPS——默认在桌面运行，远程能力走 SSH / 自托管服务器 / 云 VM（BYO 账号与账单）。
- **平台**：macOS（Apple Silicon + Intel，签名公证）/ Windows（.exe，默认 shell 可配 PowerShell/CMD）/ Linux（AppImage + .deb）。Homebrew：`brew install --cask stablyai/orca/orca`。
- **更新机制**：默认自动更新 stable 渠道；RC 构建"often daily"，可通过修饰键点击 Check for Updates 获取（Shift=最新 RC，Cmd=最新 perf 预发布，Option=兼容性验证的本地构建）。降级不会强制降级 worktree 数据。

---

## 2. 核心抽象：worktree 模型

**定义**：每个任务拥有独立磁盘目录（真实 git worktree），分支、文件、agent 终端、浏览器标签全部按 worktree 隔离。官方："every task gets its own on-disk copy of the repo via git worktree."

### 2.1 生命周期

```
Create → Work（终端/编辑器/浏览器全作用域于此 worktree）→ Review（diff 相对 start-from ref）
→ Ship（commit / push / 开 PR）→ Archive / Delete（一键删目录+分支）
```

### 2.2 创建流程

- **Add Repo**：把本地 checkout 加入侧边栏，读取 git 状态，把默认分支设为 **base ref**（新 worktree 都从它切出，可后期改）。
- **创建**：仓库旁 `+` → 输入任务名（留空则自动用海洋生物名，如 `orca-worktree-42`）→ 启动器预选默认 agent。
- **Start-from picker**：可从 base ref / 另一个本地分支（叠在审查中的 PR 上）/ 特定 commit SHA / 已存在的远程分支（自动 fetch）开始。
- **创建是异步的**：对话框立即关闭，fetch + `git worktree add` 后台执行，带实时进度、可取消、失败可 Retry。

### 2.3 隔离的代价与三种补丁机制

全新 worktree 是干净 checkout，**gitignore 的依赖/缓存/密钥都不在**。三种互补方案：

1. **Per-user shared paths**（设置里配置）：macOS 尽量用 APFS clone-copy，否则符号链接；
2. **`orca.yaml` 的 `worktree.sharedDirectories`**（检入仓库）：符号链接共享目录；
3. **`.worktreeinclude`**：按 worktree 复制（如 `.env`）。

### 2.4 管理 UI

- 侧边栏按项目分组；筛选（sleeping / default-branch / automation 创建 / detached HEAD）；pinning；拖拽排序；Cmd/Shift 多选；内联重命名。
- **Cmd-J Jump Palette** 跨所有 worktree/标签页搜索跳转（详见 §12）。
- "Unread worktrees are bolded rather than badged"（未读加粗而非徽章）。
- 仓库图标可自定义（emoji / 上传图片 / favicon / GitHub 头像）。

---

## 3. Agent 会话模型与状态可见性

**定义**："one CLI agent running in one terminal in one worktree." 会话状态被跟踪，无需点进标签页即可知道谁在工作。

### 3.1 生命周期（4 阶段）

- **Launch**：从 combobox 选 agent，Orca 以正确 cwd 启动 CLI；
- **Work**：终端状态经 OSC title 实时更新；
- **Idle**：检测 working→idle 转换，触发 agent-finished 通知；
- **Exit**：进程结束，出现 **Restart chip** 一键以相同 cwd 重启动（Codex 还保留账户）。

### 3.2 状态指示器（跨标签页与 worktree 行共享）

| 指示 | 含义 |
|------|------|
| Spinner | 工作中 |
| 琥珀色问号 | 等你输入（权限/提问） |
| 翠绿勾/点 | 完成 / 静默活跃 |
| 红点 | 阻塞、中断或失败 |
| 灰点 | 空闲 |
| 无指示 | 普通 shell，非受支持 agent |

### 3.3 Agent Dashboard（实验性）

Kanban 看板：**Needs You / Working / Done / Idle**（Idle 默认隐藏）列。卡片含 agent 图标、会话名、消息预览、项目/worktree、年龄、琥珀/绿色底色；点击卡片聚焦实时终端；子 agent 显示为可展开子项。支持搜索与过滤。

### 3.4 默认全自主哲学

- 每个支持的 agent 默认以**完整自主**启动（Claude 用 `--dangerously-skip-permissions`，Codex 用 `--dangerously-bypass-approvals-and-sandbox`，Gemini/Cursor/Crush/Kimi 等用 `--yolo`）。
- 理由是："the worktree itself is the sandbox"——worktree 是一次性的，之后可以 cherry-pick 或丢弃 diff。
- Settings → Agents → Agent Permissions 可全局切 Yolo / Manual；但**手动覆盖过的启动参数视为显式覆盖**，不受全局开关迁移。

### 3.5 状态检测机制

状态从"终端的 OSC title 序列 + agent hooks"推断（Claude Code、Codex 等会发出 working/idle 标题）。若没有指示器，多半是用户手敲二进制而非经 combobox 启动。自定义 CLI agent 只要输出 OSC title 即可自动获得状态点。

---

## 4. 会话持久化与恢复

### 4.1 Session restore（守护进程架构）

- **后台守护进程拥有 PTYs**：关掉 Orca 窗口**不会杀死**正在运行的 agent；下次启动热重连（warm-reattach）同一批进程，滚动缓冲（含关闭期间产生的输出）保留。
- 恢复在**每次启动**时执行；没有"新会话"模式——想要干净起点需显式关闭 worktree。
- **保存的状态**：侧边栏打开的工作树、每个 worktree 的标签页与分屏布局（含嵌套、焦点）、运行中的 agent 进程、终端滚动缓冲、退出时聚焦的 worktree/标签页。
- **两条恢复路径**：
  - 守护进程存活（Cmd-Q、自动更新重启、Orca 崩溃）→ agent 继续运行，重启热重连；
  - 守护进程随宿主机死亡（重启/断电）→ agent 进程丢失，但布局与最后持久化的滚动缓冲仍恢复。

### 4.2 Agent hibernation（实验性，暂停而非终止）

**问题**：很多 worktree 常开时，空闲 agent 是"live PTY holding a model session in memory"，内存累积。

**机制**：
- 满足全部条件才休眠：agent 已完成回合、worktree 非活跃、完成后无按键无新输出、agent 支持可恢复会话（Claude/Codex/Gemini 等）、空闲超阈值（默认 30 分钟，可配 1min–24h）、无移动端驱动、无待处理编排派发、无子 agent 附加。同一 worktree 的终端一起暂停。
- 重开时：以相同 resume flag（如 `claude --resume <id>`）+ 原始启动命令与环境**自动恢复**，无需点击。
- **失败回退**：resume 失败则回到新 prompt，旧 transcript 留在历史。
- 与 kill 的区别：kill 销毁进程；休眠保留可恢复会话。不可恢复的 agent（Cursor CLI、Hermes、Copilot 等）保持运行不休眠。

### 4.3 Agent Session History（历史会话面板）

- 右侧栏 Agents 标签页；**直接扫描各 CLI 留在磁盘的 transcript**（Codex `~/.codex/sessions`、Claude `~/.claude`、Cursor 日志、OpenCode `~/.local/share/opencode/opencode.db`），零额外启用。
- 统计标题（"12 shown · 47 recent"）+ 搜索（按标题/cwd/分支/模型/对话预览）。
- **Scope 切换**：Workspace / Project / All。远程工作区可浏览但只能本地恢复。
- 视图选项：按 agent 开关扫描、按 Last updated/Created 排序、按 Project/Folder/Agent 分组、隐藏空会话。
- **Resume**：在新终端以相同 cwd + 会话 ID 运行恢复命令（`claude --resume <id>` / `codex resume <id>` / `pi --session <file>` 等）；也可拖拽会话行到工作区。行内菜单：Copy resume command / Copy session ID / Copy log path / Open log / Reveal log / Open cwd。
- **限制**：Pi 依赖 hooks 报告的磁盘会话文件（非裸 ID），文件缺失则不可 Resume；恢复只能在本机（远程需 Copy command 自行执行）。

---

## 5. 并行与编排

### 5.1 Orchestration（实验性，结构化多 agent 层）

**五原语**：

| 原语 | 职责 |
|------|------|
| **Run** | 持久命名空间 + 家庭收件箱；自己不调度任何工作 |
| **Task** | 工作项：spec + 依赖 + 状态机（pending → ready → dispatched → completed / failed / blocked） |
| **Dispatch** | 任务在某个终端上的一次执行尝试；**拥有完成/心跳的生命周期权威** |
| **Message** | 收件箱邮件：状态、升级（escalations）、问题、心跳等 |
| **Decision gate** | 协调者持有的问题，**阻塞任务直到被解决** |

**首选工作流**：
1. `run-create` 建命名空间；
2. `task-create`（带 spec）；
3. `worker-start` 把任务放到 agent 上（当前或新 worktree，本地或远程 `--on windows`）；
4. `check --wait` / `check --ack` FIFO 处理消息；
5. worker 用 `worker_done --outcome succeeded|failed` 汇报。

**关键机制**：
- **Worker 契约**：恰好一次 `worker_done`（失败也要），必须带 task + dispatch ID；长任务发心跳。
- **陈旧重试安全**：完成以 dispatch ID 作用域，过期的重试**不能错误完成新派发**。
- 多 agent 寻址：`@all` / `@idle` / `@claude` / `@codex` 等组寻址。
- 受监督提问：worker 用 `ask` 阻塞式提问；协调者用 `gate-create` / `gate-resolve` 做 DAG 决策。
- 恢复工具：`dispatch-show` / `task-list` / `task-update` / 全局 `reset`。
- 终端里打印的 task ID 可点击，直达实时派发。
- 指导原则：一次性提示用 `terminal send`；需要回报与跟踪用 dispatch；持久受监督多 agent 循环用 Run+Task+workers。
- 旧 `orchestration run` 命令已退役为 no-op。

### 5.2 Scheduled automations（定时自动化）

- 命令面：`orca automations create/list/show/edit/remove/run`。
- 触发：presets（hourly/daily/weekdays/weekly）、cron 表达式、RRULE；`--timezone`（IANA）；`--missed-run-grace-minutes` 处理迟跑。
- **`--precheck`**：先跑廉价 shell 探针，失败则跳过本次调度（记录 skipped run）。
- `--reuse-session` 续用旧 automation 终端；`--fresh-session` 切回。
- `--disabled` 先创建禁用态调参，`run` 手动触发测试，再启用（安全分阶段上线）。
- 目标可指定 `--repo` / `--workspace` / 自动解析当前目录；`--project` / `--host` 支持多主机。

### 5.3 Worktree checkpoints（轻量状态评论，非快照）

- 每个 worktree 有一个 **free-text 评论字段**（UI 可见），作为"这个 worktree 在干什么"的状态快照。
- agent 用 CLI 更新：`orca worktree set --worktree active --comment "..." --json`，可带 `--workspace-status todo|in-progress|in-review|completed`。
- **约定**：先 `orca worktree current --json` 读取再写，避免覆盖用户写下的目标/约束；首行写"刚发生了什么、在哪、状态/下一步"，保留仍有效的部分、丢弃过期的。
- **解决的问题**：让人类协作方不被打断就知道进度；记录关键转折（实现完成、假设证实/证伪、审查完成、卡点、从调查转修复、从修复转验证）。
- 注意：官方明确这是**注释/状态约定，不是快照恢复机制**。

---

## 6. 代码审查与发布

### 6.1 Diff viewer

- 每个 worktree 内置相对 start-from ref 的**合并 diff**（含未跟踪文件）。
- 对比基准可切到任意提交/分支/base ref。
- 图片 diff：并排 / 滑动 / 洋葱皮三种模式；三路冲突视图 + 行内解决。
- **按块或按行暂存**（可视化 `git add -p`）。
- 可折叠文件树（宽度会话间记忆）；Word wrap 默认关（Settings → General → Diff Word Wrap）。
- 快捷键：`j`/`k` 文件、`n`/`p` hunk、`F7`/`Shift+F7` 编辑器内变更、`s` 暂存 hunk、`c` 评论。

### 6.2 Annotate AI Diff（内联审查循环）

- 悬停行槽出现 `+`（或按 `c`）→ 输入 Markdown 评论（Cmd-Enter 保存，Esc 取消）。
- **评论 pin 到行**，diff 内容偏移时跟着行移动。
- **批量发送**：审完点 "Send to agent"，全部评论合并为**单个带行锚点的 prompt**；选目标 agent（可现场启动新 agent）。
- 为什么批量：逐条发送让 agent "swing back and forth"；批量 = "one round of thinking, one revision pass"，命中率更高。
- Agent 修改后：评论仍固定，可验证修复；**Resolve 折叠线程**；再次 Send 时**未解决评论自动包含**进下一批。
- "Send Review Notes to Agent" 默认不绑快捷键（可自配）；评论要写完整句子效果最好。

### 6.3 Attribution（归属）

- **本地追踪** AI 代理写入文件的行范围；审 diff 时 AI 生成的行在 gutter 标记。
- **人类编辑 AI 代码会把归属翻回人类**。
- 用途：一眼看出哪些出自 AI 值得重点审查；安全/合规审计的 AI/人类区分；加快评审。
- 只存本地、**不写进 git**；可导出元数据（diff 工具栏）。

### 6.4 Commit & push

- 面板位于 diff 旁：审查 → 暂存（hunk/文件）→ 提交 → 推送 → 继续。
- **Generate with AI** 自动起草提交信息；**Generate pull request details with AI** 起草 PR 的 base/标题/描述/草稿状态。
- pre-commit hooks 照常运行，失败内联显示输出；**Fix with AI** 把失败交给 agent（拿到钩子输出 + 尝试的提交信息 + 暂存文件；agent 不会被要求绕过钩子/提交/推送）。
- Push 自动设 upstream；**Force push 是显式独立操作**（"Force push with lease"），按钮标注将被替换的提交数与上游分支名；用 `--force-with-lease` 防覆盖他人提交。
- **Amend 是显式操作**，已推送的提交不自动 amend。
- **Action recipes（每仓库 AI 操作配方）**：每个 AI 操作（Generate/Fix/Resolve）由 recipe 定义——选哪个 agent、CLI 参数、提示词模板；Settings → Git & Source Control → Action recipes 编辑，可全局默认或限定仓库；模板变量如 `{basePrompt}` `{branch}` `{stagedFiles}` `{stagedPatch}` `{linkedIssue}` `{baseBranch}` `{commitSummary}` `{patch}`。仓库覆盖优先于全局默认。
- 冲突：merge/rebase/cherry-pick 留下冲突时，Source Control 提供 "Resolve with AI" 或手动，可 Abort。

### 6.5 Hosted reviews、issues 与 CI

- 连接：Settings → Integrations（GitHub OAuth 最深、GitLab MR/issues、Bitbucket/Azure DevOps/Gitea PR 出现在侧栏与 Checks 面板，建 worktree 前检查远程冲突）。
- PR/MR：推送后从 Source Control 创建，确认 base 分支/标题/描述/草稿；侧栏显示状态（open/merged/closed）；PR actions 菜单（复制链接/关闭/重开）；checks/reviews/comments 内联打开；**Checks 面板可回复线程中任意评论**（不限于根评论）。
- **Auto-merge**：满足 checks/required reviews 后自动合并；merge queue 时显示 "Merge when ready"；按仓库默认 merge 方法；draft/冲突/不稳定 PR 隐藏。
- **Fix broken checks**：把失败 check 名/链接交给 agent。
- **Issues drawer**：浏览/过滤/编辑 GitHub/GitLab issue；从 issue 建 worktree 预填任务名并关联；issue 详情含 Activity 时间线（assignments/mentions/cross-refs/状态变更）。
- **GitHub Projects 视图**：跨仓库浏览卡片、按源仓库过滤、从卡片建 worktree。
- GitHub API Budget 显示：本地 `gh` CLI 剩余 REST/Search/GraphQL 配额（PR checks 或 Tasks 停刷新时有用）。

### 6.6 Linear / Jira items drawer

- 与 GitHub issues 合并为**单一 task drawer** 视图；Orca 按仓库记住上次使用的任务源（GitHub/Linear/Jira）。
- **Linear**：Settings → Integrations 贴 personal API token → 选 team(s)。功能：从 issue 建 worktree（预填名称、附 issue ID；Linear 暴露分支名时直接用其建议名）；issue 详情可改 status/assignee/priority/labels/estimate；**从 issue 启动 agent 时，描述/评论/sub-issues 里的内联图片与媒体自动进 prompt 上下文**（免手动贴截图）；新建 issue 对话框误关后同会话恢复草稿；状态同步（建 worktree 时移 "In Progress"）按 team 逐项 opt-in。
- **Jira**：支持 Cloud（站点 URL + 邮箱 + API token）与 Self-hosted Server/DC（PAT 或 用户名+密码 Basic，多站点）。功能：统一列出 GitHub/Linear/Jira issue；worktree 卡片显示 key + summary + "View on Jira"；经 available transitions 改状态、内联编辑 priority/assignee/custom fields；从 issue 建 worktree；**workspace 名称框直接贴 issue URL 建工作区**；按仓库记忆任务源。凭据经 OS keychain 加密、仅本地。
- 局限：无批量操作、无 sprint/board 视图、无 JQL 构造器（仅文本搜索）。

---

## 7. 编辑器与查看器

### 7.1 Monaco 编辑器

- 定位："Orca is intentionally editor-first, not IDE-first"——类型检查器与 linter 需在终端面板跑，内置不提供。
- **自动保存**：失焦（blur）时保存 + 短暂空闲期后保存；没有 dirty dot，因为正常流程下不存在未保存更改。
- 快捷键：`Cmd-D` 选中下一个相同出现、`Cmd-F` 文件内查找（有选区时自动带入）、`Cmd-Shift-F` **worktree 级查找**、`Cmd-Click` 跳转到定义（语言扩展支持处）、`Alt+Z` Word Wrap。
- **Changes 视图模式**：任意编辑器标签页可切换为"标签页内 HEAD 与工作区对比 diff"，不丢光标位置；`n`/`p` 浏览 hunk、`s` 暂存。
- 设置：Editor Word Wrap 默认开（与 Diff Word Wrap 相互独立）；Minimap 默认关；Editor Font Family 默认留空（留空则编辑器/diff 沿用终端字体）。
- 限制：语法高亮仅覆盖 Monaco 开箱支持的语言；无补全/格式化说明。

### 7.2 富 Markdown 编辑器

- Markdown 默认在富编辑器打开；`Cmd-Shift-M` 切换原始 Monaco 视图。slash menu、toolbar、图片与代码内联预览、内部链接自动补全。
- **Slash 菜单**：空行输入 `/`，含 headings/lists/code blocks/callouts/images/mermaid/toggle blocks；`/toggle-text`、`/toggle-h1~h5` 创建可折叠笔记，**保存为可移植的 `<details>/<summary>` markdown**（Orca 之外也能预览）。
- **Wiki 式内部链接**：`[[` 触发自动补全 worktree 内文件路径并插入相对链接。
- 搜索基于**渲染后文本**而非原始 markdown（搜 "Install" 能命中 `# Install` 与 `<h1>Install</h1>`）。
- **评审批注**：选中渲染文本即可加批注（无需切回原始 markdown），绑定到所选源文本范围，继续编辑仍显示；默认 `Cmd+Shift+A`（可重映射）。
- **Front matter**：YAML/TOML 默认同时显示于富编辑器与渲染预览，More actions 菜单可逐文件 Hide/Show。
- 表格键盘行为（Tab/Shift-Tab 移动、Enter 下行、Backspace 删行等）；**TOC**：长文档头部树形图标钉住标题大纲，可跳转/折叠。
- 未提及：数学公式、脚注、分屏预览、导出格式。

### 7.3 查看器（五种内置）

- **Mermaid**：markdown 预览内联渲染；独立 `.mmd` 在专用查看器打开，平移/缩放。
- **PDF**：滚动/缩放/文本选择；切走再切回恢复滚动位置（精确到"页 + 页内偏移"，仅会话内有效，重启清除）。
- **图片**：png/jpg/svg/webp/gif；**image-diff 模式**并排比较同一文件两个版本。
- **CSV/TSV**：表格查看器，列可排序 + 快速搜索；工具栏切回原始文本视图直接编辑单元格。
- **Jupyter Notebook（.ipynb，Beta）**：渲染 markdown、语法高亮代码单元、保留已保存输出；编辑写回磁盘且**保留 nbformat（"diffs stay clean"）**；cell 执行与富输出渲染仍在调优。

### 7.4 文件浏览器

- 每个 worktree 左侧，实时跟踪磁盘（agent 等外部改动即时出现）；创建/重命名/删除/移动均映射真实文件系统。
- **外部拖拽**：Finder/Explorer 文件拖到文件树复制进来；图片拖进 markdown 编辑器插入光标处；**文件拖到 agent 终端 = 路径粘贴到提示符**（SSH worktree 先上传再显示真实远程路径）。
- **Git 状态着色**：untracked / modified / staged / ignored。
- 右键菜单：discard / stage / rename / Copy Path / **Copy Relative Path**（默认 `Cmd+Option+Shift+C`）/ Copy（单文件到 OS 剪贴板；SSH worktree 先本地 stage 远程文件；远程文件夹不参与 Copy）/ Download / Download Folder（远程专属）/ Find in Folder（文件夹右键，`Cmd-Shift-F`）。
- 限制：Download 系列仅远程 SSH worktree 的桌面端可用，Web 客户端不可用。

---

## 8. 浏览器与 Design Mode

### 8.1 Per-worktree browser

- 每个 worktree 独立浏览器："a real Chromium window — address bar, history, devtools — embedded in a pane"。
- **标签按 worktree 隔离**，切 worktree 恢复自己的标签与滚动位置。
- 会话持久化，可**一键从 Chrome/Edge 导入 cookies** 保持登录。
- **Link Routing**（Settings → Browser）：终端/markdown/编辑器里的 http(s) 链接默认在 Orca 浏览器还是系统浏览器打开；Shift+Cmd/Ctrl+点击 临时反转；来自 Remote/SSH 源的链接永不进 Orca 浏览器。
- 视口模拟：CDP 设备模拟，`window.innerWidth` 与媒体查询可见模拟尺寸。
- 下载栏：取消/打开/在文件夹显示。
- 同一浏览器同一标签可被 agent 经 CLI 脚本化（`orca snapshot` / `click` / `fill`）。

### 8.2 Design Mode（pointer-to-code）

三步循环：
1. 工具栏开 Design Mode，光标变 picker，悬停高亮元素；
2. **点击元素** → 捕获（外部 HTML + 一小片周边 DOM + 计算后 CSS + 裁剪截图 + dev-mode 有 sourcemap 时含源文件/行号）作为**一个富附件**送入当前 agent；
3. 输入修改指令 → agent 改源码 → 热重载 → 再点元素验证。

解决的问题：把"描述 UI 问题"从模糊自然语言变成精确上下文传递，免去复制 HTML/CSS/截图/找源文件，压缩"发现 bug → 修复 → 验证"回路。

### 8.3 Browser-use profiles

- 用途：让浏览器以特定身份运行——"a logged-in user, a particular cookie jar, a custom user-agent"；适合 agent 需要登录、复现会话相关 bug、模拟多用户。
- 创建/管理：Settings → Browser → Profiles → Add profile → 命名；可选注入 cookies、user-agent、viewport size。
- 使用：从浏览器工具栏选 profile；"All tabs in that pane use it until you switch"；agent 驱动的浏览器命令继承当前激活 profile。
- **隔离**：每个 profile 独立存储分区（cookies / local storage / cache），"Profiles don't leak into each other"。
- 与 per-worktree browser 配合：worktree 内导 cookies 保持登录，profiles 固定不同身份/UA。

---

## 9. 终端

- 与 VS Code 同源（xterm.js），首次启动可导入 Ghostty 主题/字体/光标配置。
- 终端本质是标签页，融入 tabs/panes/splits 体系；终端内部还可继续分屏（Split terminal right/down）。
- **OSC 52**：Zellij/tmux/Neovim/fzf 等经 OSC 52 写系统剪贴板，默认允许（远程/SSH 下复制行为一致），可在 Settings → Terminal 关。
- **Copy Context**：右键复制该 pane 的有界转录内容，便于把 agent 最近输出贴到别处。
- Cmd-F 滚动缓冲搜索：匹配高亮、大小写、正则、导航。
- 主题：内置主题库 + 自定义 + **Ghostty 导入 + Warp 主题导入**（自动发现 ~/.warp/themes 等路径，或从任意 YAML 文件夹导入）。
- Windows 默认 shell：PowerShell / CMD / WSL（`wsl.exe --status` 成功自动提供）。
- 快捷键：Cmd-T 新终端、Cmd-Alt-T 默认 agent 新标签（每 agent 还有各自直达快捷键）、Cmd-W 关标签、Cmd-\ 右分屏、Cmd-Shift-\ 下分屏。
- **原生键绑定**：通告 kitty keyboard protocol，终端应用可识别真实 Shift+Enter/Ctrl+Enter。
- **浮动终端**（Floating Workspace）：全局 shell 面板，任何 worktree 上一键唤起（Cmd+Option+A），自带标签页，可跑后台任务/编排；起始目录可配。
- **Quick Commands**：保存常用终端命令或**可复用的 agent 提示词**（启动时提示型 agent 用），作用域 Global / Project；入口 Settings → Quick Commands 或标签栏 Add command；可插入当前终端或新开标签；同步到移动端。

---

## 10. 远程与部署（"Every worktree runs somewhere"）

| 模式 | 文件/Agent 位置 | 机器归属 | 适用 |
|------|----------------|----------|------|
| 本地 | 桌面 | 你 | 日常、快速迭代 |
| SSH 目标 | 远程主机 | 你/团队 | GPU 机、开发机、常开 VPS |
| 远程 Orca Server | 跑 Orca 桌面版或 `orca serve` 的机器 | 你/团队 | 持久共享运行时、移动端、自动化 |
| 云 VM / per-workspace env | 每个 workspace 一次性 VM/沙箱 | 你的云账号（BYO） | 隔离、临时 agent 计算 |

四种可混用（本地快速编辑 + SSH 跑 GPU + recipe 做 CI 式隔离）。

### 10.1 SSH worktrees

- **agent 与 git worktree 在远程，编辑器/diff/浏览器本地**（文件事件同步保持本地体验）。
- 设置：Settings → SSH → Add Target（host/user/port/identity file，从 OpenSSH config 导入，Test 再 Save）；Advanced：代理/跳板机/多路复用覆盖；**Reuse SSH connection** 默认开。
- Passphrase 仅存内存（可配 TTL）；密钥类型自动分流：普通 Ed25519/ECDSA/RSA 走内置传输，**GSSAPI/Kerberos 与 FIDO2 硬件密钥走系统 OpenSSH**。
- 状态灯：绿=已连接、黄=重连中、红=断开。**断开不杀 agent**，自动重连重附加；agent 状态经 SSH 实时传播。
- **应用关闭后会话存活**：远程 PTY 经 relay **租约（lease）**，关应用不杀；重开重连后租约 PTY 恢复到原标签页（含完整 scrollback）；默认 5 分钟宽限期等快速重连（可 per-target 配）。
- 端口转发：Ports 标签（Cmd+Shift+I）扫描远程 `/proc/net/tcp` 列出监听端口一键转发；转发规则跨重启/重连保留；特权远程端口自动重映射本地非特权端口。
- 文件下载：右键 Download / Download Folder（需完整 SFTP；仅系统 OpenSSH 传输的连接只能下文件；Web 客户端无下载）。
- 可 "Open in → VS Code" 走 Remote-SSH（`--remote ssh-remote+<host>`）。
- Linux 主机缺 C/C++ 工具链时：文件/git/编辑器仍可用，远程终端不可用直到装好（relay 需原生 node-pty，常见发行版安装命令文档给出）。

### 10.2 Remote Orca Server（自托管，Beta）

- 架构：**一台机器持有完整运行时**（项目、worktree、终端、agent 进程、供应商账号、会话），客户端只是 UI（桌面、浏览器、移动端、自动化）。
- 推荐路径：两端装 Orca + Tailscale 同一 tailnet；服务端 Advertise → New Link（选 Tailscale 地址）→ 生成配对链接（"Treat it like a password"）；客户端 Add Server 粘贴。
- **安全模型**：每个配对客户端独立可撤销 token；Shared Server Access 列出并可立即撤销；不建议公网直暴露，优先 Tailscale/WireGuard/可信 LAN/SSH 转发。
- 无头主机：`orca serve --pairing-address <tailscale-ip>`（前台运行，Ctrl-C 停；`--port` 固定端口；`--mobile-pairing` 给移动端）；"Use only one host mode at a time"。
- 注意：**服务器上的登录/凭据不随笔记本带过去**——Codex、Claude 等要在服务端安装并认证；无头环境用 `orca account add --agent claude` 等命令行注册。
- 对比 SSH：SSH 是笔记本驱动远程机器；Server 是远程机器持有整个会话状态，支持多客户端共享。

### 10.3 Cloud VM / per-workspace environments（实验性）

- 每个 worktree 可从仓库检入的 recipe（`orca.yaml` + 生命周期脚本）启动自己的按需环境：云沙箱/VM/本地 Docker。
- "Create spins it up; suspend/resume/destroy tear it down"——Orca 只是薄封装，提供商账号/镜像/账单都是你的（BYO cloud, not an Orca VPS）。
- 已接入：Vercel Sandbox、Fly、Modal、普通 SSH 主机、本地 Docker。
- 连接方式两种：Orca Server（recipe 启动 `orca serve` 返回配对 URL）或 SSH（返回连接详情）。
- 设置流程：启用 Cloud VM → 装 per-workspace-env skill → 让 agent 跑 "Use the orca-per-workspace-env skill to set up a per-workspace environment for this repo." → skill 依次处理前置条件 → 基础快照 → agent 认证 → `orca.yaml` recipe → doctor 验证 → 出现在 Recipes，建 worktree 时 Run on 选择。
- recipe 只在**主 checkout** 的 `orca.yaml` 有 `environmentRecipes` 条目时出现（feature 分支不算）。

---

## 11. CLI、Skills 与自动化

### 11.1 Orca CLI（驱动正在运行的 Orca 编辑器）

- 定位：从任意 shell/脚本/agent **脚本化一个正在运行的 Orca 编辑器**；Settings → General → Orca CLI 注册启用。
- **Selectors** 代替长 ID：`id:<repoId>` / `active` / `current`（从 shell 当前目录解析）/ `path:/abs` / `branch:name` / `issue:123`；远程运行时用 `id:<repoId>::<abs-path>`。
- 命令面：
  - **Repo**：`repo list/add/show/set-base-ref/search-refs`
  - **Worktree**：`worktree list/ps/current/show/create/set/rm`；create 带 `--agent` + `--prompt` 直接派活；子 worktree 自动记录 parent（`--parent-worktree` / `--no-parent`）；`--setup run|skip|inherit`
  - **Terminal**：`terminal list/show/read/send/wait --for tui-idle --timeout-ms`（光标分页 `--cursor`）/ `create/split/rename/switch/close`；handle 是 runtime-scoped，重启后需重新 list
  - **File**：`file open/diff/open-changed --mode both`
  - **Browser**：`goto` → `snapshot`（返回 `@e1` 元素引用）→ `click` / `fill` / `wait --text` → `screenshot`；`tab list/create/switch`；`capture start` / `console` / `network` / `full-screenshot` / `pdf`；`set device --name "iPhone 12"` 响应式检查；快照在导航/切标签/页面变化后需重取
  - **Computer use**：`computer list-apps / get-app-state / click / set-value / type-text / press-key / hotkey / paste-text / scroll / drag / perform-secondary-action`；元素索引作用域于最近一次 get-app-state（稀疏）；多窗口用 `--window-id`；敏感输入走 `--value-stdin` 防 shell history；`get-app-state` 默认返回 accessibility tree + 截图（`--no-screenshot` 加速，`--restore-window`）
  - **Emulator（iOS 模拟器）**：`emulator list/attach/tap（归一化坐标）/type/gesture/button/rotate/exec/kill/shutdown`；桥接随 worktree 生命周期
  - **Linear**：`linear issue/search/list/list-issues/team/project/save-issue/relation/status/assignee/priority/estimate/due-date/label/comment/attach/create`（MCP 风格读写；`--current` 解析关联 worktree；save-issue 的 label 是整体替换）
  - **Automations**：`automations list/create/run`
  - **Environment**：`environment add/list/rm`（配对远程运行时）
  - **Account**：`account list/add`（在宿主终端跑 `claude login` / `codex login`，Codex 用 device authorization 可跨机完成）
  - **Agent hooks**：`agent hooks status/on/off`
- **Agent 使用习惯建议**（官方）：优先 `--json`；优先 selector 而非解析 UI 标签；发输入前先读终端状态；用 worktree comment 做进度 checkpoint；跟踪式多 agent 派发用 Orchestration 而非临时终端提示。

### 11.2 Skills registry & MCP

- 随应用发布 skills（`orca-cli` / `orchestration` / `computer-use` / `orca-linear` / `orca-emulator` / `orca-emulator-android` / `orca-per-workspace-env`），安装到 agent 的 skill 目录：`npx skills add https://github.com/stablyai/orca --skill <name> --global`。
- **混合 stub 架构（关键设计）**：公开安装包是短 `SKILL.md` 发现桩，指示 agent：解析 CLI 可执行文件 → `orca skills get <topic>` 加载**与版本匹配的完整指南** → "Prefer --json and do not invent flags from memory"。原因："command flags live in the binary so they cannot drift from the app version"——指南从运行中的 CLI 实时出，文档与安装版本永不漂移。
- 更新：应用内 updater（后台 `npx skills update`，不占终端；状态栏显示进度；关闭对话框不取消）；无头环境 `orca skills install/update`（`--all --dry-run`、`--agent claude-code,codex`、默认 `--global` 可 `--local`）。
- 发现源：自动扫描 Claude / Codex / Agent Skills / OMP 的 skill 目录。
- 自定义 skill：任何含 `skills/<name>/SKILL.md` 的仓库可 `npx skills add`（公司内部技能）。
- **MCP**：Settings → Integrations → MCP 注册 server，工具出现在支持 MCP 的 agent CLI 里。

### 11.3 Computer use（桌面应用控制，Beta）

- agent 经 CLI 检查/操作**原生桌面应用**：list-apps → get-app-state（accessibility tree + 截图）→ click/set-value/type...（语义化操作比裸打字稳，"survive focus changes"）。
- 需要 Accessibility 权限（macOS 另需屏幕录制），授予 "Orca Computer Use"。
- 与内置浏览器的分工：浏览器管 web，computer use 管 OS/第三方应用。

---

## 12. 导航、通知与移动

### 12.1 Quick Open 与 Jump Palette

- **Quick Open（Cmd-P）**：当前 worktree 内搜文件，按"最近使用 + 匹配分数"排序；gitignored 文件作为第二遍排在已跟踪匹配之后（构建产物/env 不顶到列表上方）。
- **Jump Palette（Cmd-J）**：一个搜索框跨所有 worktree/标签页跳转。结果类别：最近 worktrees（空查询，按最后聚焦排序）→ 项目与仓库分组 → 按仓库分组的 worktrees → 缓存的 PR/MR 匹配（`#123` / `!123`）→ 打开的标签页（先当前 worktree 再全局）。**Shift-Enter 在新 split 打开**；无匹配时显示 "Create worktree" 行（但有真实匹配时 Enter 优先跳转）。

### 12.2 Notifications & Inbox

- 洞察：Orca 能区分"agent 真干完了"与"只是暂停"。
- **Agent-finished 触发**：working→idle 转换时三信号齐发——系统通知 + 声音 + worktree 上的 chip。
- **持久铃铛**：头部铃铛聚合跨所有 worktree 的未读通知；点击跳转到对应 worktree 与 pane；macOS Dock 徽章镜像未读数。
- 右键可标回未读（"triage 了但想稍后回看"）。
- Settings → Notifications：按类别开关（system/sound/chip-only）；**每类别自定义通知声音**（指向任意音频文件，MP3/WAV/OGG/M4A/AAC/FLAC，可调音量）；PR 检查失败、更新可用通知。

### 12.3 Agents feed（动态流）

- 侧栏 Agents 入口打开**跨所有 worktree 的线程化 agent 事件流**：agent 完成、阻塞提问、未读、worktree 创建等。
- 运行中的 agent **固定在顶部**；按状态分组；事件含最近回复预览；点击跳转 worktree+pane；新事件侧栏徽章；Cmd-F/Ctrl-F 聚焦过滤器。
- 定位：离开后回来补进度的统一 triage 入口，不替代铃铛/系统通知。

### 12.4 Orca Mobile（移动伴侣，Beta）

- iOS/Android，**"桌面端的遥控器"而非完整编辑器**；配对一次性，桌面始终是数据源；无云中继，关桌面即断开，重开自动重连。
- 能做的：看所有 worktree/agent 状态（多主机统一视图）、浏览文件树、Chat UI 或原始终端读会话、加载最近终端滚动内容、长按选择/复制/粘贴、附件栏（Tab/Shift+Tab）、**Live 模式逐字符直通终端**、回复 continue/yes/自由文本/照片/语音转文字、跑 Quick Commands、切 agent 账户看用量、Codex 重置额度消耗（防重复消耗）、手机建 workspace、Source Control（暂存/提交/Link existing PR）、浏览器 Web/Mobile 视图、agent 完成推送通知。
- 终端设置：文本缩放 50–200%、自动补全/自动更正默认关（防系统改写命令）。

---

## 13. 设置、隐私与插件

> 主要分类已在 §1–§12 各节内联。补充：设置可按窗格分组，`Cmd-,` 搜索，且**搜索匹配多语言词**（中文"语言"等）；`~/.orca/keybindings.json` 存自定义快捷键覆盖；Appearance 含主题/强调色/密度/字体/minimap/状态栏（Resource Manager：CPU/内存/会话/守护进程控制）/Usage percentages（已用% 或剩余%）；App Icon 三套循环；UI 语言含简体中文。

### 13.1 Privacy & Telemetry

- **每个事件的基础字段**：Orca 版本、OS、CPU 架构、粗略 OS 版本、发布渠道、**匿名本地 ID**（随机生成存本机）。明确无主机名、无用户名、无 IP。
- 六类行为观察，全部"**无自由文本**"：
  1. 生命周期——应用打开（仅估 DAU/WAU/MAU）；
  2. 仓库与工作区——只记添加方式（选择器 vs clone URL），**从不记仓库名/URL/路径/分支名/任何自由文本**；
  3. Agent——启动时记 agent 种类（固定枚举）与启动位置；从不记 prompt/模型/输出；
  4. Agent 错误——只记粗略错误类别；原始错误/堆栈只在本地诊断文件，仅显式分享诊断包时传出；
  5. 设置——白名单内开关/偏好的切换，只记哪个变了、布尔还是枚举；
  6. 隐私控制——遥测开关事件本身。
- 关键保证："No free-form strings from any UI input ever leave your machine."
- **关闭方式（任一）**：Settings → Privacy 关开关（立即且持久）；`DO_NOT_TRACK=1` 环境变量；`ORCA_TELEMETRY_DISABLED=1`。
- 数据去向：PostHog Cloud 美国区，默认保留期，少数维护者可见。未提及与 AI 提供商的任何数据共享。

### 13.2 Plugins（实验性）

- Settings → Plugins：先开启系统，再逐个审查并启用；**未经同意不运行任何插件**。
- Marketplaces：添加 git marketplace 源、浏览、预览能力（面板/命令/语言包/VM recipes）、安装/更新/回滚。
- 插件 worker 始终跑本机；SSH 工作区操作经 Orca 路由。能力与 API 形状可能变更；第三方插件视为不可信软件。

### 13.3 故障排查要点（摘录）

- Agent 不启动：手动在终端跑该 CLI 验证认证/安装；确认 CLI 在 Orca 可见 PATH。
- Diff 视图卡住：点刷新图标（两次刷新间发生了外部 git 操作，如 rebase/reset）。
- Worktree 创建失败：start-from ref 未拉取（`git fetch origin`）或目标目录已有该分支的 worktree。
- `orca` command not found：Settings → General → Orca CLI 注册（macOS shim 在 `~/.local/bin`，确保在 PATH）。
- SSH 能连但远程终端失败：远程需 Node 与网络（首次 relay 安装）；Linux 缺 C/C++ 工具链时装 `make`/`g++`/`python3`。
- 浏览器 `browser_no_tab`：当前 worktree 没有打开的标签页，用 `orca tab create --url ...`。
- 性能：关闭不用的 worktree（每个都持有文件 watcher）；多浏览器标签的分屏布局是最大 RAM 占用。
- 日志：Help → Open Logs；反馈：Help → Send Feedback / GitHub Issues / Discord。

---

## 14. 内置 Agent 集成方式

### 14.1 支持的 agent 与集成深度

- 机制本质："the agent combobox just launches a process in a terminal"——**任何 CLI agent 都能用**；combobox 里预配置 30+ 家（Claude Code、Codex、Cursor CLI、Gemini、GLM-5.2、Kimi、Qwen Code、MiniMax、OpenCode、Aider、Goose、Cline、Copilot、Grok、Devin、Droid、Hermes、OpenClaw、Trae 等），部分有深度集成（状态/用量/账户切换/hooks）。
- 深度集成示例：Claude Code（usage、hot-swap、hooks）、Codex（usage、hot-swap）、Cursor CLI、OpenCode/Pi/OMP/Antigravity/Droid/Command Code（status）、MiniMax（usage + rate-limit tracking）。
- Claude Agent Teams：默认禁用，启用后 `orca claude-teams` 启动，每个队友有原生 pane。

### 14.2 自定义 CLI agent

- 4 步：Settings → Agents → Add custom agent → 填 name / binary path or command / default arguments（可选 startup hook，如 `source .envrc`）。
- 获得：出现在每个终端 combobox；cwd 恒为当前 worktree；退出时 restart chip；输出 OSC title 即有状态点。
- 也可直接用 combobox 跑纯 bash/zsh（带 worktree 上下文的受限 shell）。

### 14.3 账户热切换（Codex / Claude）

- 场景：多账户跑满 token 配额。状态栏 chip 下拉切换，**只重写凭据指针，不重新认证**，瞬时生效。
- 账户添加：先在终端登录各账户一次（凭据落 `~/.codex` / `~/.claude`）→ Settings → Agents → Accounts 列出 + 显示各账户用量/限额 + 设置友好名。
- 隔离：托管账户跑在隔离 home，不污染系统默认登录；"System default" 行即宿主机真实登录。
- **已运行会话保持原账户直到重启**；重启保留的是重启时刻的活动账户。
- 配置同步：从真实 `~/.codex/config.toml` 镜像到托管账户；源文件缺失/为空时保留最后一次成功同步的配置并显示警告。

### 14.4 第三方模型集成方式（GLM-5.2 / Codex）

**GLM-5.2 的集成方式（对 DSS 最有参考价值的一页）**：GLM-5.2 **不是 Orca 内置 agent**，而是"通过既有 CLI agent harness 驱动"——在 Claude Code、OpenCode、Cline、Kilo Code、Roo Code、Droid、OpenClaw 等 harness 中把模型配置换成 GLM-5.2，再从 Orca 的 agent picker 启动。Orca 只提供 worktree 隔离/终端/browser/审查/会话管理；**模型访问由用户自己的 Z.ai 订阅与 harness 配置决定**（"Orca does not include or resell GLM access"）。

- **Claude Code 配置**（`~/.claude/settings.json` 的 `env` 块）：`ANTHROPIC_DEFAULT_HAIKU_MODEL=glm-4.5-air`、`ANTHROPIC_DEFAULT_SONNET_MODEL=glm-5.2[1m]`、`ANTHROPIC_DEFAULT_OPUS_MODEL=glm-5.2[1m]`、`CLAUDE_CODE_AUTO_COMPACT_WINDOW=1000000`（`[1m]` 后缀启用 1M 上下文变体，compact 窗口必须同步 1M）。
- **OpenAI 兼容 harness**（OpenCode/Cline/Kilo/Roo/Droid）：Base URL `https://api.z.ai/api/coding/paas/v4` + Z.ai API key + 模型名 `glm-5.2` + 上下文窗口 1000000；除非官方文档明确支持图片否则禁用图片输入。
- **OpenClaw**：`~/.openclaw/openclaw.json` 三处修改（providers.zai.models 注册、默认主模型 `zai/glm-5.2` + fallback、agents.defaults.models 注册），`openclaw gateway restart` 生效。
- 任意其它 harness：Settings → Agents → Add custom agent 指向 harness 二进制即可。核心原则："configure GLM-5.2 wherever the harness stores provider/model settings."
- 启示：**DSS 也可以走"OpenAI 兼容 endpoint"被其他 harness 复用**（或反过来，DSS 支持外部模型配置），Orca 的做法证明"IDE 与模型解耦、模型藏在 harness 配置里"是成立的产品形态。

**Codex 集成**：
- 基础：按 OpenAI 官方安装登录，Orca 直接读本地 `~/.codex`（无 CLI 检测/代理安装）；Windows 可跑 WSL 内的 Codex（凭据经 `\\wsl.localhost\...` 映射回宿主）。
- 深度集成：账号热切换（见 §14.3）、**Nested Task 子代理**（Codex 的 Task subagent 以子行显示在父代理下，点击只聚焦父终端——"subagents do not own a separate pane"）、**Continue in New Session**（注入 bounded handoff prompt，明确**不是 `codex resume`**）、Restart chip 用当前账号重启、状态栏用量读数。
- 与 DSS 相关：子代理不占独立 pane 的呈现选择、handoff prompt 而非 resume 的"续跑"语义，都值得在 P6 delegate 的 UI/语义设计里参考。

### 14.5 Hooks & memory

- **兼容而非重造**：Orca 直接复用 Claude Code / Codex 的 hook 与 memory 约定——读各 repo 的 `.claude/` / `.codex/` 配置，launch 时运行 hooks。
- Worktree setup hooks：worktree 创建后自动跑（`pnpm install`、`direnv allow`、恢复 `.env`），Settings → Repository → Hooks。
- **Restart survival**：hook endpoint 持久化到磁盘（`{userData}/agent-hooks/endpoint.env` / `.cmd`），每次 hook 调用重新 source，应用重启后长会话仍能触达 live server。
- **Memory 文件不碰**："Claude's CLAUDE.md and Codex's AGENTS.md ... are left alone — they belong to the agent"；在文件浏览器中可编辑。

---

## 15. Recipes（官方场景化工作流）

| Recipe | 核心流程 | 要点 |
|--------|----------|------|
| **Race three agents** | 同 prompt 喂给三个不同 agent（各一个 worktree，同一 start-from ref）→ 分屏观察 → 审胜者 diff → commit/push → 一键删落败者 | "Different agents make different mistakes"；并行比串行重试便宜；分歧点即任务最难处；落败 diff 当对照 |
| **Review an AI diff line-by-line** | 打开 diff → `j`/`k` 过文件，每 hunk 问三问（必要？最小？风格一致？）→ `c` 批注 → Send to agent → 状态点判断 → 复查（resolve 已修复）→ "Repeat until clean, then commit" | 评论用完整句子效果最好 |
| **Jump between 10 worktrees** | Cmd-J palette + 侧栏状态点（优先黄色 Needs You）+ Restart chip 批量恢复 + 持久铃铛当完成队列 | **激进删除已合并 worktree**（一键删，palette 会变快） |
| **Fix a UI bug with Design Mode** | 开 Design Mode → 点坏元素（富附件入 agent）→ 描述改法 → agent 改 → 热重载 → 再点验证 → commit | 免截图/免找选择器/免翻 DOM |
| **Remote worktree over SSH** | Settings → SSH 加主机（确认远程有 git）→ 建 worktree 选 Run on → agent 远程跑、保存流式同步 → 本地审 diff/提交 | 断线 agent 继续跑，重连自动 re-attach 不丢东西 |

---

## 16. 对 Deepseek Science 的借鉴分析

### 16.1 高优先级

**① 编排五原语 → P12 长程研究 / P5b 论文链**
- Run/Task/Dispatch/Message/Decision gate 是教科书级的多 agent 协调模型。DSS 的 paper-writing 编排链目前是硬编码链（P5b），长程研究（research_runs）需要一个持久化的任务图。
- 必抄的设计点：**dispatch ID 作用域的完成**（陈旧重试不能错误完成）+ **worker 契约**（恰好一次 worker_done + 心跳）——DSS 做 resume/重试时极可能踩"旧重试覆盖新派发"的坑。
- **Decision gate 对科研天然贴合**：假设检验等实验数据、人工确认、评审通过都是天然 gate。任务状态机 pending→ready→dispatched→completed/failed/blocked 可直接映射到 research_runs 表。
- 建议落点：`research_runs` / `tasks` 表 + gate 语义并入 P12 设计文档。

**② 用量/限速本地记账 → P10 Deepseek 深化**
- Orca 零 API 调用读 agent 本地用量文件。DSS 直连 Deepseek API，token/费用/限速自己记账是顺水推舟：每次调用落库（tokens/费用/时间窗口），状态栏或设置页展示，80% 阈值预警，按最紧限制排序。
- 科研长任务场景价值更高（预算控制、跑批前检查剩余额度）。

**③ 轻量 checkpoint（状态评论而非快照）→ P12**
- worktree 评论字段 + `todo|in-progress|in-review|completed` 状态的模式比快照便宜得多、信息密度高。DSS 每个 research run 应维护"目标/当前状态/下一步"字段，agent 在关键转折点更新（假设证实/证伪、卡点、阶段切换）。
- 抄它的约定：先读再写避免覆盖用户内容；首行"发生了什么/在哪/下一步"；只更新增量。

### 16.2 中优先级

**④ 会话休眠/恢复的分层回退 → P4 compact / P12**
- "暂停而非杀死"的语义：空闲会话把上下文落盘（DSS 已有 compact 产物可直接复用），回来时加载；**resume 失败回退新会话、旧 transcript 保留在历史**。这个三层回退（恢复成功→降级新会话→历史可查）可直接用于 DSS 的 session resume。
- hibernation 的触发条件清单（无子 agent、无挂起派发、空闲窗口）也是可抄的规则集。

**⑤ 段落级 AI 归属 → P5 论文链**
- 代码行级归属的科研版本：**论文/实验报告段落级标记"AI 生成/人工修改"**，人工编辑自动翻回，可导出审计元数据。对科研诚信与审查是刚需。
- DSS 的 artifact 模型（消息/artifact）已有基础；加一个 per-paragraph attribution 字段 + diff 时自动翻回 + 导出。

**⑥ 定时自动化 + precheck 探针 → P12**
- cron/RRULE + `--precheck`（廉价探针失败即跳过）对科研场景很有用：定时文献抓取/实验监控/综述更新，先查数据库或服务器可用性再干活。
- `--disabled` 先建禁用心调再启用、`--reuse-session` 续用旧会话——都是低成本高收益的设计。

**⑦ Agent 状态可见性 → F2 日志页延伸**
- 状态点（working/waiting/blocked/done）+ "Needs You" 分类对科研工作台特别有用：科研 agent 频繁等人工确认。DSS 前端可做"agent 仪表盘"（日志页 + 状态看板），卡片点击聚焦会话。

### 16.3 低优先级 / 参考

**⑧ worktree 目录隔离哲学 → P9 沙箱对照**
- "worktree 即沙箱，默认全自主"与 DSS 的进程沙箱方向（P9）是互补思路：目录/任务级隔离做并行安全，进程级沙箱做系统安全。科研场景也可考虑"每个研究任务独立工作目录"作为轻量隔离层，再叠进程沙箱。

**⑨ 记忆格式生态兼容 → P4 记忆**
- Orca 的教训：**不重造记忆格式，兼容 CLAUDE.md/AGENTS.md 约定**。DSS 三层记忆是自研的，可考虑提供 AGENTS.md 风格文件的导入/导出，让用户已有 agent 配置直接可用（科研用户可能同时用 Claude Code 写代码）。

**⑩ daemon 解耦架构 → 印证 DSS 架构**
- DSS 的 Rust 后端常驻天然就是 daemon 方案，比 Orca 的 PTY 托管更干净。借鉴点是恢复语义："重开即恢复、无新会话模式" + 区分"进程存活/死亡"两条恢复路径的文档化。

**⑪ CLI 驱动一切 → DSS CLI 增强参考**
- `orca` CLI 的 selector 体系（`active`/`current`/`branch:`/`issue:`）和"从 shell 脚本化编辑器"的思路，DSS 的 `dss` CLI 可参考（科研脚本化驱动 agent）。

### 16.4 不建议借鉴

- PTY 级进程托管、OSC title 状态探测（DSS agent kernel 是内部的，状态天然可知，不需要猜测机制）；
- 对第三方 CLI agent 的适配层（DSS 是自研 kernel + 自有推理）；
- 浏览器/Design Mode/emulator（超出科研场景，除非未来做实验可视化预览）；
- GitHub/Linear/Jira 深度集成（科研场景换文献库/数据源，但集成模式可参考）。

### 16.5 值得持续跟踪的更新点

Orca 活跃开发中（RC 构建 often daily），以下方向与 DSS 路线图重叠、值得关注其演进：
- orchestration 原语（五原语刚定型，legacy run 命令退役）——抄之前等它稳定；
- hibernation、Agent Dashboard、Chat UI 均为实验性，语义可能变；
- skills stub 架构（版本匹配指南）对 DSS 的 skill 体系有启发，可关注落地细节；
- 建议方式：watch GitHub `stablyai/orca` releases + docs，或定期重跑本调研。

---

## 17. 附：文档结构索引（调研时 URL 快照）

- Start Here: `/docs`、`/install`、`/first-session`
- Model: `/docs/model/worktrees`、`/tabs-panes-splits`、`/agents-sessions`、`/session-restore`、`/quick-open`
- Agents: `/docs/agents/supported`、`/claude-code`、`/glm-agent`、`/codex`、`/cursor-cli`、`/custom-cli`、`/codex-hot-swap`、`/native-chat`、`/session-history`、`/hibernation`、`/usage-tracking`、`/hooks-memory`
- Review: `/docs/review/diff-viewer`、`/annotate-ai-diff`、`/attribution`、`/commit-push`、`/github`、`/linear`、`/jira`
- Editing: `/docs/editing/monaco`、`/markdown`、`/viewers`、`/file-explorer`
- Browser: `/docs/browser/overview`、`/design-mode`、`/profiles`
- Terminal: `/docs/terminal`
- Remote: `/docs/ways-to-run`、`/ssh`、`/remote-servers`
- CLI: `/docs/cli/overview`、`/reference`、`/orchestration`、`/automations`、`/computer-use`、`/worktree-checkpoints`、`/skills`
- Mobile: `/docs/mobile`; Notifications: `/docs/notifications`、`/activity`
- Recipes: `/docs/recipes/parallel-agents`、`/review-ai-diff`、`/jump-worktrees`、`/design-mode-fix`、`/remote-worktrees`
- 其他: `/docs/settings`、`/telemetry`、`/troubleshooting`、`/github-errors`
