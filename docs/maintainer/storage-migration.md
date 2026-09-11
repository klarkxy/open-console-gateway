[简体中文](storage-migration.zh-CN.md)

# Storage And Migrations

Operator contract for upgrades, backups, and rollback. Schema details are in [Persistence](state-and-lifecycle.md#persistence).

## Data directories and cipher identity

Every database open uses the Host-resolved cipher (`Database::open_with_cipher` on CLI, desktop, and Docker). Stored account ciphertext is probed before migration and decryption errors fail closed. Key storage uses unauthenticated obfuscation: a successful UTF-8 decode alone cannot authenticate cipher identity. Retain the original cipher; rewriting ciphertext does not repair a mismatch.

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

`CURRENT_SCHEMA_VERSION = 45` (`crates/ocg-core/src/db.rs`). Opening a historical database first migrates canonically to v26, then the v27 rewrite copies the primary Key and every `sub_gateway_keys` row into one `access_keys` table (live primary id `00000000-0000-0000-0000-000000000001`), drops `sub_gateway_keys`, and drops the five legacy `accounts.usage_sync_*` columns (usage-sync metadata lives in `provider_usage_sync_state`). v33 adds the exact Custom upstream model identity; v34 adds the singleton CPA configuration table without importing or exporting CPA state. v35 collapses Provider/Plan identity to `provider_id` only: it preflights every known v34 provider/offering pair, refuses unknown pairs and lossy composite-key collisions before mutation, then rebuilds affected tables so offering columns are absent. v36 additively created `ollama_cloud_usage_state` for the unreleased Cookie-usage scrape. v37 drops that table without touching account Keys or logs, and creates `ollama_cloud_billing`. v42 unifies the typed user-defined Provider table with a sealed-Adapter seed catalog by renaming `dynamic_providers` / `dynamic_provider_models` to `providers` / `provider_models`, adding `origin` (`builtin` | `preset` | `custom`), `adapter_kind`, `offering` (`plan` | `api`), and `endpoint_per_account` columns, seeding the seven sealed builtin adapters (OpenCode Go, Zen Free, Command Code GOAT, MiniMax CN, Kimi CN, Ollama Cloud, Custom API — but not CPA, the static external integration) as `builtin` rows whose attribute columns are display mirrors, and filtering dynamic read paths on `origin`. The v41 `provider_model_protocol_preferences` `provider_id` CHECK is dropped (the protocol CHECK on `(chat_completions | messages)` is kept until v43). Account `key_cipher` / `password_cipher` bytes are validated with the Host cipher and never re-encrypted. v44 additively creates `dashboard_operations` for V4 idempotent commits, with no pre-migration backup file (like v43). v45 additively creates identity/credential/binding satellite tables and `accounts.identity_id`, with no pre-migration backup file (like v43/v44).

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
- `quota_pool_members` — `pool_id`, `account_id` (one member per backfilled identity; a second credential on that identity joins the same pool)

Satellite rows are deleted explicitly in the same transaction (the DDL declares `ON DELETE CASCADE` but the process does not enable the foreign-key pragma). On open, a v45 consistency check repairs missing satellite rows with the idempotent backfill and fails closed if inconsistencies remain.

Migration rules: each existing account becomes exactly one identity (label = account name, confidence `opaque`), one credential (`version` 1), and one binding to the account's connection (builtin provider / dynamic provider / the Custom account's own connection) with `model_scope=all` and `enabled` equal to the account's enabled flag. Routing rank is the existing `accounts.sort_order` (read from `accounts`, not duplicated). `legacy_identity_map` records account → identity / credential / binding. Managed accounts that are not yet `ready` get one `onboarding_tasks` row `in_progress` at the current step; ready managed accounts get no fabricated history. Only accounts of sealed built-in Providers that already publish purchase/expiry dates get a `subscription_records` row with source `legacy_manual`. User-defined and Custom API accounts get none: dates stay unknown, and nothing is priced at zero (D07). Platform links: the linked Key's identity becomes `declared` with `authority_site` = parent `base_url`; each platform parent gets its own identity with a `platform_observer` credential (the management credential, never used for inference). Parents and linked Keys are never merged; the relation stays declared, not verified (D04). Cooldowns are not moved: they are projected as quota windows (generic / 5h / week / month → subject `credential`; free → subject `egress` `free_channel`, declared, authoritative) and preserve the exact stored instants. An unknown metric is `null`, never zero. Migration creates one quota pool per identity (`subject` credential / identity id, `relation_confidence` unknown, `policy_mode` authoritative_limit). It never writes `verified`. A later second credential on that identity joins the same pool and marks the relation `declared`. Routing honors stored `model_scope` and binding `enabled`.

Every account insert (V3 create, managed create, user-defined Provider first Key, V4 onboarding commit, node import, V4 identity credential create) writes the satellite rows in the same transaction via the single mapper shared with this migration. Platform link / unlink updates the linked identity's confidence and site in the same transaction.

Rotate, binding edits, and second-credential writes are V4 CAS paths on top of these satellites. Transfer payloads stay on V5. The Accounts page UI is unchanged in this slice.

## Schema v44 — dashboard operations

v44 additively creates `dashboard_operations` for V4 idempotent control-plane commits:

- `operation_id` — primary key
- `kind`
- `payload_digest` — hex HMAC-SHA256 over the semantic payload (`operationId`, `connection`, `authorization` including the secret, `targets`); CAS tokens are excluded
- `result_json` — secret-free stored result
- `created_at`

The digest is keyed by a per-database random 32-byte value in `settings` under `dashboard_operation_digest_key`, created lazily and never returned by any API. This key lives in the same SQLite file as the obfuscated account Keys, so it shares the existing local-storage threat model; it prevents the stored digest from being an unkeyed hash of a secret, and it does not add protection against an attacker who holds the database file. Rows older than 30 days are pruned on insert. The migration is additive and does not create a pre-migration backup file (same as v43). Rollback remains the existing whole-directory restore.

## Schema v43 — preferred protocol CHECK and exclusive-radio repair

v43 rebuilds `provider_model_protocol_preferences` so `protocol` may be `chat_completions`, `responses`, or `messages`. It then deletes MiniMax/Kimi `force_off` override rows that sat next to a sibling `force_on` on Chat or Messages, restoring Auto so both available protocols can passthrough. Go `force_off` rows on unavailable siblings are left in place. V5 import applies the same exclusive-available repair in memory. No extra snapshot file. Roll back by restoring the whole pre-upgrade data directory.

## Schema v42 — unified provider table

v42 replaces `dynamic_providers` and `dynamic_provider_models` with `providers` and `provider_models`, and adds four new columns to `providers`:

- `origin` — `builtin` | `preset` | `custom`. Builtin rows are display mirrors of the seven sealed Adapter seeds (OpenCode Go, Zen Free, Command Code GOAT, MiniMax CN, Kimi CN, Ollama Cloud, Custom API); CPA is the static external integration and is **not** seeded. Preset rows track preset-derived dynamic Providers; custom rows track manually authored dynamic Providers.
- `adapter_kind` — mirrors the sealed `ProviderAdapterKind` for builtin rows, `configurable_http` for every dynamic row.
- `offering` — `plan` | `api`. Builtin rows are seeded from `ocg_domain::provider::builtin_offering(provider_id)`; dynamic rows are derived from `preset_id` via `ocg_domain::provider::preset_offering(preset_id)` (custom-only rows default to `api`).
- `endpoint_per_account` — `0` for builtin rows except Custom API (which is `1`); always `0` for dynamic rows.

The v41 `provider_model_protocol_preferences` table is rebuilt without its `provider_id` CHECK now that `origin` is queryable; the `protocol ∈ ('chat_completions', 'messages')` CHECK is kept. The CHECK on `provider_id` was the only provider-id constraint that referenced origin, so no other table needed changes.

v42 does **not** change the v35 Provider single-identity contract: builtin adapter routing, CPA integration, Custom API, and dynamic Configurable HTTP bindings all behave as before. Dynamic read paths add `origin IN ('preset', 'custom')` so builtin seeds never feed routing. V5 transfer payloads still carry dynamic definitions only; builtin rows are derived from the registry, and the import derives `origin` / `offering` from `preset_id` to keep cross-version compatibility.

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

v31 creates the `provider_contract_model_protocol_overrides` table. It stores one row per contract scope × model × protocol, with `state` ∈ `force_on` / `force_off`; an absent row means "auto". The composite primary key is `(scope_kind, scope_id, model_id, protocol)`. The `provider_contract_scopes` switch columns remain in the database for backward compatibility. Effective contract derivation reads `provider_contract_model_protocol_overrides`.

## Schema v32 — single-protocol Custom Endpoint

v32 replaces `account_custom_configs.base_url`, JSON `upstream_protocols`, and `auth_scheme` with `endpoint_url` and one `upstream_protocol`. Historical rows choose Chat Completions, then Responses, then Messages, append that protocol's standard inference suffix, and are disabled with verification reset to `pending`. Capabilities, evidence, and overrides for non-selected protocols are removed in the same transaction. Administrators must review and explicitly re-enable migrated Custom accounts.

## Schema v35 — Provider single identity

v35 removes the offering dimension. Provider and Plan are one product identity keyed by `provider_id`. Known v34 pairs map as `opencode/go`, `opencode-zen-free/anonymous-free`, `command-code/goat`, `minimax/cn`, `kimi/cn`, `custom/api`, and `cpa/local`. Unknown pairs and composite-key collisions fail closed before any write. The rebuild preserves accounts, ciphertext bytes, logs, pricing/catalog rows, contracts, Custom configs/capabilities, settings, and access keys. The same schema version also stores typed user-defined Providers in `dynamic_providers` and `dynamic_provider_models` (both renamed to `providers` / `provider_models` in v42). Node backups export payload V4 with `providerId` only, plus an optional/defaulted user-defined Provider definition collection. Payload V1–V3, and any other version except 4 or 5 (including a future V6 package), are rejected with an explicit unsupported-version error. This binary still exports payload V5.

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

New node exports use payload V5 and omit platform management credentials and observation snapshots. V4 imports remain supported. Imported associations are unverified, and a parent ID with conflicting kind/origin rejects the complete import transaction. Rollback uses the existing whole-directory backup procedure; v38 adds no separate backup system.

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

## Schema v39 — preset provenance

v39 adds a nullable `preset_id` to user-defined Providers. It preserves the selected configuration template when endpoints are resource-specific or names are edited; it never controls routing or platform-instance identity. Existing rows remain unclassified. V5 transfer payloads carry this optional field; older payloads without it remain accepted.
