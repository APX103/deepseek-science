# Agent 协作指南

## 分支策略

- `main`：生产主分支，只接受通过测试的合并。
- `release`：预发/Staging 分支，用于集成验证和发布前的稳定测试。
- `dev`：日常开发主分支，新功能、修复都先合并到 `dev`。

开发工作流：
1. 功能分支从 `dev` 切出（如 `feature/xxx`、`fix/xxx`）。
2. 完成后通过 PR 合并回 `dev`。
3. 发布前把 `dev` 合并到 `release` 做 staging 验证。
4. 验证通过后把 `release` 合并到 `main`，并打版本 tag。

## 版本号与 Tag

版本号遵循语义化版本（SemVer），格式 `v{major}.{minor}.{patch}`，例如 `v0.0.1`。

发布流程：
1. 在 `main` 分支上打 tag：`git tag v0.0.1`。
2. 推送 tag：`git push origin v0.0.1`。
3. GitHub Actions 自动触发 macOS App 构建并上传 artifact。

## CI/CD

- `.github/workflows/ci.yml`：push 到 `main` / `dev` / `release` 或对这些分支的 PR 时触发。
  - Rust：`cargo test --locked`、`cargo clippy --locked -- -D warnings`、`cargo fmt --check`。
  - 前端：`bun install`、`bun run build`。
- `.github/workflows/build-mac-app.yml`：推送 `v*` tag 或手动触发时构建 macOS Tauri app。

## 提交前检查

本地应至少执行：

```bash
cargo fmt --check
cargo clippy --locked -- -D warnings
cargo test --locked
cd frontend && bun run build
```

## 代码风格

- Rust：使用项目默认 `rustfmt` 配置，无自定义 `rustfmt.toml` 时按标准风格。
- 前端：跟随现有 Tailwind + React 风格，组件文件使用默认导出。
