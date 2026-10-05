[English](overview.md)

# 产品定位

这一代是 OCG3。`ocg3` 与 `open-console-gateway` 是同一项目的两个代际分支，产品名仍是 Open Console Gateway。操作命令是无界面 `ocg` CLI（Windows 上为 `ocg.exe`，Linux 与 macOS 上为 `ocg`）。执行底座是该进程拥有的一个本地 CPA。自定义 HTTP 供应商仍是该本地 CPA 上的供应商路由。完整 CLI 是当前交付物，验收尚未完成。原生 GPUI/Ely GUI 推迟。命令见 [CLI 指南](cli.zh-CN.md)。设计见[架构](../architecture.zh-CN.md)。

当前源码把公共推理经 CPA 入口交给这个自有本地 CPA。该入口不是已验收的运行结果。本页不记录运行成功，也不记录已完成的迁移。

## 已发布的 open-console-gateway 一代

下面的说明是已发布的 `open-console-gateway` 一代。它是那一代保留下来的产品说明，不是当前的 `ocg` 包。

在那一代，Open Console Gateway 是一台本地 Gateway：把内置供应商 Key、受信的 Custom API 目的地和用户定义供应商定义保存在 SQLite 数据库里，并通过回环地址 `http://127.0.0.1:9042/v1` 暴露给客户端。Provider 与 Plan 是同一个产品身份，只以 `provider_id` 为键；每张账号卡归属其中一个 Provider。客户端发送本地注册表里的 **别名** 或符合要求的 Custom 模型 ID。那一代的路由包括 OpenCode Go、Zen Free、Command Code GOAT、MiniMax CN Token Plan、Kimi Code CN、Ollama Cloud、Custom API、CPA 订阅池与已保存的用户定义供应商。Vue 3 管理面板在 `/dashboard/`，那份 SPA 只通过 `/dashboard/api/v4` 读写 JSON（`/dashboard/api/v3` 是 410 墓碑）。每个节点把数据保存在本地。

已发布的 Gateway 说明有四项工作，顺序基本符合操作者的预期：

1. 用面板签发的 **Key** 验证客户端。
2. 用本地 Alias 注册表（以及合格 Custom 声明 ID）解析客户端模型名，再经能力过滤、适配器上限、已保存的供应商合约，以及按模型协议 effective 状态后挑一张可用账号卡。
3. 把请求转换到所选 Plan 的有效上游协议，再把响应转回客户端协议。协议选择使用已保存的合约。
4. 把请求日志（`requested_model`、`resolved_alias`、`upstream_model`）、用量、冷却全部写回 SQLite，并在面板里呈现。

### 一个节点长什么样

那一代的桌面端、CLI 和 Docker 各自运行同一个 `ocg-core` 进程，绑定 `127.0.0.1:9042`。面板在系统浏览器里打开。客户端以 OpenAI、Anthropic 或 Gemini 格式访问 `/v1`。这些桌面包和容器包是那一代的已发布包。

---

[用户指南索引](../USER.zh-CN.md) · [English](overview.md) · [文档索引](../README.zh-CN.md)
