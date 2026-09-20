[简体中文](account-model-unification.zh-CN.md)

# RFC: Redesigning The Account And Provider Model

Status: **destinations/credentials are authoritative; schema v58 completes connection-owned, multi-Key Custom HTTP.** V4 now exposes destination PATCH/DELETE and read-only routing explanation. Payload V8 is destination/credential-authoritative and carries model-resolution policy and route overrides; V4–V8 import. `legacy_account_id` and remounted account inputs remain compatibility bridges. [Runtime
invariants](runtime-invariants.md) and [dashboard API](dashboard-api.md)
describe HEAD.

## 1. What is wrong

The gateway has one job: route a client request to an upstream through some
credential. The current design expresses "upstream" and "credential" through
at least five entity shapes, and each shape's identity leaks into shared
code. Counted at HEAD (non-test): about 50 frontend and 100+ Rust branches of
the form `if provider_id == X` / `isZenFreeAccount` / `isCpaIntegrationAccount`
/ `isCustomApiAccount` / `account_type === "managed"` / `type === "platform"`;
`db.rs` alone has 24.

The root causes, in order of damage:

1. **`accounts` conflates destination and credential.** One row holds the
   Key, the enable switch, the order, the cooldown, *and* (for Custom API)
   the endpoint URL, protocol, and model mappings. Every other shape then
   has to decide which half of the row it uses.
2. **Provider and Plan are one identity (`provider_id`).** A Plan is a
   commercial offering with usage windows and expiry; a Provider is a
   transport. Fusing them means a Plan's usage semantics ride on the same key
   that selects an adapter, so anything Plan-specific becomes an adapter
   special case.
3. **Non-accounts are stored as accounts.** Zen Free (`…0002`) and CPA
   (`…0003`) are reserved-UUID `accounts` rows so they can participate in
   ordering and fallback. Every traversal of `accounts` must exclude or
   special-case them.
4. **Three storage shapes for one transport.** Custom API (`accounts` +
   `custom_config` + `account_model_capabilities`), user-defined Providers
   (`providers` + `provider_models`), and platform sites (`platform_accounts`
   + `platform_links` + snapshots) all drive the same Configurable HTTP
   adapter. Guards, discovery, and balance probing are re-implemented per
   shape.
5. **V4 identity is a satellite projection over V3 rows.** The correct model
   (identity → credential → binding → quota pool) exists but is derived from
   `accounts` rather than owning the data, so it can never be the single
   source of truth while V3 is authoritative.
6. **The frontend mirrors all of this.** Three server-state owners
   (`accountsStore`, `identitiesStore`, `PlatformAccountsSection.view`), a
   route-item union type, and per-kind card branches.

Patching any one of these while keeping the others is what produced the
current shape. The fix is a new model with a migration, not another overlay.

## 2. Hard constraints versus current choices

**Kept — these are safety and scope, not design taste:**

- Key material encrypted at rest, never returned in plaintext by list or
  mutation responses; redaction in logs.
- Destination URL and DNS guards (metadata, link-local, IPv4 tricks, no
  redirect with secrets, no client-auth forwarding).
- CAS on every control-plane mutation; idempotent commits by operation id.
- No dynamic code loading: adapters are compiled in. User-defined transports
  are data bound to the sealed Configurable HTTP adapter.
- No remote sync, no admin API, no Tauri `invoke` data path.
- Local single-user trust boundary.

**On the table — every one of these may change:**

- `accounts` as the primary table and its column set.
- Provider and Plan sharing `provider_id`.
- Reserved-UUID singleton rows.
- Separate storage for Custom API / user-defined Providers / platform sites.
- V3 as a frozen, authoritative contract. V3 becomes a compatibility shim
  over the new model and is removed after one deprecation release.
- The Accounts / Providers / Aliases page split as currently drawn.
- Wording in `runtime-invariants.md`, `AGENTS.md`, and `DESIGN.md` that
  encodes the above.

## 3. Target model

Four entities. Everything else is a capability flag or an observation.

### 3.1 Destination

Where requests go and how they are shaped. Replaces Provider, Connection,
Custom API config, and platform parent.

| Field | Meaning |
| --- | --- |
| `id` | stable UUID |
| `adapter` | sealed adapter kind: `opencode_go`, `zen`, `goat`, `minimax`, `kimi`, `ollama`, `cpa`, `http` |
| `name`, `brand_family` | display |
| `base_url`, `protocols`, `auth_scheme` | transport; fixed by the adapter for sealed kinds, user data for `http` |
| `catalog` | model rows with public name, upstream id, per-protocol enablement, preferred protocol |
| `capabilities` | see 3.5 |
| `plan` | optional commercial offering: usage windows, expiry cadence, pricing source (see 3.4) |
| `max_credentials` | `1` for singletons and account-owned endpoints, `null` otherwise |
| `observer_credential_id` | optional non-inference credential used to read site data (platform management token) |
| `enabled`, `revision` | lifecycle and CAS |

A Custom API account migrates to a distinct destination with `adapter = http`,
`max_credentials = null`, stable IDs, and `public_only` resolution; multiple credentials may reference it. A user-defined Provider likewise has `max_credentials = null` but uses `public_and_upstream`. A platform
site is the same plus `observer_credential_id` and the `observer` capability.
Zen Free is `adapter = zen`, `auth_scheme = none`, `max_credentials = 1`. CPA
is `adapter = cpa` with `capabilities.external_integration`; it holds no local
inference credential.

### 3.2 Credential

One routable unit. Replaces the account row's Key half, V4 credential, and
platform link.

| Field | Meaning |
| --- | --- |
| `id`, `destination_id` | identity |
| `name`, `notes` | display |
| `secret` | encrypted; `null` for `auth_scheme = none` and for external integrations |
| `enabled`, `routing_rank` | the routing gate and global order (one order across all destinations) |
| `scope` | model scope: all, or an explicit public-name list |
| `grants` | allowed endpoint ids / origins the secret may be sent to |
| `auth_state`, `auth_state_version`, `last_error` | local verification state |
| `cooldowns` | generic and per-window `until` timestamps (moved from `accounts`) |
| `quota_pool_id` | shared pool membership |
| `onboarding_task` | optional managed-signup state machine (replaces `account_type = managed` + `setup_step`) |
| `purchase_date` | only meaningful when the destination has a `plan` |
| `revision` | CAS |

Keyless destinations still get exactly one credential row with
`secret = null`, so enablement and order are uniform: the routing planner
iterates credentials, never destinations, and no singleton check exists.

### 3.3 Quota pool

Unchanged from V4 in shape: `id`, `policy_mode`, `relation_confidence`,
members. A 429 or window exhaustion on one member fans out to the pool.

### 3.4 Plan (embedded in destination)

Commercial semantics separated from transport: `usage_source`
(`official_api`, `local_projection`, `none`), `windows` (5h / week / month
definitions), `expiry_cadence`, `pricing_source`, `manual_calibration`.
Two destinations can share an adapter and differ in plan (e.g. a future GOAT
tier), and a plan can be attached to an `http` destination if a preset
declares one. Plan is data; adapters never read `provider_id` to infer it.

### 3.5 Capabilities

Booleans and enums on the destination, derived from the sealed adapter for
sealed kinds and from the row for `http`:

`toggleable_credentials`, `testable`, `discoverable_models`,
`official_balance_probe` (host allow-list), `observer`, `managed_signup`,
`external_integration`, `billing_tier_required`, `redirect_policy`,
`auth_header_kind`, `identity_headers`.

All differentiated UI and routing behavior reads these. There is no
`is_zen_free`, `is_cpa`, `is_custom`, or `provider_id == "opencode"` in
target code, including the Rust side.

### 3.6 Observations

Append-only snapshots keyed by destination or credential: usage windows,
wallet / subscription / key-limit quotas, price tables, catalog fetch
results, probe results. Display-only, never routing input except through the
explicit cooldown writes on the credential.

## 4. Target UI

- **Accounts** lists **credentials grouped by destination**, in global
  routing order. Every group uses one card shell (the one shipped in this
  branch): destination header (brand, name, type, subtitle, capabilities
  actions) and one row per credential (name, status tag, meta, switch,
  utility, menu). Single-credential destinations render the same card with
  one row; there is no separate "account card" versus "platform card".
- **Providers** lists **destinations**. Detail = catalog, plan / pricing,
  transport settings, observer credential. Custom API destinations appear
  and edit here like any other configurable `http` destination; Accounts edits credentials only.
- **Add** is one flow: choose a destination (existing, or new from sealed
  kinds / presets / manual `http`), then add a credential. Managed signup is a
  credential onboarding task started from the same flow when the destination
  has `managed_signup`.
- **Aliases** is unchanged in purpose: one row per public name across enabled
  credentials.
- Custom API keeps a first-class entry in Add as the manual `http`
  destination with one credential. It is the fallback for any upstream and
  is never hidden.

## 5. Target API and storage

- **Dashboard API V4 becomes complete**: `destinations`, `credentials`,
  `quota-pools`, `observations`, `onboarding`, plus the existing `contract`,
  `templates`, `applications`. Every mutation is CAS; creates are idempotent
  by operation id.
- **V3 becomes a read/write shim** over the new tables for one release
  (`accounts` ↔ credential + single-credential destination projection), then
  is removed. `schema/dashboard-api-v3.schema.json` stops being the frozen
  authority; the generated V4 types are.
- **Storage**: new tables `destinations`, `destination_models`,
  `credentials`, `credential_grants`, `quota_pools`, `quota_pool_members`,
  `observations`. One migration reads `accounts`, `providers`,
  `provider_models`, `account_model_capabilities`, `platform_accounts`,
  `platform_links`, `credential_bindings`, `cpa_integration`, and V4 identity
  tables into them; the old tables are dropped after the shim release. The
  migration is total: every current row maps to exactly one destination and
  one credential, or the migration refuses to run.
- **Transfer bundle** payload V8 carries the new entities, resolution policy, and route overrides directly.

## 6. Migration

Strangler pattern; each stage ships alone.

1. **Domain model in `ocg-domain`** — `Destination`, `Credential`,
   `Capabilities`, `Plan` types and the total mapper from current rows. Pure
   code plus tests; nothing wired.
2. **Frontend capability layer** — `src/domain/account-capabilities.ts`
   computes the 3.5 capability record from today's catalog / connection
   projection. Replace all frontend identity predicates. Zero behavior
   change; unblocks the UI work while the backend moves.
3. **Frontend state consolidation** — one `destinationsStore` +
   `credentialsStore` (or one store with both maps) fed by the existing V3/V4
   reads through an adapter layer; `PlatformAccountsSection.view` and the
   route-item union disappear from views.
4. **New tables + migration + V4 complete surface**, with V3 reimplemented as
   a shim over them. Land behind the shadow-compare instrumentation and
   require routing parity on the fixture suite before the switch. Split:
   - **4a Projection (read-only, no schema).** `ocg-core` builds
     `LegacyDestinationFacts` / `LegacyCredentialFacts` from live rows
     (accounts, dynamic providers, platform parents and links, CPA, identity
     bindings and pools), runs the stage-1 mapper, joins persisted catalog
     snapshots, and returns the full destination + credential set or the
     list of rows that refuse. A test over every existing integration fixture
     asserts totality — this is the migration test plan executed as code.
   - **4b V4 read surface.** `GET /dashboard/api/v4/destinations` and
     `GET /dashboard/api/v4/credentials` served from 4a; schema, generated
     types, `contract:v4:check`, paired docs.
   - **4c Frontend reads destinations.** `destinationsStore` fed by 4b;
     Accounts renders credentials grouped by destination through the shared
     card shell; the route-item union is deleted. Split:
     - **4c-1** grouping from the projection (`destination-groups.ts`);
       `PlatformAccountsSection` stays a mutation host.
     - **4c-2** `DestinationCard` + `CredentialRow`; `PlatformAccountCard`
       deleted. A destination with one Key collapses to today's `AccountCard`;
       a platform parent stays a group even with one Key. Drag moves the
       group; row menus move Keys inside the group only.
   - **4d New tables + dual-write + V3 shim.** The point of no return; only
     after 4a–4c have run against real data. Split:
     - **4d-1 Shadow tables + backfill.** Schema v50 creates
       `destinations`, `destination_models`, `credentials`, and
       `credential_grants`. Open rebuilds them from the 4a projection when
       that projection is total. V3/V4 reads and all mutations still use the
       legacy tables. No Key material is copied. Existing v45 `quota_pools`
       / `quota_pool_members` are reused via `quota_pool_id`;
       `observations` waits for a later slice.
     - **4d-2 Dual-write.** Historical: mutations refreshed the shadow
       from a full `project()`. **Superseded on HEAD** by incremental
       destination / credential / catalog writes.
     - **4d-3 V3 shim.** V3/V4 reads switch to the new tables after
       shadow-compare stays clean. Split:
       - **4d-3a** Historical: V4 GET authorized from live `project()`
         and only returned a matching shadow. **Superseded on HEAD.**
       - **4d-3b** Historical first cut: V4 GET served a populated
         shadow but still let live `project()` refuse the request.
         **Superseded on HEAD** — a populated store is served even when
         `project()` refuses.
       - **4d-3c** V3 listings become a shim over the same tables.
5. **Routing planner reads credentials and capabilities only.** Delete the
   Rust identity predicates; reserved UUIDs survive only in the migration.
6. **UI cutover** to V4-complete: Accounts = credentials grouped by
   destination, Providers = destinations, one Add flow.
7. **Deprecation release**: V3 shim marked deprecated, transfer V8 default.
8. **Removal release**: drop V3, old tables, `account-providers.ts`,
   `platform-accounts.ts` route-item helpers.

Stages 1–3 need no schema or contract change. Stage 4 is the large one and
the point of no return; it should be preceded by a written migration test
plan covering every current shape (each sealed adapter, Custom with root and
complete-path endpoints, user-defined with and without Key, platform with
linked / unlinked / pending Keys, managed drafts at every step, Zen, CPA).

## 7. Documents this RFC supersedes when implemented

- `runtime-invariants.md`: "Provider and Plan share `provider_id`", "V3 is
  frozen", the Custom API account-ownership section, the platform parent
  section, the reserved CPA / Zen account language.
- `AGENTS.md`: the V3-frozen / V4-additive boundary sentence and "Custom API
  is account-owned".
- `DESIGN.md`: the Accounts / Providers rail descriptions that enumerate
  card kinds.
- `storage-migration.md`: new schema version and the migration runbook.

## 8. Open decisions

- Whether V3 removal happens in the same major version as the shim or one
  later. Settled: `/dashboard/api/v3` is a 410 tombstone in this line;
  operational handlers remount under V4.
- Whether `plan` is embedded in destination (proposed) or a separate table
  referenced by destination; embedded is simpler, separate allows a shared
  plan across destinations.
- Whether CPA's internal OAuth accounts should be projected as read-only
  observations under the CPA destination (proposed) or stay CPA-page-only.
- Naming: `Destination` versus keeping `Provider` for the user-facing word.
  The RFC uses `Destination` internally and leaves the UI label to
  `DESIGN.md`.

### Sealed facts settled in stage 1

`crates/ocg-domain/src/destination.rs` encodes the sealed capabilities and
plans as data. Values that HEAD did not state uniquely were decided as
follows (the code carries no remaining `TODO(rfc)` markers):

- Every secret-bearing adapter is `NoFollow`; only keyless Zen may follow.
  The legacy Go descriptor's `follow_redirects` flag is superseded.
- Auth header kind is not a capability. It derives from the destination's
  `auth_scheme`, and for `Http` from the wire protocol (Messages →
  `x-api-key`); the field was removed.
- `toggleable_credentials` was removed: every credential has an enable
  switch, including keyless ones, so the flag carried no information.
- `testable` is true for every adapter except the external integration
  (CPA), matching the runtime invariant that every ready card offers Test
  connection.
- GOAT, MiniMax, and Kimi plans have `expiry_cadence = Monthly` — their cards
  already show a monthly purchase countdown. MiniMax and Kimi windows are
  `FiveHours` + `Week`, which is how the dashboard presents their official
  usage today.
- CPA `base_url` is `None` (runtime loopback of the managed child).
- Builtin and platform `catalog` are empty at mapping time; the projection
  layer (stage 4a) joins persisted catalog snapshots after mapping.

### Migration rules settled in stage 4a

`crates/ocg-core/src/destination_projection.rs` runs the mapper against
live rows. Where the persisted shape did not match the model, the rule is:

- **The projection never invents state.** A fresh database projects only
  the schema-owned Zen singleton; the CPA destination and its keyless
  credential appear only once the integration has written its reserved
  account row.
- **Binding enablement folds into credential enablement**:
  `enabled = account.enabled && binding.enabled`. The target credential has
  one switch; a disabled binding is not routable either way, so the fold is
  lossless for routing and loses only the "which of the two switches was off"
  distinction, which the new UI does not present.
- **Quota pools are stored membership, never inferred.** A second credential
  on the same identity shares a pool only if it explicitly joined one.
- **Catalog joins never add models.** Builtin destinations take only ids
  present in the persisted catalog snapshot; platform destinations take the
  case-insensitive union of their linked Keys' declared capabilities.
- **A Custom account without a stored `custom_config` row refuses**
  (`CustomAccountMissingEndpoint`) rather than being skipped: totality means
  every row is either mapped or named in the refusal list.
- `routing_rank` is the position in the persisted account order, not the raw
  `sort_order` column.

### UI rules settled in stage 4c-2

Verified on the live Accounts page (nine destination groups, including a
four-Key New API site and a two-Key site):

- Collapse is decided on the **unfiltered** group. A multi-Key destination
  that a filter reduces to one row stays a group card.
- Platform parents are never collapsed just because they currently hold one
  Key (`isSingleAccountGroup` is false unless `max_credentials = 1`).
- Keyboard Up/Down on the group handle persists the global order across
  reload. Row **Move up** / **Move down** persist only inside that group.
- V4 `GET /destinations` and `GET /credentials` stay the grouping source;
  they never include `key_cipher`.
- Providers is unchanged: it still lists connections, not destination
  groups.

### Storage rules for stage 4d-1

- The v50 tables are a **shadow** of `project()`, not yet the source of
  truth. A projection refusal leaves them empty rather than blocking
  database open — V4 already surfaces that 409.
- `credentials` stores `has_secret` and `legacy_account_id` only. It must
  not grow a `key_cipher` or plaintext secret column.
- `quota_pools` already exists from v45; v50 must not create a second pool
  table.
- Catalog and grant order is the `project()` order, reconstructed with
  `ORDER BY rowid`. There is no position column.

### Mutation rules settled in stage 4d-2

Historical dual-write: writers rebuilt the v50 shadow from a full
`project()` snapshot. **Superseded on HEAD.** Runtime writers persist
destinations, credentials, and `destination_models` incrementally.
`refresh_destination_shadow` only syncs builtin catalogs from persisted
contracts. `replace_all_on` remains for leftover-table backfill.

### Read rules settled in stage 4d-3a

Historical: V4 GET authorized from live `project()` and returned the
shadow only when it equaled live. **Superseded on HEAD** by the 4d-3b
store-first rule below.

### Read rules settled in stage 4d-3b

**Superseded on HEAD.** V4 `GET /destinations` and `GET /credentials` serve a
populated destinations/credentials store via `load_all`. A live `project()`
refusal does not hide those rows. An empty store or leftover-table upgrade
window still falls back to `project()`.

### Read rules settled in stage 4d-3c

V3 `GET /accounts` and `GET /platform-accounts` take identity and order from a
populated v50 shadow (`credentials.legacy_account_id` and
`destinations.legacy = platform_parent`). Each listed row is still the live
legacy record — Key material is never read from the shadow. Rows the shadow
does not name are appended in live list order so the frozen V3 contract never
drops an account. An empty shadow or load error falls back to live list order.
V3 does not return `409 destinationProjectionRefused`.

### Routing rules started in stage 5

The live executor loads `routing_projection()` (populated shadow, else total
`project()`) and treats Free as exhausted when a destination with
`adapter = zen` has a credential whose `cooldowns.free_until` is still in the
future. Reserved Zen account/provider UUIDs are no longer the planner's Free
gate. Planner helpers take destination adapter when a projection row is
present (`adapter_for_account`, `account_channel_for`).
`free_channel_is_exhausted_at` on bare account rows still exists for
dashboard probes and keys off the catalog adapter `ProviderAdapterKind::ZenFree`,
not the reserved account id and not `validate_provider_binding`. Live
candidate order uses `list_accounts_for_v3` (shadow credential order).
Request materialization matches mappings to `destination.legacy` when a
projection is present (builtin/dynamic id, or Custom/platform → adapter
`http` plus the Configurable HTTP catalog key) and still falls back to
`account.provider_id` for tests without a projection. `RoutingCandidate.adapter`
comes from `destination.adapter` when present, else the mapping catalog
kind — not `account.provider_id`. Key material comes from the
credential row. Sealed adapters resolve their descriptor by
`ProviderAdapterKind` (`get_by_kind`), not by the account's reserved UUID.
Selector channel eligibility uses `channel_for_adapter`. Zen/CPA no longer
require the reserved account id on the resolve path. CPA live send uses
`AttemptSpec::is_local_external_integration` (the CPA adapter's proxy
model), not `provider_id`. Diagnostic channel selection uses
`mapping_is_zen_free` (adapter kind), not `ProviderMapping::is_zen_free`.
Custom vs dynamic HTTP uses `destination.legacy` when a projection row is
present; leftover rows and `resolve_route_with_dynamics` split Configurable
HTTP on whether a dynamic provider runtime exists, not `is_custom_api`.
Isolated live-send grants follow stored `account_custom_config` when
present, else the dynamic runtime. `/v1/models` listing admits Custom and
CPA rows by mapping adapter / the CPA integration credential, not
`CPA_ACCOUNT_ID` or `mapping.is_custom_api()`. Identity headers on the live
send and probe paths follow destination `capabilities.identity_headers` and
adapter kind (`opencode_go` session; `zen` also synthesizes anonymous
fields), not a reserved `provider_id` string compare. Resolve, probe, and
production-support APIs take an explicit `ProviderAdapterKind`; a misleading
`account.provider_id` does not win. Shadow compare labels attempts with the
route adapter, not a catalog lookup of `provider_id`. Forward-log
`scope_to_provider` uses the catalog adapter for free vs unknown vs
unpriced. Catalog mappings are
still joined by `provider_id` (`mapping_adapter_kind`, the sealed custom
catalog key); that catalog key is not an account identity predicate.

### Removal rules started in stage 8

Schema v52 dropped the physical `accounts` table after backfilling
remaining Account fields onto `credentials` and asserting every
`accounts.id` has a `credentials.legacy_account_id`. Destinations +
credentials are the account-row store. Schema v53 dropped
`account_custom_configs` and `account_model_capabilities` after copying
mappable Custom endpoint/protocol/model facts onto `destinations` /
`destination_models`. Schema v54 dropped `platform_accounts` and
`platform_links` after copying mappable parents onto destinations
(`legacy_kind=platform_parent`) and links onto inference credentials, with
the management cipher on the observer credential. Schema v55 dropped
`cpa_integration` after mapping a leftover row onto the CPA destination
(`adapter=cpa` / legacy CPA id) and putting `management_key_cipher` on
the observer credential. Inference stays keyless of the management
secret. A leftover-absent fresh database does not invent a CPA
destination. Schema v56 dropped `providers` and `provider_models` after
mapping leftover `origin IN ('preset','custom')` rows onto destinations
(`legacy_kind=dynamic`) and `destination_models`. Builtin catalog stays
sealed (`BUILTIN_PROVIDERS`); v56 does not invent destinations for
builtin seed rows. Schema v57 dropped the leftover identity satellites
after copying mappable identity / binding / onboarding / subscription
facts onto credentials + `credential_grants`. `quota_pools` /
`quota_pool_members` stay. Those six leftover identity tables are gone.
V4 GET listings stay secret-free. Keys stay encrypted
at rest. `src/domain/account-providers.ts`
is deleted; card flags use destination capabilities or catalog keys, not
reserved account UUIDs. `/dashboard/api/v3` is a 410 tombstone.

### Deprecation rules started in stage 7

New node backups export payload V8. The encrypted envelope stays v1. V8
carries `destinations` and `credentials` (including plaintext secrets,
platform and CPA observer management credentials, and identity / grant /
cooldown extras inside that envelope), plus `quotaPools` and `node`. A merge
import that omits a CPA observer key keeps the destination's existing
management key. Latest export does not emit `accounts`, platform rows, dynamic
provider definitions, or a separate identities array. V4–V6 remain
importable through an old-graph decoder that maps into the same new-model
import object. A V7 package that still carries leftover old fields must
match dest/cred or is rejected. V9 and newer are an unsupported-version
error. Remounted `/accounts*` handlers are I/O adapters; V4
destinations/credentials are the read model.

### UI rules started in stage 6

Accounts lists every destination group through one `DestinationCard` shell.
Each credential is a `CredentialRow`. Header brand and type come from
destination `brand_family` / capabilities when the group is not a platform
parent. `AccountCard` is no longer mounted on this page. Providers lists
destinations on the rail (joined to the V4 connection that still owns
mutations; platform destinations have no connection and use `destination=`).
Add is one flow: Providers `添加供应商` and `?view=providers&add=1` open the
Accounts chooser (`add=1`, or `preset:<id>` when a Providers preset bookmark
is mapped). The separate Providers preset browser is no longer the Add entry.

---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](account-model-unification.zh-CN.md) · [Docs index](../README.md)
