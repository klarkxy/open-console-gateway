[简体中文](development.zh-CN.md)

# Development

## Prerequisites

Node.js 22, the `packageManager` pin in `package.json`, and the workspace
`rust-version` (Rust 1.88 or newer, as required by the locked dependencies). Native packages are whatever
`.github/workflows/release.yml` installs on that runner.

## Dev loop

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
the quality.yml Linux Rust job pass `--features ocg-core/ollama-cloud-loopback-test`
so the Ollama Cloud gateway integration suite can install its loopback-only
test seam. That feature is default-off: application builds keep the fixed
`https://ollama.com` origin and do not compile the seam. A workspace
`cargo test` without the feature still compiles `ollama_cloud_gateway` but
runs none of its cases. `pnpm run test:tooling`
covers `scripts/*.test.mjs` and is already included in `pnpm run test`
and the Quality workflow; do not run it again after a passing full test.
`pnpm run build` is native release packaging
(`scripts/release.mjs`). Workspace `[profile.release]` uses thin LTO,
`strip`, and `panic = "abort"`.

Use Node.js 22 locally as in CI. Run Cargo tests, Clippy, contract generators,
and native builds sequentially when they share `target/`; keep the same build
configuration to reuse compiled work. After a fix, rerun the affected checks;
the final main Quality run supplies the full release gate. See the
[release procedure](releasing.md) for which checks belong locally or in CI.

## DSH plugin contract vs real smokes

`pnpm run test:dsh:plugin` is the same isolated plugin contract test that
`pnpm run test:tooling` already runs (`scripts/dsh-plugin-package.test.mjs`).
It does not start DSH, the Gateway, or any credential-dependent path.

The following commands are **manual acceptance smokes**. They need a real DSH
CLI (the installed version is reported, not pinned) and/or a locally built `target/debug/ocg-manager-cli`. They
are not part of `pnpm run test`, `pnpm run test:web`, `pnpm run test:tooling`,
or CI.

```bash
pnpm run smoke:dsh:plugin
pnpm run smoke:dsh:cli
```

`smoke:dsh:plugin` uses the installed DSH CLI (Windows: `%APPDATA%/npm/node_modules/@deepseek-ai/dsh/lib/bin.js`)
with an isolated `DSH_HOME` and a loopback models/chat stub. `smoke:dsh:cli`
drives `GET|POST|DELETE /dashboard/api/v4/applications/dsh` against a native
`ocg-manager-cli serve` with `dsh-local-host`. Pass `--expect-unsupported` when
the CLI was built without that feature, or `--relative-roots` to exercise
relative `--data-dir` / `DSH_HOME` values. The default smoke checks profile discovery, verifies that stopped Web profiles require a live session, and runs the Web HTTP lifecycle through OCG. Run
`node scripts/dsh-headless-cli-smoke.mjs --scan-user-homes` to verify selection
across isolated `.dsh` and `.dsh-editor` Homes.

`node scripts/dsh-web-runtime-smoke.mjs --ocg` exercises install, replacement and removal through the OCG V4 API against an isolated installed DSH Web runtime. Add `--desktop` to exercise the installed official Desktop Host. These smokes use temporary profiles and do not call a real model provider.

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

## Local unsigned smoke (Windows)

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
## Request debugging and log levels

`pnpm run dev` also sets `OCG_DEBUG_REQUESTS=1`, `OCG_LOG_LEVEL=debug`, and
`OCG_DEBUG_DIR=<repository>/.artifacts/debug-requests`. The startup output shows
the active path and level. Explicit environment values override these defaults;
set `OCG_DEBUG_REQUESTS=0` to disable capture. Normal CLI/installed startup does
not enable capture and defaults to `info`.

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
