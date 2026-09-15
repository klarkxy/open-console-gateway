[English](external-integrations.md)

# 外部接入

外部接入是可选、受本产品支持的本机服务，用来扩展 Open Console Gateway。管理面板保留八个核心页面；受支持的入口位于 **设置** 下方通用的 **扩展** 分组。

## CPA

CPA（CLI Proxy API）是本机订阅运行时。Open Console Gateway 可管理其当前稳定支持的 Codex、Claude、Antigravity、Kimi 和 xAI 账号流程，并路由得到的订阅池；但 OAuth 浏览器会话、Token、auth 文件和内部调度始终由 CPA 持有。OCG 只保存本地连接配置、两把 CPA 凭据和本地模型快照。Management Key 在 OCG 存储中加密，托管子进程只通过 `MANAGEMENT_PASSWORD` 接收它。CPA 自身的配置必须包含客户端 `api-keys`，因此受保护的 Inference Key 和直连客户端 Key 必然出现在 OCG 数据目录下的 CPA 本地配置中。创建客户端 Key 时，V3 仍只返回一次明文；列表只显示指纹。

只支持以下本机部署：

- **Windows x64、macOS 或 Linux x64 的桌面版或 CLI：** OCG 可以下载对应该操作系统和 CPU 的官方 CLIProxyAPI 资源，放在 OCG 数据目录下，并作为 OCG 拥有的子进程启动。启动只能手动；OCG 退出时子进程停止。OCG 从不停止它没有启动的 CPA。其他操作系统/CPU 没有官方资源，安装会以明确原因失败。
- **桌面版或 CLI：** 在同一台机器运行 CPA，并配置回环地址，例如 `http://127.0.0.1:8317`。
- **Docker：** 启用 [Docker](docker.zh-CN.md) 中的可选 Compose 并列服务。OCG 使用只读的 `http://cpa:8317` 服务地址；面板不接受局域网、互联网或跨节点 CPA 地址。

包含内嵌凭据、query、fragment、重定向或非回环主机的 URL 会被拒绝。不要把 Open Console Gateway Key 复用为任一 CPA Key。

### 连接与运维

1. 在 Windows x64、macOS 或 Linux x64 上（桌面版或 CLI），从 **扩展 → CPA** 安装或启动托管 CPA 运行时；也可以自行在回环上安装并启动 CPA，再保存其 **Management Key** 与 **Inference Key**。托管运行时由 OCG 生成这两把 Key；额外的直连客户端 Key 放在 **概览**，只显示指纹，新生成的密钥只返回一次。OCG 保护的 Inference Key 不能删除。Management Key 不会写入 CPA 的 `config.yaml`；Inference Key 与直连客户端 Key 会写入，因为 CPA 要求该文件包含 `api-keys`。
2. 打开 **扩展 → CPA**，在连接外部 CPA 时保存本地地址和两把 Key，再运行连接检测。它分别显示可达性、受支持的 CPA 版本、Management 鉴权和 Inference 鉴权。OCG 要求 CPA 7.1.0 或更高版本；更高 major 仍继续接受相同的 typed 响应与精确账号校验，不会只因版本号被拒绝。
3. 全新托管安装可以在模型目录为空时正常启动；这表示 CPA 与本机鉴权正常，并不意味着已有可路由模型。在 CPA 账号表中发起 OAuth。浏览器回调类 provider 使用 CPA 自己的回环回调端口；Kimi 与 xAI 使用设备码流程。OCG 不会运行 OAuth 回调服务器，刷新页面或重启后也不会恢复旧流程。
4. 打开 **模型目录** 并刷新。该页签按 CPA 报告的来源（`owned_by`）把已保存快照排成可选中的卡片；高亮的卡片加入路由，未选中的 ID 仍保存在快照里但不发布。首次刷新，以及之后新出现的模型，默认不加入路由，需要你再点选。在此选择标记出现之前保存的目录会继续路由全部 ID，直到你改选。全新安装的目录可以为空；OAuth 账号就绪后再刷新。然后启用 CPA 订阅池。Accounts 页中的 **CPA 订阅池** 单例卡可像其他路由候选一样排序、启停，但不会暴露 Key、不能删除，也不会把 CPA 内部 OAuth 账号伪装成 OCG 账号。托管运行时的额外直连客户端 Key 放在 **概览**，不再单独占一个页签；日常使用走 OCG 的接入 Key。

停用订阅池只会移出路由，不会忘记 CPA 配置。经确认的 **断开并清除** 会删除 OCG 保存的 CPA 配置、订阅池卡和本地模型快照；不会删除 CPA 自己的 OAuth 文件。CPA 故障只会让当前路由跳过该候选，其他合格 OCG 账号仍可继续被选择。

移除由 OCG 托管的 CPA 运行时则不同：它会删除该托管安装、本地运行时配置，以及托管 `auth/` 目录中的 CPA OAuth 凭据；外部运行的 CPA 文件始终不会被删除。

### Codex 两种登录方式

Codex 提供**浏览器登录**和**设备码登录**。设备码登录需要已安装并运行的 OCG 托管 CPA，且该版本支持 `--codex-device-login`（已核对 CPA 7.2.152）。打开授权页面并输入页面显示的设备码；ChatGPT 账号的安全设置或工作区须允许设备码登录。

设备码通道不监听 1455，因此可以避开 Windows 保留该端口的问题。OCG 启动独立的受控 CPA 登录子进程，令牌兑换和凭据保存仍由 CPA 完成，不中断现有网关。取消、约 15 分钟后过期、OCG 退出或托管运行时生命周期操作都会停止登录子进程；取消不会删除 CPA 已经保存的凭据。外部 CPA 仍使用浏览器登录：当前 CPA 版本没有 Codex 设备码登录管理 API。

### 从本机 CLI 导入登录态

CPA 账号页同时提供**新登录**和**从本机 CLI 导入**。检测仅检查文件是否存在；点击某个 CLI 的导入按钮后，才读取该凭据文件并将转换后的必要字段发送给 CPA。托管 CPA 和已连接的本机 CPA 均可使用。请在运行 CLI 的那台机器上打开 OCG 本机面板；远程面板不能读取这些来源。

| CLI | 支持的来源 | 范围 |
| --- | --- | --- |
| Codex | `$CODEX_HOME/auth.json`，默认 `~/.codex/auth.json` | 含刷新令牌的 ChatGPT OAuth；不导入 API Key、外部登录模式或仅存于系统凭据库的登录态 |
| Claude Code | `$CLAUDE_CONFIG_DIR/.credentials.json`，默认 `~/.claude/.credentials.json` | 含刷新令牌及 `user:inference` 权限的 OAuth；macOS Keychain 登录需重新授权，除非 CLI 已使用文件回退 |
| Kimi Code | `$KIMI_CODE_HOME/credentials/kimi-code.json`，默认 `~/.kimi-code/credentials/kimi-code.json` | 官方 Kimi Code OAuth 文件格式 |
| Grok CLI | `$GROK_HOME/auth.json`，默认 `~/.grok/auth.json` | `https://auth.x.ai` 下与 CPA 客户端 ID 一致的标准 OIDC 条目；拒绝其他 Key 或签发方 |
| Antigravity | 暂不支持 | 尚无可靠的兼容本机凭据存储约定；继续使用 CPA 登录 |

无法从本机导入的来源收成一条提示，详细原因放在悬停说明里；请改用上方的新登录。

账号列表上的**配额**来自 CPA 自己的本机用量记录，用来跟踪该 OAuth 账号在 CPA 里是否触达限额；这不是 ChatGPT / Claude 等供应商的官方套餐额度。没有用量时不显示。**重置配额**只清 CPA 的计数，不会向供应商申请重置。

导入是一次性复制。OCG 不修改 CLI 源文件、不将 OAuth Token 存入自身数据库或显示到页面，也不持续同步两边的状态。CPA 保存和刷新导入后的副本。两份凭据共享同一授权，刷新或撤销可能导致另一端需要重新登录。已有匹配导入不会被覆盖；需要替换时，请先明确删除 CPA 中的旧条目。导入期间请避免在其他客户端同时管理 CPA 账号。上传结果无法确认时，页面会提示先刷新账号列表；固定导入文件名用于核对相同来源身份或未变化的授权，避免盲目创建新文件。

格式已对照 CPA 7.2.152、Codex 官方存储实现、Claude Code 文件存储文档、Kimi Code `f9ca333` 和 Grok CLI 1.0.13 核查。参考 [Codex 存储实现](https://github.com/openai/codex/blob/main/codex-rs/login/src/auth/storage.rs)、[Claude 凭据存储](https://code.claude.com/docs/en/authentication#credential-management)及 [Kimi 存储实现](https://github.com/MoonshotAI/kimi-code/blob/f9ca33376604ae91ea35a4ac1d6f1d4425a5aead/packages/oauth/src/storage.ts)。

## 新增其他接入

静态外部接入出现在 **扩展** 分组。贡献时应提供 typed Dashboard V3 adapter 和有文档的本机边界，供代码评审核对。贡献路径见[扩展 Open Console Gateway](../maintainer/extending.zh-CN.md)。

---

[用户指南索引](../USER.zh-CN.md) · [English](external-integrations.md) · [文档索引](../README.zh-CN.md)
