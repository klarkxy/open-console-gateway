# Native acceptance gates plan review

Verdict: **READY** for the bounded source-only construction plan with the primary's appended finite correction controlling the earlier internal-consumer wording. The proposed leaf is proportionate to the full usable owned-CPA CLI outcome. This is `plan_review` only; no implementation audit, tests, builds, process execution, network, credentials, Git or delegation were performed. Only this normal review artifact was written. No unresolved plan defect remains in this bounded review.

## Resolved plan finding

**P2 — public Antigravity compaction does not establish a nested internal-kind admission.** Plan line 25 names Antigravity compaction as the reachable consumer for distinct parent/child permits. In the exact accepted `011f3602...` tree, `internal/runtime/executor/antigravity_executor_execute.go:233-241` calls `AdmitInternalGeneration` only when `CurrentAttemptID(ctx) == ""`. A public manager admission already sets the attempt id; this compaction branch then calls the summary Execute using that existing context. It cannot be credited as observing a distinct internal child merely because the trigger performs compaction HTTP.

The helper itself requires an existing internal gate/manager; without one it returns the original context (`sdk/cliproxy/auth/ocg_attempt_boundary.go:423-436`). A direct ordinary executor test without that gate likewise does not prove an OCG internal admission.

The appended `Finite independent plan correction` now resolves this: it separates public compaction's actual execute/stream permit from the private native-internal contract. An explicit SDK `AdmitInternalGeneration(parent, auth, model)` followed by the real native executor HTTP and result can exercise distinct child/parent permits, child refusal and no parent reuse, with its evidence labeled SDK native-internal. Automatic internal-kind execution by a product consumer requires that consumer's branch to be actually traced and observed. Do not activate fail-closed websockets or add a production selector/API to manufacture this evidence. The public Antigravity trigger remains a necessary, independently useful CLI gate. The appended correction overrides the earlier line 25 consumer claim.

No additional serial plan-review gate is required: this finite wording/case split has been incorporated into the plan and must carry into the precise construction packet.

## Current evidence actually credited

The current `runtime-build/primary-registration-mutation-suite-exits.json` identifies accepted build `011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b`, production/fixture exit 0, host times 85.060/78.033 and SDK times 0.141/0.123. `primary-registration-mutation-production.log` shows the unfiltered host command and the SDK command:

```text
go test -p 1 -count=1 -timeout 15m
go test -count=1 -timeout 15m -run TestOCGNativeDispatchGuard ./sdk/cliproxy/auth
```

The builder has those exact distinct commands (`scripts/build-cpa-runtime.mjs:106-108`). Accordingly:

- The in-package host tests `TestNativeRefreshResendAndFacts` (`runtime/cpa/review_gaps_test.go:32`), `TestNativeOrdinaryRefreshKeepsRegistrationEpoch` (`native_registration_fence_test.go:18`) and `TestNativeValidatedPinRefreshStaysOnPinnedAuth` (`validation_bridge_test.go:291`) are covered by the unfiltered host suite. Their inputs establish synthetic native Codex HTTP 401, token refresh/resend, material/epoch facts, and pinned account/endpoint behavior within their actual assertions. They are not real OAuth/login, every-provider refresh, or normal CLI import/apply evidence.
- The selected SDK tests establish the existing dispatch guard, separate direct parent/internal permit checks and bad-host/context consumption behavior (`ocg_dispatch_test.go:84-142`). They do not themselves establish native executor HTTP for internal/compact or a real transport counter.
- Upstream SDK mock stream-refresh and executor compact tests were **not** selected by that SDK filter. The executor package was not that command's package. Their presence in the frozen tree is source evidence only.

The `native-harness-work/internal-evidence-map.md` is explicitly historical f388 evidence. `primary-registration-go-test-receipt.md` is historical b903 evidence. Neither was used as current011f acceptance. The current plan correctly warns against that promotion.

## Feasibility and false-pass controls

The plan correctly identifies two current driver classifications: `assertSourceRefusal` checks counters without invoking a request (`scripts/cli-cpa-acceptance.mjs:3327-3334`), and `assertStaleVersion` makes a stale binding CAS update (`:3386-3396`), not a held credential resend. Keep their classification/CAS evidence while adding actual reachable public refusal requests. The existing revoked-grant pattern already uses binding controls, automatic-apply barriers, a public call, and physical zero-hit checks (`:3397-3417`); it provides a suitable bounded pattern. Wrong-mode/base cases must not require an invented request model if no authority was created; record actual unavailable/ungranted state and any reachable affected public model separately.

Public Antigravity Responses compaction is reachable without a new route: the actual executor tests for `opts.Alt == "responses/compact"` or `HasResponsesCompactionTrigger` (`frozen antigravity_executor_execute.go:42-43`, stream at 43-44). Its HTTP summary and returned capsule behavior need a real public trigger and branch-specific hit/result checks. SDK Codex/XAI manager Alt fixtures can close the private selector contract at their stated evidence level; direct executor/custom-base tests alone cannot close manager-boundary enforcement.

Native HTTP stream-refresh is feasible through existing ExecuteStream, synthetic token refresh and a real native executor. Require observed `stream` then `stream-refresh`, distinct attempt IDs, each final pin and A/B/evil counters. A status-401 error returned directly by ExecuteStream selects stream-refresh (`frozen conductor_stream.go:281-292`); it must not be counted as bootstrap.

Bootstrap is also source-feasible without a production change: Codex can return an initial error chunk from a native HTTP-200 SSE authentication failure before any translated payload (`codex_executor_stream.go:204-227,338-354`; terminal mapping recognizes authentication error as 401 at `codex_executor_terminal.go:178-195`). Manager `readStreamBootstrap` sees the error before its first payload (`conductor_stream.go:113-118`), and an authorized refresh can then select `GenerationKindStreamBootstrap` (`:374-395`). The fixture must avoid a prior handshake payload, which would commit the stream instead. Config enabled plus an ordinary successful stream is not enough. Record the actual branch/counters and retain the plan's limitation on claiming normal-host coverage only when that configuration and branch are observed there.

For every lane, record whether authority/pins came from the current Rust product control path or a synthetic SDK boundary. The latter can establish the sender/guard contract but must not be mislabeled as CLI import/grant integration. This follows the plan's evidence separation, not a requirement for a new operator selector. Require actual test package, filter/tags, selected test names, source identity, exit and branch/hit assertions in the eventual receipt; a zero-match test command or test presence is not a pass.

## Added public rollback regression

The primary's added line 42 is necessary and feasible using existing catalog/binding/runtime controls. Current C only rolls a sticky-setting digest (`cli-cpa-acceptance.mjs:1823-1837`), which cannot prove restoration of a different typed authority map. A={a}, B={a,b}, physical b success under B, then retained a success plus b zero-I/O denial after rollback is the right missing outcome; b remaining saved/published distinguishes applied authority from current desired configuration.

The final appended ordering now explicitly requires all binding/scope/endpoint setup before accepted A, then B as one existing saved catalog mutation immediately following A and its barrier. This avoids another automatically applied setup mutation replacing `previousConfig` with an intermediate plane. Save A's selected typed map/digest before B; prove B's distinct upstream marker, rollback's selected A map, a's authorized physical send and b's failed send with zero new upstream hits. Finish with an explicit apply of current saved B or restoration of the owned controls and a barrier. No fixture API, new store or production route is needed. A failure is a product/source issue for the existing owner, not a reason to downgrade this to digest-only evidence.

## Ownership, acceptance and recovery

The source-only constructor may edit the specifically granted C driver paths and new test adjunct files in an isolated copy of the exact frozen source. It may not run them, edit shipping Go/lock/artifacts, use real credentials, or overlap 29742's CPA/runtime ownership or 8aa's retained-script paths. The final primary packet should name those precise paths, the frozen-copy provenance, and the existing owner who will later run the selected checks. This is routine contract completion, not an expanded approval flow.

Missing reachable public import/apply/grant/generation/compaction/rollback behavior and uncovered critical native final-dispatch/no-replay/authority fences remain necessary acceptance work. Private contracts may be closed by exact executed SDK/host evidence without a manufactured public interface. Live entitlements, real provider login/refresh, sustained operation and native Linux/macOS hardware remain explicit limits; compiled artifacts or synthetic fixtures do not establish them.

The proposed machinery stays test-only and reuses existing manager/executor/control APIs. Dropping an isolated adjunct is the recovery action; no product data migration, shipping artifact rollback or new authority is involved. If the new gates expose a production defect, return it to the primary/active owner for a separate bounded source correction. Do not silently edit the accepted Go candidate.

## Review input status

The primary appended the necessary rollback section and then the independent plan correction during this review. The plan hash changed from `913E6BDA471F8746B0E165325176623211A8CDDA2E2E13DA6685BC0B1510D30F` through intermediate `EBBB4D7467DD8EA19F5C180B5EC869943D23FC133BEAAF125329F4FA31457414` to final reviewed `1E11AC7736B4AB192414C09DDFC8EECCED5257ECC36E8A6F5D4A2A9BCC4A54B9`. The appended correction and strengthened rollback ordering were read and included in this READY verdict. No unrelated drift was observed or claimed. This is plan review, not immutable implementation-diff proof.

Supporting closing hashes:

```text
scripts/cli-cpa-acceptance.mjs C510A1EA3C24CA6AC10CB864F8AD9FA89DD32B42189691C4CD64B85DDDE10D69
scripts/build-cpa-runtime.mjs EBA03C9C64830EB439289B7515EB523F205C49C3E7A1F60D5ACA090276728BD4
runtime-build/primary-registration-mutation-suite-exits.json D93733429F8E500939F839B41FAA6E74DC6D39C22C527219BBF1A627061C79D6
```

Primary retains final contract, integration, runtime/test slots, Git and full CLI acceptance. No accepted fullCLI or adjunct execution is implied by this plan review.
