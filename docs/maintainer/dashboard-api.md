[简体中文](dashboard-api.zh-CN.md)

# Dashboard API

## Dashboard V3

Dashboard JSON is `/dashboard/api/v3`. DTOs are camelCase, mutation bodies deny unknown fields, and nullable response fields serialize as `T | null`.

Control-plane identity:

- `settings_revision` — in-memory `AtomicU64` on `CoreState`, bumped after
  a successful persist. Not stored in SQLite as the CAS token.
- `process_generation` — assigned once per `CoreState`, never persisted.
  A CAS token from a previous process cannot be reused after restart.
- `pricingRevision` — immutable snapshot id. Pricing mutations also send
  `expectedPricingRevision`.

`GET /contract` returns the current process's live revision / generation token
(`ControlRevision`: `revision`, `processGeneration`, `pricingRevision`).

Mutations require top-level `expectedRevision` and `processGeneration`
(including `/auth/register`, `/auth/login`, `/auth/logout`, and
`POST /accounts/{id}/usage/refresh`). A missing `expectedRevision` returns
`400` `missingExpectedRevision`; a mismatch returns `409` `revisionConflict`
with `currentRevision` and `processGeneration` in the error envelope. The Vue
`controlPlane` store records both tokens from every V3 payload. On 409 the
client refreshes the control tokens and affected resource without replaying
the mutation; the user can review current state and submit again. Tokens are process-local
and do not coordinate separate processes sharing a data directory.

Operations that are not mutations skip CAS and never bump revision:
operational diagnostics such as `POST /settings/test-proxy` and
`POST /custom/models/discover`; update checks such as
`GET /settings/check-update` and `GET /settings/update-status` capture tokens
without bumping. `POST /settings/install-update` requires CAS and starts
atomically, but does not bump and holds no network or DB lock.

Plaintext keys never appear on `Settings`, provider, Zen, or contract DTOs.
`ConnectionInfo` (`GET /connection`) is the only secret-bearing V3 response:
it returns the primary key and every non-deleted sub-key value, including
disabled sub-keys, under dashboard session protection. Only enabled keys enter
the authentication snapshot. `CustomModelDiscoveryRequest.apiKey` is write-only.
Account list/get payloads stay secret-free. Logs and error envelopes redact
known secrets.

The frozen contract is `schema/dashboard-api-v3.schema.json`, generated from
`dashboard_v3::contract_schema_pretty()` by
`crates/ocg-core/examples/export_dashboard_v3_schema.rs`. Generated TypeScript
(`src/api/generated/dashboard-v3.ts`) is types only, with no HTTP wrappers.
`CATALOG_TYPE_NAMES` in `dashboard_v3/types.rs` is the ordered `$defs` catalog;
appending must keep existing definitions byte-identical.

Pinia stores call `dashboardV3` directly. Pages that still use older field names
go through `src/api/dashboard.ts` presenters.

`dashboard.rs` serves the SPA and preserves the V2 auth and browser WebSocket
handlers. Retired `/dashboard/api/...` REST paths are tombstoned in
`host_router` before they reach `dashboard.rs`.

## Dashboard V4

Dashboard V4 JSON is `/dashboard/api/v4`. It is a parallel, additive control
plane beside frozen V3. V3 `$defs` and routes do not gain new fields.

V4 reuses V3 session middleware. Its listings return the same `ControlRevision`
(`expectedRevision` / `processGeneration`) that V3 uses for CAS. V4 mutations are `POST /onboarding/commit`,
`POST /credentials/{id}/rotate`, `PATCH /bindings/{id}`, and
`POST /identities/{id}/credentials`; they check those tokens. Read routes
do not.

The frozen contract is `schema/dashboard-api-v4.schema.json`, generated from
`dashboard_v4::contract_schema_pretty()` by
`crates/ocg-core/examples/export_dashboard_v4_schema.rs`. Generated TypeScript
(`src/api/generated/dashboard-v4.ts`) is types only, with no HTTP wrappers.
`CATALOG_TYPE_NAMES` in `dashboard_v4/types.rs` is the ordered `$defs` catalog;
appending must keep existing definitions byte-identical.

Read-only routes remain `GET /contract`, `GET /templates`,
`GET /connections`, and `GET /accounts`. Those reads perform no outbound
requests.

`GET /templates` is the read-only add catalog: the sealed built-ins (CPA
excluded) plus the `custom-http` manual template. Presets are not part of it
yet. Templates have no user instances or secrets.

`GET /connections` is a projection of saved instances: built-ins that already
have an account, every user-defined Provider, and each Custom API account
individually; CPA is never a connection. Each connection carries lifecycle, authorization state, local
eligibility with a reason, endpoints, model targets, and a legacy identity
reference. Connection ids are deterministic UUIDv5 values derived from that
legacy identity, never from names or URLs.

`GET /accounts` returns `IdentityList { revision, identities[] }`. Each
`IdentitySummary` carries `identity` (`id`, `label`, `authorityRef`
`{ issuerOrSite, tenantOrSubject }`, `identityConfidence`, `enabled`,
`notes`), `credentials[]`, identity-level `declaredRelations[]`
(`platformAccountId`, `group`), and `legacy` (`kind` `account` |
`platform_account`, `id`). The wire shape is nested:
`credentials[].credential` (`id`, `purpose` `inference` |
`platform_observer`, `materialKind` `api_key` | `external_reference`,
`secretRef` — an opaque handle, never material, `hasMaterial`, `version`,
`enabled`, `authState` `unknown` | `valid` | `invalid`,
`authStateVersion`, `expiresAt` null when unknown) with siblings
`subject` (`account_credential` | `anonymous`), `bindings[]` (`id`,
`connectionId`, `allowedEndpointIds`, `allowedOrigins`, `modelScope`,
`enabled`, `routingRank`), `quotaWindows[]`, `onboardingTask`,
`subscription` (null when unknown), `lastError` (redacted; null when it
cannot be redacted safely), and `legacy`. The platform parent's
`platform_observer` credential is a projection in this stage (no
`credential_state` row). `authState` is local: `unknown` is never
`valid`; `valid` requires the existing verification record. The Vue
Accounts page overlays this projection for display only; mutations stay
on V3. Some enum values in the V4
catalog are reserved for the next stage and not yet produced:
`subject: external_runtime`, `policyMode: observe_only`,
`relationConfidence: unknown`, `subscription.source: managed_payment`,
`onboardingTask.state: completed`.

V4 does not treat authorization `unknown` as `valid`. Eligibility is a local
projection, never upstream health.

`POST /onboarding/commit` body: `expectedRevision`, `processGeneration` (the
same CAS tokens as V3), `operationId` (client-generated UUID), `connection`,
optional `authorization`, and `targets`.

`connection` is `kind: new` (`templateId` is `custom-http` or a preset id,
plus `name`, `endpointUrl`, `upstreamProtocol`, `authKind`) or
`kind: existing` (`connectionId`). `authorization` is `kind: api_key`
(`secretInput`, optional `accountLabel` / `notes`) or `kind: none`.
`targets` map a public model to an exact upstream model, with an optional
per-target upstream override. `new` requires a non-empty `targets` list;
`existing` requires it empty (model edits stay on V3 `PATCH /providers/{id}`).

Evaluation order: (1) parse; (2) `operationId` must be a UUID; (3) take the
`settings_update` lock, then idempotency lookup before CAS — if that `operationId` was already committed with the same
payload digest, the stored secret-free result is returned with `replayed:
true` and the current revision tokens, without checking CAS (the first write
already moved the revision); the same `operationId` with a different payload
returns `409` `operationPayloadMismatch` and writes nothing; (4) CAS check
(`409` `revisionConflict`); (5) write.

`new` reuses V3 user-defined Provider validation. Template ids pass through as
opaque preset ids; Rust still does not load presets. Omitting `authorization`
on keyed auth saves the definition only (V4 connections then show
authorization `missing`); `api_key` requires a non-empty secret on keyed
auth; `none` is valid only for no-auth templates, which always create the
singleton account. The Provider row, optional first account row, and the
operation record commit in one SQLite transaction; the dynamic-provider
snapshot is installed after commit exactly as V3 does.

`existing` in this stage accepts a new `api_key` only on user-defined
(dynamic) Provider connections with keyed auth. Built-in and Custom API
connection ids return `400` ("add Keys on Accounts"). Account row and
operation record commit in one transaction, then the revision bump only
(`reload_contracts=false`), the same as V3 plain account create.

The result is `{ revision, connectionId, credentialId | null, targetIds,
replayed }`. `connectionId` is the deterministic UUIDv5 of the dynamic
Provider; `credentialId` is the account id; `targetIds` are UUIDv5 per public
model. The response never contains the secret, ciphers, or the digest.

**Idempotent operations.** `operationId` plus a payload digest bind a commit:
the digest is hex HMAC-SHA256 over the semantic payload only — `operationId`,
`connection`, `authorization` (so the secret is covered), and `targets`.
`expectedRevision` / `processGeneration` are excluded, so a retry with
refreshed CAS tokens still replays. Schema v44 stores each commit in
`dashboard_operations`; the stored `result_json` is secret-free. Rows older
than 30 days are pruned on insert; after pruning, the same `operationId` is a
new write.

`POST /credentials/{id}/rotate` replaces the Key on one projected
credential. CAS tokens are required; there is no `operationId`. The
credential id, binding, and quota relationship stay the same. `version`
and `authStateVersion` increment together; `authState` becomes `unknown`;
the underlying account's `auth_error` / `last_error` and verification
result are cleared so the old version cannot pollute the new one. The
body is `{ secretInput }` plus CAS tokens. The result is secret-free.
Platform observer, anonymous, no-auth, and CPA credentials return `400`.
Unknown ids return `404`. A stale CAS token returns `409` and writes
nothing.

`PATCH /bindings/{id}` edits one inference binding. CAS tokens are
required; there is no `operationId`. The body is `{ modelScope?, enabled? }`
plus CAS tokens. At least one of `modelScope` or `enabled` is required.
`modelScope` is `{ kind: "all" }` or `{ kind: "only", models: [...] }`
(exact ids after the existing model-name normalization). Binding enablement
is independent of the sibling binding on the same identity. Unknown ids
return `404`. Platform observer, anonymous, no-auth, and CPA bindings
return `400`. A stale CAS token returns `409` and writes nothing. The
result is `{ revision, binding }` and is secret-free.

`POST /identities/{id}/credentials` adds a second Key to a confirmed
identity. CAS tokens are required; there is no `operationId`. The body is
`{ connectionId, secretInput }` plus CAS tokens. The write creates a new
`accounts` row that reuses the existing `identity_id`, inserts
`credential_state` and `credential_bindings`, and joins the identity's
quota pool in one SQLite transaction. A different `connectionId` is a
second product (D05); the same Plan connection is another Key on that
product. Switching Keys does not invent a fresh pool. Unknown identity or
connection ids return `404`. Builtin-immutable / Zen Free / CPA / no-auth
/ Custom API / platform-observer targets return `400`. A stale CAS token
returns `409` and writes nothing. The result is secret-free.

The dashboard consumes `GET /connections` for the Providers rail,
`POST /onboarding/commit` for user-defined Provider creation, and
`GET /accounts` as a display overlay on the Accounts page. The client generates a new `operationId` when
the draft changes, keeps that id across retries of an unchanged draft, and
regenerates it after success. Editing, deleting, adding Keys to existing
accounts, and all Accounts-page account operations stay on V3.

## Settings mutation workflow

[![Dashboard V3 settings mutation workflow](../diagrams/dashboard-v3-mutation.visual-check.1440x900.light.png)](https://klarkxy.github.io/open-console-gateway/diagrams/dashboard-v3-mutation/)

[Open the interactive diagram on GitHub Pages](https://klarkxy.github.io/open-console-gateway/diagrams/dashboard-v3-mutation/).

This sequence is specific to CAS-protected Settings writes; discovery,
diagnostic, and read operations may skip CAS as described above. The client
submits `expectedRevision` and `processGeneration`. A mismatch returns `409`;
the client refreshes the tokens and affected resource, but does not replay the
write automatically.

After CAS succeeds, the Host persists the new settings and releases the
settings lock. It rebinds the listener only when the port changed and a
listener is running. If rebind fails, the request returns `500` with code
`internal`. Compensation restores the previous port only when the live config
still contains the failed committed port, so a later successful write is not
overwritten.

## Retired V2 REST

Protected Dashboard V2 REST is retired.

- Anonymous retired REST: empty-body **401** (auth runs before the
  tombstone).
- Authenticated retired REST (including loopback local mode): **410** with
  `{ "code": "dashboardV2Removed", "message": "Dashboard API V2 has been removed; refresh the page and retry." }`.
- Unknown `/dashboard/api/...` paths that are not V3, not V4, and not a
  preserved family are also 410 once authenticated. Unknown V4 paths are V4
  `404`s, not tombstones.

Preserved `/dashboard/api` families (exact path, no trailing slash, no
extra segments):

- `auth/status`, `auth/register`, `auth/login`, `auth/logout`
- `browser/sessions/{token}/ws` (non-empty token)

V3 auth and browser WebSocket live under `/dashboard/api/v3/...`; the Vue
shell uses them. Inference routes, dashboard HTML, and `/dashboard/assets/...`
are outside the tombstone.

---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](dashboard-api.zh-CN.md) · [Docs index](../README.md)
