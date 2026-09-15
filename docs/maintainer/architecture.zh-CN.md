[English](architecture.md)

# 架构

本页定义稳定的依赖与所有权边界。运行时边缘情况、schema 历史、完整路由和发布流程
留在各自章节。

## 依赖图

```text
ocg-gateway -> ocg-domain
ocg-core    -> ocg-domain + ocg-gateway + ocg-infra
ocg-cli     -> ocg-core
src-tauri   -> ocg-core

ocg-browser-worker   独立进程；不依赖内部 ocg-* crate
Vue SPA              静态资源；只走 HTTP Dashboard V3 + V4
```

**Adapter Registry** 静态密封。运行时 Provider 定义是绑定 Configurable HTTP 的
类型化数据。

| Crate | 负责 | 禁止持有 |
| --- | --- | --- |
| `ocg-domain` | ID、`BUILTIN_PROVIDERS`、`ProviderAdapterKind`、协议表、类型化动态定义 | DB、`CoreState`、HTTP client、文件系统、时钟 |
| `ocg-gateway` | Alias 解析、`AttemptSpec`、分类、selector 状态机、无 I/O JSON 转换 | DB、`CoreState`、明文凭据、出站 HTTP |
| `ocg-infra` | Key 混淆、代理感知 HTTP helper、推理传输、SQLite 日志语句 | 产品目录、Dashboard DTO、路由策略 |
| `ocg-core` | SQLite、`CoreState`、Dashboard V3、适配器、Gateway 执行、用量同步、Host 组合 | 运行时插件加载；适配器自持 DB 或 HTTP client |
| `ocg-cli` / `src-tauri` | CLI 与 Desktop 进程组合 | 第二套控制面或 WebView 直接变更路径 |

`ocg-domain::credential` 持有身份/凭据/绑定词汇以及唯一的遗留映射器。

兼容 facade 继续留在 `ocg-core`，但新的无 I/O 目录、selector、Alias 与转换行为应进入
下层 crate。

## HTTP 组合

`crates/ocg-core/src/host_router.rs` 是单一监听器的组合根：

```text
127.0.0.1:9042
  推理入口
    OpenAI Chat / Responses / Anthropic Messages
    Gemini generateContent / streamGenerateContent
    本地 GET /v1/models
  /dashboard/api/v3       当前 Dashboard 控制面
  /dashboard/api/v4       并行、仅增量的 Dashboard 控制面
  /dashboard/api          保留 auth + browser WS；其余 REST -> 410 墓碑
  /dashboard/             Vue SPA 与静态资源
```

SPA 始终是 HTTP 客户端。Desktop capability 注册进 `CoreState`。

## Gateway 请求路径

推理实现位于 `crates/ocg-core/src/gateway/`：

1. `handler.rs` 分配 request id、验证客户端 Key、解析客户端协议并解析模型身份。
2. `GatewayExecutor` 在请求入口捕获一次价格、代理路由、合约与 Alias 解析快照。fallback
   每轮重读实时账号状态、合格 Custom runtime 与 Zen Free 冷却。协议选择使用该次保存的
   合约。
3. 候选物化先应用适配器上限和 effective 模型/协议状态，再由无 I/O selector 选择账号卡。
4. `provider_adapter.rs` 对密封 `ProviderAdapterKind` 做穷尽映射并返回纯数据
   `AttemptSpec`；不解密 Key、不打开 SQLite，也不构造 HTTP client。
5. Host 解析所选账号凭据；`forward_once` 每次只调用一次上游 `.send()`，重试与 fallback
   策略留在外层循环。
6. 分类阶段决定同账号重试、账号 fallback、冷却或终止返回；随后 Host 转换响应并写日志
   （`requested_model`、`resolved_alias`、`upstream_model`）。

未知或有歧义的模型身份在出站 HTTP 前失败。超时、流中断及其他可能已经到达上游的
结果不会自动重放。完整状态码语义见[运行时不变式](runtime-invariants.zh-CN.md)。

## Adapter 与 Provider 边界

`ocg-domain::ProviderRegistry` 保存代码持有的内置 Provider 行和穷尽适配器种类。
未知 `provider_id` 默认失败；只有匹配已持久化类型化 Provider 定义时才例外，而这些
定义始终选择既有 Configurable HTTP 适配器。

Custom API 即使使用同一个密封适配器种类，仍是账号级产品路径。CPA 是另一条静态外部
集成。

Provider 目录与合约先于账号凭据解析。保存的发现行只能激活代码持有 Alias 映射，或
继续作为精确 raw pin。

## 控制面

Vue SPA 通过 `src/api/dashboard-v3.ts` 调用 V3，通过 `src/api/dashboard-v4.ts` 调用 V4（presenter 在 `src/api/connections.ts`）。
并行的仅增量 `/dashboard/api/v4` 与冻结的 V3 并存，共用同一套会话；现已包含 onboarding commit 变更。
受 CAS 保护的变更携带 `expectedRevision` 与 `processGeneration`；价格写入另带
`expectedPricingRevision`。不变更状态的操作读取与诊断跳过 CAS。

CLI 调用相同的 HTTP-neutral service，不带 argv CAS token。共享 service 负责持久化与
revision bump，同时服务 CLI 与前端。

Settings 的持久化、重绑与补偿顺序见
[Dashboard API](dashboard-api.zh-CN.md#settings-变更流程)。账号 setup 状态见
[状态与生命周期](state-and-lifecycle.zh-CN.md#托管账号-setup-生命周期)。

## 细节归属

| 细节 | 权威章节 |
| --- | --- |
| Alias、selector、协议、重试、冷却、模型列表 | [运行时不变式](runtime-invariants.zh-CN.md) |
| Dashboard V3 DTO、CAS、V2 墓碑 | [Dashboard API](dashboard-api.zh-CN.md) |
| 锁、账号 setup、浏览器 worker、进程生命周期 | [状态与生命周期](state-and-lifecycle.zh-CN.md) |
| 数据表、迁移、备份与回滚 | [存储与迁移](storage-migration.zh-CN.md) |
| 完整 HTTP 路由 | [HTTP 路由](http-routes.zh-CN.md) |
| Workspace 结构与开发命令 | [结构](layout.zh-CN.md)、[开发](development.zh-CN.md) |
| 扩展边界 | [扩展 Open Console Gateway](extending.zh-CN.md) |

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](architecture.md) · [文档索引](../README.zh-CN.md)
