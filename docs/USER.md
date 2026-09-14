[简体中文](USER.zh-CN.md)

# User Guide

This guide is for people running Open Console Gateway as a desktop app, a headless gateway, or a Docker service. Chapters follow the order you will actually meet them: install first, troubleshoot later.

## Add integrations

- [Add a Provider](user/add-provider.md) — Create a user-defined Provider, connect one compatible upstream through Custom API, or contribute a sealed built-in Provider with its complete HTTP and routing contract.
- [Manual Client Setup](user/add-application.md) — Connect a client directly through the Gateway API.

- [New API and Sub2API accounts](user/platform-accounts.md) — Group multiple Custom API Keys, refresh scoped quotas and prices, and preserve global routing order.

## Chapters

- [What Open Console Gateway Does](user/overview.md) — Product positioning and the four jobs the gateway performs.
- [Architecture Diagrams](user/architecture.md) — Text maps of one node, a client request, Plans, and the dashboard.
- [Install And First Run](user/install.md) — Windows, macOS, and Linux installers; the SmartScreen ritual included.
- [Connect Your First Client](user/first-client.md) — Copy the Key and base URL, then prove it with one request.
- [Upgrade, Backup, Restore, And Uninstall](user/upgrade-backup.md) — Updater channel, manual upgrade, backup, restore, and uninstall.
- [The Dashboard](user/dashboard.md) — The seven core views, Extensions group, i18n, and Connection Center.
- [Legacy Applications — Retired](user/applications.md) — Retirement scope.
- [Accounts](user/accounts.md) — Plans, credentials, ordering, quota behavior, and managed onboarding.
- [Providers](user/providers.md) — Catalog, provider contracts, per-model protocol overrides, probes, and user-defined Providers.
- [Logs And Settings](user/logs-settings.md) — Request logs, settings, proxy modes, and theme.
- [Gateway Behavior](user/gateway.md) — Endpoints, authentication, aliases, Zen Free, and circuit breakers.
- [Protocol Conversion](user/protocol-conversion.md) — Preferred/supported protocols, passthrough, and conversion limits.
- [Routing, Cost, And Failover](user/routing.md) — Selection order, sticky/round-robin, cost accounting, and failover.
- [CLI](user/cli.md) — Headless CLI archive, data directory, and `serve` / `key` / `status`.
- [Docker](user/docker.md) — GHCR image, Compose setup, browser sidecar, and source builds.
- [External Integrations](user/external-integrations.md) — Local CPA setup, ownership boundaries, routing pool, and disconnect behavior.
- [Data And Security](user/data-security.md) — Data locations, credential storage, and encryption boundaries.
- [Limits](user/limits.md) — Explicit errors, unimplemented surfaces, and platform caveats.
- [Troubleshooting](user/troubleshooting.md) — Common first-run, auth, routing, and log problems.

## Reading paths

- **New user** — `overview` → `architecture` → `install` → `first-client` → `accounts` → `providers` → `gateway` → `troubleshooting`.
- **Docker / CLI operator** — `overview` → `architecture` → `docker` → `external-integrations` → `cli` → `accounts` → `providers` → `routing` → `logs-settings` → `troubleshooting`.
- **Integration author** — `add-provider` for an upstream; `add-application` for a downstream client.

---

[Docs index](README.md) · [简体中文](USER.zh-CN.md)
