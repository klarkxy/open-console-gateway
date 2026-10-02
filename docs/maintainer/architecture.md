[简体中文](architecture.zh-CN.md)

# Architecture

This page defines stable dependency and ownership boundaries. Runtime edge
cases, schema history, route inventories, and release procedures live in
their own chapters.

## Dependency graph

```text
ocg-gateway -> ocg-domain
ocg-core    -> ocg-domain + ocg-gateway + ocg-infra
ocg-cli     -> ocg-core

ocg-browser-worker   separate process; no internal ocg-* dependency
```

This branch contains the Rust workspace and headless CLI only. The Vue
workspace and Tauri desktop crate are not present; a future client remains an
HTTP consumer of Dashboard V4 and does not add a WebView mutation path.

The **Adapter Registry** is static and sealed. Runtime Provider definitions
are typed data bound to Configurable HTTP.

| Crate | Owns | Must not own |
| --- | --- | --- |
| `ocg-domain` | IDs, `BUILTIN_PROVIDERS`, `ProviderAdapterKind`, protocol tables, typed dynamic definitions | DB, `CoreState`, HTTP clients, filesystem, clocks |
| `ocg-gateway` | Alias resolution, `AttemptSpec`, classification, selector state machines, no-I/O JSON conversion | DB, `CoreState`, plaintext credentials, outbound HTTP |
| `ocg-infra` | Key obfuscation, proxy-aware HTTP helpers, inference transport, SQLite log statements | Product catalogs, Dashboard DTOs, routing policy |
| `ocg-core` | SQLite, `CoreState`, Dashboard control plane, adapters, gateway execution, usage sync, Host composition | Runtime plugin loading; adapter-owned DB or HTTP clients |
| `ocg-cli` | Persistent `serve`, non-mutating `api`, offline `schema` | A second control plane, a private mutation path, or Dashboard route definitions |

`ocg-domain::credential` holds the identity/credential/binding vocabulary and the single legacy mapper.

Compatibility facades live in `ocg-core`; new no-I/O catalog, selector,
alias, and conversion behavior belongs in the lower crates.

## HTTP composition

`crates/ocg-core/src/host_router.rs` is the composition root for one listener:

```text
127.0.0.1:9042
  inference routes
    OpenAI Chat / Responses / Anthropic Messages
    Gemini generateContent / streamGenerateContent
    local GET /v1/models
  /dashboard/api/v3       410 tombstone
  /dashboard/api/v4       live Dashboard control plane
  /dashboard/api          preserved auth + browser WS; other REST -> 410 tombstone
  /dashboard/             static assets when a host supplies them
```

`ocg-manager-cli serve` is the persistent native host. It installs
`console_router` on that listener: inference, the live V4
control plane, preserved auth, the preserved browser socket, the V3 tombstone,
and static `dist/` when that directory is present. `api` is the non-mutating
HTTP client of the listener. It does not open SQLite, including when
`--data-dir` is set. `schema v3|v4` prints offline JSON help and does not
contact `serve`.

Shared core services stay behind the HTTP handlers. `serve` registers the native
browser launcher and stopper and owned CPA restore and shutdown. The default
`dsh-local-host` feature also registers BYOK and DSH hosts. A build
with `--no-default-features` still mounts BYOK and DSH routes and answers
with their unsupported-runtime states. Tray, Dock, and signed desktop update stay
unregistered.

`api` sends an ordinary HTTP request and reads the response. It does not
upgrade the browser socket, so there is no `api GET .../ws` command. The
preserved path `/dashboard/api/browser/sessions/{token}/ws` is outside the
client allowlist. The server still mounts the socket. A remote viewer is later
work, or an external WebSocket client pointed at the server.

## Gateway request path

Inference is implemented under `crates/ocg-core/src/gateway/`:

1. `handler.rs` assigns the request id, authenticates a client Key, parses the
   client protocol, and resolves model identity.
2. `GatewayExecutor` captures one request-entry snapshot for pricing, proxy
   routes, contracts, and Alias resolution. Fallback iterations re-read live
   account state, eligible Custom runtimes, and Zen Free cooldown. Protocol
   selection uses that saved contract.
3. Candidate materialization applies adapter ceilings and effective
   model/protocol state before the no-I/O selector chooses a card.
4. `provider_adapter.rs` exhaustively maps the sealed `ProviderAdapterKind` to
   a data-only `AttemptSpec`. It does not decrypt Keys, open SQLite, or build an
   HTTP client.
5. The Host resolves the selected credential. `forward_once` performs exactly
   one upstream `.send()`; retry and fallback policy stay in the outer loop.
6. Classification decides same-account retry, account fallback, cooldown, or
   terminal return. The Host then converts the response and writes logs
   (`requested_model`, `resolved_alias`, `upstream_model`).

Unknown or ambiguous model identity fails before outbound HTTP. Timeouts,
stream interruptions, and other outcomes that may have reached the upstream
are not automatically replayed. Full status-specific behavior lives in
[Runtime invariants](runtime-invariants.md).

## Adapter and Provider boundary

`ocg-domain::ProviderRegistry` contains the code-owned built-in Provider rows
and exhaustive adapter kinds. Unknown `provider_id` values fail closed unless
they match a persisted typed Provider definition, which always selects the
existing Configurable HTTP adapter.

Legacy Custom API rows are distinct configurable `http` destinations using
the same sealed adapter kind. A connection may hold multiple credentials while
preserving public-name-only resolution. CPA is a separate static external integration.

Provider-owned catalogs and contracts are resolved before account credentials
are used. Saved discovery rows may activate code-owned Alias mappings or remain
exact raw pins.

## Control plane

Operators reach remounted operational handlers and native V4 routes at
`/dashboard/api/v4` through `api`. The removed Vue client is not part of this
branch. `/dashboard/api/v3` is a 410 tombstone. Live dashboard JSON is V4 only.
CAS-protected mutations carry `expectedRevision` and `processGeneration`.
`--cas-current` copies that pair from public `GET /dashboard/api/v4/auth/status`
only when the body omits them, and it leaves a supplied value in place. It does
not fill pricing fields. A multiplier write carries `expectedPricingRevision`
from that provider's pricing snapshot: `pricingRevision` on `opencode`,
`providerPricingRevision` on `command-code`. A provider pricing refresh carries
`expectedProviderPricingRevision` from the provider pricing read. Operational
reads and diagnostics that do not mutate state skip CAS. `api` does not replay
a mutation after 409, 429, a timeout, or a port rebind.

Shared services own persistence and revision bumps for the HTTP handlers. The
CLI does not keep a second copy of that business logic.

The listener requires a router factory installed by its host. `serve` installs
`console_router`. Tests may install the same library factory through
`start_gateway_on`. The listener does not choose a composition itself. `api`
and `schema` do not install a factory.

`account_control` owns credential rotation, HTTP destination replacement and
deletion, routing-card layout, built-in catalog addition and editing,
public-model publication, and model-metadata declaration. Configurable HTTP
catalog refresh keeps its prepare, lock-free discovery, and commit stages in
its adapter. These operations remain owned by their existing complete modules:
onboarding, billing, identity credential creation, quota retry, bindings,
temporary policy, CPA selection, platform import, application installation,
settings rebind, official catalog/price/usage refresh, account transfer, and
legacy provider/account compatibility writes. The CLI reaches those modules by
HTTP. `api` does not open the database and does not call the legacy `key` verbs.

The settings-specific persist/rebind/compensation sequence is shown in
[Dashboard API](dashboard-api.md#settings-mutation-workflow). Account setup
states are shown in
[State and lifecycle](state-and-lifecycle.md#managed-account-setup-lifecycle).

## Detail ownership

| Detail | Authoritative chapter |
| --- | --- |
| Alias, selector, protocol, retry, cooldown, model-list behavior | [Runtime invariants](runtime-invariants.md) |
| Dashboard V4 DTOs, remounted handlers, CAS, V2/V3 tombstones | [Dashboard API](dashboard-api.md) |
| Locks, account setup, browser workers, process lifecycles | [State and lifecycle](state-and-lifecycle.md) |
| Tables, migrations, backups, rollback | [Storage and migrations](storage-migration.md) |
| Complete HTTP route inventory | [HTTP routes](http-routes.md) |
| Workspace layout and development commands | [Layout](layout.md), [Development](development.md) |
| Extension boundaries | [Extending Open Console Gateway](extending.md) |

---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](architecture.zh-CN.md) · [Docs index](../README.md)
