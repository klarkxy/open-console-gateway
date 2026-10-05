[简体中文](development.zh-CN.md)

# Development

## Prerequisites

The workspace `rust-version` is Rust 1.88 or newer, as required by the locked
dependencies. This branch has no root `package.json`, Vue workspace, or Tauri
crate. Node 24 is used for the optional `scripts/cli-acceptance.mjs` check and the optional `scripts/cli-cpa-acceptance.mjs` harness.
This generation does not restore the frontend toolchain. No frontend package is reintroduced. The CLI package is `ocg-cli` and the executable is `ocg`. Its implicit data directory is `~/.ocg3`; these commands pass `--data-dir` so they do not open a previous generation's `~/.ocg-mgr-cli` or an installed profile. CPA here means the one local runtime owned by `ocg serve`. The reviewed artifact lock is present. A `serve` in this loop is still not an accepted child, because whole CLI acceptance remains pending. The native GUI stays postponed while the CLI is incomplete.

## Dev loop

Typecheck, test, and build the headless CLI with Cargo. The feature-off build
drops the default `dsh-local-host` feature, so local DSH and BYOK configuration
are unavailable; inference, the control plane, browser and CPA remain. Run the default binary against an isolated data directory. Do not use
an installed data directory for development:

```bash
cargo check -p ocg-cli --locked
cargo test -p ocg-cli --locked
cargo build -p ocg-cli --locked
cargo build -p ocg-cli --locked --no-default-features
cargo run -p ocg-cli -- --data-dir tmp/dev-data serve --port 19042
```

On some Windows hosts, HNS/WSL/Docker reserves port ranges that include
`9042`; `19042` avoids that conflict. Nothing watches Rust sources: stop the
process and rerun the command after changing gateway crates. A fresh host
creates its primary Gateway Key during state initialization. Read
`GET /dashboard/api/v4/connection` into a private file with `api --output`. The [CLI guide](../user/cli.md) uses
synthetic placeholders for the request bodies. Do not put a key on the command
line.

`.githooks` runs `cargo fmt --all` on staged `*.rs` when Git hooks are enabled.

`serve` owns the local CPA child. Startup is the restore path and exit is the shutdown path. `POST /dashboard/api/v4/external-integrations/cpa/runtime/stop`
clears the stored intent. Failed recovery is reported once and is not looped.
Another CPA instance must not use the same auth directory. The reviewed lock is present, and the current Go build identity is `011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b`. Primary production and fixture host suites exited 0. Manifest `fullCLIAccepted` stays false, so product listen and child acceptance remain pending. The numbers and the lock digest are under Artifact trust. Earlier `af3c47e4…` worker logs, and the `f3886f65…` fixture failure on `processReload`, are historical. This page does not record a new Go, Rust, or Node product run.

## Checks

Pick the smallest Cargo check that covers the changed boundary:

| Change | Check |
| --- | --- |
| One Rust crate | `cargo test -p <package> --locked` |
| Core behavior | `cargo test -p ocg-core --locked --features ollama-cloud-loopback-test <filter>` |
| CLI | `cargo test -p ocg-cli --locked` |
| CLI typecheck | `cargo check -p ocg-cli --locked` |
| CLI build | `cargo build -p ocg-cli --locked` |
| Feature-off CLI | `cargo build -p ocg-cli --locked --no-default-features` |
| V3 DTO schema | `cargo test -p ocg-core --locked dashboard_v3::types` |
| V4 DTO schema | `cargo test -p ocg-core --locked dashboard_v4::types` |

The quality workflow passes `--features ocg-core/ollama-cloud-loopback-test`
so the Ollama Cloud gateway integration suite can install its loopback-only
test seam. That feature is default-off: application builds keep the fixed
`https://ollama.com` origin and do not compile the seam. A workspace
`cargo test` without the feature still compiles `ollama_cloud_gateway` but
runs none of its cases. Workspace `[profile.release]` uses thin LTO,
`strip`, and `panic = "abort"`.

Rust schema tests are the contract gate on this branch. The former
`pnpm run contract:v3:check`, `contract:v4:check`, `build:web`, and
`test:tooling` commands belonged to the removed frontend workspace. No frontend package or toolchain is reintroduced. Checked-in `schema/dashboard-api-v3.schema.json` and `schema/dashboard-api-v4.schema.json` stay byte-for-byte until `cargo run -p ocg-core --example export_dashboard_v3_schema --locked` and `cargo run -p ocg-core --example export_dashboard_v4_schema --locked` are run. This page does not hand-edit those files and does not record those runs. `contract_schema()` already includes `RoutingExplanation`; schemars follows new fields when the generator runs. Future contract tooling changes belong to actual implementation work.
The release workflow still contains historical desktop packaging steps and is
not the current quality gate. Current jobs are in [CI](ci.md).
[Releasing](releasing.md) still describes the removed desktop package.

Run Cargo tests, Clippy, and native builds sequentially when they share
`target/`; keep the same build configuration to reuse compiled work. After a
fix, rerun the affected checks.

Rust unit tests live in sibling `tests.rs` modules (`src/db.rs` declares
`mod tests;` and the tests are in `src/db/tests.rs`). Do not add tests that
assert on source text, workflow YAML, or documentation prose.

Direct `Database::update_account` does not bump revision; that is
intentional and is not the CLI path.

## Optional acceptance

`node scripts/cli-acceptance.mjs` is an independent developer check against a
binary you already built. It is not a `quality.yml` job. The script creates
its own synthetic requests, isolated home, and cipher. Do not pass a live key,
a user profile, or an installed data directory. This page does not record a
result from that script. It is separate from `scripts/cli-cpa-acceptance.mjs`. A pass of either script is not CPA runtime acceptance.

```bash
node scripts/cli-acceptance.mjs target/debug/ocg tmp/cli-acceptance
```

On Windows the binary name is `target/debug/ocg.exe`. Omit both
arguments to use that default binary and a timestamped directory under
`tmp/ocg3-cli-delivery/`.

## CPA acceptance harness

The optional harness is `scripts/cli-cpa-acceptance.mjs`. `--binary` locates an executable and is not trust. The harness does not write `runtime/cpa/artifact-lock.json`. The operator path is implemented: `prepareNativeOperator`, `runNativeOperatorWorkflows`, and `settleBindingApply` run after `nativeAdmission()` returns empty. That function returns pending before serve, listen, or a child while the deny-external proxy is absent, while the isolated profile is absent, or while the compile-mode flags are missing or both set. The reviewed lock is present, so a missing lock is not the current gate. Candidate hashes are not trust. `fullCLIAccepted` is not a harness field and is not trust. Whole CLI acceptance and runtime acceptance remain pending. The Rust feature `ollama-cloud-loopback-test` selects the CPA lock variant: feature off (`cfg` not that feature) is `production`, and feature on is `native-loopback-fixture`. The same feature is the Ollama Cloud loopback seam under Checks.

```text
node scripts/cli-cpa-acceptance.mjs --binary <ocg.exe> --host-dir <variant-directory> [--scratch <profile-uuid-dir>] --test-feature-cli
node scripts/cli-cpa-acceptance.mjs --binary <ocg.exe> --host-dir <variant-directory> [--scratch <profile-uuid-dir>] --production-feature-off
node scripts/cli-cpa-acceptance.mjs --lock-fixtures
node scripts/cli-cpa-acceptance.mjs --native-fixtures
node scripts/cli-cpa-acceptance.mjs --list
node scripts/cli-cpa-acceptance.mjs --prerequisites --binary <ocg.exe> --host-dir <variant-directory>
```

| Argument | What it locates | What it does not do |
| --- | --- | --- |
| `--binary` | The `ocg` executable for this run. The Windows name is `ocg.exe`. | It does not choose the trusted variant. A path is not a lock. |
| `--host-dir` | The directory that contains that variant's `manifest.json` and `ocg-cpa-host.exe`. The production default in the Go API is `runtime-build`. The fixture default is `runtime-build/native-loopback-fixture`. | It does not select trust, read a lock from that directory, or accept `fullCLIAccepted`. |
| `--scratch` | One UUID directory under `tmp/ocg3-cli-delivery/orchestration-20261004/acceptance-work/profiles`. | It is not a credential profile and not a host directory. |
| `--test-feature-cli` | Names the supplied binary as the Rust `ollama-cloud-loopback-test` build. The expected lock variant is `native-loopback-fixture`. | It does not turn the feature on, write the map, or accept fixture bytes. |
| `--production-feature-off` | Names the supplied binary as the feature-off build. The expected lock variant is `production`. | It does not make a production hash trusted. |
| `--lock-fixtures` | In-memory lock cases only. | No product, listen, or child process. |
| `--native-fixtures` | In-process parser, map, and scenario-plan checks. An earlier standalone result, from before this lock, was `blocked-dependency`, exit 0, runnable 88, pending 44, and product runtime started false. | That result is historical in-process evidence. Acceptance remains pending. |
| `--scenario`, `--prerequisites`, `--list` | The existing stage runner. | They do not admit native product execution by themselves. |

`--test-feature-cli` and `--production-feature-off` are mutually exclusive. `--lock-fixtures` and `--native-fixtures` are separate commands. There is no `--proxy` argument. A caller-supplied proxy URL is not an asset and cannot relax the deny rule. No argument writes or replaces the lock. The only lock path is `runtime/cpa/artifact-lock.json`. That file is present. A missing file would still block before `schema v4` and before `serve`.

| Compile mode | Lock `variant` | Manifest top `variant` | Manifest `buildTags` | Host directory the operator must pass |
| --- | --- | --- | --- | --- |
| `--production-feature-off`, and every non-native stage | `production` | `production` | `[]` | production output |
| `--test-feature-cli` | `native-loopback-fixture` | `native-loopback-fixture` | `["ocg_native_loopback_fixture"]` | fixture output |

Records are unique on normalized `os` / `arch` / `variant`. The builder normalizes `windows` / `win32` to `windows`, `linux` to `linux`, and `darwin` / `macos` to `macos`. It normalizes `amd64` / `x86_64` / `x64` to `x86_64`, and `arm64` / `aarch64` to `aarch64`. Both variant manifests carry top-level `os`, `arch`, and `executable`. On this tree those fields are `windows`, `x86_64`, and `ocg-cpa-host.exe`. Selection for this machine is the current platform, `verification` `host-suite`, and that variant. `compiled-only` does not become that selection. A fixture record whose OS is not Windows is rejected. Production and fixture Windows host-suite hashes must differ. The selected record's SHA-256 must match both `manifest.executableSHA256` and the bytes of `<host-dir>/<record.executable>`. That match checks the lock record. Manifest `executableSHA256` by itself is not the lifecycle digest. Candidate hashes stay out of this page.

Both variants carry the same lock-level `sourceCommit`, `sourceVersion`, `protocolVersion`, `buildIdentity`, `hostSHA256`, `overlaySHA256`, and `requiredCapabilities` when they are built from the same frozen inputs. The manifest for the selected variant must match that identity. A differing input is a different source. The placeholder SHA `baff0e76f37b32f8e16618b6533f9a435dc36b4bc737302745b4b9353ed45542` is not a record. A manifest hash that merely matches itself is not a record. `manifest.fullCLIAccepted` is ignored.

The harness opens its own loopback HTTP proxy on `127.0.0.1` with an ephemeral port. The product setting is the existing manual proxy (`proxyMode: manual`), written by the normal settings path into the owned child config. The proxy refuses every non-loopback HTTP target and every non-loopback `CONNECT` before DNS and before dial. It forwards only `127.0.0.1` and `localhost`, so already-accepted custom upstream stages keep working. It is not permission to dial a provider. Native scenarios do not start when this proxy is down. They do not add a direct-provider client, and they do not treat a metadata `base_url` of `127.0.0.1` as a grant.

Test assets stay under the UUID scratch profile. They are not installed CLI homes. Codex, Claude, Kimi Code, and Grok CLI files live at `<profile>/home/native/...` through `CODEX_HOME`, `CLAUDE_CONFIG_DIR`, `KIMI_CODE_HOME`, and `GROK_HOME`. Staged CPA auth is `<data-dir>/cpa/auth/<basename>.json` only after that directory already exists. The endpoint map is the process environment `OCG_CPA_TEST_ENDPOINTS` on the `ocg` child, and only after variant admission. `--native-fixtures` does not install the map. Feature-off admission may install it only to prove the production child ignores it. Unknown keys, duplicate keys, the key `cpa`, and a malformed origin are refused before a child environment is built. Antigravity discovery stays `supported: false` with the product reason `Antigravity local credential storage is not compatible with this import; use login.` The harness does not call OAuth start.

A normal CLI has no `fakeReady` or `policyReady` switch. Unit tests call private injected-byte helpers in `artifact.rs`. Those helpers are not product entries. A passing parser test is not an accepted host. Node-checked fixture tests are not runtime acceptance.

The same `serve` process owns startup and shutdown, as the dev loop describes. `install`, `rollback`, and `apply` do not read `OCG_CPA_BASE_URL`, including a malformed value. That variable does not redirect the owned host and does not block those three operations. Stored historical rows and bytes stay. Explicit legacy refusals stay on the control routes in the [dashboard API](dashboard-api.md#owned-cpa-control).

## Artifact trust and evidence

Product trust is one compile-time embed of `crates/ocg-core/../../runtime/cpa/artifact-lock.json`, the repository file `runtime/cpa/artifact-lock.json`. Absence fails compilation until that reviewed lock is written. An empty, non-UTF-8, or schema-invalid embed fails closed at `verify`. The product does not load a lock from disk or from `OCG_CPA_HOST_DIR`, and it does not accept manifest `executableSHA256` as the digest. `PINNED_SHA256` (`baff0e76f37b32f8e16618b6533f9a435dc36b4bc737302745b4b9353ed45542`) stays defined in `artifact.rs`. It is not an accepted digest and it is not a fallback. `selected_trusted_sha()` is the only product lifecycle digest. `verify(dir)` is the only product manifest verifier. Both read the same embed, the same `parse_lock` rules, and the same normalized `(os, arch, variant)`. `resolve_dir`, an explicit directory, and `OCG_CPA_HOST_DIR` only locate `manifest.json` and the executable bytes. Manifest location is not trust.

`apply` calls `load_artifact` and `artifact::install` before it renders the child. `load_artifact` is `resolve_dir` of the explicit directory or `OCG_CPA_HOST_DIR`, then `verify`. `record.artifact_sha256` is the verified `artifact.sha256`. `install` refuses every digest other than `selected_trusted_sha()`, including the historical placeholder, before it creates the version directory. `skip_spawn` skips only the child process inside `launch`. `synthetic_ready` copies `record.artifact_sha256` and does not choose trust. `accept_ready` and `accept_restored` still require that digest to equal `selected_trusted_sha()`. `write_managed` stores `asset_sha256` from `selected_trusted_sha()`. The current version string stays `PINNED_VERSION_CANONICAL` (`8.0.10`). When the config file is absent, `write_managed` returns before it reads the digest. `owned_device_launch` requires `artifact_sha256` to equal `selected_trusted_sha()` with exact case-sensitive comparison, then `installed_executable` checks the marker, regular file, reparse point, permissions, and streamed bytes. File existence is not enough. A different digest, including the historical placeholder, returns `pinned CPA executable is not installed`. `execution_report` and the non-skip launch path keep calling `installed_executable` on the stored digest. They do not invent a second digest. `version_accepted` still returns `pinned CPA install only accepts v8.0.10`.

Source constants, not accepted runtime, are `PINNED_COMMIT` `6fecc6e5567912661654a4eaf9b8f5436facd1c2`, `PINNED_VERSION` `v8.0.10`, `PINNED_VERSION_CANONICAL` `8.0.10`, 16 `REQUIRED_CAPABILITIES`, and lock schema 1. Linux and macOS records stay `compiled-only` evidence on this Windows tree. `verification` is `host-suite` or `compiled-only`. Both still require the byte hash.

| Evidence | What it establishes |
| --- | --- |
| Source | Candidate Rust and harness text. The root lock is present. Absence of that file would still fail the embed. |
| Unit | Private injected-byte helpers. They are not product entries. A parser pass is not an accepted host. `verified_ready` cannot be set from a unit test. A database seed that calls `explain` is not live Ready proof. |
| Host-suite | Lock `verification` `host-suite` for the selected platform and variant, still with the byte hash. On this Windows tree that is the Windows record. A fixture record whose OS is not Windows is rejected. |
| CLI | `ocg` command behavior. Parser exit 0 is not CLI acceptance. Whole CLI acceptance is pending. |
| Runtime | A reviewed lock, matching host bytes, and a product listen plus child. The lock is present. Listen and child acceptance remain pending. Node-checked fixture tests are not this level. |
| Compiled-only | Lock `verification` `compiled-only`, still with the byte hash. It is not interactive acceptance. Linux and macOS records stay here on this Windows tree. |

Current artifact and compile facts:

- The reviewed root lock `runtime/cpa/artifact-lock.json` is present. It is schema 1, UTF-8, four records, and its file SHA-256 is `bf89d3a47323fc17281a42ee4d7874e03249cfe27dc765297055b65dde3a7839`. Build identity is `011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b`. The records are Windows `x86_64` production `host-suite`, Windows `x86_64` fixture `host-suite`, Linux `x86_64` `compiled-only`, and macOS `aarch64` `compiled-only`. `selected_trusted_sha()` remains the only product lifecycle digest, and `verify(dir)` remains the only product manifest verifier. There is no permissive verifier and no caller-supplied trust. `fullCLIAccepted` on the manifests stays false. The lock is not a full CLI claim.
- Both current manifests record `windows`, `x86_64`, and `ocg-cpa-host.exe`, source commit `6fecc6e5567912661654a4eaf9b8f5436facd1c2`, source version `v8.0.10`, and that same build identity. Production also records compiled-only `linux` / `x86_64` / `ocg-cpa-host-linux-amd64` and `macos` / `aarch64` / `ocg-cpa-host-darwin-arm64`. The fixture manifest records no other platform. Go source review `tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/registration-mutation-fence-independent-review.md` is READY for this identity. Primary suites in `tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/primary-registration-mutation-suite-exits.json` exited 0: production host 85.060s and SDK 0.141s, fixture host 78.033s and SDK 0.123s. `wholeCLIAccepted` in that receipt stays false. This page does not record a new builder run.
- `documented_runtime_dir()` is implemented under `cfg(test)` and only locates the production base or, with `ollama-cloud-loopback-test`, `native-loopback-fixture`. `verify` and `install` use the compiled selected trust. This page did not run `verify`.
- `RoutingResolvedMapping.migrationRequired` is implemented, including a catalog-only historical remote row. An omitted older payload is false. The checked-in schema files are the Rust example export. Both built `ocg` binaries' `schema v3` and `schema v4` match those files: four passes, recorded in `tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/primary-compiled-schema-check.json`. Checkpoint A built both binaries and compiled the workspace tests without running them. Checkpoint B, `task_614235fe74`, is running the Rust assertions, Clippy, the default build, the no-default build, and the operator workflows. That pass is not accepted. This page did not run Cargo or a product runtime. The wire is in [Owned CPA routing explain](dashboard-api.md#owned-cpa-routing-explain). Explicit migration of a stored historical remote row remains unimplemented.

Closeout stays false. Harness CLI arguments are unchanged. The standalone `--native-fixtures` result at `2026-10-04T10:39:26.282Z` and the `--lock-fixtures` result at `2026-10-04T10:39:26.423Z` predate this lock. They recorded real lock present false, closeout false, and no product runtime. They remain historical in-process evidence. `harness-source-receipt.md` is an earlier receipt. The selective gaps below stay in `native-harness-work/internal-evidence-map.md`.

Isolation and restoration send PATCH `{enabled:false}` and `{enabled:true}`, then the existing `waitAutomaticApply`. An `unchanged` phase stops as a product dependency. It does not describe the target credential as absent. The isolation operator path did not run. The bounded source review of that harness correction is recorded, including one earlier `node --check` that exited 0. Product mutation hooks, trusted host bytes, and full CLI runtime acceptance remain separate prerequisites.

The internal evidence map keeps the selective gaps below. Its reviewed identity `f3886f65568469a0e8d4c0a1d56e12f5916415cd7fb328cc9c61c7778f5f2135`, and a later fixture full suite that failed on `processReload` with empty child output, are historical. Both primary suites for `011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b` exited 0. That pair supersedes the older `processReload` failure, so `processReload` is not the current blocked cause. The current open acceptance is checkpoint B, which is still running. A missing selector is not a missing product capability. Do not mark these labels as operator passes from source or guard evidence.

- `source-compact`: `TestOCGNativeDispatchGuardFixture` shared compact, `overlay/sdk/cliproxy/auth/ocg_fixture_rewrite_test.go`. Direct guard `GenerationKindExecute`, with no compact executor or HTTP. Existing upstream `CodexExecutorCompact` and `AntigravityCompactionAltResponsesCompact` tests sit outside the recorded SDK run filter. They do not prove complete native OAuth or operator compact. Compact executor execution remains an operator gap.
- `internal`: `TestOCGNativeDispatchGuard` parent and internal keep separate permits, `ocg_dispatch_test.go`. Separate ids and consumption on a direct SDK guard, with no native provider HTTP. Configurable HTTP `TestReloadStampBlocksHeldFollowup` and internal-resend do not substitute for native internal execution.
- `refresh-resend`: `TestNativeRefreshResendAndFacts`, `TestNativeOrdinaryRefreshKeepsRegistrationEpoch`, and `TestNativeValidatedPinRefreshStaysOnPinnedAuth`. Executed Go synthetic Codex non-stream token refresh, 401, second send, material, epoch, usage, and pinned auth A, with no B and no evil. Normal CLI, real OAuth, and every provider remain open.
- `stream-refresh`: upstream `TestManager_ExecuteStream_UnauthorizedRefreshesCurrentAuthBeforeFallback`. Source-only in this map, outside the recorded SDK filter, with a mock executor. Executed native HTTP stream refresh and a final pin remain open.
- `original-host`: mismatch and host mutation do not consume, and a tagged bad host is not repaired. Executed guard rejection, no consumption, and rewrite, with no physical HTTP. The public CLI cannot independently set the child outbound Host.
- `generation-kinds`: positive `TestNativeAntigravityExecuteReportsChatPin` helper branching. An executed kind-specific grant narrowing or revocation test is absent. The helper branch is not enforcement evidence.

`stream-bootstrap` stays pending until `Codex.StreamBootstrapBuffering` is shown enabled and that stream runs. That pending row is enablement evidence. CLI import, discovery, grants, and apply remain evidence gaps. The scenario list stays whole.

## Request debugging and log levels

Set the capture variables on the `cargo run` yourself. Nothing on this branch
turns them on:

```bash
OCG_DEBUG_REQUESTS=1 OCG_LOG_LEVEL=debug OCG_DEBUG_DIR="$PWD/.artifacts/debug-requests" \
  cargo run -p ocg-cli -- --data-dir tmp/dev-data serve --port 19042
```

Startup output shows the active path and level. `OCG_DEBUG_REQUESTS=0`
disables capture. A normal `serve` does not enable capture and defaults to
`info`.

Each authenticated, within-limit inference POST saves a `client` JSON file before
protocol parsing. Each prepared upstream attempt saves an `upstream` file after
protocol conversion and wire normalization, before the final authorization check.
Files share the `x-ocg-request-id` response header and include attempt number,
URI, headers, and complete JSON content (messages, tools and inline media), with
credential fields and known authentication secrets redacted. An upstream file
means a prepared attempt, not proof that it was sent. Invalid JSON/binary bodies
are explicitly marked `invalid_json_omitted` with length and hash; unauthorized
and over-limit bodies are not captured. Response bodies/SSE are not captured or
buffered. This is content-level debugging, not byte-identical HTTP packet capture.

Capture files contain private conversation content. The default directory is Git
ignored; keep any override in a private directory. Files inherit the directory's
Windows ACL; on Unix new directories/files use modes 0700/0600. Completed files
are published by rename; failed writes warn without failing forwarding. After
each successful write, the oldest owned captures are removed to retain the latest
1,000 files (client and upstream files count separately). This is a file-count
limit, not a byte quota. Files ending in `.partial` are incomplete, not captures.

`OCG_LOG_LEVEL` sets the minimum persisted runtime severity at startup:
`trace`, `debug`, `info`, `warn`, or `error`. `trace` adds content-free request
shape/fingerprint; `debug` adds reception, attempt preparation and upstream
headers/timing; `info` includes response readiness and completed attempt outcomes;
`warn`/`error` retain rejection/failure diagnostics. Response readiness is not
stream completion: the attempt outcome records the later stream result.
Existing lifecycle/control-plane events use the same threshold. Request accounting
and billing rows remain independent of this filter. In **Logs → Runtime logs**,
level, category, and request ID filters apply before the latest-200 limit.


---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](development.zh-CN.md) · [Docs index](../README.md)
