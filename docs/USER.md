[简体中文](USER.zh-CN.md)

# User Guide

This guide is for people running Open Console Gateway as a desktop app, a headless gateway, or a Docker service. Chapters are grouped by when you meet them: install and connect first, operate and recover later.

## Add Integrations

- [Add a Provider](user/add-provider.md) — Create a user-defined Provider, connect one compatible upstream through Custom API, or contribute a sealed built-in Provider with its complete HTTP and routing contract.
- [Manual Client Setup](user/add-application.md) — Connect a client directly through the Gateway API.
- [New API and Sub2API accounts](user/platform-accounts.md) — One sortable site account with multiple Keys; fetch each Key's models and route in your order.

## Start Here

- [What Open Console Gateway Does](user/overview.md) — Product positioning and the four jobs the gateway performs.
- [Architecture Diagrams](user/architecture.md) — Text maps of one node, a client request, Plans, and the dashboard.
- [Install And First Run](user/install.md) — Windows, macOS, and Linux installers; the SmartScreen ritual included.
- [Connect Your First Client](user/first-client.md) — Copy the Key and base URL, then prove it with one request.
- [The Dashboard](user/dashboard.md) — The eight core views, Extensions group, i18n, and Connection Center.
- [Applications](user/applications.md) — The DSH plugin installation flow, recovery, and removal.

## Accounts And Models

- [Accounts](user/accounts.md) — Plans, credentials, ordering, quota behavior, and managed onboarding.
- [Account Actions And Refresh](user/account-actions-and-refresh.md) — Saving without waiting on reloads, removing one local Key or a group, and what each refresh scope covers.
- [Providers](user/providers.md) — Catalog, provider contracts, per-model protocol overrides, probes, and user-defined Providers.
- [Plan And API Presets](user/provider-presets.md) — Browse Plan/API presets from Accounts or Providers; fixed presets supply address, protocol, authentication, and a default model.
- [Individual Supplier Models](user/provider-models.md) — Add and edit HTTP model mappings, public aliases and allowed upstream protocols; delete local catalog rows.
- [Model Catalog Refresh](user/model-catalog-refresh.md) — What Refresh model catalog does: directory updates, protocol evidence, first-snapshot defaults, and per-model tests.
- [Model Metadata And Reasoning Tiers](user/model-metadata.md) — Context windows, modalities, and reasoning levels: where each fact comes from, and how to declare one yourself.

## Running The Gateway

- [Gateway Behavior](user/gateway.md) — Endpoints, authentication, and aliases.
- [Routing And Failover](user/routing.md) — Selection order, sticky/round-robin, usage windows, circuit breakers, and failover.
- [Protocol Conversion](user/protocol-conversion.md) — Preferred/supported protocols, passthrough, and conversion limits.
- [Temporary Unavailability](user/temporary-unavailability.md) — Global or per-connection Settings rules that skip a Key or model locally after a matching upstream error, then retry on real traffic.
- [Logs And Settings](user/logs-settings.md) — Request logs, settings, and proxy modes.

## Deployment

- [CLI](user/cli.md) — Headless CLI archive, data directory, `serve` / `key` / `status`, and bundled skill sync.
- [Docker](user/docker.md) — GHCR image, Compose setup, browser sidecar, and source builds.
- [External Integrations](user/external-integrations.md) — Local CPA setup, ownership boundaries, routing pool, and disconnect behavior.

## Care And Recovery

- [Data And Security](user/data-security.md) — Data locations, credential storage, and encryption boundaries.
- [Upgrade, Backup, Restore, And Uninstall](user/upgrade-backup.md) — In-app and manual upgrade, backup, restore, and uninstall.
- [Limits](user/limits.md) — Explicit errors, unimplemented surfaces, and platform caveats.
- [Troubleshooting](user/troubleshooting.md) — Common first-run, auth, routing, and log problems.

## Reading Paths

- **New user** — `overview` → `architecture` → `install` → `first-client` → `accounts` → `providers` → `gateway` → `troubleshooting`.
- **Docker / CLI operator** — `overview` → `architecture` → `docker` → `external-integrations` → `cli` → `accounts` → `providers` → `routing` → `temporary-unavailability` → `logs-settings` → `troubleshooting`.
- **Integration author** — `add-provider` for an upstream; `add-application` for a downstream client.

---

[Docs index](README.md) · [简体中文](USER.zh-CN.md)
