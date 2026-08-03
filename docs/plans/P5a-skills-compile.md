# P5a — skills 系统 + compile_pdf（不含论文编排链）

> 对应 roadmap P5 / modules.md §5。状态：进行中（2026-08-03）。论文编排链（paper-writing orchestration）留 P5b。

## 目标
1. `dss-skills`：SKILL.md frontmatter 解析 + 多源加载（builtin/global/claude/project）+ BM25+Jaccard/RRF 检索。
2. `compile_pdf` 工具（Tectonic 子进程 + 容错）+ `POST /api/sessions/{sid}/compile` 端点。
3. `search_skills`/`list_skills`/`skill` 工具接入 ToolRegistry（让 agent 能查/用 skill）。
4. 内置 templates（至少 1 套 article）。

## 验收点
1. `cargo build` 无警告；`cargo test`（skills 单元测试：frontmatter 解析、BM25 检索）全绿。
2. curl：写一个最小 `.tex` 文件 → `POST /compile` → 产出 PDF（Tectonic 编译成功）。
3. compile_pdf 工具：agent 经工具调用编译 .tex → tool_results 含 success/pdf_path。
4. skills 检索：search_skills 工具按 query 返回相关 skill 名+描述。

## 回顾

**实际做了什么**：
- 新建 `dss-skills` crate：`frontmatter.rs`（YAML frontmatter 解析、只读顶层跳过缩进行、NAME_RE/DESCRIPTION_MAX/SKILL_MAX_BYTES 约束）、`bm25.rs`（BM25 k1=1.2/b=0.75 + Jaccard + RRF k=60/threshold=0.029 融合）、`catalog.rs`（`SkillCatalog`：builtin include_dir! + global + project 三源加载、首跑 seed builtin 到 global、search/get/list）、`skill.rs`。内置 skills：paper-writing、lit-survey（include_dir! 嵌入）。
- `dss-tools`：加 `compile_pdf` 工具（Tectonic `-X compile`、超时 180s、kill_on_drop、解析成功/失败+log 尾）、`search_skills`/`list_skills`/`skill` 工具；`ToolContext` 加 `skill_catalog` 字段 + `with_skill_catalog`。
- `dss-api`：AppState 加 `catalog`（build_state 加载 builtin+global+seed）；stream_sse 每 session 构 ToolContext 时叠加 project 源；新增 `POST /api/sessions/{sid}/compile` 端点（Tectonic 编译）。

**验证结果**：
- `cargo test` 全 workspace **26 测试全绿**（7 skills + 5 gates + 12 compact unit + 2 compact 集成）；0 警告。
- 启动日志确认 catalog seed：global skills 目录出现 lit-survey、paper-writing。
- **compile 端点**：POST /compile main.tex → `success:true`、生成 main.pdf（8KB）。✅
- **search_skills 工具**：agent 经 stream-sse 调 search_skills → tool_results 返回 paper-writing / lit-survey（带描述、BM25 排序）。✅

**遗留（P5b）**：
- paper-writing 编排链（lit-survey/paper-structure/... 多 skill 协作）。
- compile_pdf 浮动环境 `\iffalse` 容错重编译。
- claude/custom skill 源、5 源完整覆盖。
- 长程自主研究 skill。

**补充（2026-08-03）：skills/templates 端点接真实**
- 加 `GET /api/skills`（catalog list）、`GET /api/templates`（include_dir! 嵌入的 templates）、`GET /api/templates/{id}`（.tex 纯文本）端点（dss-api/meta.rs）。
- 前端 listSkills/listTemplates/getTemplate 切真实 fetch（离线回退 mock）。
- curl 实测：skills 返回 [lit-survey, paper-writing]；templates 返回 [article]；templates/article 返回真实 .tex。

## 改动点

### 1. 新建 `dss-skills` crate
- `frontmatter.rs`：解析 `---\nyaml\n---\nbody`，只读顶层（跳过缩进行）；约束 NAME_RE/DESCRIPTION_MAX/SKILL_MAX_BYTES。
- `skill.rs`：`Skill{ name, description, source, body }`。
- `loader.rs`：5 源加载（P5a 先做 builtin(include_dir!) + global(data_dir/skills) + project(workspace/.dss/skills)；claude/custom 留 P5b）。首跑复制 builtin 到 global（不覆盖）。
- `bm25.rs`：BM25(k1=1.2,b=0.75) + Jaccard，RRF(k=60, threshold=0.029) 融合。`search(catalog, query) -> Vec<SkillHit>`。
- `catalog.rs`：`SkillCatalog{ skills, index }`，`load(sources)` / `search(query)`。
- 内置 skills：放 `skills/` 目录，include_dir! 嵌入（P5a 先放 1-2 个通用 skill 如 paper-writing 占位；完整论文链 skill 留 P5b）。

### 2. compile_pdf（dss-tools 加工具 + dss-api 加端点）
- `dss-tools/builtin/compile.rs`：`compile_pdf{path, out_name?}` → Tectonic 子进程（`tectonic -X compile main.tex`，cwd=workspace），解析成功/失败、收集 .log 末尾、返回 `{success, pdf_path, message}`。容错：浮动环境 `\iffalse` 重编译（P5a 先做基础编译，容错重编译留 P5b）。
- `dss-api`：`POST /api/sessions/{sid}/compile`（CompileReq{path,out_name?} → CompileResult）。

### 3. skills 工具接入
- `dss-tools/builtin/skills.rs`：`search_skills{query}` / `list_skills` / `skill{name}` 工具，经 SkillCatalog（从 ToolContext 拿，P5a 在 ToolContext 加 `skill_catalog: Option<Arc<SkillCatalog>>`）。
- `register_all` 注册 compile_pdf + skills 工具。

### 4. 内置 template
- `templates/article.tex`（最小 ctexart 模板）；`GET /api/templates` 返回列表（前端已有 mock，P5a 接真实）。

## 工作顺序
1. 写计划（本文件）。
2. dss-skills：frontmatter/loader/bm25/catalog + 单元测试；内置 skills 目录。
3. compile_pdf 工具 + POST /compile 端点。
4. skills 工具 + ToolContext 接 catalog。
5. cargo build/test 绿；curl 验收（compile .tex→PDF；search_skills）。
6. 回填回顾 + 更新 HANDOFF。

## 风险
- Tectonic 首次编译拉 CTAN 包较慢（已在 /opt/homebrew/bin/tectonic）。
- BM25 实现需与 modules.md 常量一致（k1=1.2，独立于 memory 的 1.5）。
- include_dir! 需内置 skills/templates 目录存在（P5a 建最小内容）。

## 不做（P5b）
- paper-writing 编排链（lit-survey/paper-structure/... 的多 skill 协作）。
- compile_pdf 浮动环境容错重编译。
- claude/custom skill 源、5 源完整覆盖。
- 长程自主研究 skill。
