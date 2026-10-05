[English](README.md)

# Open Console Gateway — OCG3

`ocg3` 与 `open-console-gateway` 是同一项目的两代，关系如同 Python 3 与 Python 2。本目录是 OCG3 代。产品名仍是 Open Console Gateway。CLI 命令是 `ocg`。Rust 包名是 `ocg-cli`。

OCG3 是本地多 Plan 网关。它的执行目标是本进程拥有的一份本地 CPA 运行时，不是去连接另一个 CPA 产品。这份 CLI 仍未完成，因此原生桌面 GUI 继续延后。不规划 WebUI 或基于 WebView 的主界面。

`ocg` 的默认数据目录是 `~/.ocg3`。该命令不打开、不移动、不删除上一代的 `~/.ocg-mgr-cli`。用 `--data-dir` 指定目录。

- [架构](docs/architecture.zh-CN.md) — 责任、请求流、额度、重试、完整 CLI 验收与原生 GUI 设计。
- [迁移边界](docs/maintainer/cpa-migration.zh-CN.md) — 保留能力、CPA 职责、兼容与回滚。
- [CPA 验证结论](docs/maintainer/cpa-validation.zh-CN.md) — 已完成实验与尚未实现的保证。
- [当前 CLI 指南](docs/user/cli.zh-CN.md) — 构建、配置和操作本分支。
- [文档索引](docs/README.zh-CN.md) · [维护者指南](docs/MAINTAINER.zh-CN.md)。

已发布的 OCG2 安装包与桌面指南描述以前的版本，不能据此认为 CPA 底层 OCG3 已经可用。

[贡献者](docs/CONTRIBUTORS.md) · [许可证](LICENSE)
