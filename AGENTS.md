# Open Console Gateway — agent guidance

Open Console Gateway is a local multi-Plan console: Rust workspace, Vue 3 dashboard, and Tauri desktop Host. Treat current code as authoritative. Preserve unrelated changes.

## Boundaries that affect changes

- The dashboard uses HTTP `/dashboard/api/v4`; mutations use CAS. `/dashboard/api/v3` is a 410 tombstone. Contract changes belong in `schema/dashboard-api-v4.schema.json` and generated types. There is no V2 or V3 REST surface and no Tauri `invoke` commands; do not add either.
- Provider and Plan share `provider_id` on catalog rows. Adapter implementations stay static/sealed; user-defined Provider data binds Configurable HTTP. Custom API is a one-credential `http` destination; CPA is a separate static external integration.
- Preserve authentication, Key obfuscation/redaction, URL validation, cooldown state writes, SSE pass-through, data integrity, and supported compatibility. There is no remote sync and no Admin API; do not add either.
- Changes to user-visible facts update paired English and `.zh-CN.md` guides. Keep capability tables in `docs/user/`, not the root README.
- Rust tests belong in sibling `tests.rs` modules. Test behavior, not source text, documentation wording, or workflow spelling. Frontend tests likewise never assert literal UI copy: domain functions return semantic codes, copy mapping lives in exported `*_KEYS` tables (`Record<Code, MessageKey>`), and views compose text with `t()`.
- Frontend server state has a single owner: the Pinia stores in `src/stores/`. Loads are generation-guarded so stale responses never commit, mutations commit their results into the store in place, revalidations keep current content rendered (loading gates show skeletons only before the first successful load), and `dropSession` wipes cached resources on logout/401. Views keep only UI-local state (modals, drafts, filters, optimistic layers) and must not copy store data into local refs; shared presentation logic stays pure in `src/domain/`.

## Read for the affected task

- Gateway routing, aliases, provider/catalog, protocols, Keys, proxy, usage, or CPA: [runtime invariants](docs/maintainer/runtime-invariants.md).
- Vue appearance: [DESIGN.md](DESIGN.md) and `src/theme.ts`. Keep the **Key** name and fixed navigation.
- SQLite/schema: [storage migration](docs/maintainer/storage-migration.md).
- Commands and checks: [development](docs/maintainer/development.md).
- Release: [releasing](docs/maintainer/releasing.md).
- Extension paths: [extending](docs/maintainer/extending.md).
- Known limits: [known debt](docs/maintainer/known-debt.md).
- User docs: [docs/USER.md](docs/USER.md). Maintainer index: [docs/MAINTAINER.md](docs/MAINTAINER.md).

Use checks that can expose a failure in the changed behavior. Documentation-only work needs paired English and `.zh-CN.md` content review, not a Rust or frontend build. Contract changes require `pnpm run contract:v4:check`; Vue changes require `pnpm run build:web`.

Quit the release tray app before local Tauri development. Report source checks, builds, and real desktop use as distinct evidence.
