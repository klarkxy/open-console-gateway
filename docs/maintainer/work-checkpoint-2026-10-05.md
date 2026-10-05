[简体中文](work-checkpoint-2026-10-05.zh-CN.md)

# Work checkpoint — 2026-10-05

Development was paused at the user's request before reinstalling the workstation. This commit is a recoverable work-in-progress snapshot, not a release or a completed CLI acceptance. No version bump or release tag is intended. Preserve the complete source and documentation changes already present in this branch.

## Fixed product decisions

- The executable is `ocg` / `ocg.exe`, crate `ocg-cli`, default profile `~/.ocg3`; retain explicit `--data-dir`, existing profile formats and `OCG_MANAGER_ENCRYPTION_KEY`.
- `ocg3` and `open-console-gateway` are different generation branches of the same project.
- One locally owned CPA is the sole execution foundation. OCG owns configuration, access Keys, public models, Plan evidence and runtime lifecycle. No external CPA product mode, outer OCG retry/fallback or restoration of the former kernel on CPA failure.
- Complete headless CLI acceptance before native GUI implementation. GUI target remains Rust/GPUI/Ely, Windows/Linux/macOS, A overview home, C Plan master-detail and optional B compact. The native GUI is not implemented.

## Evidence at pause

| Area | Observed status |
| --- | --- |
| Pinned CPA | Build identity `011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b`, CPA v8.0.10 at upstream commit `6fecc6e5567912661654a4eaf9b8f5436facd1c2`. Four artifact records are in `runtime/cpa/artifact-lock.json`. |
| CPA checks | Prior primary runs: unfiltered host suites passed in 85.060/78.033 seconds; SDK checks covered **only** `TestOCGNativeDispatchGuard`, 0.141/0.123 seconds. These do not establish the new adjunct tests or whole CLI acceptance. |
| Rollback/backup source | Independent six-file review closed accepted-port/envelope recovery, persistence before publishing readiness, snapshot identity/artifact/read bounds, and private YAML Debug redaction. Eight exact focused fixture cases were reported passed; whole Rust quality remains open. |
| General CLI journey | G3 and G5 reached 68 passing stages, then failed at restore. Pre-restore JSON/stream inference, credits, transfer, client configuration and restart observations passed. G3 sent one premature restore request (503); G5 correctly withheld it when actual readiness had not arrived within 20 seconds. |
| Restore diagnostic | Unchanged production binary and restored profile automatically reached installed/running/owned state after about 29 seconds (32 seconds wall time), revision 3→4 and matching applied/desired digest. No manual control start or inference request was used. This establishes delayed automatic recovery, not successful restored inference. |
| Final G attempt | Ready wait was increased to 120 seconds; backup create/restore have 180-second bounds. G6 was interrupted at pause after DSH setup. Restore inference and browser process/profile cleanup remain unaccepted. |
| Native CLI gates | `cli-cpa-acceptance.mjs` source review passed, including the corrected seeded-bearer mismatch gate. Primary Node syntax/list/in-memory checks passed: 137 cells, 90 callable, two public Antigravity compaction cells, 42 private SDK/host requirements still open. Physical execution is unrun. |
| Retained controls | `cli-retained-controls-acceptance.mjs` source review passed, including seeded Codex BYOK journal recovery with a fresh-fingerprint user-edit refusal. All 17 runtime stages remain unrun. |
| Native SDK adjuncts | Three test source files passed bounded independent source reviews. They remain **uncompiled and unrun**. Test-local transport counters are separate from native executor server-arrival evidence. No product-bypass finding or shipping Go patch was established. |
| Hermetic Rust migration | Partial, uncompiled work in seven files; the worker connection stalled before completing consumers and fixtures. The current tree is not claimed buildable. |
| Platforms | Linux/macOS CPA artifacts were cross-compiled; no native device acceptance. No real provider OAuth, installed profiles or sustained production operation was tested. |

The last G binary was a default-feature debug build: SHA-256 `2e8fd0a9d9324fd9a7e0f719f2f67dfda0a5fb3ddb38e95bf1e597f2fb4ed2d5`. Accepted Windows CPA SHA-256: `8648fd683cef40bb3ffc8dc9af68caf9f7d5a8737cf442dd575669f730f0fc6a`. These binaries are local build outputs, not included in this source checkpoint.

## Partial migration and remaining work

The partial changes affect `cpa_test_endpoints.rs` and its tests, `state.rs`, `usage_sync.rs`, and `cpa_projection/{types,build,endpoints}.rs`. The selected design uses a feature-only per-instance immutable endpoint map, existing parser, `Arc` and `OnceLock`. Installation must precede first render; duplicate/late/racing installs reject, malformed/absent captured authority remains correctly sealed, and missing participating built-in endpoints fail closed. No fixture metadata is persisted.

Finish threading the same authority through projection render, callbacks, live identity, validated sends and explanation, then migrate harnesses and managed verification. Install controlled usage seams before gateway startup. Preserve measured staged/complete CAS revisions and distinct entry-conflict versus held stale-completion cases. Add only the typed disabled-binding model-test refusal; other infrastructure failures retain service errors. The legacy global managed target override must be retired after callers migrate. Do not weaken retry, quota, cancellation, deadline or no-replay assertions.

Two focused tests still need precise diagnostics: `failed_apply_keeps_applied_client_route_distinct_from_desired` (Excluded versus Client) and `accepted_history_rollback_restores_the_selected_map` (reason classification). Clock expiry was ruled out for the first posture failure; its firing exclusion was not identified. Preserve assertions and capture structured nonsecret facts before choosing a fix.

The deep synthetic directory exposed Windows atomic-replace failures beyond the normal Win32 path limit. Shorter fixture directories passed all four BYOK configuration/removal journeys; extended-path product compatibility is a separate unresolved observation. Do not change global Windows settings as an implicit repair.

## Resume after reinstall

1. Clone the repository and switch to branch `ocg3`. Read [architecture](../architecture.md), [migration](cpa-migration.md), [development](development.md) and this checkpoint. Do not infer acceptance from target documentation.
2. Read the archived plans and reviews in [checkpoint evidence](checkpoints/2026-10-05/README.md). They are historical evidence and selected contracts; old worker IDs and runtime leases are closed. Use fresh task identities and one source owner per path.
3. Complete the partial hermetic migration, compile affected consumers, then run finite local fixtures before broad Rust quality checks. Source/compile/runtime receipts remain distinct.
4. Rebuild the pinned CPA from the committed host, patch sources and artifact lock using `scripts/build-cpa-runtime.mjs`; recreate production and native-loopback fixture trees. Verify provenance and hashes. No publication or relock merely to rename evidence labels.
5. Copy archived native adjuncts into a fresh isolated pinned SDK/host tree; compile and run the seven SDK cases and two host cases with exact nonzero test counts. The archived copy manifest records the prepared baseline; no test binary was compiled before pause.
6. Finish actual G restore inference/browser cleanup, native C operator gates and all retained-control stages. Aggregate their private SDK/host evidence honestly; source-only OPEN rows do not count as PASS.
7. Finish default/no-default feature quality, schema comparisons, former executor retirement with behavior proof, paired documentation and a local Windows bundle with colocated CPA discovery. No bundle or release was accepted before pause.

Source, test adjuncts, selected plans and bounded reports are committed. Build/dependency caches, CPA binaries, isolated databases, snapshots, real credentials and user/global agent configuration are excluded. Recreate disposable evidence on the new workstation; do not expect `tmp/` or `target/` to survive a clone.
