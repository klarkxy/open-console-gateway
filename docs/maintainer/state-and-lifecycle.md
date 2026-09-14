[简体中文](state-and-lifecycle.zh-CN.md)

# State, Credentials, And Lifecycle

## State, credentials, and settings

`CoreStateInner` (`state.rs`) is shared by gateway, dashboard, and CLI.

Lock order: (1) `settings_update`, (2) `db`, (3) `config`, (4)
`http_client`, (5) `gateway`, (6) `pricing`, (7) `zen_free_models`,
(8) `provider_contracts`, (9) `routing`, (10) `credential_snapshot`.
Never acquire in reverse. Do not hold the routing lock across DB or
network I/O. Async gates: `settings_host_effects` (persist → listener
rebind → compensation) is acquired before `gateway_lifecycle` when a
settings write also rebinds. Never hold a `parking_lot` lock across those
awaits.

From schema v27 the authoritative table is `access_keys`. Two credential
tiers share that table (current schema v37) and one auth snapshot:

- Primary key: fixed id `00000000-0000-0000-0000-000000000001`, display
  name `"Primary"`. Always enabled, never deleted. Public `AppConfig` and
  dashboard APIs still expose `gateway_key`; sanitized config JSON stores
  `gateway_key` as `""` after v27.
- Sub keys: non-primary rows, active ceiling 64, soft-delete keeps
  identity/name and clears the value. Lifecycle only through
  `/dashboard/api/v3/keys*`. CLI has no sub-key commands.

Primary/sub values are mutually exclusive
(`gateway_keys::ensure_primary_value_allowed`) on dashboard, settings, and
sub-key enable paths.

`AppConfig` uses serde defaults for backward-compatible loading. Unknown
historical fields are ignored on load and omitted by the next canonical
settings write. Downstream client root URL priority:
non-empty `OCG_CLIENT_ROOT_URL` (read-only, never written back) > SQLite
manual value > frontend derivation from production origin / dev Gateway
port.

Dashboard authentication is skipped for **direct** requests when the
gateway binds loopback. Requests carrying standard reverse-proxy
forwarding headers still require login. Non-loopback binds use a single
administrator (Argon2 hash in SQLite, HttpOnly session cookie). Docker may
bootstrap the first administrator with **both** `OCG_ADMIN_USERNAME` and
`OCG_ADMIN_PASSWORD`; setting only one fails startup; otherwise the first
registration wins.

Settings fetches GitHub Release metadata via `GET /dashboard/api/v3/settings/check-update`.
Installed desktop runtimes with updater support can download, verify, and install
signed updates; development builds, CLI, and Docker only receive metadata and
release links. The outbound request is triggered by the user.

## Account lifecycle and browser runtime

Schema v16 added `account_type` (`key | managed`) and `setup_step`
(`google_account → opencode_registration → payment → key_verification → ready`).
Existing rows migrate to `key + ready`. A managed draft is persisted
immediately with an empty key and `enabled=false`; selector, enable, and
the request path all require both `ready` and a non-empty key.
`google_account` is labeled **sign-in identity** in the UI and is
skippable.

`AppConfig::default()` seeds `opencode_invite_url` with
`DEFAULT_OPENCODE_INVITE_URL` (demo). Normalized values must be a
credential-free HTTPS URL up to 2,048 characters whose host is exactly
`opencode.ai` or `console.opencode.ai`. The dashboard edits that value on
the OpenCode Go provider **Other** tab. Creating a managed draft can edit the invite
URL and write it back to `opencode_invite_url` when it differs. Signup,
registration, and payment remain manual in the isolated browser; the user
copies the key back. Never add CDP autofill or automated payment clicks.

Managed setup may move **forward exactly one step** or **rewind to any
earlier unfinished step**. The ordinary setup PATCH writes those step
changes; a separate key-verification request writes `ready`. A real key
probe returning `2xx` transitions to `ready + enabled`; `429` also proves
validity and records cooldown. Any other HTTP response—including
redirects, `4xx` other than `429`, and `5xx`—plus network or timeout
errors remains at `key_verification`.

### Managed account setup lifecycle

[![Managed account setup lifecycle](../diagrams/managed-account-lifecycle.visual-check.1440x900.light.png)](https://klarkxy.github.io/open-console-gateway/diagrams/managed-account-lifecycle/)

[Open the interactive diagram on GitHub Pages](https://klarkxy.github.io/open-console-gateway/diagrams/managed-account-lifecycle/).

The ordinary setup PATCH may advance exactly one step or return to an earlier
unfinished step; it never writes `ready`. A separate key-verification request
moves the account to `ready + enabled` on `2xx` or `429`. Invalid-key `4xx`
responses and redirects keep the draft pending and return `400`; network,
timeout, and `5xx` failures keep it pending and return `502`, so the user can
retry or rewind.

Official Go usage (`go_usage.rs`, `https://opencode.ai/zen/go/v1/usage`) is
the calibration baseline; `usage_sync.rs` coordinates it. Manual
`POST /dashboard/api/v3/accounts/{id}/usage/refresh` and the background
reconciler share one fetch + key-CAS + three-window calibration path.

Ready+enabled accounts reconcile about hourly when they had local activity
in the last 24h, otherwise about daily. Disabled, non-ready, and empty-key
accounts are excluded. Startup avoids stampedes: global concurrency 1,
pacing, bounded jitter, and injectable clock/jitter/fetch seams.

Manual refresh is throttled to one attempt per account per 15s, dedupes
in-flight attempts, and honors Retry-After / `nextAllowedAt`. Local max Go
usage ≥80% can expedite reconciliation at most once per 15 minutes.

A real inference `429` keeps existing cooldown/selector writes and schedules
an official sync ~1–2 minutes later, never inline. Official failures or
`status=rate-limited` never write inference cooldown. After success, schedule
around the earliest `resetsAt` with bounded jitter while respecting the
active/inactive cadence. Failure backoff is 5m → 15m → 1h → 6h; never erase
last success or the previous baseline.

Sync metadata lives in `provider_usage_sync_state`; v27 drops the leftover
`accounts.usage_sync_*` columns. The public Go docs have not listed this path.

Zen Free is database-owned: it can be enabled, disabled, and reordered,
but cannot be created or deleted through generic account APIs. Command Code
accounts are routable when enabled, ready, and keyed; the Provider matrix owns
their model supply, with GOAT preset rows on and additional rows off by default.
Custom is catalog-routable after declaration; verification is optional.

Browser: `GET /dashboard/api/v3/browser/capabilities`,
`POST /accounts/{id}/browser`, `DELETE /accounts/{id}/browser-profile`,
and `/browser/sessions/{token}/ws`. Targets include Google signup/login,
GitHub signup/login, the configured invite, and the OpenCode console
(`https://opencode.ai/auth`). The worker host allowlist includes
`accounts.google.com`, `github.com`, `opencode.ai`,
`console.opencode.ai`, and `auth.opencode.ai`. Remote tokens are
memory-only, administrator-session-bound, and Origin-checked; they expire
after 30 minutes idle or four hours total.

Desktop native browser hooks are registered by `src-tauri/src/host/` into
`CoreState`. Vue still calls HTTP. Windows discovers Edge then Chrome;
macOS checks Chrome, Edge, and Chromium; Linux searches `PATH` for
Chrome/Chromium/Edge. The external browser uses
`browser-profiles/<account_id>`, `--no-first-run`,
`--no-default-browser-check`, and a new window. Never add CDP,
automation, `--no-sandbox`, or disabled web security.

`crates/ocg-browser-worker` keeps one Chromium per node. An account switch
sends SIGTERM to the current process group and waits for profile flush,
forcing termination only after the bounded timeout. The sidecar runs as
UID/GID 10001 with a read-only root and no capabilities; a shared runtime
volume holds a random control token. Chromium must create its own
user/PID/network namespaces and renderer seccomp sandbox, so the browser
service uses `seccomp=unconfined` and cannot use `no-new-privileges`. It
still does not mount SQLite or publish a host port. The project-scoped
browser bridge is not Docker `internal`, because Chromium needs outbound
HTTPS to Google/OpenCode.

Profile deletion stops the browser, validates account IDs against path
traversal, and atomically renames both new and legacy profiles into
staging. Purge only after the database commit; restore staging on failure.
Reset keeps a completed account's key; a pending managed account also
returns to `google_account`. Delete confirmations must state that cookies
and profile are removed.

## Persistence

`crates/ocg-core/src/db.rs` defines the SQLite schema, migrations, and
queries. Current schema is **v45**. Version history lives in
[storage-migration.md](storage-migration.md). `provider_contracts.rs` owns
provider contract scopes, per-model/per-protocol overrides, effective
contract derivation, and model-protocol evidence. `models.rs` defines
shared serde types and `AppConfig`. Local Key storage is
`ocg-infra::crypto` (facade `ocg_core::crypto`): AES-256-GCM `v2:`
ciphertext, not a KMS. Legacy XOR still decrypts; a correct
`open_with_cipher` rewrites remaining account `key_cipher` /
`password_cipher` rows to v2 in one transaction. Windows desktop uses
`MachineBoundCipher`; CLI/Docker use `StaticKeyCipher` from
`OCG_MANAGER_ENCRYPTION_KEY` or `<data-dir>/.encryption-key`. Production
hosts must call `Database::open_with_cipher` so ciphertext probes use the
already resolved cipher. A schema newer than this build supports fails
closed.

Historical versions still matter on upgrade:

- v16: managed setup columns.
- v21: usage-sync metadata (later moved off `accounts` in v27).
- v22: immutable provider/offering bindings, provider pricing/usage,
  quota windows, provider-aware forward logs.
- v23: Plan verification, Alias / upstream log identity, optional native
  cost, Custom config tables.
- v24: actual proxy route leg on forward logs (`auto` / `proxy` /
  `direct`; historical empty string = unrecorded).
- v25: `provider_model_catalogs` (last successful Zen Free snapshot).
- v26: `provider_contract_scopes` and `provider_contract_model_protocols`.
  Additive.
- **v27:** copy primary `gateway_key` + `sub_gateway_keys` into
  `access_keys`; drop `sub_gateway_keys`; drop leftover
  `accounts.usage_sync_*`. After the database is at canonical v26, an
  existing (non-empty) library gets a unique sibling
  `data.sqlite.pre-v3.<UTC>.bak` plus a SHA-256 sidecar **before any v27
  write**. A brand-new empty directory creates v27 directly and does not
  write that copy. Operator recovery:
  [storage-migration.md](storage-migration.md).
- **v29:** removes SCNet Token Plans from the catalog and deletes any
  existing SCNet account rows during migration.
- **v30:** backfills `account_custom_configs.upstream_protocol` into a
  JSON `upstream_protocols` set (1–3 of chat_completions / responses /
  messages); Custom config/capability edits keep the account enabled but
  reset `verification_status` to `pending`.
- **v31:** adds `provider_contract_model_protocol_overrides` for
  per-model/per-protocol enablement and stops reading the deprecated
  `provider_contract_scopes` switch columns.
- **v32:** replaces Custom `base_url`, protocol-set JSON, and configurable
  auth with one complete `endpoint_url` and one `upstream_protocol`. Historical
  Custom rows choose Chat Completions, then Responses, then Messages, and are
  disabled/pending for administrator review while non-selected protocol state
  is removed in the same migration.
- **v33:** adds non-null `account_model_capabilities.upstream_model`,
  backfilled from `model_id`.
- **v34:** adds the singleton CPA integration configuration.
- **v35:** collapses Provider and Plan identity to `provider_id` after a
  fail-closed preflight and rebuild; it also persists typed user-defined
  Provider definitions. See [Storage and migrations](storage-migration.md)
  for the pre-v35 backup and rollback procedure.
- **v36:** additive `ollama_cloud_usage_state` for the unreleased Cookie-usage
  scrape.
- **v37:** drops `ollama_cloud_usage_state` without touching account Keys or
  logs, and creates `ollama_cloud_billing`.

GUI data directory: Windows `%USERPROFILE%\.ocg-mgr` or macOS/Linux
`~/.ocg-mgr`. CLI default: `~/.ocg-mgr-cli`. Docker stores SQLite, keys,
and `.encryption-key` in `ocg-data`; long-lived cookies and browser state
live in `ocg-browser-profiles`. Stop and back up those two sensitive
volumes together. `ocg-browser-runtime` contains only the runtime control
token and should not be backed up. Open Console Gateway does not encrypt browser
profiles.

Forward-log inserts go through `ocg-infra::sqlite_logs` (one explicit
statement per helper). Callers own timestamps, diagnostics, cost policy,
redaction, and transactions.

## Per-node boundaries

Each node owns its account data and is managed through its own dashboard.

## Lifecycle Classes

Keep these four classes separate. Do not cancel one from another.

| Class | Start | Stop | Notes |
| --- | --- | --- | --- |
| **Gateway listener** (`GatewayLifecycle`) | `start_gateway` / `bind` | `stop` (signal-only) or `stop_and_wait` (CLI) | TCP bind, dashboard trust, forward-log backfill, HTTP server. Rebind is slot-aware (same-port stop-then-bind, new-port bind-first). Does not start or cancel process-level workers. |
| **Control-plane workers** (`ControlPlaneWorkers`) | `ensure_started` from `start_gateway` (once per `CoreState`) | none — exits when the owning `CoreState` is dropped | Official usage reconciler. No public cancel API. Listener stop must not kill it. |
| **Desktop capabilities** | Tauri setup: auto-start (Windows x64 / macOS / Linux x64 release/installed), Dock (macOS), updater starter | process exit | Host-registered capabilities. CLI/Docker leave hooks unset. `auto_start` and `show_dock_icon` stay capability-gated on the HTTP settings form. |
| **Browser runtime** | Native hooks on desktop; remote worker in Docker | account switch / profile reset / process exit | Native Browser vs sidecar are different hosts of the same `BrowserRuntime` slot. |

Tauri `src/lib.rs`: start uses `start_gateway` (listener + usage workers);
exit uses `host::gateway::stop_listener` (listener only). Settings port
changes rebind through `GatewayLifecycle` / `settings_host_effects` with
config-fingerprint compensation; concurrent failed port writes must not
clobber a successful timeout write.

Updater is configured as a `CoreState` starter.
`src-tauri/capabilities/default.json` has no updater permission.
Updater outbound follows the process-wide **default-leg** proxy policy
(List mode included).
---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](state-and-lifecycle.zh-CN.md) · [Docs index](../README.md)
