# Gateway Lab

Headless, development-only CLI that stands in as multiple loopback Gateway upstreams. It is not a product UI, not a daemon, and not a substitute for the real Gateway.

Default mode is **deterministic local simulation**. Remote MiniMax traffic happens only when a live scenario is armed and both environment variables below are set.

## Commands

```powershell
node tools/gateway-lab/cli.mjs serve --profile default
node tools/gateway-lab/cli.mjs register --gateway http://127.0.0.1:<port> --runtime .artifacts/gateway-lab/<run-id>/runtime.json
node tools/gateway-lab/cli.mjs scenario --runtime .artifacts/gateway-lab/<run-id>/runtime.json --name http_429 --endpoint chat --model upstream-chat
node tools/gateway-lab/cli.mjs reset --runtime .artifacts/gateway-lab/<run-id>/runtime.json
node tools/gateway-lab/cli.mjs verify --cli .\target\debug\ocg-manager-cli.exe --suite local
```

`--suite` is `local` (default), `live`, or `all`.

Compatibility entry (same black-box routing suite as before):

```powershell
node scripts/routing-lab/run.mjs --cli .\target\debug\ocg-manager-cli.exe
```

## What it is

- Node.js 22 built-in HTTP / fetch / `node:test` only. No extra packages, database, container, or UI.
- Bind address is loopback only (`127.0.0.1`, `::1`, `localhost`). Non-loopback `--host` / profile hosts are rejected. Inference listeners use ephemeral ports. A separate loopback **control** port owns reset/script/journal. `scenario` requires `--endpoint` (or `--listener`) plus `--model` or `--key`; a missing selector is a validation error, not an unmatched empty isolation key.
- Default profile: independent Chat Completions, Responses, and Messages endpoints (own Key, own `/v1/models` catalog, own public model) plus a shared public model `lab-route` across routing slots.
- Three model identities: public name (Gateway client), upstream name (Gateway → lab), live outbound `minimax-m3`. A custom live model is **invalid, not configurable** — profile and client construction reject any other value. Returns rewrite **protocol model fields only**, never message text or tool arguments.
- `register` uses Dashboard V4 + CAS (`POST /dashboard/api/v4/onboarding/commit` and the normal enable path). It does not write SQLite.
- In-memory lab state. Restart is a reset. Artifacts: `.artifacts/gateway-lab/<run-id>/`.
- Cleanup stops only PIDs and listeners this process started.

## Local vs remote evidence

| Suite | Remote HTTP | Pass condition |
| --- | --- | --- |
| `local` | Must be **0** | Synthetic JSON/SSE only. `remoteCalls=0` is asserted. |
| `live` | OpenAI Chat Completions compatible POST to `OCG_LAB_REMOTE_CHAT_URL` with `OCG_LAB_REMOTE_KEY` | Real remote status. A remote failure is a failure, never a local fake success. |
| `all` | Local first (zero remote), then live if configured | Live missing env is `NOT_RUN`, not a pass. |

Live defaults: concurrency 1, `max_tokens` 512, timeout 60s, at most 24 remote calls per verify, no retry. Chat / Responses / Messages are converted by this lab’s own adapters; Gateway conversion is the system under test, not an oracle.

Reports never include Keys or full prompts. They store SHA-256 fingerprints / prompt digests.

## Report statuses

`PASS` / `FAIL` / `UNSUPPORTED` / `NOT_RUN`. Only `PASS` counts as a pass. Any `UNSUPPORTED` or `NOT_RUN` row makes the process exit **2**. Failures exit **1**. The local report includes a coverage checklist keyed by stable `scenarioId` and `evidenceKind` (`gateway_black_box`, `rust_integration`, `lab_fixture`, `live_remote`). Direct-to-lab fixture checks cannot satisfy Gateway behavior. Named matrix rows that were not executed are `NOT_RUN` and therefore also exit 2. Local verify may spawn exact `cargo test -p ocg-core --test <file> <fn> -- --exact` commands as `rust_integration` evidence.

Each scenario records binary identity, expected vs actual upstream hits, remote call count, and a replay command.

## Out of scope

- Production Rust / Vue / schema / `package.json` changes
- Fixing Gateway product defects (those are `FAIL` rows with a replay command)
- Reading or logging real Keys from disk
- Auto-recovery, daemons, containers, release integration
- Using Gateway protocol code as the live conversion oracle

## Tests

```powershell
node --test tools/gateway-lab/test/*.test.mjs
node --test scripts/routing-lab.test.mjs
```
