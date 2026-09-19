# Routing lab

Black-box runtime check of real gateway routing against synthetic loopback upstreams. It does not touch production source or the official account library.

This directory is a **compatibility entry**. The simulator, Dashboard V4 client, and scenario runner live in [`tools/gateway-lab/`](../../tools/gateway-lab/README.md). `node scripts/routing-lab/run.mjs` still writes `.artifacts/routing-lab/` and keeps the previous CLI flags.

The orchestrator process hosts three independent loopback listeners with a shared event journal, then starts an isolated CLI gateway with a fresh data directory and dummy credentials.

## Replay

Pass a gateway executable. Parent should supply a freshly built CLI when the current `target/debug` binary is older than the dirty tree.

```powershell
node scripts/routing-lab/run.mjs --cli ".\target\debug\ocg-manager-cli.exe"
```

`--shadow` sets `OCG_SHADOW_COMPARE=1` on the freshly spawned gateway only. The default launch still strips that variable. Cleanup always stops the PID this script started and verifies listener plus gateway ports are closed. SIGINT/SIGTERM run that same owned cleanup; this lab never broad-kills unrelated processes.

## What it checks

- Chat / Responses / Messages 3x3 JSON+SSE conversions, model mapping, provider-specific fake auth, `store=false` on Responses conversions
- Gemini `generateContent` JSON+SSE onto the Chat upstream when the binary accepts it
- Direct listener negatives and revoked-grant / unknown-model zero-send
- Strict-priority stop on success, 429 fallthrough, **no replay on 503**, **no replay on post-connect drop**
- Reordered accounts, disabled account skip, binding `modelScope` skip
- Shared-quota sibling skip via `POST /dashboard/api/v4/identities/{id}/credentials` with `quotaSharing.kind=shared`
- Sticky-global and round-robin account selection

Listeners bind ephemeral `127.0.0.1` ports. Ports `19143` / `19144` are never used.

## Evidence

Generated data and reports stay in `.artifacts/routing-lab/`; each run replaces only that laboratory's dedicated `data/` directory.

- `report.json` — every scenario, binary SHA-256, ports, PIDs
- `journal.json` — chronological upstream hits
- `runtime.json` / `cleanup.json` — owned PIDs and stop proof
- `RESULTS.md` — human summary
- `data/` — isolated dummy gateway directory
