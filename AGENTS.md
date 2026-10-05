# Open Console Gateway — agent guidance

OCG3 is a local multi-Plan gateway with a complete headless CLI and an adopted native GPUI/Ely GUI target for Windows, Linux, and macOS. The adopted execution target is **CPA as the sole execution foundation**; OCG owns configuration, access Keys, public models, Plan evidence, and runtime management. Read [architecture](docs/architecture.md), [migration](docs/maintainer/cpa-migration.md), and [completed CPA findings](docs/maintainer/cpa-validation.md).

Current Rust source still implements the former gateway and external CPA integration; the native GUI is not implemented. Target documents own design; source determines current implementation. Preserve unrelated changes and never claim migration or GUI acceptance is complete because it is documented.

## Execution and product boundaries

- CPA owns provider execution, authentication refresh, translation, per-attempt credential selection, streaming, ordinary cooldown, and retries. Do not build an OCG outer retry/fallback loop or transparently restore the former kernel after CPA failure.
- OCG owns CLI/control services, configuration, client Key authentication, model scopes/aliases, Plan facts, trusted quota adapters, logs, and owned runtime lifecycle.
- Precise quota evidence has explicit credential/model/pool scope, identity/version, windows, and reset time. Unknown stays unknown; ordinary 429 is not persistent Plan exhaustion. Estimates never control admission.
- Mandatory quotas and selective no-replay belong at synchronous per-attempt boundaries inside CPA. Async usage/completion is observational. The experimental probe is not production enforcement; SDK/patch integration remains unimplemented.
- Desktop design uses Rust, GPUI, and Ely: A overview as home, C Plan master-detail, B optional compact mode. No WebUI, WebView-based main interface, or tray is in the adopted scope. CLI completeness covers initialization, configuration, Keys, accounts/Plans, models, quota, logs, backup/restore, and start/stop.
- Current implementation priority is complete CPA-based CLI acceptance before GUI construction; preserve the adopted GUI design while completing the headless workflows and execution guarantees.
- GUI and CLI reuse shared V4/CAS controls; the control service owns storage, evidence, and CPA lifecycle. Closing the GUI does not stop the gateway. Bootstrap/authorization/reconnect and full shared backup/restore remain implementation gaps. Keep Windows/Linux/macOS targets separate from observed native platform acceptance.

## Compatibility while migrating

- Current HTTP control remains /dashboard/api/v4 with CAS. V3 stays tombstoned; remounted kernel DTO tooling is not V3 REST. Add no other control API or arbitrary CPA raw-management proxy.
- V4 contract source is schema/dashboard-api-v4.schema.json; remounted DTOs remain in schema/dashboard-api-v3.schema.json. Use the Rust schema checks in [development](docs/maintainer/development.md); this branch has no frontend package/toolchain.
- Preserve authentication/redaction, URL/proxy validation, private secrets, storage integrity, persisted authoritative deadlines, credential-version fences, cancellation, SSE delivery, and migration/rollback.
- Existing provider/configurable-HTTP code describes current behavior until consumers migrate. Map each supported provider/application capability to CPA or an explicit gap; do not silently retire it.
- OCG owns product configuration and generated CPA projections. CPA owns OAuth/refresh state. Manage only owned processes/directories and keep inference/management private to prevent authorization bypass.
- No remote node sync, multitenant control plane, or Tauri invoke data paths are added by this design.

## Read for the affected task

- Architecture, native GUI layouts/platforms, quotas, retries, provider mapping, CPA, lifecycle: [architecture](docs/architecture.md) and [migration](docs/maintainer/cpa-migration.md).
- Current operation: [CLI](docs/user/cli.md), [HTTP contracts](docs/maintainer/dashboard-api.md), [routes](docs/maintainer/http-routes.md).
- SQLite/encryption/backup: [storage migration](docs/maintainer/storage-migration.md).
- Commands/checks: [development](docs/maintainer/development.md) and [CI](docs/maintainer/ci.md).
- Documentation: [conventions](docs/maintainer/conventions.md), [user guide](docs/USER.md), [maintainer guide](docs/MAINTAINER.md).

## Verification and documentation

Rust behavior tests belong in sibling tests.rs modules. Test behavior, not source text or literal copy. Select checks that expose changed behavior; distinguish source, build, packaged runtime, live-provider behavior, and deployment.

Documentation-only changes require paired English/Chinese content and link review, not Rust builds or further CPA experiments. New-version design has one source in docs/architecture.*; current/historical operation guides identify their scope. Product capability tables stay in docs/user/.

Release guides describe historical desktop packaging. Use an implemented publication procedure for the actual deliverable; documenting architecture does not authorize publication/deployment.
