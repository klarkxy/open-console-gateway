[English](http-routes.md)

# HTTP 路由

所有路由共享一个端口：推理、Dashboard V3、Dashboard V4、V2 墓碑与 SPA。详见[架构](architecture.zh-CN.md)。

已退役的 `/dashboard/api/...` REST 在匿名时返回空 body 的 **401**（鉴权先于墓碑），已鉴权时（含回环本地模式）返回 **410** `{ "code": "dashboardV2Removed", "message": "Dashboard API V2 has been removed; refresh the page and retry." }`。既非 V3、非 V4，也非保留家族的未知 `/dashboard/api/...` 路径，在已鉴权时同样 410。未知的 V4 路径是 V4 的 `404`，不是墓碑。保留的 `/dashboard/api` 家族（精确路径，无尾斜杠，无额外段）：`auth/status`、`auth/register`、`auth/login`、`auth/logout`，以及 `browser/sessions/{token}/ws`（token 非空）。受保护的 V2 REST 保持退役；新 JSON 属于 V3 或 V4。

## 推理（路径未改）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/v1/chat/completions` | OpenAI Chat |
| POST | `/v1/responses` | OpenAI Responses（无状态；`store` / `previous_response_id` / `conversation` / `background` → 400） |
| POST | `/v1/messages` | Anthropic Messages |
| GET | `/v1/models` | 本地列表；需要鉴权 |
| POST | `/claude-desktop/v1/messages` | 角色别名改写后走 Messages |
| GET | `/claude-desktop/v1/models` | 三个角色别名 |
| POST | `/v1beta/models/{model}:*` 与 `/v1/models/{model}:*` | Gemini 客户端格式 |

## Dashboard V3（`/dashboard/api/v3`）

公开：`/auth/status`、`/auth/register`、`/auth/login`、`/auth/logout`。

会话保护（非穷尽；见 `dashboard_v3/mod.rs`）：`/contract`、`/connection`、 `/settings`、`/settings/test-proxy`、`/claude-desktop/models`、 `/settings/check-update`、`/settings/update-status`、 `/settings/install-update`、`/providers/{provider_id}/pricing`、 `/providers/{provider_id}/pricing/refresh`、 `/providers/{provider_id}/pricing/multipliers`、`/keys`、`/keys/primary/regenerate`、`/keys/{id}`、`/keys/{id}/regenerate`、 `/accounts`、`/accounts/managed`、`/accounts/order`、`/accounts/{id}`、 `/accounts/{id}/toggle`、`/accounts/{id}/browser`、 `/accounts/{id}/browser-profile`、`/accounts/{id}/setup`、 `/accounts/{id}/setup/verify-key`、`/accounts/{id}/reset-cooldown`、 `/accounts/{id}/custom-config`、`/accounts/{id}/model-capabilities`、 `/accounts/{id}/acknowledgements`、`/accounts/{id}/usage`、 `/accounts/{id}/usage/refresh`、`/accounts/{id}/provider-usage`、 `/accounts/{id}/verify`、`/providers`、`/providers/{provider_id}`、`/providers/models/discover`、`/providers/test`、`/providers/model-capabilities`、 `/providers/zen-free`、`/providers/zen-free/models`、 `/providers/zen-free/models/refresh`、 `/providers/{provider_id}/models/refresh`、`/provider-contracts`、 `/provider-contracts/provider/{scope_id}/model-protocol-overrides`、 `/provider-contracts/custom-endpoint/{scope_id}/model-protocol-overrides`、 `/providers/{provider_id}/protocol-probes`、`/browser/capabilities`、 `/browser/sessions/{token}/ws`、`/gateway/status`、 `/application-models`、`/dashboard/summary`、 `/dashboard/daily-tokens-by-model`、`/logs/gateway`、`/logs/forward`、 `/logs/forward/models`、`/logs/forward/keys`、 `/custom/models/discover`。

`GET /contract` 返回当前进程的 live revision / generation token（`revision`、`processGeneration`、`pricingRevision`）。

Go/Zen 协议探测是 `POST /providers/{provider_id}/protocol-probes`。Custom 在该路径被拒绝（`protocol probes for Custom API are account-owned`）。Custom 连接验证是 `POST /accounts/{id}/verify`；模型发现是 `POST /custom/models/discover`。历史 V2 `POST /accounts/{id}/protocol-probes` 为 410。用户定义供应商使用 `POST /providers`、`GET|PATCH|DELETE /providers/{provider_id}`、`POST /providers/models/discover` 与 `POST /providers/test`。保存不依赖发现与测试；真实测试可能消耗上游额度。

## Dashboard V4（`/dashboard/api/v4`）

会话保护（见 `dashboard_v4/mod.rs`）：`GET /contract`、`GET /templates`、`GET /connections`、`GET /accounts`、`POST /onboarding/commit`、`POST /credentials/{id}/rotate`。

## 静态面板

`GET /dashboard`、`GET /dashboard/`、`GET /dashboard/assets/{*path}`。

---

[维护者指南索引](../MAINTAINER.zh-CN.md) · [English](http-routes.md) · [文档索引](../README.zh-CN.md)
