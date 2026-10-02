[English](ci.md)

# CI 工作流

工作流在 `.github/workflows/`。本页只记录 YAML 里看不出来的拆分。

## quality.yml

在 pull request、推送到 `main`，以及 `workflow_call` 时运行。没有 Compose job。两条 job：

- **rust**（`ubuntu-22.04`，30 分钟）— `cargo fmt --all -- --check`，然后
  `cargo test --workspace --locked --no-fail-fast --features ocg-core/ollama-cloud-loopback-test`，
  然后带同一 feature 与 `-D warnings` 的 `cargo clippy --workspace --all-targets --locked`。
- **windows-cli**（`windows-latest`，30 分钟）— `cargo test -p ocg-manager-cli --locked`
  与 `cargo clippy -p ocg-manager-cli --all-targets --locked -- -D warnings`。
  这条 job 不传 Ollama loopback feature。

workspace 测试里的 Rust DTO schema 测试是当前 V3/V4 契约门禁。
`scripts/cli-acceptance.mjs` 不是这个文件里的 job。

## release.yml

该文件是历史桌面发布器。它仍在树里，但不能作为当前 CLI 的发布路径。本页不授权发布，也不修改该工作流。

preflight 与 build 仍运行 `pnpm/action-setup`、`pnpm install --frozen-lockfile`、
`pnpm run test:tooling`、`pnpm run release:check` 和 `pnpm run build`。Windows GUI
冒烟从 `package.json` 读取版本。build job 仍传入 `TAURI_SIGNING_PRIVATE_KEY`、
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD` 和 `TAURI_UPDATER_PUBLIC_KEY`。当前分支没有根
`package.json`，也没有 `src-tauri/` 目录，因此这些步骤不能发布 `ocg-manager-cli`。

同一文件仍包含那套旧发布器的桌面包冒烟（安装包、DMG、AppImage）和 updater manifest 步骤。
写本页时没有重跑这些步骤。其中的平台检查不是当前证据。CLI 宿主上的已签名桌面更新仍然不可用，见 [CLI 指南](../user/cli.zh-CN.md)。

## container.yml

该文件在 GitHub Release 被发布时触发，也可通过带 tag 的 `workflow_dispatch` 触发。这次只读了触发头和 resolve job 的头部。镜像名、digest 和平台结果没有复查。它不是当前 CLI 的发布路径。

## pages.yml

该文件在 `workflow_dispatch` 时触发，也在推送到 `main` 且改动工作流、`docs/**` 或 `scripts/build-pages.mjs` 时触发。job 运行 `node scripts/build-pages.mjs`。当前分支的 `scripts/` 里没有这个脚本。本页不把该工作流当作已核实的发布器。

## CI 覆盖不到的

`quality.yml` 覆盖带 loopback feature 的 Linux workspace Rust 测试与 Clippy，以及 Windows 原生 CLI 包的测试与 Clippy。它不运行 Compose、pnpm、桌面 crate 或 `scripts/cli-acceptance.mjs`。

`release.yml` 不是当前 CLI 发布。 [发布流程](releasing.zh-CN.md) 仍描述已移除的桌面包，因此它不是当前质量清单。

这些工作流之外还有：第三方客户端配置与推理、备份与恢复演练、真实上游账号，以及 Google 或 OpenCode 登录。真实支付不是例行要求。数据库不支持降级；回滚使用升级前备份，见[存储与迁移](storage-migration.zh-CN.md)。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](ci.md) · [文档索引](../README.zh-CN.md)
