[English](cli.md)

# CLI

CLI 是同一个 `ocg-core` 进程的无头宿主。下载对应平台压缩包并解压，让 `dist/` 与可执行文件同级——否则 `serve` 无面板可发。Windows 下可执行文件是 `ocg-manager-cli.exe`；Linux 解压后可能需要 `chmod +x ocg-manager-cli`。

CLI 数据目录默认 `~/.ocg-mgr-cli`，所有平台一致，可用 `--data-dir <path>` 覆盖。混淆密钥默认放在 `<data-dir>/.encryption-key`，也可用 `--encryption-key <key>` 参数或 `OCG_MANAGER_ENCRYPTION_KEY` 环境变量覆盖。

CLI 只提供 `serve`、`key`、`status`。`key` 管 OpenCode Go 账号凭据，不是面板 Key，也不碰 Custom 目的地或 Zen Free 卡片；按模型协议覆盖等留在面板里操作。CLI 写入会直接 bump 该进程的 settings revision，命令行没有 `expectedRevision`。

```text
ocg-manager-cli
├── serve         Start the gateway server
│   --host        Address to listen on (default 127.0.0.1)
│   -p, --port    Gateway port (sets and saves config)
│   --dashboard-dir  Directory containing the built web dashboard
├── key list      List OpenCode Go API-key accounts (excludes Zen Free)
├── key add <name> <key>
│   --username    OpenCode-Go login account
│   --password    OpenCode-Go login password
├── key remove <id>      Remove an account
├── key enable <id>      Enable an account
├── key disable <id>     Disable an account
├── key ping [id]
│   --model       Model to send (default mimo-v2.5)
│   --message     User message (default "ping")
│   --max-tokens  max_tokens for the ping (default 3)
└── status        Show data dir, gateway port/key, upstream, account totals
```

`key add` 写入的是关闭的 OpenCode Go 草稿。确认 Key 可用后（`key ping`，再 `key enable`）再打开。

无头 Gateway 的最快搭法：

```bash
./ocg-manager-cli key add main sk-...
./ocg-manager-cli key list
./ocg-manager-cli key ping <id>
./ocg-manager-cli key enable <id>
./ocg-manager-cli serve --port 9042
```

`serve --port <port>` 把端口写进 SQLite；之后不带该参数的 `serve` 会继续使用这个值。

`key ping` 读取混淆后的 Key、发一条极小的 chat completion，然后打印真实上游状态码和一段响应摘要——不用开面板就能确认每个 Key 是 `401`/`403`/`429` 还是 `200`。

---

[用户指南索引](../USER.zh-CN.md) · [English](cli.md) · [文档索引](../README.zh-CN.md)
