# 设计系统（DeepSeek 风格）

> **本文回答**：前端 UI 视觉规范是什么？从零搭建时怎么落实「DeepSeek 蓝色 / 超级简约 / 细线条」？

> 状态：方向已定（用户要求「和 DeepSeek 设计风格完全一致」）；色彩 token 已于 2026-08-11 浏览器实测校准（见下「DeepSeek 设计 token」段），圆角为有意偏离官网。

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

> 数据来源：2026-08-11 浏览器实测 chat.deepseek.com（深色登录页 `:root`/`.ds-input--border` computed style）+ deepseek.com（亮色首页 CTA 卡片 computed style）。色值精确，圆角为本工作台有意偏离（见下）。

### 色彩

**亮色模式**（light）

| token | 值 | 用途 |
|-------|-----|------|
| `--brand` | `#3B82F6` | 主品牌蓝（按钮/链接/聚焦/强调） |
| `--brand-hover` | `#2563EB` | 主色 hover |
| `--brand-soft` | `rgba(59,130,246,0.10)` | 主色淡背景（选中态/badge） |
| `--bg` | `#FFFFFF` | 页面底色 |
| `--surface` | `#F9FAFB` | 卡片/侧栏次级背景 |
| `--surface-2` | `#F3F4F6` | 悬浮/hover 背景 |
| `--border` | `#E5E7EB` | 1px 细边框（核心：细线条） |
| `--border-strong` | `#D1D5DB` | 输入框聚焦前边框 |
| `--text` | `#0F1115` | 主文本 |
| `--text-secondary` | `#64748B` | 次文本 |
| `--text-tertiary` | `#9CA3AF` | 占位/禁用 |
| `--danger` | `#EF4444` | 错误/删除 |
| `--success` | `#10B981` | 成功 |

**深色模式**（dark，DeepSeek 默认）

| token | 值 | 用途 |
|-------|-----|------|
| `--brand` | `#5686FE` | 深底下提亮的品牌蓝（实测登录页主按钮内层） |
| `--bg` | `#151517` | 页面底色（实测值） |
| `--surface` | `#1B1B1C` | 卡片/侧栏（实测 `.ds-input--border` 背景） |
| `--surface-2` | `#2A2A2E` | hover |
| `--border` | `rgba(255,255,255,0.12)` | 细边框（半透明白，实测值） |
| `--text` | `#F9FAFB` | 主文本（实测值） |
| `--text-secondary` | `#9CA3AF` | 次文本 |

> 实测记录见 [决策 D-T06](decisions.md)（已闭环）。

### 排版

- **字体族**：`Inter, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif`（实测，DeepSeek 用 Inter）。中文回退 `PingFang SC` / `Microsoft YaHei`。等宽：`JetBrains Mono`（代码块默认等宽字体）。
- **字号阶**：`12 / 13 / 14 / 16 / 18 / 20 / 24 / 30px`（DeepSeek 偏小字，body 14px 实测）。
- **字重**：`400` 正文 / `500` 强调 / `600` 标题。避免 700+ 重字重。
- **行高**：正文 `1.6`，标题 `1.3`。

### 线条与圆角（核心特征）

- **边框**：**统一 1px solid**（细线条是 DeepSeek 标志）。禁用 2px+ 边框。
- **圆角**：克制——按钮 `8px`，卡片/输入框 `8-10px`，小元素 `4-6px`，大容器 `12px` 封顶。**不用**大圆角胶囊（pill 形输入框）。
- **（偏离说明）** 2026-08-11 实测发现 DeepSeek 官网实际用大圆角：卡片 `16px`、输入框 `28px`、按钮接近 pill（`4096px`，即 `9999px` 写法）。本工作台**有意偏离**：工作台是高密度信息界面（多栏、表格、日志列表、代码块），克制的小圆角（8-12px）更适合长时阅读与编辑场景；仅品牌主按钮等关键 CTA 可适当放大。如未来要做「完全像素级复刻官网」可再议。
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
