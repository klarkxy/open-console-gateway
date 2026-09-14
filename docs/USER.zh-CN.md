[English](USER.md)

# 用户指南

本指南面向把 Open Console Gateway 当作桌面应用、无头 Gateway 或 Docker 服务运行的人。章节按你实际会撞上它们的顺序排列：先装起来，排障在后。

## 新增集成

- [新增供应商](user/add-provider.zh-CN.md) — 创建用户定义供应商、通过 Custom API 接入单个兼容上游，或贡献一个具备完整 HTTP 与路由契约的密封内置供应商。
- [手动客户端配置](user/add-application.zh-CN.md) — 通过 Gateway API 直接连接客户端。

- [New API 与 Sub2API 账号](user/platform-accounts.zh-CN.md) — 归组多个 Custom API Key，手动刷新分范围额度和价格，保留全局路由顺序。

## 章节

- [产品定位](user/overview.zh-CN.md) — 产品定位与 Gateway 承担的四个职责。
- [架构图](user/architecture.zh-CN.md) — 节点、客户端请求、Plan 与面板的文字图。
- [安装与首次启动](user/install.zh-CN.md) — Windows、macOS、Linux 安装包；附赠 SmartScreen 仪式。
- [接入第一个客户端](user/first-client.zh-CN.md) — 复制 Key 与 API Base URL，用一个请求验证连通。
- [升级、备份、恢复与卸载](user/upgrade-backup.zh-CN.md) — 升级通道、手动升级、备份、恢复与卸载。
- [管理面板](user/dashboard.zh-CN.md) — 七个核心页面、扩展分组、国际化与接入中心。
- [旧应用功能——已退役](user/applications.zh-CN.md) — 退役范围。
- [账号](user/accounts.zh-CN.md) — Plan、凭据、排序、额度行为与托管注册。
- [供应商](user/providers.zh-CN.md) — 目录、供应商合约、按模型协议覆盖、探测与用户定义供应商。
- [日志与设置](user/logs-settings.zh-CN.md) — 请求日志、设置、代理模式与主题。
- [Gateway 行为](user/gateway.zh-CN.md) — 端点、鉴权、别名、Zen Free 与熔断。
- [协议转换](user/protocol-conversion.zh-CN.md) — 推荐/已验证协议、透传与转换边界。
- [路由、费用与故障转移](user/routing.zh-CN.md) — 选择顺序、粘性/轮询、费用估算与故障转移。
- [CLI](user/cli.zh-CN.md) — 无头 CLI 压缩包、数据目录与 `serve` / `key` / `status`。
- [Docker](user/docker.zh-CN.md) — GHCR 镜像、Compose 部署、浏览器 Sidecar 与源码构建。
- [外部接入](user/external-integrations.zh-CN.md) — 本机 CPA 配置、数据归属、路由订阅池与断开行为。
- [数据与安全](user/data-security.zh-CN.md) — 数据目录、凭据存储与加密边界。
- [限制](user/limits.zh-CN.md) — 明确报错、未实现的表面与平台边界。
- [常见问题](user/troubleshooting.zh-CN.md) — 首次启动、鉴权、路由与日志的常见问题。

## 阅读路径

- **新用户** — `overview` → `architecture` → `install` → `first-client` → `accounts` → `providers` → `gateway` → `troubleshooting`。
- **Docker / CLI 运维** — `overview` → `architecture` → `docker` → `external-integrations` → `cli` → `accounts` → `providers` → `routing` → `logs-settings` → `troubleshooting`。
- **集成作者** — 上游供应商读 `add-provider`；下游客户端读 `add-application`。

---

[文档索引](README.zh-CN.md) · [English](USER.md)
