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

**The authoritative route list is the router construction in
`crates/ocg-core/src/dashboard_v4/mod.rs` (V4-native routes) and the remounted
kernel in `crates/ocg-core/src/dashboard_v3/mod.rs`.** This page describes
semantics and route families; it does not enumerate paths. Add, move, or
retire routes in those routers — do not mirror them here.

## Inference

The `/v1/...` inference surface is registered in
`crates/ocg-core/src/gateway/mod.rs` (`inference_router`) and dispatched by
the handlers in `crates/ocg-core/src/gateway/handler.rs`: OpenAI Chat
Completions and Responses, Anthropic Messages, the authenticated local
`GET /v1/models`, and the Gemini client format
`/v1beta/models/{model}:*` (also mounted under `/v1/`). The same endpoints
are documented user-facing in [gateway.md](../user/gateway.md).

Semantics the router does not state at a glance:

- `/v1/responses` is stateless: `store`, `previous_response_id`,
  `conversation`, and `background` are rejected with **400**.
- Gemini `{model}:{action}` dispatch (`gemini_model_action`):
  `generateContent` and `streamGenerateContent` proxy upstream;
  `countTokens` returns **501** as an expected fallback (Gemini CLI switches
  to local estimation, and the request is not persisted as a failure);
  `embedContent` returns **501** (embeddings are unsupported); an unknown
  action is a Gemini-format **404**.
- `GET /v1/models` is a local inventory (code-owned aliases plus eligible
  saved names); it requires auth and performs no upstream I/O.

## Dashboard V3 tombstone (`/dashboard/api/v3`)

`/dashboard/api/v3` and `/dashboard/api/v3/*` are retired. Anonymous
requests get empty-body **401**. Authenticated requests get **410**
`dashboardV3Removed`.

## Dashboard V4 (`/dashboard/api/v4`)

Public (remounted, session-free): `/auth/status`, `/auth/register`,
`/auth/login`, `/auth/logout`, plus the preserved WebSocket family listed
above. Everything else is session-protected and groups into the families
below; each family names its owning module. Exact paths and methods live in
the two router constructions cited above.

Remounted kernel (`crates/ocg-core/src/dashboard_v3/`):

- **Connection, settings, updater** (`connection`, `settings`,
  `proxy_test`, `updater` modules): read or mutate the single saved
  connection, test the outbound proxy, and check or install updates.
- **Gateway Keys** (`keys` module): create, list, and regenerate the
  administrator Gateway Keys.
- **Account lifecycle** (`accounts`, `usage`, `usage_refresh`,
  `account_model_test`, `account_verify`, `managed_key_verify`,
  `account_transfer` modules): create, reorder, and toggle accounts;
  browser onboarding and profiles; setup with Key verification; cooldown
  reset; custom config and model capabilities; usage and provider-usage
  reads and refresh (including Command Code usage refresh); per-account
  model tests and connection verify.
- **Providers** (`providers`, `pricing`, `dynamic_providers` modules):
  sealed and static provider rows with pricing multipliers; user-defined
  Provider CRUD, model discovery, and live connection test; Zen `-free`
  rows and model refresh.
- **Provider contracts and model protocols** (the `provider-contracts`
  family in the kernel): contract listing, per-scope model-protocol
  overrides (provider and custom-endpoint scopes), catalog refresh, and
  reset-static for static protocol tables.
- **Protocol probes** (`providers` module): `POST
  /providers/{provider_id}/protocol-probes` probes Go/Zen protocol support.
  Custom is rejected there (`protocol probes for Custom API are
  account-owned`), and the V2 `POST /accounts/{id}/protocol-probes` is 410.
  Custom connection verify and model discovery belong to the account
  lifecycle family and `custom_discovery` (`POST /accounts/{id}/verify`,
  `POST /custom/models/discover`).
- **Platform accounts** (`platforms` module): list, inspect, and refresh
  platform accounts and platform linking.
- **CPA** (`cpa` module): the remounted `/external-integrations/cpa/*`
  catalog read and write.
- **Observability** (`observability` module and the `application-models`
  handler): gateway status, dashboard summary and daily tokens, and
  gateway/forward logs with model and Key filters.
- **Browser** (`browser` module): capability listing and the account
  website WebSocket.

V4-native (`crates/ocg-core/src/dashboard_v4/`):

- **Control plane**: `GET /contract` is the V4-native ControlRevision
  (`revision`, `processGeneration`, `pricingRevision`); `/templates`,
  `/connections`, and `/accounts` list creation templates, connections,
  and account identities.
- **Destinations and catalog** (`destinations`, `destination_catalog`
  modules): list destinations and credentials; mutate a destination or its
  catalog. Destination mutations are CAS-protected and limited to
  configurable HTTP rows; sealed and platform-managed rows are immutable.
- **Credentials** (`credentials` module): rotate a credential and retry
  its quota state.
- **Billing** (`billing` module): per-account credit configuration,
  calibration, and grants — all local estimates.
- **Official API references** (`official_api` module): official-API status
  and balance refresh for matching presets, and the official price table
  per provider.
- **Platform Keys** (`platform_keys` module): import platform Keys into an
  account.
- **Onboarding and bindings** (`onboarding`, `bindings`, `identities`
  modules): the atomic preset commit, Key-to-model bindings, and adding a
  credential to an identity.
- **Routing** (`routing`, `routing_cards` modules): `GET /routing/explain`
  is read-only and performs no upstream I/O or Key decryption;
  `/routing/cards` lists and replaces routing cards.
- **CPA catalog** (`cpa` module): the V4-native `/cpa/models` endpoints.
- **Catalog removal and alias publication** (`catalog`, `publication`
  modules): remove models from a provider-contract catalog and read or
  patch the public alias surface.
- **Applications** (`applications` module): the DSH application status and
  install endpoints.

Compatibility shims and data-shape notes that the routers do not show:

- `GET /account-records` and `GET /platform-accounts` keep the remounted
  compatibility list bodies, reconstructed from destinations and
  credentials. Key ciphertext stays on credential SQL rows and is not
  copied into V4 GET destination/credential DTOs. `GET /accounts` is the
  V4 identity listing, also reconstructed from credentials.
- User-defined Providers use the kernel provider family: `POST /providers`,
  `GET|PATCH|DELETE /providers/{provider_id}`, `POST
  /providers/models/discover`, and `POST /providers/test`. Save succeeds
  independently of discovery and test; a real test may consume upstream
  quota.

## Static dashboard

`GET /dashboard`, `GET /dashboard/`, `GET /dashboard/assets/{*path}`.
---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](http-routes.zh-CN.md) · [Docs index](../README.md)
