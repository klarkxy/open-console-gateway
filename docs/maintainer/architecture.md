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
src-tauri   -> ocg-core

ocg-browser-worker   separate process; no internal ocg-* dependency
Vue SPA              static assets; HTTP Dashboard V3 + V4 only
```

The **Adapter Registry** is static and sealed. Runtime Provider definitions
are typed data bound to Configurable HTTP.

| Crate | Owns | Must not own |
| --- | --- | --- |
| `ocg-domain` | IDs, `BUILTIN_PROVIDERS`, `ProviderAdapterKind`, protocol tables, typed dynamic definitions | DB, `CoreState`, HTTP clients, filesystem, clocks |
| `ocg-gateway` | Alias resolution, `AttemptSpec`, classification, selector state machines, no-I/O JSON conversion | DB, `CoreState`, plaintext credentials, outbound HTTP |
| `ocg-infra` | Key obfuscation, proxy-aware HTTP helpers, inference transport, SQLite log statements | Product catalogs, Dashboard DTOs, routing policy |
| `ocg-core` | SQLite, `CoreState`, Dashboard V3, adapters, gateway execution, usage sync, Host composition | Runtime plugin loading; adapter-owned DB or HTTP clients |
| `ocg-cli` / `src-tauri` | Process composition for CLI and Desktop | A second control plane or direct WebView mutation path |

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
  /dashboard/api/v3       current Dashboard control plane
  /dashboard/api/v4       parallel, additive Dashboard control plane
  /dashboard/api          preserved auth + browser WS; other REST -> 410 tombstone
  /dashboard/             Vue SPA and assets
```

The SPA remains an HTTP client. Desktop capabilities are registered into
`CoreState`.

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

Custom API is an account-owned product path that uses the same
sealed adapter kind. CPA is a separate static external integration.

Provider-owned catalogs and contracts are resolved before account credentials
are used. Saved discovery rows may activate code-owned Alias mappings or remain
exact raw pins.

## Control plane

The Vue SPA calls V3 through `src/api/dashboard-v3.ts` and V4 through
`src/api/dashboard-v4.ts` (presenters in `src/api/connections.ts`). A parallel
additive `/dashboard/api/v4` sits beside frozen V3 and shares the same session;
it carries the onboarding commit mutation.
CAS-protected mutations carry `expectedRevision` and `processGeneration`;
pricing writes also carry `expectedPricingRevision`. Operational reads and
diagnostics that do not mutate state skip CAS.

The CLI calls the same HTTP-neutral services without an argv CAS token. Shared
services own persistence and revision bumps for both the CLI and the frontend.

The settings-specific persist/rebind/compensation sequence is shown in
[Dashboard API](dashboard-api.md#settings-mutation-workflow). Account setup
states are shown in
[State and lifecycle](state-and-lifecycle.md#managed-account-setup-lifecycle).

## Detail ownership

| Detail | Authoritative chapter |
| --- | --- |
| Alias, selector, protocol, retry, cooldown, model-list behavior | [Runtime invariants](runtime-invariants.md) |
| Dashboard V3 DTOs, CAS, V2 tombstones | [Dashboard API](dashboard-api.md) |
| Locks, account setup, browser workers, process lifecycles | [State and lifecycle](state-and-lifecycle.md) |
| Tables, migrations, backups, rollback | [Storage and migrations](storage-migration.md) |
| Complete HTTP route inventory | [HTTP routes](http-routes.md) |
| Workspace layout and development commands | [Layout](layout.md), [Development](development.md) |
| Extension boundaries | [Extending Open Console Gateway](extending.md) |

---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](architecture.zh-CN.md) · [Docs index](../README.md)
