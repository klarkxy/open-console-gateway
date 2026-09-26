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
relative `--data-dir` / `DSH_HOME` values. The default smoke installs into
`web` and a selected `coding` profile. Run
`node scripts/dsh-headless-cli-smoke.mjs --scan-user-homes` to verify selection
across isolated `.dsh` and `.dsh-editor` Homes.

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
---

[Maintainer guide index](../MAINTAINER.md) · [简体中文](development.zh-CN.md) · [Docs index](../README.md)
