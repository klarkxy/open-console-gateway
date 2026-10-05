[简体中文](MAINTAINER.zh-CN.md)

# Maintainer guide

Start with the [CPA architecture](architecture.md), [migration boundaries](maintainer/cpa-migration.md), and [completed findings](maintainer/cpa-validation.md). These define the new version; the former kernel remains in source pending migration.

The [native GUI section](architecture.md#9-native-desktop-gui) owns the adopted GPUI/Ely layouts, shared control/lifecycle boundaries, and Windows/Linux/macOS validation target. Concept images and platform targets are not runtime acceptance evidence; the current checkout has no GPUI GUI.

## Current implementation references

- [Development](maintainer/development.md) — Current Rust CLI builds and checks.
- [CLI](user/cli.md) — Implemented commands and operations.
- [HTTP contracts](maintainer/dashboard-api.md) and [routes](maintainer/http-routes.md) — Existing V4/CAS behavior, not completed CPA integration.
- [Storage migration](maintainer/storage-migration.md) — Existing encryption, backups, and rollback.
- [Application integration](maintainer/byok-applications.md) — Client-specific behavior, not another execution architecture.
- [Conventions](maintainer/conventions.md) — Design/source separation and documentation rules.
- [CI](maintainer/ci.md) — Current checks and historical publisher limitations.
- [Work checkpoint — 2026-10-05](maintainer/work-checkpoint-2026-10-05.md) — Paused CLI work, evidence and reinstall/resume steps.

## Historical release references

[Artifacts](maintainer/release-artifacts.md) and [release procedure](maintainer/releasing.md) describe earlier desktop distribution. They are not a usable CPA-based OCG3 publisher. Release notes remain under releases/.

This documentation update records the native GUI direction without implementing a GUI, migrating the runtime/database, or publishing anything. During implementation, preserve complete CLI operation and shared V4/CAS controls. Keep compatibility/security guarantees while replacing execution; do not develop an outer retry scheduler.

[Docs index](README.md) · [Agent guidance](../AGENTS.md)
