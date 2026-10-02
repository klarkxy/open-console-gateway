[English](architecture.md)

# 架构

本页定义稳定的依赖与所有权边界。运行时边缘情况、schema 历史、完整路由和发布流程
留在各自章节。

## 依赖图

```text
ocg-gateway -> ocg-domain
ocg-core    -> ocg-domain + ocg-gateway + ocg-infra
ocg-cli     -> ocg-core

ocg-browser-worker   独立进程；不依赖内部 ocg-* crate
```

当前分支只包含 Rust workspace 和无头 CLI。Vue workspace 与 Tauri 桌面 crate
不在此分支；未来客户端仍只通过 HTTP 使用 Dashboard V4，不增加 WebView 变更路径。

**Adapter Registry** 静态密封。运行时 Provider 定义是绑定 Configurable HTTP 的
类型化数据。

| Crate | 负责 | 禁止持有 |
| --- | --- | --- |
| `ocg-domain` | ID、`BUILTIN_PROVIDERS`、`ProviderAdapterKind`、协议表、类型化动态定义 | DB、`CoreState`、HTTP client、文件系统、时钟 |
| `ocg-gateway` | Alias 解析、`AttemptSpec`、分类、selector 状态机、无 I/O JSON 转换 | DB、`CoreState`、明文凭据、出站 HTTP |
| `ocg-infra` | Key 混淆、代理感知 HTTP helper、推理传输、SQLite 日志语句 | 产品目录、Dashboard DTO、路由策略 |
| `ocg-core` | SQLite、`CoreState`、Dashboard 控制面、适配器、Gateway 执行、用量同步、Host 组合 | 运行时插件加载；适配器自持 DB 或 HTTP client |
| `ocg-cli` | 常驻 `serve`、非变更 `api`、离线 `schema` | 第二套控制面、私有变更路径，或 Dashboard 路由定义 |

`ocg-domain::credential` 持有身份/凭据/绑定词汇以及唯一的遗留映射器。

兼容 facade 位于 `ocg-core`；新的无 I/O 目录、selector、Alias 与转换行为应进入
下层 crate。

## HTTP 组合

`crates/ocg-core/src/host_router.rs` 是单一监听器的组合根：

```text
127.0.0.1:9042
  推理入口
    OpenAI Chat / Responses / Anthropic Messages
    Gemini generateContent / streamGenerateContent
    本地 GET /v1/models
  /dashboard/api/v3       410 墓碑
  /dashboard/api/v4       当前唯一的 Dashboard 控制面
  /dashboard/api          保留 auth + browser WS；其余 REST -> 410 墓碑
  /dashboard/             宿主提供静态资源时的资源入口
```

`ocg-manager-cli serve` 是常驻的原生宿主。它在上述监听器上安装
`console_router`：推理、当前 V4 控制面、保留的 auth、保留的浏览器套接字、V3
墓碑，以及存在 `dist/` 时的静态资源。`api` 是该监听器的非变更 HTTP 客户端。
即使带了 `--data-dir`，它也不打开 SQLite。`schema v3|v4` 打印离线 JSON 帮助，
不联系 `serve`。

共享的 core service 留在 HTTP 处理器后面。`serve` 注册原生浏览器启动与停止、
自有 CPA 的恢复与关闭。默认 `dsh-local-host` 还注册 BYOK 与 DSH 宿主。
`--no-default-features` 构建仍挂载 BYOK 与 DSH 应用路由，并返回对应的 unsupported-runtime
状态。托盘、Dock 和已签名的桌面更新保持未注册。

`api` 发送普通 HTTP 请求并读取响应。它不升级浏览器套接字，因此没有
`api GET .../ws` 命令。保留路径 `/dashboard/api/browser/sessions/{token}/ws`
不在客户端允许列表中。服务器仍挂载该套接字。观看远程会话是以后的远程查看器，
或指向服务器的外部 WebSocket 客户端。

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

遗留 Custom API 行是绑定同一密封适配器种类的独立可配置 `http` 目的地。
连接可以持有多个凭据，同时保持仅公开模型名称的解析边界。CPA 是另一条静态外部集成。

Provider 目录与合约先于账号凭据解析。保存的发现行只能激活代码持有 Alias 映射，或
继续作为精确 raw pin。

## 控制面

操作者通过 `api` 在 `/dashboard/api/v4` 调用挂回的操作处理器和原生 V4 路由。
已移除的 Vue 客户端不属于当前分支。`/dashboard/api/v3` 是 410 墓碑。活的面板 JSON 只走 V4。
受 CAS 保护的变更携带 `expectedRevision` 与 `processGeneration`。`--cas-current`
只在请求体缺少这两个字段时，从公开的 `GET /dashboard/api/v4/auth/status` 复制它们，
已给出的值保持原样。该标志不填价格字段。倍率写入携带的 `expectedPricingRevision`
来自该 Provider 的价格快照：`opencode` 用 `pricingRevision`，`command-code` 用
`providerPricingRevision`。Provider 价格刷新携带的 `expectedProviderPricingRevision`
来自该 Provider 的价格读取。不变更状态的操作读取与诊断跳过 CAS。`api` 在 409、429、
超时或端口重绑之后不重放变更。

共享 service 为 HTTP 处理器负责持久化与 revision bump。CLI 不另存一套业务逻辑。

listener 必须使用宿主预先安装的路由工厂。`serve` 安装 `console_router`。测试可以通过
`start_gateway_on` 安装同一库级工厂。listener 不自行选择路由组合。`api` 与 `schema`
不安装工厂。

`account_control` 负责凭据轮换、HTTP 目标替换和删除、路由卡片布局、内置目录新增和编辑、
公开模型发布以及模型元数据声明。可配置 HTTP 目录刷新仍在其 adapter 中保留准备、锁外发现和提交。
以下操作继续由各自现有完整模块负责：onboarding、计费、身份凭据创建、额度重试、绑定、
临时策略、CPA 选择、平台导入、应用安装、settings 重绑、官方目录/价格/用量刷新、账号迁移，
以及旧 provider/account 兼容写入。CLI 通过 HTTP 到达这些模块。`api` 不打开数据库，
也不调用遗留的 `key` 动词。

Settings 的持久化、重绑与补偿顺序见
[Dashboard API](dashboard-api.zh-CN.md#settings-变更流程)。账号 setup 状态见
[状态与生命周期](state-and-lifecycle.zh-CN.md#托管账号-setup-生命周期)。

## 细节归属

| 细节 | 权威章节 |
| --- | --- |
| Alias、selector、协议、重试、冷却、模型列表 | [运行时不变式](runtime-invariants.zh-CN.md) |
| Dashboard V4 DTO、挂回的处理器、CAS、V2/V3 墓碑 | [Dashboard API](dashboard-api.zh-CN.md) |
| 锁、账号 setup、浏览器 worker、进程生命周期 | [状态与生命周期](state-and-lifecycle.zh-CN.md) |
| 数据表、迁移、备份与回滚 | [存储与迁移](storage-migration.zh-CN.md) |
| 完整 HTTP 路由 | [HTTP 路由](http-routes.zh-CN.md) |
| Workspace 结构与开发命令 | [结构](layout.zh-CN.md)、[开发](development.zh-CN.md) |
| 扩展边界 | [扩展 Open Console Gateway](extending.zh-CN.md) |

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](architecture.md) · [文档索引](../README.zh-CN.md)
