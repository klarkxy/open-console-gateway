[English](ci.md)

# CI 工作流

工作流在 `.github/workflows/`。本页只记录 YAML 里看不出来的拆分。

## quality.yml

在 pull request 与 `main` 上直接运行，也可由生产 tag 通过 `workflow_call`
调用。手动候选构建跳过。三条并行 job：

- **Web** — `contract:v3:check`、`typecheck`、`test:web`、Vite 生产构建、
  `DESIGN.md` lint，以及
  `docker compose -f compose.example.yaml config --quiet`。
  `pnpm run test:tooling` 不在此 job。
- **Rust** — `cargo fmt --all -- --check`、锁定依赖的 workspace 测试与
  Clippy `-D warnings`，并 `--exclude ocg-manager`（桌面 crate 需要 WebKit
  头文件和占位 `dist/index.html`；由 Windows job 覆盖；Linux 上的
  `src-tauri` 编译在 release 矩阵）。
- **Windows Tauri** — 对 stub `dist/index.html` 跑
  `cargo test -p ocg-manager --lib` 与 Clippy `-D warnings`，同时覆盖
  Windows 登录自启注册表同步。

## release.yml

触发：`workflow_dispatch` 与 `v*` tag。

- 手动触发：按所选平台生成未签名冒烟产物；即使 ref 是 tag 也不注入生产签名。
- `v*` tag **push**：完整三平台矩阵、仓库签名密钥、质量门加上 Ubuntu
  预检（版本清单、发版辅助测试、签名对与
  `src-tauri/updater-public-key.sha256`）。随后原生构建、CLI/GUI 冒烟、
  `draft-release` → `verify-release` → `publish-release`。

`verify-release` 要求 GitHub 附件名称与组装后的 `release/` 集合一致
（当前 16 个文件）。draft job 传递数字 Release ID，因为 tag 查询端点
看不到 draft。发布进入 `release-moving-channels` 串行队列。`latest`
只对严格更高的稳定 SemVer 前进。预发布 tag 设 `prerelease=true` 且
`make_latest=false`。

Windows GUI 冒烟是 `scripts/smoke-windows-release.ps1`（自启用 V3 CAS，
NSIS `/UPDATE` 原地升级）。macOS 检查 universal `lipo` 与 ad-hoc
`codesign`，并重跑 Linux quality.yml 已覆盖的 Unix CPA 进程归属测试
（`cpa_runtime::host`）。Linux 在 Xvfb 下启动 AppImage。

## 升级签名

密钥只在仓库外生成一次：

```powershell
node node_modules/@tauri-apps/cli/tauri.js signer generate -w <secure-path>/ocg-updater.key
```

私钥与密码存为 repository secrets `TAURI_SIGNING_PRIVATE_KEY` 与
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。公钥**内容**存入 variable
`TAURI_UPDATER_PUBLIC_KEY`。`src-tauri/updater-public-key.sha256` 是已提交
信任锚点；轮换属于 break-glass（新密钥对、为既有客户端准备直接安装引导、
审查指纹变更）。至少保留两份独立加密备份。升级签名不是操作系统代码签名。
Windows 安装包未签名；macOS 使用 ad-hoc（`-`）。

## container.yml

用 `github.token` 公开的 Release 不会启动此工作流。签名 tag 流水线结束后，
对该 tag 显式触发（稳定版 `publish_latest=true`）。

原生构建：amd64 用 `ubuntu-24.04`，arm64 用 `ubuntu-24.04-arm`。冒烟（主镜像
+ 浏览器）只在 amd64 运行。镜像先按 digest 推送；用户可见标签只在本地
OCI index 预检、匿名拉取两个精确版本标签、以及 GitHub provenance 之后创建。
`X.Y.Z` 与 `sha-*` 不可变；`X.Y` 与 `latest` 是单调移动通道。浏览器镜像是
GHCR 包，不是 Release 附件。

新的浏览器 package 在设为**公开**前保持私有；第一次运行预期停在匿名拉取
门禁，随后以相同 digest 重跑完成发布。之后每个 Release 都必须在第一次运行
时通过该门禁。

## pages.yml

把 `docs/` 发布到 GitHub Pages（入口 `docs/index.html`）。首次部署前把仓库
Pages 源设为 **GitHub Actions**。

## CI 覆盖不到的

质量门覆盖前端、排除桌面 crate 的 Linux Rust，以及 Windows 桌面单元测试。
原生安装包冒烟只在候选或 tag 流程运行。容器冒烟仅 amd64。

这些工作流未覆盖真实桌面交互、第三方客户端配置与推理、备份恢复、真实上游账号、
Google/OpenCode 登录、noVNC 输入与 Cookie 跨重启保留。按照[发布流程](releasing.zh-CN.md)
选择适用的人工检查并记录未执行项。真实支付不是例行发布要求。数据库不支持降级；
回滚使用升级前备份，见[存储与迁移](storage-migration.zh-CN.md)。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](ci.md) · [文档索引](../README.zh-CN.md)
