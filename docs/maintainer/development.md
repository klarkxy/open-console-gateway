[简体中文](development.zh-CN.md)

# Development

## Prerequisites

The workspace `rust-version` is Rust 1.88 or newer, as required by the locked
dependencies. This branch has no root `package.json`, Vue workspace, or Tauri
crate. Node 24 is used only for the optional `scripts/cli-acceptance.mjs` check.
The former pnpm scripts return when that frontend work is restored.

## Dev loop

Typecheck, test, and build the headless CLI with Cargo. The feature-off build
drops the default `dsh-local-host` feature, so local DSH and BYOK configuration
are unavailable; inference, the control plane, browser and CPA remain. Run the default binary against an isolated data directory. Do not use
an installed data directory for development:

```bash
cargo check -p ocg-manager-cli --locked
cargo test -p ocg-manager-cli --locked
cargo build -p ocg-manager-cli --locked
cargo build -p ocg-manager-cli --locked --no-default-features
cargo run -p ocg-manager-cli -- --data-dir tmp/dev-data serve --port 19042
```

On some Windows hosts, HNS/WSL/Docker reserves port ranges that include
`9042`; `19042` avoids that conflict. Nothing watches Rust sources: stop the
process and rerun the command after changing gateway crates. A fresh host
creates its primary Gateway Key during state initialization. Read
`GET /dashboard/api/v4/connection` into a private file with `api --output`. The [CLI guide](../user/cli.md) uses
synthetic placeholders for the request bodies. Do not put a key on the command
line.

`.githooks` runs `cargo fmt --all` on staged `*.rs` when Git hooks are enabled.

`serve` restores an owned CPA runtime when it starts and shuts that process
down when it exits. `POST /dashboard/api/v4/external-integrations/cpa/runtime/stop`
clears the stored intent. Failed recovery is reported once and is not looped.
Do not point another CPA instance at the same auth directory.

## Checks

Pick the smallest Cargo check that covers the changed boundary:

| Change | Check |
| --- | --- |
| One Rust crate | `cargo test -p <package> --locked` |
| Core behavior | `cargo test -p ocg-core --locked --features ollama-cloud-loopback-test <filter>` |
| CLI | `cargo test -p ocg-manager-cli --locked` |
| CLI typecheck | `cargo check -p ocg-manager-cli --locked` |
| CLI build | `cargo build -p ocg-manager-cli --locked` |
| Feature-off CLI | `cargo build -p ocg-manager-cli --locked --no-default-features` |
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
`test:tooling` commands belonged to the removed frontend workspace. Restore
the Node generator and generated TypeScript checks with the future frontend.
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
result from that script.

```bash
node scripts/cli-acceptance.mjs target/debug/ocg-manager-cli tmp/cli-acceptance
```

On Windows the binary name is `target/debug/ocg-manager-cli.exe`. Omit both
arguments to use that default binary and a timestamped directory under
`tmp/ocg3-cli-delivery/`.

## Request debugging and log levels

Set the capture variables on the `cargo run` yourself. Nothing on this branch
turns them on:

```bash
OCG_DEBUG_REQUESTS=1 OCG_LOG_LEVEL=debug OCG_DEBUG_DIR="$PWD/.artifacts/debug-requests" \
  cargo run -p ocg-manager-cli -- --data-dir tmp/dev-data serve --port 19042
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
