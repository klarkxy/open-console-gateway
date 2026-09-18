[English](overview.md)

# 产品定位

Open Console Gateway 是一台本地 Gateway：把内置供应商 Key、受信的 Custom API 目的地和用户定义供应商定义保存在 SQLite 数据库里，并通过回环地址 `http://127.0.0.1:9042/v1` 暴露给客户端。Provider 与 Plan 是同一个产品身份，只以 `provider_id` 为键；每张账号卡归属其中一个 Provider。客户端发送本地注册表里的 **别名** 或符合要求的 Custom 模型 ID；当前可路由的是 OpenCode Go、Zen Free、Command Code GOAT、MiniMax CN Token Plan、Kimi Code CN、Custom API 与已保存的用户定义供应商。Vue 3 管理面板在 `/dashboard/`，当前 SPA 只通过 `/dashboard/api/v4` 读写 JSON（`/dashboard/api/v3` 是 410 墓碑）。每个节点把数据保存在本地。

Gateway 只做四件事，顺序基本符合直觉：

1. 用面板签发的 **Key** 验证客户端。
2. 用本地 Alias 注册表（以及合格 Custom 声明 ID）解析客户端模型名，再经能力过滤、适配器上限、已保存的供应商合约，以及按模型协议 effective 状态后挑一张可用账号卡。
3. 把请求转换到所选 Plan 的有效上游协议，再把响应转回客户端协议。协议选择使用已保存的合约。
4. 把请求日志（`requested_model`、`resolved_alias`、`upstream_model`）、用量、冷却全部写回 SQLite，并在面板里呈现。

## 一个节点长什么样

桌面端、CLI 和 Docker 都运行同一个 `ocg-core` 进程，默认绑定 `127.0.0.1:9042`。
面板在系统浏览器里打开；AI 客户端用 OpenAI、Anthropic 或 Gemini 格式访问 `/v1`。

整合后的[架构图](architecture.zh-CN.md#一个本地节点)展示控制面、推理面、SQLite
状态，以及彼此独立的客户端 Key 与账号凭据路径。

---

[用户指南索引](../USER.zh-CN.md) · [English](overview.md) · [文档索引](../README.zh-CN.md)
