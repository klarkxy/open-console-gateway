[English](http-routes.md)

# HTTP 路由

所有路由共享一个端口：推理、Dashboard V4（含挂回去的 V3 处理器）、V2 与 V3 墓碑、以及 SPA。详见[架构](architecture.zh-CN.md)。

被墓碑化的 `/dashboard/api/...` REST：匿名时返回空 body 的 **401**（鉴权先于墓碑），已鉴权时（含回环本地模式）返回 **410** `{ "code": "dashboardV2Removed", "message": "Dashboard API V2 has been removed; refresh the page and retry." }`。`/dashboard/api/v3` 前缀是另一套 410：`{ "code": "dashboardV3Removed", "message": "Dashboard API V3 has been removed; refresh the page and retry." }`。既非 V3 墓碑前缀、非 V4，也非保留家族的未知 `/dashboard/api/...` 路径，在已鉴权时同样 410。未知的 V4 路径是 V4 的 `404`，不是墓碑。保留的 `/dashboard/api` 家族（精确路径，无尾斜杠，无额外段）：`auth/status`、`auth/register`、`auth/login`、`auth/logout`，以及 `browser/sessions/{token}/ws`（token 非空）。受保护的 V2 REST 返回墓碑；活的 Dashboard JSON 只走 V4。

**权威路由清单是 `crates/ocg-core/src/dashboard_v4/mod.rs`（V4 原生路由）与 `crates/ocg-core/src/dashboard_v3/mod.rs`（挂回内核）中的路由构造。** 本页只描述语义与路由族，不再逐条枚举路径。新增、移动或下线路由请改那两处路由构造，不要在这里镜像。

## 推理

`/v1/...` 推理面在 `crates/ocg-core/src/gateway/mod.rs`（`inference_router`）注册，由 `crates/ocg-core/src/gateway/handler.rs` 中的 handler 分发：OpenAI Chat Completions 与 Responses、Anthropic Messages、需要鉴权的本地 `GET /v1/models`，以及 Gemini 客户端格式 `/v1beta/models/{model}:*`（同样挂在 `/v1/` 下）。同一组端点也在用户向的 [gateway.md](../user/gateway.zh-CN.md) 中说明。

路由本身看不出的语义注意事项：

- `/v1/responses` 为无状态：`store`、`previous_response_id`、`conversation`、`background` 一律 **400**。
- Gemini `{model}:{action}` 分发（`gemini_model_action`）：`generateContent` 与 `streamGenerateContent` 代理上游；`countTokens` 返回 **501**，属于预期回退（Gemini CLI 转为本地估算，该请求不会记为失败）；`embedContent` 返回 **501**（不支持向量嵌入）；未知 action 返回 Gemini 格式的 **404**。
- `GET /v1/models` 是本地清单（代码所有的别名加上符合条件的已保存名称）；需要鉴权，不调用上游。

## Dashboard V3 墓碑（`/dashboard/api/v3`）

`/dashboard/api/v3` 与 `/dashboard/api/v3/*` 已退役。匿名请求得到空 body 的 **401**。已鉴权请求得到 **410** `dashboardV3Removed`。

## Dashboard V4（`/dashboard/api/v4`）

公开（挂回、免会话）：`/auth/status`、`/auth/register`、`/auth/login`、`/auth/logout`，以及上文列出的保留 WebSocket 家族。其余路由全部会话保护，按族划分如下；每族标注所属模块，具体路径与方法以两处路由构造为准。

挂回内核（`crates/ocg-core/src/dashboard_v3/`）：

- **连接、设置与更新器**（`connection`、`settings`、`proxy_test`、`updater` 模块）：读取或修改保存的连接、测试出站代理、检查并安装更新。
- **Gateway Key**（`keys` 模块）：创建、列出与重新生成管理员 Gateway Key。
- **账号生命周期**（`accounts`、`usage`、`usage_refresh`、`account_model_test`、`account_verify`、`managed_key_verify`、`account_transfer` 模块）：创建、排序、启停账号；浏览器注册与 Profile；设置与 Key 校验；冷却重置；自定义配置与模型能力；用量与供应商用量的读取和刷新（含 Command Code 用量刷新）；按账号模型测试与连接验证。
- **供应商**（`providers`、`pricing`、`dynamic_providers` 模块）：密封与静态供应商行及价格倍率；用户定义供应商的增删改查、模型发现与真实连接测试；Zen `-free` 行与模型刷新。
- **供应商契约与模型协议**（内核中的 provider-contracts 家族）：契约列表、按 scope 的模型协议覆盖（provider 与 custom-endpoint 两种 scope）、目录刷新与静态协议表 reset-static。
- **协议探测**（`providers` 模块）：`POST /providers/{provider_id}/protocol-probes` 探测 Go/Zen 协议支持。Custom 在此被拒绝（`protocol probes for Custom API are account-owned`），V2 `POST /accounts/{id}/protocol-probes` 为 410。Custom 连接验证与模型发现属于账号生命周期家族与 `custom_discovery`（`POST /accounts/{id}/verify`、`POST /custom/models/discover`）。
- **平台账号**（`platforms` 模块）：列出、查看、刷新平台账号与平台关联。
- **CPA**（`cpa` 模块）：挂回的 `/external-integrations/cpa/*` 目录读写。
- **可观测性**（`observability` 模块与 application-models 处理器）：网关状态、面板汇总与按日 Token、网关/转发日志（含模型与 Key 过滤）。
- **浏览器**（`browser` 模块）：能力列表与账号网页 WebSocket。

V4 原生（`crates/ocg-core/src/dashboard_v4/`）：

- **控制面**：`GET /contract` 是 V4 原生 ControlRevision（`revision`、`processGeneration`、`pricingRevision`）；`/templates`、`/connections`、`/accounts` 分别列出创建模板、连接与账号身份。
- **目的地与目录**（`destinations`、`destination_catalog` 模块）：列出目的地与凭据；变更目的地或其目录。目的地变更带 CAS，只允许可配置 HTTP 行；密封与平台管理行不可改。
- **凭据**（`credentials` 模块）：轮换凭据、重试其配额状态。
- **账务**（`billing` 模块）：按账号的积分配置、校准与发放，均为本地估算。
- **官网 API 参考**（`official_api` 模块）：匹配预设的官网 API 状态与余额刷新、按供应商的官网价格表。
- **平台 Key**（`platform_keys` 模块）：向账号导入平台 Key。
- **入驻与绑定**（`onboarding`、`bindings`、`identities` 模块）：预设原子提交、Key 到模型的绑定、为身份新增凭据。
- **路由**（`routing`、`routing_cards` 模块）：`GET /routing/explain` 只读，不发出站请求也不解密 Key；`/routing/cards` 列出与替换路由卡。
- **CPA 目录**（`cpa` 模块）：V4 原生 `/cpa/models` 端点。
- **目录移除与别名发布**（`catalog`、`publication` 模块）：从供应商契约目录移除模型，读取或修改对外别名。
- **应用**（`applications` 模块）：DSH 应用状态与安装端点。

路由本身看不出的兼容垫片与数据形状说明：

- `GET /account-records` 与 `GET /platform-accounts` 保持挂回的兼容列表体，从 destinations 与 credentials 重建。Key 密文只留在凭据 SQL 行，不会进入 V4 GET 目的地/凭据 DTO。`GET /accounts` 是 V4 身份列表，同样从凭据重建。
- 用户定义供应商使用内核供应商家族：`POST /providers`、`GET|PATCH|DELETE /providers/{provider_id}`、`POST /providers/models/discover` 与 `POST /providers/test`。保存不依赖发现与测试；真实测试可能消耗上游额度。

## 静态面板

`GET /dashboard`、`GET /dashboard/`、`GET /dashboard/assets/{*path}`。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](http-routes.md) · [文档索引](../README.md)
