[English](http-routes.md)

# HTTP 路由

所有路由共享一个端口：推理、Dashboard V4（含挂回去的 V3 处理器）、V2 与 V3 墓碑、以及 SPA。详见[架构](architecture.zh-CN.md)。

被墓碑化的 `/dashboard/api/...` REST：匿名时返回空 body 的 **401**（鉴权先于墓碑），已鉴权时（含回环本地模式）返回 **410** `{ "code": "dashboardV2Removed", "message": "Dashboard API V2 has been removed; refresh the page and retry." }`。`/dashboard/api/v3` 前缀是另一套 410：`{ "code": "dashboardV3Removed", "message": "Dashboard API V3 has been removed; refresh the page and retry." }`。既非 V3 墓碑前缀、非 V4，也非保留家族的未知 `/dashboard/api/...` 路径，在已鉴权时同样 410。未知的 V4 路径是 V4 的 `404`，不是墓碑。保留的 `/dashboard/api` 家族（精确路径，无尾斜杠，无额外段）：`auth/status`、`auth/register`、`auth/login`、`auth/logout`，以及 `browser/sessions/{token}/ws`（token 非空）。受保护的 V2 REST 返回墓碑；活的 Dashboard JSON 只走 V4。

## 推理

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/v1/chat/completions` | OpenAI Chat |
| POST | `/v1/responses` | OpenAI Responses（无状态；`store` / `previous_response_id` / `conversation` / `background` → 400） |
| POST | `/v1/messages` | Anthropic Messages |
| GET | `/v1/models` | 本地列表；需要鉴权 |
| POST | `/v1beta/models/{model}:*` 与 `/v1/models/{model}:*` | Gemini 客户端格式 |

## Dashboard V3 墓碑（`/dashboard/api/v3`）

`/dashboard/api/v3` 与 `/dashboard/api/v3/*` 已退役。匿名请求得到空 body 的 **401**。已鉴权请求得到 **410** `dashboardV3Removed`。

## Dashboard V4（`/dashboard/api/v4`）

公开（挂回）：`/auth/status`、`/auth/register`、`/auth/login`、`/auth/logout`。

会话保护的挂回操作路由（非穷尽；见 `dashboard_v3/mod.rs`）：
`/connection`、`/settings`、`/settings/test-proxy`、
`/settings/check-update`、
`/settings/update-status`、`/settings/install-update`、
`/providers/{provider_id}/pricing`、
`/providers/{provider_id}/pricing/refresh`、
`/providers/{provider_id}/pricing/multipliers`、`/keys`、
`/keys/primary/regenerate`、`/keys/{id}`、`/keys/{id}/regenerate`、
`/account-records`、`/accounts`（`POST` 创建）、`/accounts/managed`、
`/accounts/order`、`/accounts/{id}`、
`/accounts/{id}/toggle`、`/accounts/{id}/browser`、
`/accounts/{id}/browser-profile`、`/accounts/{id}/setup`、
`/accounts/{id}/setup/verify-key`、`/accounts/{id}/reset-cooldown`、
`/accounts/{id}/custom-config`、`/accounts/{id}/model-capabilities`、
`/accounts/{id}/usage`、
`/accounts/{id}/usage/refresh`、`/accounts/{id}/provider-usage`、
`/accounts/{id}/verify`、`/providers`、`/providers/{provider_id}`、
`/providers/models/discover`、`/providers/test`、`/providers/model-capabilities`、
`/providers/zen-free`、`/providers/zen-free/models`、
`/providers/zen-free/models/refresh`、
`/providers/{provider_id}/models/refresh`、`/provider-contracts`、
`/provider-contracts/provider/{scope_id}/model-protocol-overrides`、
`/provider-contracts/custom-endpoint/{scope_id}/model-protocol-overrides`、
`/providers/{provider_id}/protocol-probes`、`/browser/capabilities`、
`/browser/sessions/{token}/ws`、`/gateway/status`、
`/application-models`、`/dashboard/summary`、
`/dashboard/daily-tokens-by-model`、`/logs/gateway`、`/logs/forward`、
`/logs/forward/models`、`/logs/forward/keys`、
`/custom/models/discover`。

`GET /contract` 是 V4 原生 ControlRevision
（`revision`、`processGeneration`、`pricingRevision`）。

`GET /account-records` 与 `GET /platform-accounts` 保持挂回的兼容列表体，从 destinations 与 credentials 重建。Key 密文只留在凭据 SQL 行，不会进入 V4 GET 目的地/凭据 DTO。`GET /accounts` 是 V4 身份列表，同样从凭据重建。

Go/Zen 协议探测是 `POST /providers/{provider_id}/protocol-probes`。Custom 在该路径被拒绝（`protocol probes for Custom API are account-owned`）。Custom 连接验证是 `POST /accounts/{id}/verify`；模型发现是 `POST /custom/models/discover`。V2 `POST /accounts/{id}/protocol-probes` 为 410。用户定义供应商使用 `POST /providers`、`GET|PATCH|DELETE /providers/{provider_id}`、`POST /providers/models/discover` 与 `POST /providers/test`。保存不依赖发现与测试；真实测试可能消耗上游额度。

V4 原生会话保护路由（见 `dashboard_v4/mod.rs`）：`GET /contract`、`GET /templates`、`GET /connections`、`GET /accounts`（身份）、`GET /destinations`、`PATCH|DELETE /destinations/{id}`、`GET /credentials`、`GET /routing/explain`、`GET|POST /applications/dsh`、`POST /onboarding/commit`、`POST /credentials/{id}/rotate`、`PATCH /bindings/{id}`、`POST /identities/{id}/credentials`、`GET|PUT /cpa/models`、`POST /provider-contracts/{scope_kind}/{scope_id}/catalog/remove`、`GET|PATCH /alias-publication`。目的地变更带 CAS，只允许可配置 HTTP 行；密封与平台管理行不可改。路由解释只读，不发出站请求也不解密 Key。

## 静态面板

`GET /dashboard`、`GET /dashboard/`、`GET /dashboard/assets/{*path}`。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](http-routes.md) · [文档索引](../README.md)
