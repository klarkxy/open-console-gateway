[English](development.md)

# 开发

## 前置要求

Node.js 22、`package.json` 的 `packageManager` 钉，以及 workspace 的
`rust-version`（锁定依赖要求 Rust 1.88 或更新版本）。原生依赖以 `.github/workflows/release.yml` 在对应 runner
上安装的为准。

## 开发模式

退出已安装的托盘程序，避免占用单实例锁和 `9042` 端口，然后：

```bash
pnpm install
pnpm run dev
```

`pnpm run dev` 以独立的开发默认端口 `OCG_GATEWAY_PORT=19042` 运行 `tauri dev`。
部分 Windows 主机的 HNS/WSL/Docker 保留端口范围包含 `9042`，开发默认端口可避开该冲突。安装版仍默认 `9042`。Vite
提供 `http://127.0.0.1:30001/dashboard/`，并把 `/dashboard/api`（含
WebSocket）代理到该 Gateway 端口。启动前设置 `OCG_GATEWAY_PORT` 可同时覆盖
Tauri 与 Vite；变量生效时，设置页以只读方式显示实际端口。

`pnpm install` 会启用 `.githooks`（暂存 `*.rs` 时运行 `cargo fmt --all`）。

## 检查

以 `package.json` 中的脚本名为准。选能覆盖本次改动边界的最小检查：

| 改动 | 检查 |
| --- | --- |
| 单个前端或脚本测试 | `node --experimental-strip-types --test <file>` |
| Vue / dashboard | 相邻测试，再 `pnpm run build:web` |
| 单个 Rust crate | `cargo test -p <package>` |
| Core / Dashboard V3 | `cargo test -p ocg-core <filter>` |
| Desktop Host | `cargo test -p ocg-manager --lib` |
| V3 或 V4 Schema 或生成类型 | `pnpm run contract:v3:check` / `pnpm run contract:v4:check` |
| `DESIGN.md` / 主题 | `pnpm run design:lint` |

`pnpm run test` 是跨前端/Rust 门禁。`pnpm run test:tooling` 覆盖
`scripts/*.test.mjs`，属于发版/工具门禁，不属于 `pnpm run test`。
`pnpm run build` 只做发版验证（`scripts/release.mjs`）。workspace
`[profile.release]` 使用 thin LTO、`strip` 和 `panic = "abort"`。

Rust 单元测试放在同名子模块：`src/db.rs` 声明 `mod tests;`，测试正文在
`src/db/tests.rs`。不要写断言源码文本、工作流 YAML 或文档正文的测试。

CLI 沙箱（只创建 OpenCode Go 卡；不能创建 Custom、子 Key 或设置）：

```bash
ocg-manager-cli --data-dir /tmp/ocg-cli-test key add smoke sk-smoke
ocg-manager-cli --data-dir /tmp/ocg-cli-test serve --port 19042
```

直接 `Database::update_account` 不 bump revision；这是有意的，也不是 CLI
路径。

## 本地未签名冒烟（Windows）

从托盘退出已安装的 release。对齐 `package.json`、
`src-tauri/tauri.conf.json`、两份 `Cargo.toml` 和 `compose.example.yaml`
中的版本，然后运行 `pnpm run build`。

没有 `TAURI_SIGNING_PRIVATE_KEY` 时只生成普通本地包，不能用于应用内升级。
可选签名变量：`TAURI_SIGNING_PRIVATE_KEY`、
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`、`TAURI_UPDATER_PUBLIC_KEY`（必须匹配
`src-tauri/updater-public-key.sha256`），以及
`OCG_REQUIRE_UPDATER_ARTIFACTS=1`。

本地 Tauri 构建可能改写 `src-tauri/Cargo.toml` 与
`src-tauri/gen/schemas/*.json`——只保留有意修改。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](development.md) · [文档索引](../README.zh-CN.md)
