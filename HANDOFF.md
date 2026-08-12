# 交接说明（HANDOFF）

> 写给接手 Agent：本文记录当前分支事实、最近交付、已观察到的验证证据和仍然有效的债务。
> 更新日期：2026-08-11。

---

## 1. 项目与阅读顺序

**Deepseek Science** 是本地优先的科研 AI 工作台：Rust 后端（agent 内核 + HTTP/SSE）、React 前端和 Tauri 2 桌面壳，以 DeepSeek 系列模型为主要推理引擎。

接手时按以下顺序阅读：

1. `README.md`：产品边界和文档导航。
2. `docs/roadmap.md`：阶段目标；历史阶段状态不能替代当前源码核对。
3. `docs/architecture.md` 与 `docs/api-contract.md`：进程、crate 和前后端契约。
4. `docs/decisions.md`：已完成、部分完成和仍有效的债务。
5. 涉及 UI 时再读 `docs/design-system.md`；涉及持久化、compact、日志时再读对应模块文档。

## 2. 当前 Git 快照

- 当前分支：`fix/openalex-live-config`。
- 当前 HEAD：`634ceb9`，与 `origin/fix/openalex-live-config` 一致；相对本地及远端 `dev` 为 ahead 3、behind 0。
- 本轮清理仍在工作区、尚未提交：移除模板 mock fallback 和旧 mock 数据，修正文档状态。不要覆盖或回退这些改动。
- 合并方向仍是功能/修复分支 → `dev` → `release` → `main`；提交、推送和 PR 均需用户明确授权。

## 3. 当前实现与最近产品化工作

主线 P0–P8、F2、P5b 和 P2b gates 已有实现；这里不沿用旧文档中的固定测试数量，只记录可从当前源码确认的能力：

- 内置 registry 当前注册 21 个工具，覆盖文件、bash/python、网页抓取与搜索、OpenAlex 文献、PDF 编译、skills、plan/delegate、记忆和 ask-user；README 使用耐久的“20+”表述。MCP 工具在运行时动态挂载。
- `search_papers` / `fetch_paper` 已接 OpenAlex；数据源 key、provider、MCP 和 skills 设置具备持久化或运行时重建路径。日志保留设置的读取一致性仍有缺陷，见下一节。
- 记忆已从基础召回扩展为可更新、去重、追溯和审批的 Claim Store，并接入前端管理、retention 与评测埋点。
- SSE 使用强类型 `AgentEvent`；ask-user 回答可从 composer 继续；运行取消、工具批次后的历史 checkpoint 和重载恢复均已有实现。
- 文件工作区、图片/Markdown/PDF 预览、plan 审批/继续执行、A2A、MCP 与 Tauri sidecar 路径均已有实现。
- 本轮已删除 `frontend/src/mock/data.ts`；模板列表和模板正文现在只请求真实后端，错误交给现有调用方处理，不再伪造模板内容。

## 4. 本轮已观察的验证证据

以下只记录本轮实际运行的检查，不把旧提交说明当成当前全量 oracle：

- `test ! -e frontend/src/mock/data.ts` 通过。
- `rg -n "mockLogs|mockTemplates|mock/data" frontend/src` 无匹配。
- `cd frontend && bun test` 通过，共 119/119；`bun run build` 通过，Vite 仅报告既有的主 bundle 超过 500 kB 警告。
- `cargo fmt --check` 与 `cargo clippy --locked -- -D warnings` 通过。
- Rust 隔离运行中 355 个当前环境可执行的测试通过；另有 26 个依赖本地 listener 或沙箱能力的测试被当前托管环境以 `Operation not permitted` 阻塞，不能据此宣称完整 `cargo test --locked` 全绿。
- `git diff --check`、死引用扫描和逐条 TODO/FIXME/XXX 复核均通过；`paper-writing` 中的 `[TODO: cite]` 已确认为生成协议。

仍未覆盖的是可放行这些环境相关测试的发布环境复跑，以及 Tauri GUI/打包验收。

## 5. 真实剩余工作（按优先级）

1. **补发布环境验收**：在允许本地 listener、macOS sandbox/Tectonic/Python 依赖的环境复跑被阻塞的 26 个测试；执行 `cargo tauri build`，验证 `.app`/`.dmg` 拉起与退出，并按 `docs/plans/gui-test-guide.md` 完成人工 GUI 冒烟。
2. **修复日志保留设置读回**：POST 会持久化 `log` 配置，但公开设置仍从启动时的 `state.settings.log` 取值；保存后的 GET/UI 可能读到旧值。retention loop 使用启动快照是另一个明确保留的行为。
3. **补运行态/持久化债务**：真正的 LRU；`compaction_state` 读写与 L2 compact；frames、verification/compaction archives 持久化；artifacts provenance/ledger。
4. **收口部分完成能力**：生产级 bash/python 沙箱、长进程/状态/venv 与包管理；可配置且稳定的搜索源；DeepSeek 并行 sampling 等能力边界验证。当前没有 `install_packages` 工具。
5. **P9+ 排期**：向量文献知识库、长程自主研究和学科插件仍是增强方向，不属于当前已交付能力。

`paper-writing` skill 中的 `[TODO: cite]` 是生成论文时保留的引用占位协议，不是仓库待办，不要清除。

## 6. 环境与工作规则

- Rust 工具链可能不在默认 PATH；本仓库提交前检查以根目录 `AGENTS.md` 为准。
- 前端使用 bun；开发端口 5173，`/api` 代理到默认后端端口 17896。
- API key 只通过配置或环境变量注入，严禁打印、提交或写入测试产物。
- 起本地服务前确认端口占用，结束后清理后台进程；数据目录默认 `~/.deepseek-science`。
- 改 API 契约时前后端和文档必须同步；改 UI 时遵守 `docs/design-system.md`；不顺手重构或引入未经论证的依赖。
- 每个阶段都要记录实际执行的 oracle 和残留风险；没有运行的检查必须明确写成未运行。
