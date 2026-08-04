# P8 — Tauri 壳 + 打包

> 对应 roadmap P8。状态：**Tauri 壳已搭建，编译通过。完整 .app/.dmg 打包待 `tauri build`。**

## 做了什么

- `src-tauri/` Tauri v2 壳（独立 Cargo workspace）：
  - `main.rs`：找空闲端口（17896）→ spawn `dss-backend serve --port` → 轮询 `/api/health` 等就绪（10s）→ 注入端口到 webview（`window.__BACKEND_PORT__` + `localStorage dss_backend_port`）→ 关窗杀进程。
  - `tauri.conf.json`：前端 `frontend/dist` / dev `localhost:5173`、beforeBuildCommand `bun run build`、窗口 1280×800、bundle resources 后端二进制。
  - `capabilities/default.json`：core:default 权限。
  - 图标：`cargo tauri icon` 从蓝色源图生成全套（.icns/.ico/各尺寸 PNG）。
- `cargo build`（src-tauri）成功编译。

## 验收

- ✅ `cargo build`（src-tauri）通过。
- ⏳ **完整 `cargo tauri build`（出 .app/.dmg）**：需前端 `bun run build` + 后端 `cargo build --release` + Tauri 打包，耗时较长（~10min）。命令：`cd src-tauri && cargo tauri build`。
- ⏳ **运行时验证**（双击 .app 全流程）：需完整打包后。

## 已知限制

- 后端二进制路径：开发模式从 `target/debug/dss-backend` 找；打包模式从 resource_dir 找（tauri.conf.json resources 配了 `../target/debug/dss-backend`——打包前需先 `cargo build --release` 并改 resources 路径）。
- 健康检查用 `reqwest::blocking`（setup 同步上下文）。
- Tauri dev 模式（`cargo tauri dev`）需后端已编译。
