[简体中文](MAINTAINER.zh-CN.md)

# Maintainer Guide

This guide is for people changing code, cutting releases, debugging the gateway, and validating desktop bundles. It documents the architecture and operating contracts as implemented at HEAD.

## Chapters

- [Layout](maintainer/layout.md) — Crate and directory layout.
- [Development](maintainer/development.md) — Dev loop and checks.
- [Architecture](maintainer/architecture.md) — Four-layer crates, adapter identity, request flow, and text diagrams.
- [Dashboard API](maintainer/dashboard-api.md) — Frozen V3 contract, additive V4 surface, CAS tokens, and mutation rules.
- [State, Credentials, And Lifecycle](maintainer/state-and-lifecycle.md) — `CoreState`, locks, credentials, and persistence.
- [HTTP Routes](maintainer/http-routes.md) — Inference routes, V3/V4 paths, the V2 tombstone, and auth/session routes.
- [Runtime Invariants](maintainer/runtime-invariants.md) — Detailed gateway, alias, Zen Free, plan catalog, access key, proxy, and usage-sync semantics.
- [Storage And Migrations](maintainer/storage-migration.md) — SQLite schema v49, historical migrations, backup, and the operator runbook.
- [Extending Open Console Gateway](maintainer/extending.md) — Sealed provider extension procedure.
- [Release Artifacts](maintainer/release-artifacts.md) — Supported platform matrix and package names.
- [CI Workflows](maintainer/ci.md) — Quality, release, and container workflows.
- [Release Procedure](maintainer/releasing.md) — Version bump, tag, build, and publish checklist.
- [Known Debt And Non-Goals](maintainer/known-debt.md) — Documented gaps and deliberate non-goals.
- [Coding Conventions](maintainer/conventions.md) — crate DAG, security boundaries, and documentation ownership.

## Reading paths

- **Contributor** — `layout` → `development` → `architecture` → `state-and-lifecycle` → `http-routes` → `conventions`.
- **Release owner** — `release-artifacts` → `ci` → `releasing` → `known-debt`.
- **UI / theme work** — Read `DESIGN.md` first, then `src/theme.ts` and the Vue surface you are changing.

---

[Docs index](README.md) · [简体中文](MAINTAINER.zh-CN.md)
