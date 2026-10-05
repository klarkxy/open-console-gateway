[简体中文](dashboard-api.zh-CN.md)

# Dashboard API

> Scope: published dashboard operation reference, plus the current candidate contracts in [Owned CPA control](#owned-cpa-control) and [Owned CPA routing explain](#owned-cpa-routing-explain). Those contracts are in the checked-in schema export. Whole CLI acceptance and runtime acceptance are pending. The native desktop GUI stays postponed. UI steps do not apply to this headless CLI phase. Generation design is in [architecture](../architecture.md).

## Billing and local credit estimates (V4)

`GET /dashboard/api/v4/accounts/{id}/billing` presents timed quota, cash, or credits together with its observation source and available actions. Here `id` identifies one account (one Key); several accounts in a supplier container remain independent. The read makes no upstream request. Existing official balance and quota refresh endpoints retain their provider-specific observation adapters.

`PUT .../billing/credits` configures a personal credit estimate. Initial setup supplies current buckets; later rate/settings edits preserve balances. `POST .../billing/credits/calibrate` corrects current bucket balances, `POST .../billing/credits/grants` adds a grant or top-up, and `DELETE .../billing/credits` disables this estimate. Mutations require `expectedRevision` and `processGeneration` and return the updated `BillingStatus`. Requests that start after calibration settle against that new baseline; pending and unpriced requests remain visible. Estimated exhaustion never changes routing eligibility.

Step Plan uses this local estimation/calibration contract until an official usage API is available. The former private console-token endpoints and `StepFunUsageStatus` contract are retired. StepFun ordinary API balance remains separate from the `/step_plan` channel.

Personal credits are configurable only for `http` destinations of legacy kind `custom_account` or `dynamic`. Platform-linked Keys, observer credentials and sealed built-in Plans retain their existing billing contracts. Credit calibration rejects pending requests; it does not move their baseline while they are in flight.

## Dashboard V3

The `/dashboard/api/v3` HTTP mount is **removed**. The dashboard speaks V4
only. Anonymous `/dashboard/api/v3` and `/dashboard/api/v3/*` return
empty-body **401** (auth runs before the tombstone). Authenticated requests
(including loopback local mode) return **410**
`{ "code": "dashboardV3Removed", "message": "Dashboard API V3 has been removed; refresh the page and retry." }`.

Operational V3 handlers are remounted under `/dashboard/api/v4` with the same
relative paths, except the account-list shim is `GET /account-records` so it
does not collide with V4 `GET /accounts` (identities). `GET /contract` is the
existing V4 ControlRevision. **The remounted handlers are a compatibility
shim** over the destination and credential tables. New clients should use V4
`GET /destinations` and `GET /credentials`. Remounted listings reconstruct from destinations and credentials. Key ciphertext stays on credential SQL rows and never appears on V4 GET destination/credential DTOs. DTOs are camelCase, mutation bodies deny unknown fields, and nullable response fields serialize as `T | null`.

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
`controlPlane` store records both tokens from every remounted V3 payload. On 409 the
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

The `src/api/dashboard.ts` presentation client wraps `dashboardV3` and projects
the fields each page and store needs.

`dashboard.rs` serves the SPA and preserves the V2 auth and browser WebSocket
handlers. Other `/dashboard/api/...` REST paths are tombstoned in
`host_router` before they reach `dashboard.rs`.

## Dashboard V4

Dashboard V4 JSON is `/dashboard/api/v4`. This is the only live dashboard
JSON prefix: additive V4 routes plus remounted V3 operational handlers.
The checked-in schema JSON is the stdout of `export_dashboard_v3_schema` and `export_dashboard_v4_schema`. The CPA fields below, including `migrationRequired`, are in that JSON. This page did not run the export.

V4 reuses V3 session middleware. Its listings return the same `ControlRevision`
(`expectedRevision` / `processGeneration`) that V3 uses for CAS. V4 mutations are `POST /onboarding/commit`,
`POST /credentials/{id}/rotate`, `POST /credentials/{id}/quota-retry`,
`PATCH /bindings/{id}`,
`POST /identities/{id}/credentials`, `POST|DELETE /applications/dsh` (which also
binds the GET inspection fingerprint; DELETE has no `keyId`), `PUT /destinations/{id}/catalog`,
`POST /destinations/{id}/catalog/refresh`, `POST /destinations/{id}/model-tests`,
`POST /platform-accounts/{id}/import-keys`, `PUT /cpa/models`,
`POST /provider-contracts/{scope_kind}/{scope_id}/catalog/remove`, and
`PATCH /alias-publication`; they check those tokens. Read routes
do not.

The checked-in additive V4 contract is `schema/dashboard-api-v4.schema.json`, generated from
`dashboard_v4::contract_schema_pretty()` by
`crates/ocg-core/examples/export_dashboard_v4_schema.rs`. Generated TypeScript
(`src/api/generated/dashboard-v4.ts`) is types only, with no HTTP wrappers.
`CATALOG_TYPE_NAMES` in `dashboard_v4/types.rs` is the ordered `$defs` catalog;
appending must keep existing definitions byte-identical.

Read-only routes are `GET /contract`, `GET /templates`,
`GET /connections`, `GET /accounts` (identities), `GET /account-records`
(remounted V3 account-list shim), `GET /destinations`, `GET /credentials`,
`GET /accounts/{id}/billing`, `GET /accounts/{id}/official-api`,
`GET /providers/{id}/official-api/pricing`, `GET /routing/cards`,
`GET /applications/dsh` (optional `profilePath` and `runtimeUrl`),
`GET /cpa/models`, and `GET /alias-publication`. Those reads perform no outbound
requests.

The official-api family — `GET /accounts/{id}/official-api`,
`POST /accounts/{id}/official-api/balance`, and
`GET|POST /providers/{id}/official-api/pricing` — exposes official-API preset
financial evidence. The GETs are local projections; the CAS-protected POSTs are
the only network paths. See
[official API implementation](../../crates/ocg-core/src/official_api.rs).

`GET /templates` is the read-only add catalog: the sealed built-ins (CPA
excluded) plus the `custom-http` manual template. Presets are not part of the
template catalog. Templates have no user instances or secrets.

`GET /connections` is a projection of saved instances: built-ins that already
have an account and every configurable HTTP connection, including each
persisted Custom API destination grouped with all of its Keys; CPA is never a connection. Each connection carries lifecycle, authorization state, local
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
`platform_observer` credential is a projection (no
`credential_state` row). `authState` is local: `unknown` is never
`valid`; `valid` requires the existing verification record. The Vue
Accounts page overlays this projection for display; Key rotation, quota retry,
binding
edits, and additional identity credentials use V4, while the remaining
account mutations stay on V3.

`GET /destinations` and `GET /credentials` are secret-free, revision-tagged, local-only projections. `DestinationCredentialDto` may include optional nullable `quotaRecovery` (camelCase). Absence means no confirmed exhaustion, not verified upstream health. `status` on that object is presentation only (`waiting` | `ready` | `probing`). `IdentitySummary` credentials do not carry this field.
CAS-protected `PATCH /destinations/{id}` fully replaces editable HTTP name, endpoint, auth,
protocol, mappings, and route overrides. It never accepts Key material and unions safe grants
only for explicit `authorizeCredentialIds`. `DELETE /destinations/{id}` requires zero referencing
credentials. Sealed and platform-managed destinations reject both mutations. A
populated destinations/credentials store is served even when live
`project()` would refuse. An empty store or leftover-table upgrade window
falls back to `project()`; only that empty-store fallback can return
`409` `destinationProjectionRefused` with a `details` array naming each
refused row.

Node transfer (`POST /accounts/transfer/export|preview|import`) is remounted
on V4. The latest export uses the current transfer payload: `destinations` and `credentials`
(plaintext secrets, platform and CPA observer management credentials, and
identity / grant / cooldown extras stay inside the encrypted envelope), plus
`quotaPools` and `node`, and explicit HTTP protocol routes.
Merging a package that has no CPA observer key preserves the destination's
existing management key. It does not emit
`accounts`, `platformAccounts`, `platformLinks`, `dynamicProviders`, or
`identities`. Those portable types are transfer-only and are not V4 listing
DTOs. The supported import range, per-version defaults, and the explicit-routes rejection are the current [account-transfer implementation](../../crates/ocg-core/src/dashboard_v3/account_transfer.rs). Local quota recovery is not a portable field: it is omitted from export, retained on an unchanged target Key, and cleared when the Key is replaced.

`GET /routing/cards` returns one revision-tagged snapshot of `cards`, `destinations` and `credentials`. `PUT /routing/cards` accepts CAS tokens and the complete ordered card list. A card has `id`, `destinationId` and ordered `credentialIds`; every inference credential, including disabled rows, must appear exactly once under its existing destination. Observer credentials are excluded. Layout and flattened routing ranks commit together, and the response returns the complete committed snapshot. Multiple cards share one destination; creating or removing an empty extra card does not create or delete a supplier.

`GET /routing/explain?model=...&clientProtocol=...` is the existing authenticated read. Its current candidate contract is [Owned CPA routing explain](#owned-cpa-routing-explain). The checked-in schema JSON includes this read, including `migrationRequired`.

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
`existing` requires it empty (connection edits use V4 `PATCH /destinations/{id}`).

Evaluation order: (1) parse; (2) `operationId` must be a UUID; (3) take the
`settings_update` lock, then idempotency lookup before CAS — if that `operationId` was already committed with the same
payload digest, the stored secret-free result is returned with `replayed:
true` and the current revision tokens, without checking CAS (the first write
already moved the revision); the same `operationId` with a different payload
returns `409` `operationPayloadMismatch` and writes nothing; (4) CAS check
(`409` `revisionConflict`); (5) write.

`new` reuses V3 user-defined Provider validation. Template ids pass through as
opaque preset ids; preset forms stay frontend-owned and Rust consumes only the
offering projection generated from `resources/provider-presets.json`.
Omitting `authorization`
on keyed auth saves the definition only (V4 connections then show
authorization `missing`); `api_key` requires a non-empty secret on keyed
auth; `none` is valid only for no-auth templates, which always create the
singleton account. The Provider row, optional first account row, and the
operation record commit in one SQLite transaction; the dynamic-provider
snapshot is installed after commit exactly as V3 does.

`existing` accepts a new `api_key` on keyed dynamic Providers and legacy Custom HTTP connections. Built-in, platform-managed, and no-auth
connection ids return `400`. Account row and
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
refreshed CAS tokens still replays. Each commit is stored in
`dashboard_operations`; the stored `result_json` is secret-free. Rows older
than 30 days are pruned on insert; after pruning, the same `operationId` is a
new write.

`POST /credentials/{id}/rotate` replaces the Key on one projected
credential. CAS tokens are required; there is no `operationId`. The
credential id, binding, and quota relationship stay the same. `version`
and `authStateVersion` increment together; `authState` becomes `unknown`;
the underlying account's `auth_error` / `last_error` and verification
result are cleared so the old version cannot pollute the new one.
Rotating replaces the Key and clears local quota recovery. The
body is `{ secretInput }` plus CAS tokens. The result is secret-free.
Platform observer, anonymous, no-auth, and CPA credentials return `400`.
Unknown ids return `404`. A stale CAS token returns `409` and writes
nothing.

`QuotaRecoveryDto` is `{ status: "waiting" | "ready" | "probing", reason:
"quota_exhausted" | "insufficient_balance", window: "five_hours" | "week" |
"month" | "unknown", observedAt: string (RFC3339), resetsAt: string | null,
nextRetryAt: string (RFC3339), failureCount: number }`.

`POST /credentials/{id}/quota-retry` uses the existing flattened
`MutationExpectation` body (`expectedRevision`, `processGeneration`) with no
`operationId`. The result is `{ revision: ControlRevision, credential:
DestinationCredentialDto }` and is secret-free. It permits one next normal
selection: no outbound request, no enablement change, and no backoff clear.
It is idempotent while status is already `ready` or `probing` and may return
the current updated row. Unknown ids return `404`. A stale CAS token returns
`409` and writes nothing.

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
regenerates it after success. Editing and deleting accounts and the
remaining Accounts-page operations stay on V3; Key rotation, quota retry,
binding edits, and adding a Key to an existing identity use V4.

## Settings mutation workflow



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

Successful V4 projection saves call the existing `note_product_apply` after the saved receipt is in hand. Destination PATCH awaits that call once after the outer commit result. Binding PATCH, credential rotation, destination delete, refresh, and builtin removal call it only after the synchronous save succeeds and the settings lock is released. Identity creation and onboarding replay call it when the operation is not a replay. On a binding, `patch_locked` stores the row and bumps the settings revision; the handler then calls `note_product_apply` and does not call `schedule_owned_apply`. Model test keeps one pre-probe `schedule_owned_apply` and has no product-save hook. Quota retry and read routes have no projection hook. `commit_configuration_update` runs the mutation, routing reconciliation, imported-runtime preparation, and temporary-policy compilation while the database transaction is open, commits, then installs the imported runtime and that same compiled snapshot. Compilation does not replace the live snapshot. A compile failure leaves the transaction uncommitted. `publish_temporary_policy` still compiles and installs outside this transaction and is unused after this commit. `note_product_apply` keeps the successful save receipt when apply fails. The one-shot receipt-failure branches are test-only. Source for this boundary is present. The sibling tests were inspected and were not executed for this page. The checked-in schema JSON is that export. Cargo assertions and normal CLI acceptance remain pending.

## V2 REST tombstone

Protected Dashboard V2 REST answers with a fixed tombstone.

- Anonymous V2 REST: empty-body **401** (auth runs before the
  tombstone).
- Authenticated V2 REST (including loopback local mode): **410** with
  `{ "code": "dashboardV2Removed", "message": "Dashboard API V2 has been removed; refresh the page and retry." }`.
- Unknown `/dashboard/api/...` paths that are not the V3 tombstone prefix,
  not V4, and not a preserved family are also 410 once authenticated.
  Unknown V4 paths are V4 `404`s, not tombstones.

Preserved `/dashboard/api` families (exact path, no trailing slash, no
extra segments):

- `auth/status`, `auth/register`, `auth/login`, `auth/logout`
- `browser/sessions/{token}/ws` (non-empty token)

The `/dashboard/api/v3` prefix is a separate 410 family
(`dashboardV3Removed`). The Vue shell and product views call
`/dashboard/api/v4` only (`requestV3` and `requestV4` share that base;
`dashboardV3.listAccounts` uses `GET /account-records`). Inference routes,
dashboard HTML, and `/dashboard/assets/...` are outside the tombstone.

## Owned CPA control

`GET /dashboard/api/v4/external-integrations/cpa` and an enabled-only `PUT` that does not write return `CpaIntegration`. V4 remounts these V3 DTOs. V3 REST remains the 410 tombstone above. Checked-in `schema/dashboard-api-v3.schema.json` and `schema/dashboard-api-v4.schema.json` are the Rust example export. Source marks the retired request fields with schemars skip. This page does not edit the schema JSON. Whole CLI acceptance and runtime acceptance remain pending.

There is no external CPA product mode and no remote selector. The shared prefix stays `/dashboard/api/v4/external-integrations/cpa`. Old routes and fields are the retained refusal and migration surface. Omitting `target` addresses the owned child. A saved historical row does not select `integration`. Stored historical bytes stay. Explicit migration is required and is not implemented. `OCG_CPA_BASE_URL` is not parsed by this view and is not a product switch.

`legacyMigrationRequired` (`legacy_migration_required`) is a required boolean. It is true only when `cpa_integration()` is `Some`: a stored historical remote CPA row is present. It is redacted. It does not copy the URL, keys, account id, or catalog, and it is not derived from `OCG_CPA_BASE_URL`.

`runtimeUnavailableReason` is owned runtime health only: the existing platform-unsupported text, the execution-record error or `cpa execution is unavailable`, or JSON `null`. The migration sentence and the `OCG_CPA_BASE_URL` sentence are not values of this field. A stored row can set `legacyMigrationRequired` while `runtimeUnavailableReason` is null, and the reverse is possible. The two fields do not imply each other. `CpaRuntime` does not gain `legacyMigrationRequired`. Its `unavailableReason` uses the same health rule. `installed`, `running`, and `owned` stay the execution-report values. Historical data does not turn an installed or running child into unavailable, and it does not turn an absent child into installed or running.

Owned lifecycle on `CpaIntegration`:

| Field | Value |
| --- | --- |
| `configured` | True only when an owned managed record exists or the execution report says the owned executable is installed. A historical row alone leaves it false. |
| `runtimeOwned` | Same predicate as `configured`. |
| `runtimeRunning` | `ExecutionReport.running`. |
| `runtimeSupported` | `cpa_runtime_supported()`. |
| `installedVersion` | Execution report `current_version` when that artifact is installed, otherwise the owned managed record's `current_version`, otherwise null. |
| `latestVersion` | Execution report `latest_version` (`v8.0.10` from the current report). |
| `updateAvailable` | Execution report flag. |
| `currentOperation` | Execution report `current_operation`. |
| `baseUrl` | Owned loopback from the execution report port when that port is set, otherwise `http://127.0.0.1:{managed.port}` when a managed record exists, otherwise `""`. The saved remote URL, `OCG_CPA_BASE_URL`, and `DEFAULT_CPA_BASE_URL` are absent from this value. |
| `baseUrlReadOnly` | Always true. |

Retired singleton outputs stay parseable and stay empty or false. They do not advertise an external account or catalog:

| Field | Fixed value | Reason |
| --- | --- | --- |
| `managementKeyConfigured` | `false` | The historical management cipher is not an owned execution source. |
| `inferenceKeyConfigured` | `false` | The historical inference cipher is not an owned execution source. |
| `enabled` | `false` | Owned enablement is per native credential, not this singleton. |
| `accountId` | `null` | The historical `CPA_ACCOUNT_ID` is not the owned account. |
| `modelCount` | `0` | Owned catalogs are per destination in `destination_models`. This integer cannot name one canonical owned-native catalog. The global `provider_model_catalogs` CPA row is not copied here. |
| `modelsRefreshedAt` | `null` | `destination_models` has no refreshed-at column, and one timestamp cannot cover destination-scoped catalogs. |

`revision` and `processGeneration` are unchanged. A GET does not bump revision and does not write the historical row, account, catalog, or destination bytes.

Requests stay parseable and are refused before I/O and before any write. The candidate schema stops advertising the retired inputs.

| Type | Excluded from schema | Still accepted by serde |
| --- | --- | --- |
| `CpaControlTarget` | variant `integration` | `"integration"` |
| `CpaIntegrationUpdate` | `baseUrl`, `managementKey`, `inferenceKey` | those three properties |
| `CpaTestRequest` | `baseUrl`, `managementKey`, `inferenceKey` | those three properties |

The candidate `CpaControlTarget` schema enum is `["owned"]`. `target` on `CpaIntegrationUpdate`, `CpaTestRequest`, and `CpaRuntimeInstall` still references that enum. Refusal strings, still before I/O:

- Explicit `target: "owned"` plus any of `baseUrl`, `managementKey`, or `inferenceKey`: `owned CPA control does not accept a remote base URL or key`.
- Explicit `target: "integration"`, or any of those three fields with `target` omitted: `Stored remote CPA configuration requires explicit migration and was left unchanged` when a historical row exists, otherwise `Remote CPA is not a target in this product`.
- `DELETE` of the integration uses the same sentences. The stored bytes stay.

`CpaIntegrationUpdate.enabled` stays in the schema. An enabled-only PUT returns the owned view and does not rewrite the historical row or account. The response `enabled` is false.

`OAuthStatusQuery` and `CpaTargetQuery` are serde query types. They are not `JsonSchema` types, so they are absent from the generated schema. `target=integration` on those queries still deserializes and is refused before I/O. URL and redaction checks on any path that still builds a client are unchanged. Legacy persistence is not deleted. User-readable comments on `CpaIntegration`, `CpaControlTarget`, `CpaIntegrationUpdate`, and `CpaTestRequest` call these retired compatibility fields.

## Owned CPA routing explain

Existing `GET /dashboard/api/v4/routing/explain` only. The handler makes one call to `explain_owned_routes(state, public_model, callable_protocol, now)`. `cpa_execution.rs` contains `pub(crate) mod explain`. That facade is the stable read. This page does not describe the legacy selector or materializer as current execution truth. `contract_schema()` includes `RoutingExplanation`, and the checked-in schema JSON is the example export of that contract. `dashboard_v4/types/tests.rs` builds `RoutingExplanation` and `RoutingResolvedMapping` with mapping `destinationId`, `adapterKind`, and `migration_required`. It asserts wire `migrationRequired` false, and it deserializes an older payload that omits that key to false. It also covers exclusion `authority: null`, `desiredRoutes: []`, and the `ownedProjection` fields below. This page did not execute that test and did not run the export. Handler tests that seed a database and call `explain` are not live Ready proof. `verified_ready` cannot be set from a unit test. The read does not decrypt, admit, or write, and it does not call a selector, sticky mutation, `OCG_CPA_BASE_URL`, a materializer, or an outbound client.

The request is unchanged. `model` is required after trim. `clientProtocol` defaults to `chat_completions`. Accepted values remain `chat_completions`, `responses`, `messages`, and `gemini`. Any other value is the existing invalid request. Public Gemini is not a fourth callable protocol: the facade receives `chat_completions`, and the response `clientProtocol` stays `gemini`. Authenticated V4, `ControlRevision`, and process generation stay on the response. The adapter does not take `settings_update`. The facade captures settings revision, process generation, pricing revision, routing mode, and `conversationSticky` under the existing `settings_update` guard, and the adapter formats those captured values. `routingMode` and `conversationSticky` are that captured config, displayed only. `conversationBinding` is `not_evaluated`. `expectedBasePolicyFirstPick` is null in every routing mode. Public eligibility also requires a selected routeable mapping with the same destination, provider, and actual upstream. The public model is not that key. Drift joins the existing `state_changed` path. This capture adds no public wire field. Source for the capture is present. Cargo and normal CLI acceptance remain pending.

Facade `QueryResolution` is the only alias fact. `known == false`, `ambiguous == true`, or `kind == None` is the existing invalid request `model is unknown or ambiguous`. A known row with `routeable == false` is a successful explanation. Disabled, draft, and validation-only mappings stay in `resolved.mappings`. `resolved.kind` and `resolved.alias` copy `QueryResolutionKind` and `alias`. Each mapping copies `destinationId`, `providerId`, `upstreamModel`, `routeable`, `adapterKind`, and `migrationRequired`. `RoutingResolvedMapping` has `destinationId`, `adapterKind`, and `migration_required`. The wire name of the last field is `migrationRequired`. `#[serde(default)]` makes an omitted older payload false, and new responses emit the field.

Canonical class is `Alias` or `PinnedRaw`. A raw-shaped request matches the stored upstream spelling exactly. A case variant is unknown. An exact raw spelling wins over a differently spelled canonical or public alias before identities collapse. The request that is itself the canonical alias stays an alias. Two distinct raw provider identities stay ambiguous. A known catalog row with no matching applied route stays known and `routeable == false`. A desired-only row stays known and nonrouteable. An applied validation-only row stays nonrouteable even when the desired plane is not validation-only. Remote placement sets `migration_required` and forces `routeable == false`. `routeable` is current applied configuration eligibility after quota and placement. It is not destination enablement, catalog enablement, onboarding draft, or the union of the desired and applied `validation_only` flags.

`QueryMapping.migration_required` is copied onto the redacted public `RoutingResolvedMapping`, including a catalog-only historical remote row that has no `RouteFact`. The source test for that row expects `migrationRequired` true, `routeable` false, empty eligible, exclusion, and desired lists, and a redacted remote URL. This page did not execute that test. Cargo and normal CLI acceptance remain pending. The owned-native destination marker `cpa-owned-native` is not a provider id. Provider ids come from native credential storage (`provider_id` `cpa`). A second real provider that resolves the same name stays ambiguous. `ValidationUse` is gone.

A route is public-eligible only when all of these hold: plane is applied; `client_configuration_eligible`; posture is client and `validation_only` is false; `runtime.owned_running`; `origin_verified`, `verified_ready`, `policy_ready`, `pin_capabilities_ready`, and `tuple_aligned`; not `state_changed`, `stopped`, `poisoned`, `policy_malformed`, or `unavailable`; not `known_restriction_blocks` and not `trial_pending`; quota is not `malformed`; not `migration_required` and historical placement is not `remote`. The static flag alone is not a send promise. Eligible rows still carry `callerPending`, `secretRecheckPending`, and `sendPending`. Rank is a field. List order is facade order. There is no SDK next credential. Missing quota evidence is `unknown`. It is not an unlimited quota and it is not, by itself, a public-eligible block. A known applicable reset blocks. An unknown reset is `trialPending` and is not a consumed trial. Expired evidence stays visible with `applicable: false`. A malformed document is unavailable, not an empty open list. Desired-only, validation-only, current revoked, stopped, untrusted, state-changed, and historical-migration rows stay out of `eligible`.

`desiredRoutes` is the desired plane only. It is not merged into `eligible` or `exclusions`. `exclusions` are applied routes that are not public-eligible, in facade order. Each wire code is one entry. Every entry carries the same `authority` object, including precise quota, even when the route is absent from desired. `RoutingExclusion` gains `authority: RoutingRouteAuthority | null`. This handler emits an object. Null remains valid for an older payload that has no structured proof.

`RoutingRouteAuthority` carries plane, credential id, route credential version, current database version, provider, binding, auth id, registration epoch, routing rank, destination id, legacy account id, account label, destination label, public model, upstream model, spelling (`empty`, `same`, or `distinct_upstream`), protocol, endpoint id, origin, endpoint fingerprint, validation only, channel, adapter kind, material, posture, configuration exclusion codes, enabled and draft flags, setup step, native provider, native mode, capability listed, grants cover, native operations, `callerPending`, `secretRecheckPending`, `sendPending`, quota, `knownRestrictionBlocks`, `trialPending`, `clientConfigurationEligible`, `migrationRequired`, and historical placement. The object omits material fingerprint, cipher, hop token, policy token, auth file path, and private relative path. A public route endpoint fingerprint is included. `RoutingNativePin` is protocol, endpoint id, origin, endpoint fingerprint, and HTTP method. `RoutingGrantDisposition` is `granted` with pins, `not_granted`, `local_only`, or `unavailable`. `RoutingOperationFact` is one generation kind plus that disposition. `RoutingQuotaFact.state` is `unknown`, `evidence`, or `malformed`. Evidence copies subject identity, optional public model, window, source, observed time, observation id, reset (`known`, `unknown_reset`, or `expired`), reset time, and `applicable`. Recovery leases and attempt ids are not copied. Channel is `go` or `free` only. Adapter kind stays the destination `AdapterKind` string and is not derived from the channel.

`ownedProjection` copies `desired` and `applied` generation, revision, and digest; `applyStatus`, `desiredRunning`, `runtimeChildGeneration`; `unavailable`, `stateChanged`, `stopped`, `poisoned`; `originVerified`, `verifiedReady`, `policyReady`, `policyMalformed`, `tupleAligned`, `pinCapabilitiesReady`; `ownedRunningBefore`, `ownedRunningAfter`, and `ownedRunning`. `ownedRunning` is true only when both local observations saw the child. `verifiedReady` is the captured plane bit. `applyStatus: applied` does not imply either one. A disagreement between the two running observations does not set `stateChanged`.

Existing `RoutingExclusionCode` and `RuntimeOnlyUncertainty` variants stay, with their current wire spellings, so older payloads still parse. This handler does not emit `mapping_protocol_incompatible`, `credential_disabled`, `binding_disabled`, `model_scope_denied`, `goat_not_eligible`, `goat_unverified`, `candidate_materialization_failed`, `production_route_unsupported`, `account_disabled`, `setup_not_ready`, `channel_mismatch`, `credential_missing`, `auth_error`, `cooling_down`, `free_channel_unavailable`, `quota_waiting`, `quota_due`, or `quota_probing`. Appended exclusion codes are `identity`, `rebound`, `version`, `setup_blocked`, `disabled`, `draft`, `scope`, `model`, `protocol`, `capability`, `native_presence`, `native_mode`, `native_targets`, `not_granted`, `material`, `configuration_unavailable`, `validation_only`, `quota_known_reset`, `quota_unknown_reset`, `quota_malformed`, `migration_required`, `state_changed`, `stopped`, `untrusted`, `owned_not_running`, `origin_unverified`, `not_ready`, `policy_not_ready`, `pin_capabilities`, `tuple_unaligned`, and `policy_malformed`. Appended uncertainties emitted from the facade or a real runtime bit are `cpa_selection_not_evaluated` and `quota_trial_not_evaluated`. Also emitted when the fact is true: `conversation_binding_not_evaluated`; `credential_recheck_pending` when a returned route has `secretRecheckPending`; `state_changed_after_snapshot` when `runtime.state_changed`. `retry_exclusions_not_applied` and `upstream_result_unknown` stay on the enum and are not emitted.

---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](dashboard-api.zh-CN.md) · [Docs index](../README.md)
