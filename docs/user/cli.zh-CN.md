[English](cli.md)

# CLI

CLI 是同一个 `ocg-core` 进程的无头宿主。下载对应平台压缩包并解压，让 `dist/` 与可执行文件同级——否则 `serve` 无面板可发。Windows 下可执行文件是 `ocg-manager-cli.exe`；Linux 解压后可能需要 `chmod +x ocg-manager-cli`。

CLI 数据目录默认 `~/.ocg-mgr-cli`，所有平台一致，可用 `--data-dir <path>` 覆盖。混淆密钥默认放在 `<data-dir>/.encryption-key`，也可用 `--encryption-key <key>` 参数或 `OCG_MANAGER_ENCRYPTION_KEY` 环境变量覆盖。

CLI 提供 `serve`、`key`、`status` 和 `skill sync`。请运行当前可执行文件及相关子命令的 `--help` 查看该版本准确用法。`key add` 和 `key ping` 只针对 OpenCode Go；`key list` 与 `status` 会统计各 Provider 的 API Key 账号，`key remove/enable/disable` 可按 ID 修改其他 Provider 的账号。修改前应在面板确认 ID 的归属。面板 Key、Custom 目的地、按模型协议覆盖等留在面板里操作。CLI 写入会直接 bump 该进程的 settings revision。

`serve` 和 `status` 默认隐藏主 Gateway Key。需要查看时，可由本人在私有终端执行 `status --show-key`；不要把输出粘贴到 agent 对话或共享日志。旧式 `key add` 会把上游 Key（及可选密码）放进进程参数，也可能留在 shell 历史中；由 agent 协助时请在本机面板录入凭据。`--encryption-key <key>` 也有同样问题；新安装通常使用数据目录里自动生成的密钥文件即可。

原生 `serve` 运行期间，面板的**应用 > DSH**可以把 OCG 插件安装到同一台机器、同一个系统用户的 DSH。官方 Docker 镜像不支持这项本机安装。

原生正式版 CLI 内置 `ocg-manager` Codex skill。`serve` 启动时会把它安装或升级到 `~/.agents/skills/ocg-manager`；`skill sync` 可在不启动网关的情况下同步。只有内置 skill 内容变化时，既有 OCG 管理的版本才会备份到 `~/.agents/skill-backups/`；同名但不属于 OCG 管理的 skill 不会覆盖。开发构建的 `serve` 不自动同步；Docker 构建不会把 skill 安装到容器或宿主机。

`key add` 写入的是就绪且已启用的 OpenCode Go 账号；依赖它之前先用 `key ping` 复核。

先启动无头 Gateway，再到本机面板添加上游 Key：

```bash
./ocg-manager-cli serve --port 9042
```

保存账号后，如需实际探测上游，可在另一个终端运行 `key list` 和 `key ping <id>`。

`serve --port <port>` 把端口写进 SQLite；之后不带该参数的 `serve` 会继续使用这个值。

`key ping` 读取混淆后的 Key、发一条极小的 chat completion，然后打印真实上游状态码和一段响应摘要——不用开面板就能确认每个 Key 是 `401`/`403`/`429` 还是 `200`。
分享诊断信息时，应把这段由上游返回的摘要当作敏感内容处理。

---

[用户指南索引](../USER.zh-CN.md) · [English](cli.md) · [文档索引](../README.zh-CN.md)
