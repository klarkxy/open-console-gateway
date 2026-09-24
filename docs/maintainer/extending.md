[简体中文](extending.zh-CN.md)

# Extending Open Console Gateway

Provider adapters and external integrations are the current extension paths.

## 1. Provider or Plan: sealed and static

Use this only for an OCG-owned upstream family with a complete routing,
catalog, protocol, key, and failure contract.

1. Add identities and catalog facts in `ocg-domain` (`ids.rs`, `provider.rs`),
   extend `ProviderAdapterKind` exhaustively, and keep each static Provider's
   contract scope stable. Provider and Plan are one `provider_id` identity.
   Custom stays `ConfigurableHttp`.
2. Add required protocol rows in `ocg-domain::protocol` and Alias mappings in
   `ocg-gateway::alias`. The request path uses the saved contract.
3. Implement `resolve_route` in `ocg-core` so it returns an `AttemptSpec`
   only. Adapters cannot own DB, `CoreState`, or a raw reqwest client.
4. Register the sealed adapter in `BUILTIN_PROVIDERS` (leftover `providers` /
   `provider_models` tables are gone after v56). The adapter's `endpoint_url` /
   `upstream_protocol` / `auth_kind` / `offering` / `endpoint_per_account`
   values are display mirrors of the sealed registry — traffic and routing
   still flow through the sealed adapter code constants. Add the new id to
   the `builtin_offering` map in `ocg-domain::provider` (`plan` for paid
   families, `api` for free or one-credential destinations). CPA is the static
   external integration and is **not** catalogued here.
5. Fail closed until control-plane and routing semantics exist, then test the
   domain, gateway, and core boundaries.

The Provider registry remains static and sealed.
Each static Provider owns its catalog, evidence, and override state under its
single `provider_id` identity.

A new **preset** only needs an entry in `resources/provider-presets.json`.
The build script generates `PRESET_OFFERINGS` from each entry's `offering`;
`preset_offering(preset_id)` reads that generated table. Do not maintain a
second handwritten Rust map. Presets without a plan offering use `"api"`.

## 2. Applications

The Applications page currently contains one DSH-specific V4 flow and one
OCG-owned package; it is not a generic registry or a promised extension point.
Add a second application only from its concrete install, credential, lifecycle,
and acceptance contract. See [Applications](../user/applications.md).

## 3. External integration: static local-service adapter

Use this for a local service integrated through a code-reviewed adapter. It appears in
the general **Extensions** navigation group below Settings, not in
Providers, Plans, or the Add Account selector.

- Define a narrow typed Dashboard V4 contract and CAS-protected mutations; do
  not add a raw management proxy or arbitrary upstream path/body forwarding.
- Make the ownership boundary explicit. OCG may retain only what it needs to
  connect and route; the external service retains its own OAuth tokens, auth
  files, browser callbacks, and internal scheduler. For an externally operated
  service, lifecycle also remains external. The managed CPA mode separately
  owns its installed files and child process; see [Runtime Invariants](runtime-invariants.md#external-integrations).
- Keep the service local: loopback for Desktop/CLI, or an explicit private
  Compose sibling. Remote service addresses and arbitrary process control are
  outside this boundary. Managed CPA lifecycle operations are limited to the
  OCG-owned runtime; installation and updates are user-triggered.
- Reuse OCG's ordering/selection/logging conventions only where the product
  contract calls for it. Do not invent internal accounts, costs, or quotas the
  external service does not expose.

CPA is the current instance of this path. Reuse its existing helpers where they
fit; justify a shared framework with concrete requirements from the integrations
that will use it.

## Dashboard V4 endpoint changes

New provider, destination, or credential semantics go to `dashboard_v4`
(`types.rs` and its `CATALOG_TYPE_NAMES`; routes in `dashboard_v4/mod.rs`).
Run `pnpm run contract:v4:check`. `/dashboard/api/v3` is a 410 tombstone:
do not add V3 HTTP routes or DTO fields.

Remounted operational handlers still live in `dashboard_v3/` and mount under
V4. If a remounted DTO or route must change:

1. Add or extend DTOs in `dashboard_v3/types.rs` and append new names to
   `CATALOG_TYPE_NAMES`. Do not change existing `$defs` objects.
2. Mount routes in `dashboard_v3/mod.rs` (they are nested under V4);
   mutations use `parse_mutation_json` and `check_expectation` and preserve
   secret redaction.
3. Prefer existing persistence/control helpers and keep `dashboard_v3`
   independent of `gateway`.
4. Add a focused integration test, update `src/api/dashboard-v3.ts`, and run
   `pnpm run contract:v3:check`.

---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](extending.zh-CN.md) · [Docs index](../README.md)
