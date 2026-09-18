[简体中文](http-routes.zh-CN.md)

# HTTP Routes

All routes share one port: inference, Dashboard V4 (including remounted V3
handlers), V2 and V3 tombstones, and SPA. See [Architecture](architecture.md).

Tombstoned `/dashboard/api/...` REST returns empty-body **401** when anonymous
(auth runs before the tombstone) and **410**
`{ "code": "dashboardV2Removed", "message": "Dashboard API V2 has been removed; refresh the page and retry." }`
when authenticated, including loopback local mode. The `/dashboard/api/v3`
prefix is a separate 410 family:
`{ "code": "dashboardV3Removed", "message": "Dashboard API V3 has been removed; refresh the page and retry." }`.
Unknown `/dashboard/api/...`
paths that are not the V3 tombstone prefix, not V4, and not a preserved family are also 410 once
authenticated. Unknown V4 paths are V4 `404`s, not tombstones. Preserved
`/dashboard/api` families (exact path, no trailing slash, no extra
segments): `auth/status`, `auth/register`, `auth/login`, `auth/logout`,
and `browser/sessions/{token}/ws` (non-empty token). Protected
V2 REST is tombstoned; live dashboard JSON is V4 only.

## Inference

| Method | Path | Notes |
| --- | --- | --- |
| POST | `/v1/chat/completions` | OpenAI Chat |
| POST | `/v1/responses` | OpenAI Responses (stateless; `store` / `previous_response_id` / `conversation` / `background` → 400) |
| POST | `/v1/messages` | Anthropic Messages |
| GET | `/v1/models` | Local list; auth required |
| POST | `/v1beta/models/{model}:*` and `/v1/models/{model}:*` | Gemini client format |

## Dashboard V3 tombstone (`/dashboard/api/v3`)

`/dashboard/api/v3` and `/dashboard/api/v3/*` are retired. Anonymous
requests get empty-body **401**. Authenticated requests get **410**
`dashboardV3Removed`.

## Dashboard V4 (`/dashboard/api/v4`)

Public (remounted): `/auth/status`, `/auth/register`, `/auth/login`,
`/auth/logout`.

Session-protected remounted operational routes (non-exhaustive; see
`dashboard_v3/mod.rs`):
`/connection`, `/settings`, `/settings/test-proxy`,
`/settings/check-update`,
`/settings/update-status`, `/settings/install-update`,
`/providers/{provider_id}/pricing`,
`/providers/{provider_id}/pricing/refresh`,
`/providers/{provider_id}/pricing/multipliers`, `/keys`,
`/keys/primary/regenerate`, `/keys/{id}`, `/keys/{id}/regenerate`,
`/account-records`, `/accounts` (`POST` create), `/accounts/managed`,
`/accounts/order`, `/accounts/{id}`,
`/accounts/{id}/toggle`, `/accounts/{id}/browser`,
`/accounts/{id}/browser-profile`, `/accounts/{id}/setup`,
`/accounts/{id}/setup/verify-key`, `/accounts/{id}/reset-cooldown`,
`/accounts/{id}/custom-config`, `/accounts/{id}/model-capabilities`,
`/accounts/{id}/usage`,
`/accounts/{id}/usage/refresh`, `/accounts/{id}/provider-usage`,
`/accounts/{id}/verify`, `/providers`, `/providers/{provider_id}`,
`/providers/models/discover`, `/providers/test`, `/providers/model-capabilities`,
`/providers/zen-free`, `/providers/zen-free/models`,
`/providers/zen-free/models/refresh`,
`/providers/{provider_id}/models/refresh`, `/provider-contracts`,
`/provider-contracts/provider/{scope_id}/model-protocol-overrides`,
`/provider-contracts/custom-endpoint/{scope_id}/model-protocol-overrides`,
`/providers/{provider_id}/protocol-probes`, `/browser/capabilities`,
`/browser/sessions/{token}/ws`, `/gateway/status`,
`/application-models`, `/dashboard/summary`,
`/dashboard/daily-tokens-by-model`, `/logs/gateway`, `/logs/forward`,
`/logs/forward/models`, `/logs/forward/keys`,
`/custom/models/discover`.

`GET /contract` is the V4-native ControlRevision
(`revision`, `processGeneration`, `pricingRevision`).

`GET /account-records` and `GET /platform-accounts` keep the remounted
compatibility list bodies, reconstructed from destinations and
credentials. Key ciphertext stays on credential SQL rows and is not
copied into V4 GET destination/credential DTOs. `GET /accounts` is the
V4 identity listing, also reconstructed from credentials.

Go/Zen protocol probes are `POST /providers/{provider_id}/protocol-probes`.
Custom is rejected there (`protocol probes for Custom API are account-owned`).
Custom connection verify is `POST /accounts/{id}/verify`; model discovery is
`POST /custom/models/discover`. V2
`POST /accounts/{id}/protocol-probes` is 410. User-defined Providers use
`POST /providers`, `GET|PATCH|DELETE /providers/{provider_id}`,
`POST /providers/models/discover`, and `POST /providers/test`. Save succeeds
independently of discovery and test; a real test may consume upstream quota.

V4-native session-protected routes (see `dashboard_v4/mod.rs`): `GET /contract`,
`GET /templates`, `GET /connections`, `GET /accounts` (identities),
`GET /destinations`, `GET /credentials`,
`GET|POST /applications/dsh`,
`POST /onboarding/commit`, `POST /credentials/{id}/rotate`,
`PATCH /bindings/{id}`, `POST /identities/{id}/credentials`,
`GET|PUT /cpa/models`,
`POST /provider-contracts/{scope_kind}/{scope_id}/catalog/remove`,
`GET|PATCH /alias-publication`.

## Static dashboard

`GET /dashboard`, `GET /dashboard/`, `GET /dashboard/assets/{*path}`.
---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](http-routes.zh-CN.md) · [Docs index](../README.md)
