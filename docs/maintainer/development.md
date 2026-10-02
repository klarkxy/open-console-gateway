[简体中文](development.zh-CN.md)

# Development

## Prerequisites

Node.js 22, the `packageManager` pin in `package.json`, and the workspace
`rust-version` (Rust 1.88 or newer, as required by the locked dependencies).
Native packages are whatever `.github/workflows/release.yml` installs on that
runner.

## Dev Loop

Quit the installed tray app so it does not hold the single-instance lock or
port `9042`, then:

```bash
pnpm install
pnpm run dev
```

`pnpm run dev` runs `tauri dev` with `OCG_GATEWAY_PORT=19042`, a separate
development default. On some Windows hosts, HNS/WSL/Docker reserves port
ranges that include `9042`; the development default can avoid that conflict.
Installed builds still default to `9042`. Vite serves
`http://127.0.0.1:30001/dashboard/` and proxies `/dashboard/api` (including
WebSockets) to that gateway port. Override both Tauri and Vite with
`OCG_GATEWAY_PORT` before starting; Settings shows the effective port as
read-only while the variable is set.

### Choosing a dev mode

- `pnpm run dev` (default): Tauri watches the Rust workspace and rebuilds and
  restarts the whole desktop app on change, which drops the in-process
  gateway and every in-flight request. Extra arguments forward to the Tauri
  CLI: `pnpm run dev -- --no-watch` disables the Rust watcher so a running
  dev build keeps serving until you restart it manually; Vite HMR for the
  dashboard still applies, and saved Rust changes take effect only on the
  next manual restart.
- `pnpm run dev:split`: a headless `ocg-manager-cli` gateway plus Vite, with
  no Tauri process. This is the mode for dashboard, HTTP API, and
  routing/protocol work. The gateway listens on `OCG_GATEWAY_PORT` (default
  `19042`) against an isolated data directory (`tmp/dev-data`, override with
  `OCG_DEV_DATA_DIR`), so it can run alongside the installed app. Nothing
  watches Rust sources: after changing gateway crates, stop the script and
  rerun it to rebuild `ocg-manager-cli`. The dashboard at
  `http://127.0.0.1:30001/dashboard/` proxies to the split gateway, and Vue
  changes still hot-reload. On the first run against a fresh data directory,
  retrieve the development Gateway Key from a private terminal with
  `target/debug/ocg-manager-cli --data-dir tmp/dev-data status --show-key`.
- Desktop host work (tray, autostart, native browser, updater) still needs
  `pnpm run dev`: the CLI does not register those host capabilities.

The split gateway is a separate process from any gateway your agents use.
Restarting it still ends its in-flight streams; keep agents on the installed
app or another long-lived instance when they must not be interrupted.

`pnpm install` enables `.githooks` (`cargo fmt --all` on staged `*.rs`).

For managed CPA, click **Start** once after upgrading to startup recovery.
A successful manual start is remembered across Tauri backend rebuilds: each
new backend restores CPA in the background using its existing configuration
and auth directory. **Stop** clears that intent. Host exit still cleans up
its child, and failed recovery is reported once without a restart loop.
Do not run another CPA instance against the same auth directory to work
around development restarts.

## Checks

`package.json` scripts are the names to run. Pick the smallest check that
covers the changed boundary:

| Change | Check |
| --- | --- |
| One frontend or script test | `node --experimental-strip-types --test <file>` |
| Vue / dashboard | adjacent test, then `pnpm run build:web` |
| One Rust crate | `cargo test -p <package>` |
| Core / Dashboard V3 | `cargo test -p ocg-core --features ollama-cloud-loopback-test <filter>` |
| Desktop Host | `cargo test -p ocg-manager --lib` |
| V3 or V4 schema or generated types | `pnpm run contract:v3:check` / `pnpm run contract:v4:check` |
| `DESIGN.md` / theme | `pnpm run design:lint` |

`pnpm run test` is the cross-frontend/Rust gate. `pnpm run test:rust` and
the quality.yml Linux Rust job pass
`--features ocg-core/ollama-cloud-loopback-test` so the Ollama Cloud gateway
integration suite can install its loopback-only test seam. That feature is
default-off: application builds keep the fixed `https://ollama.com` origin
and do not compile the seam. A workspace `cargo test` without the feature
still compiles `ollama_cloud_gateway` but runs none of its cases.

`pnpm run test:tooling` covers `scripts/*.test.mjs` and is already included
in `pnpm run test` and the Quality workflow; do not run it again after a
passing full test.

`pnpm run build` is native release packaging (`scripts/release.mjs`). Workspace
`[profile.release]` uses thin LTO, `strip`, and `panic = "abort"`.

Use Node.js 22 locally as in CI. Run Cargo tests, Clippy, contract generators,
and native builds sequentially when they share `target/`; keep the same build
configuration to reuse compiled work. After a fix, rerun the affected checks;
the final main Quality run supplies the full release gate. See the
[release procedure](releasing.md) for which checks belong locally or in CI.

## DSH Plugin Contract vs Real Smokes

`pnpm run test:dsh:plugin` is the same isolated plugin contract test that
`pnpm run test:tooling` already runs (`scripts/dsh-plugin-package.test.mjs`).
It does not start DSH, the Gateway, or any credential-dependent path.

The following commands are **manual acceptance smokes**. They need a real DSH
CLI (the installed version is reported, not pinned) and/or a locally built
`target/debug/ocg-manager-cli`. They are not part of `pnpm run test`,
`pnpm run test:web`, `pnpm run test:tooling`, or CI.

```bash
pnpm run smoke:dsh:plugin
pnpm run smoke:dsh:cli
```

`smoke:dsh:plugin` uses the installed DSH CLI (Windows:
`%APPDATA%/npm/node_modules/@deepseek-ai/dsh/lib/bin.js`) with an isolated
`DSH_HOME` and a loopback models/chat stub. `smoke:dsh:cli` drives
`GET|POST|DELETE /dashboard/api/v4/applications/dsh` against a native
`ocg-manager-cli serve` with `dsh-local-host`. Pass `--expect-unsupported` when
the CLI was built without that feature, or `--relative-roots` to exercise
relative `--data-dir` / `DSH_HOME` values. The default smoke checks profile
discovery, verifies that stopped Web profiles require a live session, and runs
the Web HTTP lifecycle through OCG. Run
`node scripts/dsh-headless-cli-smoke.mjs --scan-user-homes` to verify selection
across isolated `.dsh` and `.dsh-editor` Homes.

`node scripts/dsh-web-runtime-smoke.mjs --ocg` exercises install, replacement,
and removal through the OCG V4 API against an isolated installed DSH Web
runtime. Add `--desktop` to exercise the installed official Desktop Host.
These smokes use temporary profiles and do not call a real model provider.

Rust unit tests live in sibling `tests.rs` modules (`src/db.rs` declares
`mod tests;` and the tests are in `src/db/tests.rs`). Do not add tests that
assert on source text, workflow YAML, or documentation prose.

CLI sandbox (OpenCode Go cards only; no Custom, sub keys, or settings):

Debug `serve` builds skip automatic user-skill synchronization. Native release
`serve` builds and desktop startup synchronize the embedded skill; use an
isolated `USERPROFILE` (Windows) or `HOME` (macOS/Linux) for release smokes.
Explicit `skill sync` always writes to the selected user home, including in
debug builds. The sample Key below is synthetic; do not put real secrets in
agent-run command arguments.

```bash
ocg-manager-cli --data-dir /tmp/ocg-cli-test key add smoke sk-smoke
ocg-manager-cli --data-dir /tmp/ocg-cli-test serve --port 19042
```

Direct `Database::update_account` does not bump revision; that is
intentional and is not the CLI path.

## Local Unsigned Smoke (Windows)

Quit the installed release from the tray. Align versions in `package.json`,
`src-tauri/tauri.conf.json`, both `Cargo.toml` files, and
`compose.example.yaml`, then `pnpm run build`.

Without `TAURI_SIGNING_PRIVATE_KEY` the script writes plain local packages
that cannot drive in-app upgrades. Optional signing variables:
`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`,
`TAURI_UPDATER_PUBLIC_KEY` (must match
`src-tauri/updater-public-key.sha256`), and
`OCG_REQUIRE_UPDATER_ARTIFACTS=1`.

A local Tauri build may rewrite `src-tauri/Cargo.toml` and
`src-tauri/gen/schemas/*.json` — keep only the intended edits.

## Request Debugging and Log Levels

Program diagnostics use `tracing` on stderr, initialized by the desktop, CLI,
and browser worker before startup. `RUST_LOG` controls this channel independently
of dashboard rows: the normal default is `warn,ocg=info`, and the development
scripts default to `warn,ocg=debug`. Explicit values override those defaults;
for example, `RUST_LOG=warn,ocg_core::gateway=trace` enables detailed gateway
events. Invalid filters fall back to the normal default and emit a warning.
Each line includes a timestamp, severity, and source; request events add the
request ID and attempt. Request bodies, Keys, and upstream error content do not
belong in program logs. CLI command results and native startup prompts retain
their existing output. This channel does not create a second SQLite log store.

The desktop process that owns the single instance, and a normal native CLI
process after it resolves the data directory, also append
`<data-dir>/logs/program.log`. Rotation happens before a formatted event would
pass 10 MiB and keeps that active file plus `program.log.1` through
`program.log.4`. The process holds `<data-dir>/logs/.program-log.lock` for its
lifetime and does not remove that file. Each event is buffered only up to 10 MiB,
cut on a UTF-8 boundary with a `[truncated]` marker, then sanitized once before
either output. Sanitization strips terminal escapes, redacts credential and
header fields, redacts URL userinfo, and omits body and payload values. The same
text is written to stderr and the file; a closed stderr does not drop the file
write. A symlink, junction, or other reparse point on `logs` or any ancestor
refuses the sink and does not change the link target. Existing directories and
log files are made private (Unix `0700`/`0600`, or a protected Windows DACL for
the current user and LocalSystem) or the sink stays off. An existing
`program.log` or archive already larger than 10 MiB also refuses activation and
is left byte for byte as it was. `OCG_PROGRAM_LOG_FILE=off` leaves the process
on stderr only. Docker's `--no-default-features` CLI build omits the
`program-log-file` feature, so it stays stderr-only, as do the CPA supervisor,
browser workers, and a secondary desktop instance. If the file sink cannot
lock, open, tighten permissions, or rotate its files, it disables itself and
stderr continues.

The old `OCG_LOG_LEVEL` environment variable applies only to compatibility
database writes of historical mixed records. No new
mixed `gateway_logs` rows are written by application code. Operation receipts
are in `operation_logs`; logical requests are SQL projections over the original
`forward_logs` attempts, with no second request store. Neither channel uses the
program severity filter. The Logs page defaults to logical requests, with
separate operation and historical tabs. Base history is not automatically
purged. Existing `diagnostic_json` expires after 30 days at database open; its
base row and totals remain.

`pnpm run dev` defaults to `OCG_DEBUG_REQUESTS=1` and
`OCG_DEBUG_DIR=<repository>/.artifacts/debug-requests`. It displays the program
filter and capture directory. Explicit values override these defaults;
`OCG_DEBUG_REQUESTS=0` disables captures. Normal CLI and installed startup do
not enable content capture. `OCG_LOG_LEVEL` is no longer defaulted by dev.

An authenticated, within-limit inference POST saves a `client` JSON capture
before protocol parsing. A prepared upstream attempt saves an `upstream`
capture after conversion, before final authorization; that file does not prove
the request was sent. Records contain request ID, attempt, URI, headers, and
JSON content, with credential fields and known authentication secrets redacted.
Response bodies and SSE are never captured or buffered. Malformed/binary or
oversized content retains length, hash, and an explicit omission marker.

Each encoded attachment is at most 2 MiB; owned captures together are limited
to 100 MiB, 7 days, and 1,000 files. Startup and writes prune only OCG-owned
regular files, including older UUID names and abandoned owned `.partial`
files. Foreign files, links, and subdirectories are left alone. There is no
background cleanup timer. A process-scoped mutex and an operation-scoped file
lock protect pruning/publication across writers; contention skips that capture.
The directory and files use private permissions, including a protected owner
and LocalSystem DACL on Windows. Empty files are private before content is
written. Symlinks/reparse ancestors are refused. Publication uses rename, and
failure warns without changing forwarding. Captures contain conversation
content; the default directory is Git ignored.

`RUST_LOG` trace events contain body length and fingerprint only. Debug events
describe reception and preparation; info describes response readiness and
attempt outcomes; warnings/errors describe failures without upstream content.
Response readiness is not stream completion. Request accounting and usage
remain independent of this filter.


---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](development.zh-CN.md) · [Docs index](../README.md)
