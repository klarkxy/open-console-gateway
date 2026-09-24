[English](release-artifacts.md)

# 发布产物

Open Console Gateway 为三个平台提供桌面安装包、各一份 CLI 压缩包，以及一份多架构容器镜像。

| Runner | GUI | CLI |
| --- | --- | --- |
| Windows 10/11 x64 | NSIS 当前用户安装包 | x64 ZIP |
| macOS 11+ | Universal DMG（x64 + ARM64） | Universal tar.gz |
| Linux x64 | AppImage + deb | x64 tar.gz |

稳定的产物命名：

```text
ocg-manager_<version>_windows-x64-setup.exe
ocg-manager_<version>_windows-x64-setup.exe.sig
ocg-manager-cli_<version>_windows-x64.zip
ocg-manager_<version>_macos-universal.dmg
ocg-manager_<version>_macos-universal.app.tar.gz
ocg-manager_<version>_macos-universal.app.tar.gz.sig
ocg-manager-cli_<version>_macos-universal.tar.gz
ocg-manager_<version>_linux-x64.AppImage
ocg-manager_<version>_linux-x64.AppImage.sig
ocg-manager_<version>_linux-x64.deb
ocg-manager_<version>_linux-x64.deb.sig
ocg-manager-cli_<version>_linux-x64.tar.gz
compose.example.yaml
cpa-config.example.yaml
latest.json
SHA256SUMS
```

每个 CLI 压缩包包含可执行文件、`dist/` 和 `LICENSE`。`serve` 依赖同级 dashboard
资源，因此要分发整个压缩包。Windows 没有便携 GUI 安装包。
`ocg-manager` Codex skill 内置在桌面与 CLI 二进制中，不另占压缩包文件。原生正式版启动时会把该构建对应的 skill 同步到当前用户的 `~/.agents/skills`；打包后的 CLI 冒烟检查使用隔离 Home 并核对安装结果。Docker 构建不会在容器或宿主机同步 skill。

`linux/amd64` 与 `linux/arm64` 容器单独发布为
`ghcr.io/klarkxy/opencode-go-mgr`。GitHub Release 包含七份平台 payload、macOS
升级压缩包、四份升级签名、Compose 与 CPA 配置示例、`latest.json` 和
`SHA256SUMS`，当前共 16 个附件。本地验证器和工作流都要求 GitHub 附件的名称与
数量同组装后的 `release/` 目录完全一致。

## scripts/release.mjs

`pnpm run build` 运行此脚本。它校验版本钉、可选升级签名
（`OCG_REQUIRE_UPDATER_ARTIFACTS=1` 在缺密钥时 fail-close；公钥必须匹配
`src-tauri/updater-public-key.sha256`），用 `@tauri-apps/cli` 构建当前平台，
把 CLI 与 `dist/`、`LICENSE` 打包，写 `SHA256SUMS`，并原子替换 `release/`。
未签名的本地构建只适合冒烟。

macOS 升级压缩包需要 `app` bundle target。deb 不是 Tauri 原生升级产物，因此
用 `tauri signer sign` 显式签名。`pnpm run release:check` 做同样校验，不构建
原生安装包。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](release-artifacts.md) · [文档索引](../README.zh-CN.md)
