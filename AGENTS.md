# Open Console Gateway — agent guidance

Open Console Gateway is a local multi-Plan console: Rust workspace, Vue 3 dashboard, and Tauri desktop Host. Treat current code as authoritative. Preserve unrelated changes.

## Boundaries that affect changes

- The dashboard uses HTTP `/dashboard/api/v3` (frozen) and `/dashboard/api/v4` (additive); mutations use CAS. Contract changes belong in the respective `schema/dashboard-api-v{3,4}.schema.json` and generated types. Do not revive retired V2 REST or add Tauri `invoke` commands. V4 contract changes require `pnpm run contract:v4:check`.
- Provider and Plan share `provider_id`. Adapter implementations stay static/sealed; user-defined Provider data binds Configurable HTTP. Custom API is account-owned; CPA is a separate static external integration.
- Preserve authentication, Key obfuscation/redaction, URL validation, cooldown state writes, SSE pass-through, data integrity, and supported compatibility. Do not reintroduce remote sync or an Admin API.
- Changes to user-visible facts update paired English and `.zh-CN.md` guides. Keep capability tables in `docs/user/`, not the root README.
- Rust tests belong in sibling `tests.rs` modules. Test behavior, not source text, documentation wording, or workflow spelling.

## Read for the affected task

- Gateway routing, aliases, provider/catalog, protocols, Keys, proxy, usage, or CPA: [runtime invariants](docs/maintainer/runtime-invariants.md).
- Vue appearance: [DESIGN.md](DESIGN.md) and `src/theme.ts`; shared presentation logic lives in `src/domain/`. Keep the **Key** name and fixed navigation.
- SQLite/schema: [storage migration](docs/maintainer/storage-migration.md).
- Commands and checks: [development](docs/maintainer/development.md).
- Release: [releasing](docs/maintainer/releasing.md).
- Extension paths: [extending](docs/maintainer/extending.md).
- Known limits: [known debt](docs/maintainer/known-debt.md).
- User docs: [docs/USER.md](docs/USER.md). Maintainer index: [docs/MAINTAINER.md](docs/MAINTAINER.md).

Use checks that can expose a failure in the changed behavior. Documentation-only work needs paired English and `.zh-CN.md` content review, not a Rust or frontend build. V3 contract changes require `pnpm run contract:v3:check`; Vue changes require `pnpm run build:web`.

Quit the release tray app before local Tauri development. Report source checks, builds, and real desktop use as distinct evidence.
