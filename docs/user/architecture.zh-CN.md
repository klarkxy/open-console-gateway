[English](architecture.md)

# 架构

Open Console Gateway 是一个本地节点。Desktop、CLI 与 Docker 只是承载同一
`ocg-core` 进程的不同 Host。默认监听地址为 `127.0.0.1:9042`。每个节点把数据保存在本地。

## 一个本地节点

[![Open Console Gateway 单节点架构](../diagrams/local-node.visual-check.1440x900.light.png)](https://klarkxy.github.io/open-console-gateway/diagrams/local-node/)

[在 GitHub Pages 打开交互图](https://klarkxy.github.io/open-console-gateway/diagrams/local-node/)可以切换主题、追踪关系或导出其他格式。

Dashboard 与推理入口共用 `9042`，但使用两类不同凭据。客户端 **Key** 用于 AI
工具向 Open Console Gateway 鉴权；选定账号后，账号凭据只会发往该账号配置的上游，Zen
Free 没有凭据。Vue SPA 通过 HTTP Dashboard V3 通信。

## 请求生命周期

一次推理请求按固定顺序执行：

1. 使用 `access_keys` 中的客户端 **Key** 完成鉴权。
2. 解析客户端协议，并解析 Alias、精确内置 raw ID、用户定义 Provider 公开模型，
   或符合条件的 Custom 模型 ID。
3. 物化兼容账号，再按卡片顺序应用严格优先、全局粘性或轮询策略。
4. 由密封适配器构建一次尝试，解析所选账号凭据，并发送一次上游请求。协议选择
   使用已保存的合约。
5. 把响应或 SSE 流转换回客户端格式，随后记录请求身份、上游身份、用量与冷却状态。

未知模型返回 `400`。有歧义的精确 raw ID 返回 `ambiguous_model_id`，并停留在本地。
符合条件的发送前错误或 Provider 特定错误可以继续账号 fallback；有歧义或不安全的
请求在账号选择前失败。

## 产品归属

| 界面 | 负责 | 相关界面 |
| --- | --- | --- |
| **访问密钥** | 面向客户端的主 Key 与子 Key | 账号凭据在 **账号** |
| **账号** | 账号 Key、启停、顺序、备注、冷却与用量状态 | 目录与协议合约在 **供应商** |
| **供应商** | 内置目录、模型/协议合约、价格范围，以及用户定义 Provider 的 Endpoint/鉴权/映射 | Custom API 映射留在账号卡 |
| **Custom API 账号** | 一个 API URL、一个账号级上游协议、公开模型 → 上游 ID 映射 | 共享 Provider 定义在 **供应商** |
| **扩展 / CPA** | 一个静态本机外部集成边界 | 内置路由家族仍在 **账号** / **供应商** |
| **旧应用功能** | 已退役 | 客户端使用普通 Gateway API |

Adapter Registry 静态密封。用户定义 Provider 仅作为类型化数据持久化，并始终绑定
Configurable HTTP。

## 本地模型列表

这些读取使用已保存的本地状态。目录刷新由 **供应商** 页显式触发。

| Endpoint | 公布内容 |
| --- | --- |
| 已鉴权 `GET /v1/models` | 当前可路由的代码持有 Alias、已保存 Zen/Command/CN 映射、用户定义 Provider 公开模型，以及符合条件的 Custom 声明 ID |
| `GET /dashboard/api/v3/application-models` | Go 可路由 Alias 与当前 Go 价格快照的交集；不含 Custom API、用户定义 Provider 与 CN Plan |
| `GET /claude-desktop/v1/models` | 只公布三个 Claude Desktop 角色 Alias |

保存目录行在代码分配 Alias 前只保留精确 raw pin。与已公布内置 Alias 冲突的
Custom ID 不会进入公布列表。

## 协议转换

客户端可以使用 OpenAI Chat Completions、OpenAI Responses、Anthropic Messages、
Gemini `generateContent` / `streamGenerateContent` 或 Claude Desktop 入口。客户端与
上游协议组合同时受支持且启用时直接透传；否则整份请求与响应会转换到模型的 effective
上游协议，再转换回来。Gemini 是客户端格式：Gateway 把它转换到所选 Plan 的
Chat Completions 或 Messages 上游。

完整推荐/支持矩阵和转换限制见[协议转换](protocol-conversion.zh-CN.md)。

## 继续阅读

| 任务 | 指南 |
| --- | --- |
| 安装并接入客户端 | [安装](install.zh-CN.md)、[首个客户端](first-client.zh-CN.md) |
| 添加账号并排序 | [账号](accounts.zh-CN.md)、[路由](routing.zh-CN.md) |
| 管理目录与合约 | [供应商](providers.zh-CN.md) |
| 理解 Alias 与错误 | [Gateway](gateway.zh-CN.md) |
| 查看 crate 与 Host 边界 | [维护者架构](../maintainer/architecture.zh-CN.md) |

---

[用户指南索引](../USER.zh-CN.md) · [English](architecture.md) · [文档索引](../README.zh-CN.md)
