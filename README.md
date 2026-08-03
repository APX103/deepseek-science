# Deepseek Science — 设计文档集

> **状态：规划中（未开始实现）。** 本目录目前只包含设计与方案文档。
>
> 所有架构、模块、API、路线图文档均位于 [`docs/`](docs/)。

---

## 这是什么

**Deepseek Science** 是一个**全新本地优先的科研 AI 工作台**：以 **Deepseek** 系列模型为主力推理引擎，面向科研场景做深度强化。全新工程——后端（Rust）、前端（React + DeepSeek 风格）、Tauri 桌面壳全部从零搭建。

- **范围**：全部从零。后端用 Rust 实现；前端全新 React 工程、DeepSeek 视觉风格（蓝色 / 简约 / 细线条，见 [design-system](docs/design-system.md)）并内置日志列表（见 [logging](docs/logging.md)）；Tauri 壳全新实现。
- **目标**：性能、内存安全、单文件部署、并发友好；并为 Deepseek 深度集成、实验数据分析、文献知识图谱、长程自主研究、跨学科数据处理与可视化预留扩展点。

## 文档导航

按阅读顺序排列。每个文档开头都有「本文回答什么问题」的小结。

| 文档 | 回答的问题 |
|------|-----------|
| [项目概览](docs/overview.md) | 做什么、不做什么、产品边界、目标用户与场景 |
| [整体架构](docs/architecture.md) | 分层、进程模型、运行时拓扑、与前端/Tauri 的边界 |
| [技术栈选型](docs/tech-stack.md) | 后端 Rust crate、前端栈、Tauri 壳的选型论证与备选 |
| [模块详细设计](docs/modules.md) | agent kernel / tools / skills / mcp / memory / compact / verify 逐模块 |
| [API 契约](docs/api-contract.md) | HTTP/SSE 端点、事件流格式 |
| [数据模型与存储](docs/data-model.md) | SQLite schema、消息/会话/记忆/artifact 模型、迁移 |
| [增强方向设计预留](docs/enhancements.md) | Deepseek 集成 / 实验数据分析 / 文献知识图谱 / 长程自主研究 |
| [学科扩展插件体系](docs/domain-plugins.md) | 跨学科数据处理与可视化的插件化机制 + 调研清单 |
| [设计系统](docs/design-system.md) | DeepSeek 蓝 / 超级简约 / 细线条 视觉规范 |
| [日志系统](docs/logging.md) | 日志列表功能：系统日志 + agent 执行记录统一视图 |
| [开发路线图](docs/roadmap.md) | 分阶段交付计划，每阶段可独立验收 |
| [决策记录](docs/decisions.md) | 设计决策、待办、暂缓项的变更与追溯 |

### 规划记录（实现期再填充）

- [`docs/plans/`](docs/plans/) — 每个开发阶段一份实施计划与回顾。
- [`docs/research/`](docs/research/) — 调研笔记（Rust 生态、学科工具链、Deepseek 能力边界等）。

## 如何阅读

1. **想快速了解项目**：读 [概览](docs/overview.md)。
2. **想理解整体设计**：概览 → 架构 → 模块。
3. **要做某模块开发**：先看 [路线图](docs/roadmap.md) 定位当前阶段，再看对应模块（[模块](docs/modules.md)）和数据/API 契约（[api-contract](docs/api-contract.md) / [data-model](docs/data-model.md)），最后看该阶段的 [plans](docs/plans/)。
4. **要决策一个跨阶段问题**：查 [决策记录](docs/decisions.md)。

## 文档约定

- **状态标签**：文档段落用 `> 状态：已定 / 待定 / 调研中` 标注成熟度。
- **决策标注**：关键设计选择用 `> 决策：…` 引用块标出，并在 [决策记录](docs/decisions.md) 登记。
- **未实现**：所有代码示例仅作设计示意，项目尚未开始实现。
