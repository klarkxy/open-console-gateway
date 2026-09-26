[简体中文](storage-migration.zh-CN.md)

# Storage And Migrations

Operator contract for upgrades, backups, and rollback. Schema details are in [Persistence](state-and-lifecycle.md#persistence).

## Schema v63 — explicit HTTP protocol routes

v63 additively adds nullable `destinations.protocol_routes_json`. `NULL` and an empty list retain legacy behavior: the destination's existing base URL and authentication apply to its legacy protocol set. A nonempty list stores one to three unique protocol routes, each with its complete endpoint and authentication; the first route remains consistent with the legacy base fields. Malformed, duplicate, or unknown route values reject the write or transfer rather than being repaired at read time.

No read path performs DDL. The migration preserves disabled rows and their evidence/overrides. A later official refresh can enable only an unknown Auto row when it supplies protocol evidence; confirmed disabled or explicit `force_off` state remains off. This migration does not create a `pre-v63` backup. Before upgrading a production data directory, make a complete, consistent backup yourself. Older binaries cannot open schema 63; rollback means restoring that full pre-upgrade data directory with the same cipher identity.

Portable export now uses payload V11 and carries explicit routes with destinations, catalog state, and grants. V4–V10 payloads that omit routes remain importable as legacy destinations. A payload older than V11 that contains nonempty routes is rejected to avoid data loss; V1–V3 and V12 or later are unsupported.

## Schema v62 — personal credit estimates

v62 adds nullable `credentials.credit_meter_json` and `forward_logs.credit_receipt_json`. Each credential owns its configuration, credit buckets, calibration baseline, and estimated consumption. A settlement receipt and its debit commit in the same transaction; repeated stream finalization cannot debit twice, and log removal cannot replenish the stored balance. Supplier containers and existing quota-sharing metadata do not own these personal meters.

The earlier v61 console-session column is retained as historical schema but cleared when upgrading to v62; no runtime code reads or renews those tokens. Configuration rewrites preserve a credit meter only for the same credential, destination, and endpoint. Key rotation on that account does not reset its balance. Inference grants, cooldowns, and quota recovery are unchanged. These additive changes are transactional and create no separate pre-v62 backup. Rollback requires restoring the whole pre-upgrade data directory; older binaries refuse schema v62.

An exclusive `.database-open-gate.lock` serializes initialization. Each open
database holds a shared lock on `.database-open.lock`. Pending credit
receipts are recovered only by an opener that can first acquire the exclusive
lock, after all previous database handles have closed. A concurrent CLI status
read therefore leaves active receipts untouched. If another handle survives a
gateway crash, recovery waits for a later cold open. Do not remove or replace
either lock file while the directory is in use; upgrades must stop older binaries
that do not participate in this lock.

## Schema v60 — per-Key quota recovery

v60 additively stores confirmed per-Key quota exhaustion on `credentials.quota_recovery_json` (nullable TEXT JSON). `migrate_to_v60` requires schema v59, calls `quota_recovery::ensure_column`, then writes `schema_version` 60. An already-v60 open still runs `ensure_column`. There is no credentials-table rewrite and no pre-v60 SQLite snapshot. Ordinary cooldown columns, credential IDs, routing order, and Key ciphertext stay as stored.

The JSON holds epoch, reason, window map (optional reset instants), observed time, next retry, and failure count. The probing lease is process-local and is not persisted; a restart reloads wait and backoff from the column. Recovery is independent of quota pools and ordinary cooldown. Local destination-projection rewrites snapshot and restore the column in place. Node transfer does not export it. Metadata edits that do not replace the Key leave it in place; rotate, Key replacement, and managed-key writes set it NULL.

Older binaries refuse a v60 database (`existing_version > CURRENT_SCHEMA_VERSION`) and do not apply the recovery gate. Roll back by restoring the whole pre-upgrade data directory; that is not a behavior-preserving downgrade of the migrated file.

## Schema v59 — runtime authorization and model authority

Schema v59 persists `credentials.authorization_connection_id`, preserving the existing endpoint-grant namespace without changing grant values, Key ciphertext, credential IDs or global order. Platform Keys retain their historical per-Key authorization identity; shared HTTP Keys retain their connection identity. Normal routing reads this field directly.

Existing Custom protocol judgments are converted into the destination catalog in one transaction. Conflicting judgments for a shared model reject the upgrade instead of combining permissions. CPA's selected catalog is also materialized into `destination_models`. A nonempty v58 database receives a verified `data.sqlite.pre-v59.<timestamp>.bak` and `.sha256` sidecar before mutation. Restore the matching pre-upgrade directory to roll back; no down-migration is provided.

## Routing cards (schema v59, no new tables)

`settings.routing_cards_v1` stores versioned card IDs, destination references and credential membership. Card identity is separate from the shared destination configuration. `credentials.routing_rank` remains the runtime order: one CAS layout write validates the complete inference-credential set and commits its flattened ranks and card metadata in one transaction. Reads never reorder credentials; legacy rank-only writes reconcile card boundaries against the saved ranks. Adjacent and empty cards remain distinct. Payload V9 adds validated `routingCards`; V4–V8 imports derive cards from their credential order.

## Data directories and cipher identity

Every database open uses the Host-resolved cipher (`Database::open_with_cipher` on CLI, desktop, and Docker). Stored account ciphertext is probed before migration and decryption errors fail closed. New writes use authenticated AES-256-GCM (`v2:`). Unprefixed legacy XOR still decrypts so backups restore; a successful Host-cipher open rewrites those rows to v2. A successful UTF-8 decode of XOR is not treated as v2 success. Retain the original cipher; rewriting ciphertext does not repair a mismatch.

| Surface | Default data directory | Cipher identity |
| --- | --- | --- |
| Windows desktop (Tauri) | `%USERPROFILE%\.ocg-mgr` | `MachineBoundCipher` from `USERNAME`, `COMPUTERNAME`, and `APPDATA`. The data directory is not the cipher seed; there is no `.encryption-key` on this path. |
| macOS / Linux desktop (Tauri) | `~/.ocg-mgr` | `StaticKeyCipher` from `<data-dir>/.encryption-key` (created on first launch). |
| CLI | `~/.ocg-mgr-cli`, or `--data-dir <path>` | Priority: `--encryption-key` > `OCG_MANAGER_ENCRYPTION_KEY` > `<data-dir>/.encryption-key`. |
| Docker | container `--data-dir /data` (Compose volume `ocg-data`) | Same CLI resolution. Optional `OCG_MANAGER_ENCRYPTION_KEY` is an explicit restore override; a normal volume keeps `.encryption-key`. Files in `/data` must stay writable by UID/GID `10001`. |

Keep each surface on its own cipher identity:

- Windows desktop data cannot decrypt account ciphertext on another Windows user or machine, nor under the CLI/Docker static cipher.
- Copying a GUI directory onto the CLI default path (or the reverse) uses a different directory and, on Windows, a different cipher.
- If the process was started with `--encryption-key` or `OCG_MANAGER_ENCRYPTION_KEY`, restoring only `.encryption-key` is not enough; supply the same explicit secret again.

## Upgrades and backups

SQLite migrations run in place when the GUI or CLI starts. Before opening a newer binary:

1. Stop every process that has the data directory open (desktop tray **Quit**, CLI Ctrl+C / service stop, `docker compose stop`). WAL files belong with `data.sqlite`.
2. Back up the **whole** data directory, including `.encryption-key` and `browser-profiles/` when present; for Docker, both `ocg-data` and `ocg-browser-profiles`. Keep the matching cipher material listed above.
3. The signed desktop updater manages its own stop and restart; CLI and Docker upgrades stay manual.

Downgrades are not supported: never point an older binary at a migrated database. To roll back, restore the whole-directory backup made before the upgrade.

## Schema v27 and the pre-v3 snapshot

`CURRENT_SCHEMA_VERSION = 63` (`crates/ocg-core/src/db.rs`). Historical migrations v1–v57 remain described below. v58 adds `destinations.model_resolution`, backfills `adapter_defined` / `public_only` / `public_and_upstream`, changes legacy Custom destinations to unbounded credential capacity, preserves every destination and credential ID, and writes a verified pre-v58 SQLite backup for a non-fresh canonical v57 source before mutation. v60 additively stores `credentials.quota_recovery_json` (see above).

## Schema v45 — identity / credential / binding satellites

v45 splits the legacy Account into identity-container / credential / binding semantics without moving Key material. The `accounts` row remains the physical credential. Additive satellite tables carry what that row could not express. All new ids are deterministic UUIDv5 values over the same namespace as connection ids, so the migration is idempotent and retry-safe. No pre-migration backup file (additive, like v43/v44). Rollback remains the existing whole-directory restore.

Tables:

- `upstream_identities` — `id`, `label`, `identity_confidence` (`opaque` | `declared`), `authority_site`, `authority_subject`, `enabled`, `notes`, `created_at`, `updated_at`
- `accounts.identity_id` — new column
- `credential_state` — `account_id` PK → `accounts`, `credential_id` UNIQUE, `version` = 1, `auth_state_version` = 1, `rotated_at`
- `credential_bindings` — `id`, `account_id`, `connection_legacy_kind`, `connection_legacy_id`, `model_scope` JSON `{kind:all}` | `{kind:only,models}`, `enabled`, `created_at`, `updated_at`
- `legacy_identity_map` — `legacy_kind`, `legacy_id`, `new_kind`, `new_id`, `migration_version`
- `onboarding_tasks` — `id`, `account_id`, `kind` `managed_registration`, `step`, `state` `in_progress` | `completed`, …
- `subscription_records` — `account_id` PK, `source` `legacy_manual` | `managed_payment`, `purchase_date`, `expires_on`, `recorded_at`
- `quota_pools` — `id`, `subject_kind`, `subject_ref`, `relation_confidence`, `policy_mode`, `created_at`
- `quota_pool_members` — `pool_id`, `account_id` (one member per backfilled identity; additional credentials use independent pools unless sharing is explicitly selected)

Satellite rows are deleted explicitly in the same transaction (the DDL declares `ON DELETE CASCADE` but the process does not enable the foreign-key pragma). On open, a v45 consistency check repairs missing satellite rows with the idempotent backfill and fails closed if inconsistencies remain. During v45 backfill, missing required `accounts` columns fail the open through ordinary SQL errors; they are not skipped.

Migration rules: each existing account becomes exactly one identity (label = account name, confidence `opaque`), one credential (`version` 1), and one binding to the account's connection (builtin provider / dynamic provider / the Custom account's own connection) with `model_scope=all` and binding `enabled=true`. The account enable switch remains the routing gate; an independently disabled binding stays disabled across reopen/repair. Routing rank is the existing `accounts.sort_order` (read from `accounts`, not duplicated). `legacy_identity_map` records account → identity / credential / binding. Managed accounts that are not yet `ready` get one `onboarding_tasks` row `in_progress` at the current step; ready managed accounts get no fabricated history. Only accounts of sealed built-in Providers that already publish purchase/expiry dates get a `subscription_records` row with source `legacy_manual`. User-defined and Custom API accounts get none: dates stay unknown, and nothing is priced at zero (D07). Platform links: the linked Key's identity becomes `declared` with `authority_site` = parent `base_url`; each platform parent gets its own identity with a `platform_observer` credential (the management credential, never used for inference). Parents and linked Keys are never merged; the relation stays declared, not verified (D04). Cooldowns are not moved: they are projected as quota windows (generic / 5h / week / month → subject `credential`; free → subject `egress` `free_channel`, declared, authoritative) and preserve the exact stored instants. An unknown metric is `null`, never zero. Migration creates one quota pool per identity (`subject` credential / identity id, `relation_confidence` unknown, `policy_mode` authoritative_limit). It never writes `verified`. Under the current v46 write path, a later credential is independent by default; explicitly sharing with a same-identity credential joins its pool and marks the relation `declared`. Routing honors stored `model_scope` and binding `enabled`.

Every account insert (V3 create, managed create, user-defined Provider first Key, V4 onboarding commit, node import, V4 identity credential create) writes the satellite rows in the same transaction via the single mapper shared with this migration. Platform link / unlink updates the linked identity's confidence and site in the same transaction.

Rotate, binding edits, second-credential writes, and configurable-destination PATCH/DELETE are V4 CAS paths. New exports use portable payload V11 (envelope v1). It carries explicit protocol routes along with destinations, credentials, per-model route overrides, and `modelResolution`. V4–V10 remain importable when they omit routes; a pre-V11 payload carrying routes is rejected. V12+ is rejected. Legacy Custom rows keep stable IDs and `public_only` resolution while becoming connection-owned and multi-Key. Destination/credential merge remains transactional.

## Schema v46 — persisted binding grants

v46 additively stores credential-binding grants as saved facts:

- `credential_bindings.allowed_endpoint_ids` — JSON string array of connection endpoint ids
- `credential_bindings.allowed_origins` — JSON string array of normalized Origins (`scheme://host[:port]`)

Empty arrays mean no grant. NULL is only valid during the one-time migration; v46 backfills existing rows once from the currently configured assigned connection endpoints (same ids as `/connections`). New Keys capture that same safe default: sealed adapters stay on static official endpoint scope with no origins; Custom and dynamic default URLs include same-origin existing route endpoints; a foreign-Origin model override is not granted implicitly. Rotation, connection/URL/model edits, repair, and reopen never manufacture or expand saved grants. Explicit grants are preserved through repair/reopen/import.

V4 `BindingDto.allowedEndpointIds` / `allowedOrigins` project those stored facts. Optional PATCH of both grant fields together validates ids and normalized Origins against currently configured selected connection endpoints and rejects foreign ids, malformed origins, and nonconfigured origins atomically; accepted values are stored in canonical form; empty both revokes. Optional `POST /identities/{id}/credentials` `quotaSharing` is `{kind:"independent"}` by default (including omitted old clients) or `{kind:"shared", credentialId}` for an explicit same-identity inference credential. Existing v45 identity pools are preserved. Explicit join uses the source pool if any, otherwise creates a pool containing only the selected source and the new member. Ordinary shared-pool cooldown writes keep the maximum per-window deadline across members (including the source); an explicit manual clear still clears the pool. `GET /accounts` `CredentialSummary.quotaPoolId` projects stored pool membership (`null` when the credential is not a member), including singleton identity pools and even when `quotaWindows` is empty. Optional `operationId` reuses the v44 HMAC dashboard-operation ledger. V6 portable identity graphs require grants and reject malformed references before the import transaction; V4/V5 imports receive one-time safe grants. No pre-migration backup file (additive, like v43–v45). Rollback remains the existing whole-directory restore.

## Schema v47 — persisted onboarding drafts

v47 additively stores onboarding lifecycle on the existing `providers` row:

- `providers.onboarding_draft` — integer boolean, `NOT NULL DEFAULT 0`

Existing rows migrate to configured. A draft may omit Key and model targets; an explicit draft that already has a Key and models still stays off routing, aliases, catalog, and the gateway. Ordinary V3 provider upsert/update preserves the flag and does not silently enable a draft. Completing a draft through V4 onboarding with `mode=complete` clears the flag in the same transaction as the operation receipt. V6 node export includes drafts and requires `onboardingDraft` on each portable provider; V4/V5 packages must omit that field. Blank model lists are valid only for drafts. No pre-migration backup file (additive, like v43–v46). Rollback remains the existing whole-directory restore.

## Schema v49 — unpublished public models

v49 additively creates `unpublished_public_models` for public names hidden from authenticated `GET /v1/models`:

- `public_model` — primary key, stored case-folded
- `updated_at`

Missing names stay published. Hidden names remain routable. The write path is `PATCH /dashboard/api/v4/alias-publication`. Node transfer does not carry this table. No pre-migration backup file (additive, like v43–v47). Rollback remains the existing whole-directory restore.

## Schema v50 — destination shadow

v50 additively persists a shadow of the stage-4a `project()` destination and credential set. The tables are not yet the source of truth: V3/V4 reads and all mutations still use the live legacy rows. Control-plane writes that `project()` reads rebuild the shadow in the same SQLite transaction; reopen still rebuilds from live rows. A projection refusal empties the four tables and does not fail database open.

Tables:

- `destinations` — `id`, `legacy_kind` (`builtin` | `dynamic` | `custom_account` | `platform_parent`), `legacy_id`, `adapter`, `name`, `brand_family`, `base_url`, `protocols_json`, `auth_scheme`, `capabilities_json`, `plan_json`, `max_credentials`, `observer_credential_id`, `enabled`
- `destination_models` — catalog rows keyed by `(destination_id, public_model_key)`; `public_model_key` is the case-folded public name
- `credentials` — `has_secret` and `legacy_account_id` only as secret-adjacent facts; no `key_cipher`, `password_cipher`, or plaintext secret
- `credential_grants` — `endpoint_id` / `origin` grants

`credentials.quota_pool_id` is a nullable text id that reuses the existing v45 `quota_pools` / `quota_pool_members` tables. v50 does not create a second pool table and does not create `observations`. JSON columns store `serde_json` of the existing domain types. Dates are RFC3339 text. Booleans are `0`/`1`. No pre-migration backup file (additive, like v43–v49). Rollback remains the existing whole-directory restore.

## Schema v51 — credential secret store

v51 additively stores Host-cipher Key and password material on `credentials`:

- `credentials.key_cipher` — `TEXT NOT NULL DEFAULT ''`
- `credentials.password_cipher` — nullable `TEXT`

Existing rows copy ciphertext from `accounts` through `legacy_account_id`. Persist-on-open rebuilds copy the same way so a shadow replace does not wipe secrets. Live send prefers a non-empty credential cipher and falls back to the `accounts` row. V4 GET listings stay secret-free. The `accounts` table is not dropped in this version. No pre-migration backup file (additive). Rollback remains the existing whole-directory restore.

## Schema v52 — drop `accounts`

v52 makes `credentials` (joined to `destinations`) the account-row store and physically drops `accounts`:

- Adds remaining Account columns on `credentials` (`username`, `referral_code`, `cooldown_until`, `created_at`, `updated_at`, `auth_error`, `account_type`, `setup_step`, `provider_id`, `credential_kind`, `quota_scope`, `identity_id`, plus verification and usage-window columns still written at runtime).
- Backfills those columns from `accounts` via `legacy_account_id` while the table still exists.
- Rebuilds the destination shadow once before the drop so every live account has a credential row.
- Refuses the migration if any `accounts.id` lacks `credentials.legacy_account_id` (no invented rows).
- Rewrites child-table foreign keys that pointed at `accounts`, then `DROP TABLE accounts`.

A Host open after v52 does not empty a populated destinations/credentials store just because `project()` can no longer read `accounts`. `get_account` / `list_accounts` reconstruct `Account` from credentials (`legacy_account_id` stays the stable remounted id). Runtime writes target credentials (and destinations when provider/name/url change). V4 GET destination/credential DTOs stay secret-free; Key ciphertext remains only on the credential SQL row. Fresh databases never keep an `accounts` table after migrate. After v53, `account_custom_configs` and `account_model_capabilities` are also gone. After v54, `platform_accounts` and `platform_links` are also gone. After v55, `cpa_integration` is also gone. After v56, leftover `providers` / `provider_models` are also gone. Identity satellites remain. No pre-migration backup file. Rollback remains the existing whole-directory restore.

## Schema v53 — drop leftover Custom tables

v53 makes `destinations` + `destination_models` the store for Custom HTTP endpoint, protocol, and model mappings, then physically drops the leftover tables:

- Before the drop, every unlinked leftover `account_custom_configs` / `account_model_capabilities` row must map onto a Custom destination (`legacy_kind=custom_account`, `legacy_id=account_id`) and its `destination_models`. Empty leftover URLs or unknown leftover protocols refuse the migration; no URL or protocol is invented.
- Linked platform Keys that only exist as `platform_links` plus leftover custom config stay on leftover `platform_*` tables until v54. Their leftover custom rows are not mapped to a Custom destination; leftover model rows union onto the platform parent catalog.
- Readable leftover capabilities are intersected onto every Custom credential's stored scope, including Keys with no leftover rows (`All ∩ []` and `Only[x] ∩ []` become `Only[]`). A leftover table whose columns cannot map is refused when it still has rows, and is never treated as an empty capability set.
- Then `DROP TABLE account_custom_configs;` and `DROP TABLE account_model_capabilities;`.

A Host open after v53 does not empty a populated destinations/credentials store. `account_custom_config` / `list_account_model_capabilities*` reconstruct from the Custom destination (or the platform parent destination catalog for a linked Key). Writes persist destinations and `destination_models` and refresh `credentials.destination_id` when needed. V4 GET destination/credential DTOs stay secret-free. Fresh databases never keep those two leftover tables after migrate. After v54, `platform_accounts` and `platform_links` are also gone. After v55, `cpa_integration` is also gone. After v56, leftover `providers` / `provider_models` are also gone. Identity satellites remain. No pre-migration backup file. Rollback remains the existing whole-directory restore.

## Schema v54 — drop leftover platform tables

v54 makes destinations + credentials the store for platform parents and links, then physically drops the leftover tables:

- Before the drop, every leftover `platform_accounts` row must map to a destination (`legacy_kind=platform_parent`, `legacy_id=parent.id`) with `base_url`, `name`, and `platform_kind` (`new_api` | `sub2api`; also mirrored on `brand_family`). Empty leftover URLs or unknown leftover kinds refuse the migration; no site or kind is invented.
- Management `credential_cipher` lands on the observer credential (`destinations.observer_credential_id`). That credential `has_secret` is true when a cipher is present. Parent `version` / `snapshot` survive as additive destination columns `platform_version` / `platform_snapshot`.
- Every leftover `platform_links` row must map to the linked inference credential (`legacy_account_id=account_id`) with `destination_id` equal to the platform parent destination. `group_json`, link version, and link snapshot survive as additive credential columns. A leftover link whose parent destination or inference credential is missing refuses the migration.
- Then `DROP TABLE platform_links;` and `DROP TABLE platform_accounts;`.

A Host open after v54 does not empty a populated destinations/credentials store. `list_platform_accounts` / `list_platform_links` and create/update/delete/link/unlink/refresh/import reconstruct and persist from destinations + credentials. `project()` derives platform parents from those rows. V4 GET destination/credential DTOs stay secret-free (no management cipher / `key_cipher`). Fresh databases never keep those two leftover tables after migrate. After v55, `cpa_integration` is also gone. After v56, leftover `providers` / `provider_models` are also gone. Identity satellites remain. No pre-migration backup file. Rollback remains the existing whole-directory restore.

## Schema v55 — drop leftover CPA table

v55 makes destinations + credentials the store for the singleton CPA integration, then physically drops the leftover table:

- Before the drop, a leftover `cpa_integration` row must map onto the CPA destination (`adapter=cpa` / legacy builtin `cpa`) with `base_url` on the destination when present, and `management_key_cipher` on the observer credential (`destinations.observer_credential_id`). The reserved inference credential stays keyless of the management secret. Empty leftover management cipher or missing leftover columns refuse the migration; no CPA destination is invented when leftover is absent.
- Then `DROP TABLE cpa_integration;`.
- CPA model snapshots stay in `provider_model_catalogs`. Loopback / compose `base_url` overrides still follow existing runtime invariants.

A Host open after v55 does not empty a populated destinations/credentials store. `cpa_integration()` / `upsert_cpa_integration` / `delete_cpa_integration` reconstruct and persist from the CPA destination + observer credential. `project()` does not read `cpa_integration`. V4 GET destination/credential DTOs stay secret-free (no management cipher / `key_cipher`). Fresh databases never keep the leftover table after migrate and do not invent a CPA destination. After v56, leftover `providers` / `provider_models` are also gone. Identity satellites and `provider_model_catalogs` remain. No pre-migration backup file. Rollback remains the existing whole-directory restore.

## Schema v56 — drop leftover dynamic Provider tables

v56 makes destinations + `destination_models` the store for user-defined / preset HTTP Providers, then physically drops leftover dynamic Provider storage:

- Before the drop, every leftover `origin IN ('preset','custom')` provider must map to a destination (`legacy_kind=dynamic`, `legacy_id=provider.id`) with name, `base_url`, protocols, auth, origin, offering, preset, timestamps, and `onboarding_draft`. Every leftover `provider_models` row for those ids maps onto `destination_models` (including `upstream_override`). Empty required URL for keyed HTTP, unknown adapter, or unmapped leftover `provider_models` refuse the migration; no URL or adapter is invented.
- Builtin seed rows are not copied. Sealed catalog stays compiled-in (`BUILTIN_PROVIDERS` / adapter registry). v56 does not invent destinations for builtin seeds and does not invent runtime adapter rows.
- Then `DROP TABLE provider_models;` and `DROP TABLE providers;`.

A Host open after v56 does not empty a populated destinations/credentials store. `list_control_plane_dynamic_providers` / get / upsert / delete / onboarding commit / transfer merge reconstruct and persist from destinations + `destination_models`. V4 connections/templates serve builtins from the sealed catalog and user-defined rows from destinations. `project()` does not require leftover Provider tables. V4 GET destination/credential DTOs stay secret-free. Fresh databases never keep leftover `providers` / `provider_models` after migrate and do not invent a user-defined destination. After v57 the six leftover identity tables are also gone. `quota_pools`, `provider_model_catalogs`, and `dashboard_operations` remain. No pre-migration backup file. Rollback remains the existing whole-directory restore.

## Schema v57 — drop leftover identity satellites

v57 makes credentials + `credential_grants` the store for identity, binding, onboarding, and subscription facts, then physically drops the leftover satellite tables:

- Additive credential columns hold leftover identity/binding/state facts (`identity_confidence`, `authority_site`, `authority_subject`, `identity_enabled`, `identity_label`, `identity_notes`, `credential_version`, `auth_state_version`, `rotated_at`, `binding_id`, `binding_enabled`, `subscription_source`, `subscription_expires_on`). Leftover grants copy onto `credential_grants` when missing. Leftover onboarding copies onto `credentials.onboarding_json`. Leftover subscription copies onto credential purchase/expires fields.
- Before the drop, every leftover `upstream_identities` row that is referenced by a credential or platform destination must map. Every leftover `credential_state` / `credential_bindings` / `onboarding_tasks` / `subscription_records` row must map to a credential (`legacy_account_id` / `identity_id`). An orphan binding or an identity with no reconstructible credential/destination refuses the migration. Fresh leftover-absent databases do not invent identities.
- Reserved identity UUIDs stay in code. `legacy_identity_map` is only a migration bridge.
- Then `DROP TABLE` `upstream_identities`, `credential_state`, `credential_bindings`, `legacy_identity_map`, `onboarding_tasks`, and `subscription_records`. Reopen of an already-v57 database also `DROP TABLE IF EXISTS` those leftovers.
- `quota_pools` / `quota_pool_members` stay (`quota_pool_members.account_id` is the remounted `credentials.legacy_account_id`). `credential_grants`, `provider_model_catalogs`, and `dashboard_operations` stay.

A Host open after v57 does not empty a populated destinations/credentials store. `list_identity_model` / create-credential / rotate / update grants / onboarding / transfer V6+V7 identity import / platform identity label updates reconstruct and persist without leftover identity tables. V4 `GET /accounts` stays identities, reconstructed, secret-free (no `key_cipher` / management cipher). Fresh databases never keep those six leftover tables after migrate, keep `quota_pools`, keep Zen, and do not invent an extra identity. Those six leftover identity tables are gone, so this Stage 8 leftover drop is complete for identity satellites. No pre-migration backup file. Rollback remains the existing whole-directory restore.

## Schema v58 — connection-owned Custom HTTP

v58 adds non-null `destinations.model_resolution`. Builtins backfill to `adapter_defined`, dynamic HTTP to `public_and_upstream`, and legacy Custom/platform destinations to `public_only`. Legacy Custom rows keep their destination `id`, `legacy_id`, credentials, ordering, scopes, grants, cooldowns, quota pools, and model mappings; only `max_credentials` becomes `NULL`, allowing later Keys to reference the same destination. Equal names or URLs never merge. A canonical non-fresh v57 source receives a unique verified `data.sqlite.pre-v58.*.bak` plus SHA-256 sidecar before mutation. Fresh databases skip the backup. Reopen is idempotent.

## Schema v48 — inert columns and empty leftover tables

v48 removes four inert columns:

- `provider_contract_scopes.chat_completions_enabled`
- `provider_contract_scopes.responses_enabled`
- `provider_contract_scopes.messages_enabled` — unread since v31; live enablement is the model protocol override and preference tables
- `accounts.free_alias_enabled` — inert `0`; Zen Free uses `accounts.enabled`

It also drops leftover `dynamic_providers` / `dynamic_provider_models` (and their indexes) when those tables exist and are empty. A nonempty leftover on a v47 source fails closed before any drop or version claim; schema stays 47 and the rows are left in place. A current-schema database does not delete nonempty leftover rows.

Before any v48 write on a non-empty v47 database, the process writes a unique never-overwritten sibling snapshot:

```text
data.sqlite.pre-v48.<timestamp>.bak
data.sqlite.pre-v48.<timestamp>.bak.sha256
```

The snapshot is a standalone v47 SQLite file (`VACUUM INTO`); the sidecar's first field is the lowercase SHA-256 of the `.bak`. A brand-new empty directory creates the current schema directly and does not write this copy. There is no down-migration; roll back by restoring the whole pre-upgrade data directory.

## Schema v44 — dashboard operations

v44 additively creates `dashboard_operations` for V4 idempotent control-plane commits:

- `operation_id` — primary key
- `kind`
- `payload_digest` — hex HMAC-SHA256 over the semantic payload (`operationId`, `connection`, `authorization` including the secret, `targets`); CAS tokens are excluded
- `result_json` — secret-free stored result
- `created_at`

The digest is keyed by a per-database random 32-byte value in `settings` under `dashboard_operation_digest_key`, created lazily and never returned by any API. This key lives in the same SQLite file as the account Keys, so it shares the existing local-storage threat model; it prevents the stored digest from being an unkeyed hash of a secret, and it does not add protection against an attacker who holds the database file. Rows older than 30 days are pruned on insert. The migration is additive and does not create a pre-migration backup file (same as v43). Rollback remains the existing whole-directory restore.

## Schema v43 — preferred protocol CHECK and exclusive-radio repair

v43 rebuilds `provider_model_protocol_preferences` so `protocol` may be `chat_completions`, `responses`, or `messages`. It then deletes MiniMax/Kimi `force_off` override rows that sat next to a sibling `force_on` on Chat or Messages, restoring Auto so both available protocols can passthrough. Go `force_off` rows on unavailable siblings are left in place. Import of payload versions before V6 applies that exclusive-available repair in memory. V6 and later backups keep an explicit `force_off`. No extra snapshot file. Roll back by restoring the whole pre-upgrade data directory.

## Schema v42 — unified provider table

v42 replaces `dynamic_providers` and `dynamic_provider_models` with `providers` and `provider_models`, and adds four new columns to `providers`:

- `origin` — `builtin` | `preset` | `custom`. Builtin rows are display mirrors of the seven sealed Adapter seeds (OpenCode Go, Zen Free, Command Code GOAT, MiniMax CN, Kimi CN, Ollama Cloud, Custom API); CPA is the static external integration and is **not** seeded. Preset rows track preset-derived dynamic Providers; custom rows track manually authored dynamic Providers.
- `adapter_kind` — mirrors the sealed `ProviderAdapterKind` for builtin rows, `configurable_http` for every dynamic row.
- `offering` — `plan` | `api`. Builtin rows are seeded from `ocg_domain::provider::builtin_offering(provider_id)`; dynamic rows are derived from `preset_id` via `ocg_domain::provider::preset_offering(preset_id)` (custom-only rows default to `api`).
- `endpoint_per_account` — `0` for builtin rows except Custom API (which is `1`); always `0` for dynamic rows.

The v41 `provider_model_protocol_preferences` table is rebuilt without its `provider_id` CHECK now that `origin` is queryable; the `protocol ∈ ('chat_completions', 'messages')` CHECK is kept. The CHECK on `provider_id` was the only provider-id constraint that referenced origin, so no other table needed changes. Opening an already-migrated current-schema database does not recreate `dynamic_providers` or `dynamic_provider_models`. Schema v48 drops those leftover names only when they exist and are empty. A nonempty leftover on a v47 source refuses the upgrade and keeps schema 47; a current-schema database does not delete nonempty leftover rows.

v42 does **not** change the v35 Provider single-identity contract: builtin adapter routing, CPA integration, Custom API, and dynamic Configurable HTTP bindings all behave as before. Dynamic read paths add `origin IN ('preset', 'custom')` so builtin seeds never feed routing. V5 transfer payloads carry dynamic definitions only; builtin rows are derived from the registry, and the import derives `origin` / `offering` from `preset_id` to keep cross-version compatibility.

Before any v42 rewrite on a non-empty v41 database, the process writes a unique never-overwritten sibling snapshot:

```text
data.sqlite.pre-v42.<timestamp>.bak
data.sqlite.pre-v42.<timestamp>.bak.sha256
```

The snapshot is a standalone v41 SQLite file (`VACUUM INTO`, `quick_check` on both sides); the sidecar's first field is the lowercase SHA-256 of the `.bak`. A brand-new empty directory creates the current schema directly and does not write this copy. Verify the sidecar from the data directory before any restore:

```bash
sha256sum -c data.sqlite.pre-v42.<timestamp>.bak.sha256      # Linux
shasum -a 256 -c data.sqlite.pre-v42.<timestamp>.bak.sha256  # macOS
```

Downgrade is the existing whole-directory restore; there is no down-migration path.

## Schema v41 — selected model protocol

v41 adds provider_model_protocol_preferences for the sealed MiniMax CN and Kimi CN scopes. It stores the chosen Chat/Messages protocol independently of per-protocol enable overrides. Migration is additive and does not change existing routes, Keys or account dates. Preference and override writes share one transaction. Static-baseline reset clears the choice. V5 transfer contracts carry an optional preferences collection; older packages without it remain importable. Roll back by restoring the whole pre-upgrade data directory. (The v42 rebuild drops the table's `provider_id` CHECK so rows may also belong to builtin or dynamic ids; the `protocol` CHECK is preserved.)

## Schema v40 — model route overrides

Schema v40 adds nullable `provider_models.upstream_override` JSON containing an explicit model protocol and endpoint. Null inherits the unchanged Provider defaults; no account credentials or existing routes are rewritten. Provider replacement and node import persist the full model list atomically. V5 node packages carry the optional `upstreamOverride`; older packages without it retain inheritance. Older readers reject the unknown field rather than silently dropping model routing settings. Downgrade by restoring the pre-upgrade data-directory backup. (The v42 rename above also renames the table to `provider_models`; the column and its semantics are unchanged.)

## Schema v31 — per-model/per-protocol overrides

v31 creates the `provider_contract_model_protocol_overrides` table. It stores one row per contract scope × model × protocol, with `state` ∈ `force_on` / `force_off`; an absent row means "auto". The composite primary key is `(scope_kind, scope_id, model_id, protocol)`. The `provider_contract_scopes` switch columns remain in the database for backward compatibility until v48. Effective contract derivation reads `provider_contract_model_protocol_overrides`.

## Schema v32 — single-protocol Custom Endpoint

v32 replaces `account_custom_configs.base_url`, JSON `upstream_protocols`, and `auth_scheme` with `endpoint_url` and one `upstream_protocol`. Historical rows choose Chat Completions, then Responses, then Messages, append that protocol's standard inference suffix, and are disabled with verification reset to `pending`. Capabilities, evidence, and overrides for non-selected protocols are removed in the same transaction. Administrators must review and explicitly re-enable migrated Custom accounts.

## Schema v35 — Provider single identity

v35 removes the offering dimension. Provider and Plan are one product identity keyed by `provider_id`. Known v34 pairs map as `opencode/go`, `opencode-zen-free/anonymous-free`, `command-code/goat`, `minimax/cn`, `kimi/cn`, `custom/api`, and `cpa/local`. Unknown pairs and composite-key collisions fail closed before any write. The rebuild preserves accounts, ciphertext bytes, logs, pricing/catalog rows, contracts, Custom configs/capabilities, settings, and access keys. The same schema version also stores typed user-defined Providers in `dynamic_providers` and `dynamic_provider_models` (both renamed to `providers` / `provider_models` in v42). Node backups export payload V6 with `providerId` only, plus an optional/defaulted user-defined Provider definition collection. Payload V1–V3, and any version other than 4, 5, or 6 (including a future V7 package), are rejected with an explicit unsupported-version error. That schema's transfer contract exported payload V6; Current builds export payload V11. V4/V5 imports still rebuild identity satellites with the deterministic 1:1 mapper.

Before any destructive v35 rebuild on a non-empty v34 database, the process writes a unique never-overwritten sibling snapshot:

```text
data.sqlite.pre-v35.<timestamp>.bak
data.sqlite.pre-v35.<timestamp>.bak.sha256
```

The snapshot is a standalone v34 SQLite file (`VACUUM INTO`, `quick_check` on both sides); the sidecar's first field is the lowercase SHA-256 of the `.bak`. A brand-new empty directory creates the current schema directly and does not write this copy. Verify the sidecar from the data directory before any restore:

```bash
sha256sum -c data.sqlite.pre-v35.<timestamp>.bak.sha256      # Linux
shasum -a 256 -c data.sqlite.pre-v35.<timestamp>.bak.sha256  # macOS
```

## Schema v36 — Ollama Cloud usage state

v36 creates the `ollama_cloud_usage_state` table. One row per configured
account holds:

- `cookie_cipher` — the obfuscated browser-session Cookie for the
  `https://ollama.com/settings` usage scrape. It uses the same
  key-obfuscation facility as account keys and is explicitly not
  AEAD; it is never returned by any API and never enters an export payload.
- `status` — `unconfigured`, `ok`, `unauthorized`, or `failed`.
- `snapshot` — the sanitized JSON from the last successful scrape (5h/7d
  windows, per-model request counts, optional plan/balance). Written only on
  success; failures update status columns and never clear it.
- `last_error`, `last_success_at`, `last_attempt_at`, `next_eligible_at`,
  `failure_streak` — manual-refresh throttle (30 seconds) and last-attempt
  metadata.

The row is keyed by `account_id` with `ON DELETE CASCADE`, so account
deletion removes the usage state; clearing the Cookie deletes the row and
returns the capability to the unconfigured state. The migration is additive:
existing tables, rows, and routing facts stay as they are. It does not create
a new backup family. Rollback remains the existing whole-directory restore.

## Schema v38 — platform account ownership

v38 adds `platform_accounts` and `platform_links`. Existing account IDs, Keys, order, cooldowns, models, and logs are preserved. Parent origins are immutable; links reference existing Custom API accounts. Linking and endpoint materialization share one transaction. Parent deletion is restricted while linked Keys exist; child deletion removes its link.

New node exports use payload V6 and omit platform management credentials, platform observer secrets, and observation snapshots. V4 and V5 imports remain supported. Imported associations are unverified, and a parent ID with conflicting kind/origin rejects the complete import transaction. Rollback uses the existing whole-directory backup procedure; v38 adds no separate backup system.

## Schema v39 — preset provenance

v39 adds a nullable `preset_id` to user-defined Providers. It preserves the selected configuration template when endpoints are resource-specific or names are edited; it never controls routing or platform-instance identity. Existing rows remain unclassified. V5 transfer payloads carry this optional field; older payloads without it remain accepted.

## Schema v37 — Ollama Cloud billing

v37 drops `ollama_cloud_usage_state` (the unreleased Cookie scrape, including
obfuscated Cookie ciphertext and last-good snapshots) and creates
`ollama_cloud_billing`:

- `account_id` — primary key, `ON DELETE CASCADE`
- `billing_tier` — `pro` / `max` / `team`

Absence of a row is unconfigured (`null` on the account field). Existing
Ollama accounts migrate with no row, stay routeable, and keep their Keys and
logs. New creates require a paid tier and `accounts.purchase_date`. Node
export/import carries the billing tier. The migration does not create a new
backup family. Rollback remains the existing whole-directory restore.

## Schema v33 — Custom upstream model identity

v33 adds the non-null `account_model_capabilities.upstream_model` column.
Existing rows are backfilled from `model_id`, preserving their former
public-name = upstream-ID behavior exactly. New Custom mappings may retain a
distinct public model name and exact upstream model ID; no suffix normalization
or generated Alias is applied by this migration.

Before any v27 write, an existing (non-empty) library gets a unique, never-overwritten sibling snapshot:

```text
data.sqlite.pre-v3.<timestamp>.bak
data.sqlite.pre-v3.<timestamp>.bak.sha256
```

The snapshot is a standalone v26 SQLite file (`VACUUM INTO`, `quick_check` on both sides); the sidecar's first field is the lowercase SHA-256 of the `.bak`. A brand-new empty directory creates the current schema directly and does not write this copy. The snapshot is a rollback point, not a substitute for the whole-directory backup. Verify the sidecar from the data directory before any restore:

```bash
sha256sum -c data.sqlite.pre-v3.<timestamp>.bak.sha256      # Linux
shasum -a 256 -c data.sqlite.pre-v3.<timestamp>.bak.sha256  # macOS
```

On Windows, compare `Get-FileHash -Algorithm SHA256` with the first field of the sidecar. A hash mismatch means do not restore that file.

## Rollback and failed opens

**There is no down-migration.** Rollback is an offline, exact-file restore:

1. Stop every process that has the directory open.
2. Verify the sidecar hash as above; stop if it does not match.
3. Copy the verified `.bak` over `data.sqlite`, and remove the stale `data.sqlite-wal` / `data.sqlite-shm` left behind by the previous live file.
4. Start a v26-capable binary with the same cipher identity, or retry the v27 upgrade on that restored v26 file. Restoring after a successful v27 open discards every write made since the snapshot.

A failed v27 transaction rolls back: the live file must remain schema 26 with `sub_gateway_keys` intact. Leave any pre-v3 files in place; a later successful open creates another unique name instead of overwriting. A wrong or missing Host cipher fails closed; never rewrite `key_cipher` / `password_cipher`. `ocg-manager-cli status` opens the database and will attempt v27, so it migrates rather than inspecting schema read-only.

---
[Maintainer guide index](../MAINTAINER.md) · [简体中文](storage-migration.zh-CN.md) · [Docs index](../README.md)
