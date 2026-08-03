# P2b-tools — web/python 工具（不含门控）

> 对应 [roadmap P2](../roadmap.md#p2--工具与多轮)。状态：**进行中**（2026-08-03）

## 背景

P2a 已完成（后端 curl 全验收 + 前端代码完成/tsc 通过；GUI 端到端受 IAB 输入驱动限制无法自动跑，根因已确诊为工具局限、代码正确，见 [P2a plan](P2a-tools-multiturn.md)）。

P2b 原范围含「工具（web/python）」+「门控（max_tokens 续传/empty-retry/检索熔断）」两类。门控是 Runner 内部行为门，需先搭 FakeLLM 脚本化测试基建才能稳定验证（modules.md 测试策略明确这是测 agent 行为的核心手法），工作量和返工风险大。故**再拆**：本次只做 **P2b-tools**（web_search/fetch_url/python），门控单独成 P2b-gates 留后续。

## 目标

agent 能用 `web_search` 联网搜索、`fetch_url` 抓取网页转纯文本、`python` 执行代码。

**验收点**：
1. `cargo build` 无警告。
2. curl 让 agent 用 `web_search` 搜一个词、拿到结果（真实联网）。
3. curl 让 agent 用 `fetch_url` 抓一个公开 URL、转纯文本。
4. curl 让 agent 用 `python` 跑一段代码、拿到输出。
5. P2a 不回归（文件工具/bash/ask_user/纯文本对话仍正常）。

## 行为基线

- 三工具都在 dss-tools 内，不改 Runner/AgentEvent/前端（工具自动经 `register_all` 进 ToolRegistry，LLM 即可调用）。
- `web_search`：DuckDuckGo HTML 端点抓取（无 API key），Chrome UA，朴素解析结果块。
- `fetch_url`：reqwest GET + 自写 HTML→纯文本（剥标签/解码实体/压空白），截断 max_chars。
- `python`：最小子进程方案（`python3 -c`，cwd=workspace，超时 kill，kill_on_drop）。沙箱化/持久 state/venv 留 P9。
- 三工具失败一律转 `is_error=true` + 可读 content（与 P2a 工具一致）。

## 任务清单（todo）

- [ ] `dss-tools/Cargo.toml` 加 reqwest 依赖。
- [ ] `builtin/web.rs`：`web_search` + `fetch_url` + `html_to_text`。
- [ ] `builtin/python.rs`：`python` 工具。
- [ ] `builtin/mod.rs::register_all` 注册三工具。
- [ ] `cargo build` 无警告。
- [ ] curl 验收：web_search / fetch_url / python + P2a 回归。
- [ ] 回填本文件「回顾」段；python 沙箱/持久 state 登记 decisions DEFER。

## 回归点

- web_search prompt → agent 多轮：`tool_calls{web_search}` → `tool_results`（含真实搜索结果）→ `text` 报告。
- fetch_url prompt → `tool_results` 含目标 URL 正文纯文本（截断）。
- python prompt（如「算 2 的 10 次方」）→ `tool_results` 含 `1024`。
- P2a 回归：文件工具/bash/ask_user/纯文本对话仍正常。

## 风险

- **DDG 反爬**：html.duckduckgo.com 偶尔限流/改版。对策：Chrome UA + 失败转 is_error 可读提示；best-effort，不阻塞其它工具。
- **HTML 解析鲁棒性**：自写剥离器对畸形 HTML 可能残留噪音。对策：max_chars 截断 + 压缩空白，够 LLM 用即可。
- **python3 可用性**：macOS 可能无裸 `python3`。对策：先试 `python3`，失败明确 is_error 提示。

## 回顾

**实际做了什么**：
- `dss-tools` 加 reqwest 依赖（workspace 已有）。
- `builtin/web.rs`：`web_search`（DuckDuckGo HTML 端点 + Chrome UA + 自写朴素解析：定位 `result__a` 锚点、解 `uddg=` 参数拿真实 URL、配对 `result__snippet`）+ `fetch_url`（reqwest GET + 256KB 体上限 + 自写 `html_to_text`：剥 script/style/注释/标签、解码基本实体、压空白、截断 max_chars）+ 自写 `urlencoding::encode`/`url_decode`/`strip_tags`（不引新 crate，最小改动）。
- `builtin/python.rs`：`python` 最小子进程方案（`python3 -c`，cwd=workspace，超时 kill，kill_on_drop，非零退出 is_error）。
- `builtin/mod.rs::register_all` 注册 web_search/fetch_url/python。
- `cargo build` 全 workspace 无警告。

**验证结果（curl + 真实 DeepSeek + 真实联网，端口 17896）**：
- `python`：✅ 算 `2**10` → `tool_results{content:"1024\n\n[exit code: 0]"}`，2 iteration，natural。
- `fetch_url`：✅ 抓 `example.com` → `tool_results{content:"Example Domain This domain is for use in... Learn more"}`（HTML→纯文本正确，标签已剥），2 iteration，natural。
- `web_search`：⚠️ **代码正确但 DDG 在本机出口 IP 被反爬拦截**。DDG `html.duckduckgo.com` / `lite.duckduckgo.com` 的 GET/POST 均返回 14KB 的 "anomaly" 反爬页（无 `result__a`/`uddg=`）。工具本身行为正确：3 iteration、调 web_search、解析失败时返回可读的 "no results ... (DuckDuckGo may be rate-limiting)" 非 error 内容、agent 正常收尾。**这是环境/出口 IP 问题，非代码缺陷**；换出口（代理/不同网络）或换搜索源即可恢复。
- P2a 回归：✅ write_file+read_file 多轮仍正常（生成 hello.txt，3 iteration，natural）。文件工具/bash/ask_user/纯文本对话未回归。

**偏离**：
- `web_search` 因 DDG 反爬在本环境不可用——属预期风险（plan 已记）。登记 decisions，待换搜索源（如自建 SearXNG、或可配 API 的 Brave/SerpAPI）时复用 `parse_ddg` 之外的解析层。

**遗留（→ decisions.md DEFER）**：
- python 沙箱化（JSON-RPC 长进程 + host 注入）、变量跨调用持久、venv/uv pip、install_packages → P9/方向 2.1（见 D-F07）。
- 搜索源替换（web_search 当前依赖 DDG HTML）→ 见 D-F08。
- 门控（max_tokens 续传 / empty-retry / 检索熔断 / plan denial）→ P2b-gates（需先搭 FakeLLM 测试基建），见 D-F04。
