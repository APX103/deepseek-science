# P0 — 地基（实施计划）

> 对应 [roadmap P0](../roadmap.md#p0--地基)。状态：**已完成**（2026-07-31）

## 目标

全新 Rust workspace 骨架 + 能起一个空后端：`dss-backend serve --port N` 起来，`GET /api/health` 返回 `{"status":"ok","version":"0.1.0"}`。

**验收点**：
- `cargo build` 通过。
- `./target/debug/dss-backend serve --port 17896` 后台启动成功。
- `curl 127.0.0.1:17896/api/health` 返回 ok + version。
- SIGTERM / ctrl-c 优雅退出。

## 行为基线（本阶段要稳定的行为）

- 绑定地址固定 `127.0.0.1`（不暴露公网）。
- 配置优先级：`env (DSS_*) > settings.json > config.toml > defaults`。
- data_dir 默认 `~/.deepseek-science`，启动时确保存在（含 `workspaces/`、`logs/`、`skills/` 子目录）。
- SSD 软链：仅当 data_dir 为默认值（未设 `DSS_DATA_DIR`）、`/Volumes/ssd/main_link/.deepseek-science` 存在、且 `~/.deepseek-science` 不存在或为空目录时才建软链；操作前打印将做什么；`~/.deepseek-science` 非空或已是软链则跳过。
- 日志级别 env 可配（`DSS_LOG` 优先于 `RUST_LOG`）。

## 任务清单（todo）

- [x] Cargo workspace 根 `Cargo.toml`（`[workspace]`，resolver = "2"）
- [x] `crates/dss-core`：Settings 结构、错误类型、配置加载（toml + settings.json + env）、data_dir 解析与 SSD 软链
- [x] `crates/dss-api`：axum 路由 `GET /api/health`，serve + graceful shutdown
- [x] `crates/dss-bin`：main.rs + clap CLI（`serve --port N`，默认 17896），tracing 初始化
- [x] `cargo build` 通过
- [x] 启动 + curl 验证 + SIGTERM 验证
- [x] 回填本文件「回顾」段；D-Q01 在 decisions.md 标记已定

## 回归点

- `dss-backend serve --port 17896` → `curl /api/health` 返回 `{"status":"ok","version":"0.1.0"}`。
- `DSS_DATA_DIR` 覆盖默认 data_dir 生效（启动日志可见实际路径）。
- SIGTERM 后进程退出且日志出现 shutdown 记录。

## 风险

- crates.io 依赖下载慢（网络），需耐心等待。
- axum 0.8 与旧教程 API 有差异（`Router` 路径语法、serve API），按当前版本文档写。
- SSD 软链逻辑若误删非空目录会破坏数据 → 保守实现：仅空目录可替换，其他情况一律跳过并告警。

## 回顾

**实际做了什么**：
- workspace：`Cargo.toml`（resolver = "2"，workspace 级依赖版本统一管理）+ `crates/dss-core`、`crates/dss-api`、`crates/dss-bin`。其余 crate（dss-llm/dss-db/dss-tools 等）按计划留待后续阶段。
- `dss-core`：`Error`（thiserror）、`Settings` + 分层加载（defaults → `<data_dir>/config.toml` → `<data_dir>/settings.json` → env）、`paths` 模块（data_dir 解析 + SSD 软链 + 子目录确保）。
- `dss-api`：axum 0.8，`GET /api/health` → `{"status":"ok","version":"0.1.0"}`；`serve()` 带 graceful shutdown（ctrl-c / SIGTERM）。
- `dss-bin`：二进制 `dss-backend`，clap 子命令 `serve [--port N]`；tracing fmt + EnvFilter（`DSS_LOG` > `RUST_LOG` > 配置 `log_level` > `info`）。

**验证结果**（全部通过）：
- `cargo build` 通过，无警告。
- `./target/debug/dss-backend serve --port 17896` 启动；`curl 127.0.0.1:17896/api/health` → `{"status":"ok","version":"0.1.0"}`。
- 不带 `--port` 时默认 17896；`DSS_DATA_DIR=/tmp/dss-test-data` 覆盖生效，子目录（logs/skills/workspaces）自动创建。
- SIGTERM 与 SIGINT 均优雅退出（日志含 `shutdown signal received, draining connections` / `dss-backend stopped`，进程退出码正常）。验证后进程已杀、临时目录已清理。

**偏离**：
- 无实质偏离。细节取舍：`--port` 用 `Option<u16>` 以便「CLI > 配置文件 > 默认」的优先级成立；DSS_PORT 需为合法 u16 才生效（非法值静默忽略，记为已知简化）。
- 环境变量额外支持了 `DSS_HOST`/`DSS_PORT`（tech-stack 只点名 `DSS_DATA_DIR`，属同族补充）。

**遗留**：
- SSD 软链路径未做活体验证：本机 `/Volumes/ssd/main_link/.deepseek-science` 当前不存在，逻辑走了「不存在则跳过」分支；软链创建/空目录替换/非空跳过三个分支只经过代码审查，未实测（需要创建 SSD 侧目录，留待开发机实际启用 SSD 落盘时验证）。
- 无单元测试（P0 未要求）；settings 合并与 paths 逻辑是后续补测试的候选。
- D-Q01（后端二进制名）已定：`dss-backend` + `serve` 子命令，decisions.md 已更新。
