# 设计系统（DeepSeek 风格）

> **本文回答**：前端 UI 视觉规范是什么？从零搭建时怎么落实「DeepSeek 蓝色 / 超级简约 / 细线条」？

> 状态：方向已定（用户要求「和 DeepSeek 设计风格完全一致」）；token 值待实现期用浏览器 devtools 精确校准。

---

## 设计意图

用户原话：「前端的 UI 的设计，最好是和 Deepseek 的设计风格要完全一致，就是那种蓝蓝的风格，然后超级简约，然后细细的线条。」

**三大关键词**：
1. **蓝色**（DeepSeek 品牌蓝为主色）
2. **超级简约**（大量留白、克制装饰、信息层级清晰）
3. **细线条**（1px 细边框、无重阴影、无毛玻璃）

本项目前端是**全新工程**（见 [概览](overview.md)），从零搭建时即采用 DeepSeek 风格作为原生视觉语言——不存在「改造旧主题」的负担，设计 token 从第一天就按 DeepSeek 规范建立。所有组件从零编写，视觉与代码完全独立。

---

## DeepSeek 设计 token（规范底子）

> 数据来源：浏览器实测 chat.deepseek.com（Inter 字体、深色背景 `#151517`、细线条、简约布局）+ DeepSeek 公开品牌色。标记 ⚠️ 的值需实现期用 devtools 复核。

### 色彩

**亮色模式**（light）

| token | 值 | 用途 |
|-------|-----|------|
| `--brand` | `#4D6BFE` ⚠️ | 主品牌蓝（按钮/链接/聚焦/强调） |
| `--brand-hover` | `#3D5AE0` ⚠️ | 主色 hover |
| `--brand-soft` | `rgba(77,107,254,0.10)` ⚠️ | 主色淡背景（选中态/badge） |
| `--bg` | `#FFFFFF` | 页面底色 |
| `--surface` | `#F9FAFB` ⚠️ | 卡片/侧栏次级背景 |
| `--surface-2` | `#F3F4F6` ⚠️ | 悬浮/hover 背景 |
| `--border` | `#E5E7EB` ⚠️ | 1px 细边框（核心：细线条） |
| `--border-strong` | `#D1D5DB` ⚠️ | 输入框聚焦前边框 |
| `--text` | `#111827` ⚠️ | 主文本 |
| `--text-secondary` | `#6B7280` ⚠️ | 次文本 |
| `--text-tertiary` | `#9CA3AF` ⚠️ | 占位/禁用 |
| `--danger` | `#EF4444` | 错误/删除 |
| `--success` | `#10B981` | 成功 |

**深色模式**（dark，DeepSeek 默认）

| token | 值 | 用途 |
|-------|-----|------|
| `--brand` | `#5B7CFF` ⚠️ | 深底下提亮的品牌蓝 |
| `--bg` | `#151517` | 页面底色（实测值） |
| `--surface` | `#1F1F23` ⚠️ | 卡片/侧栏 |
| `--surface-2` | `#2A2A2E` ⚠️ | hover |
| `--border` | `rgba(255,255,255,0.08)` ⚠️ | 细边框（半透明白） |
| `--text` | `#F9FAFB` | 主文本（实测值） |
| `--text-secondary` | `#9CA3AF` ⚠️ | 次文本 |

> ⚠️ 待校准项汇总在 [决策 D-T06](decisions.md)。

### 排版

- **字体族**：`Inter, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif`（实测，DeepSeek 用 Inter）。中文回退 `PingFang SC` / `Microsoft YaHei`。等宽：`JetBrains Mono`（代码块默认等宽字体）。
- **字号阶**：`12 / 13 / 14 / 16 / 18 / 20 / 24 / 30px`（DeepSeek 偏小字，body 14px 实测）。
- **字重**：`400` 正文 / `500` 强调 / `600` 标题。避免 700+ 重字重。
- **行高**：正文 `1.6`，标题 `1.3`。

### 线条与圆角（核心特征）

- **边框**：**统一 1px solid**（细线条是 DeepSeek 标志）。禁用 2px+ 边框。
- **圆角**：克制——按钮 `8px`，卡片/输入框 `8-10px`，小元素 `4-6px`，大容器 `12px` 封顶。**不用**大圆角胶囊（pill 形输入框）。
- **阴影**：**几乎不用**。用边框 + 背景层级区分，而非投影。必要时仅 `0 1px 2px rgba(0,0,0,0.04)` 极淡。
- **分割**：用 1px 边框线或背景色阶，不用毛玻璃/渐变。

### 间距

8 点网格：`4 / 8 / 12 / 16 / 24 / 32 / 48px`。DeepSeek 留白慷慨，倾向偏大间距。

### 交互

- **hover**：背景色阶变化（`--surface` → `--surface-2`），非阴影抬起。
- **active/focus**：`--brand` 描边或淡背景。输入框聚焦用 `--brand` 1px 边框（**不**用粗光环）。
- **过渡**：`150ms ease`，克制。

---

## 实现策略

### 从零搭建
- `frontend/src/index.css`：按 DeepSeek 设计 token 建立CSS 变量体系 + 基础工具类（蓝色色阶、1px 边框、平面、无毛玻璃/渐变/光环）。
- 组件（`App` 三栏、`Message`、`Markdown`、`WorkspacePanel`、`PlanPanel`、`SettingsModal`、`ArtifactPreview`、**日志页**等）：全新编写，视觉用 DeepSeek 风格。
- 富渲染依赖（KaTeX、PDF.js、3D Mol、react-markdown）照常引入——是功能，与主题无关。

### 默认
- 暗色模式为**默认**（DeepSeek 默认深色），亮色作为可切换。

---

## 验收（视觉）

- 整体观感：蓝色主调、大量留白、1px 细线、平面（无毛玻璃/重阴影/大圆角胶囊）。
- 拿 DeepSeek 官方界面（chat.deepseek.com）做视觉对照，色调与线条风格接近。
- 明暗模式都达标。

---

## 对其他文档的影响

- [概览](overview.md) 范围边界：前端是「全新工程」的一部分（从零搭建，DeepSeek 风格）。
- [架构](architecture.md)：前端与 Tauri 壳均从零实现；后端 API 契约由本项目自定（见 [API 契约](api-contract.md)）。
- [路线图](roadmap.md)：前端在 F1 阶段全新搭建，可与后端主线并行推进。

---

下一步：读 [日志系统](logging.md)。
