# Independent native CLI gates change review

Verdict: **REVISE**. One verified false-pass defect remains in the new archive attribution gate. This is bounded `change_review` of the frozen C driver diff, not plan review or runtime acceptance. No product, Cargo, Go, listener, network, credential access, Git mutation, source edit, or delegated work was performed. Only this normal review artifact was written.

## Frozen scope and identity

Reviewed only `scripts/cli-cpa-acceptance.mjs` against `integration-work/primary-before-native-gates.mjs`, using the native construction packet, accepted plan and independent plan review, and source checkpoint. Targeted existing Rust control/ingress and exact accepted 011f executor sources were read to resolve executable semantics.

- Baseline SHA256: `C510A1EA3C24CA6AC10CB864F8AD9FA89DD32B42189691C4CD64B85DDDE10D69`.
- Reviewed current SHA256: `8FFCD54B5B29FE99F5020FD3E29D74B6D5CD933D67925AC08D9D8696995B60AF`.
- Both hashes matched before and after review. The assigned script remained untracked in Git (`?? scripts/cli-cpa-acceptance.mjs`). Initial workspace status was recorded with substantial pre-existing unrelated WIP; no unrelated changes were edited or audited.
- Assigned diff: one script, 445 insertions and 30 deletions; existing 137 catalog cells remain.

## Verified finding

**P2 — a known seeded bearer mismatch still passes archive restore.** `scripts/cli-cpa-acceptance.mjs:4091-4103` supplies a known owned fixture token for the supported archive families. `:4117-4128` compares hashes and returns `matched: false` when the actual physical bearer differs, but `:4280-4285` ignores `matched` and unconditionally emits `native-oauth-archive-restore` PASS. This means an otherwise successful restored send using a different synthetic credential can pass the newly requested physical credential attribution gate. A LIMIT string inside a PASS detail is not an assertion or an essential open blocker.

The default family order selects Codex first (`:2081` and `:4130-4134`); its owned seeded access token is deterministic from `nativeSourceDocuments()` (`:2708-2719`). The permitted in-memory check confirmed `physicalBearerAttribution('codex', seededArchiveToken('codex')).matched === true` and the same helper with a different synthetic bearer returns false. No tokens were printed or persisted.

**Essential revision:** require a matching physical bearer hash when the seeded token and selected fixture identity are unambiguous. A known mismatch must fail this attribution gate, or remain an explicit required OPEN/BLOCKED result that cannot pass. If identity mapping is genuinely ambiguous, record that exact limitation separately and keep attribution incomplete. Preserve the other archive assertions and hash-only diagnostics; do not invent DTO file-path or material-fingerprint evidence.

## Other changed paths checked

- Rollback setup is before accepted A. The A-to-B interval contains one catalog PUT (`:1911-1917`) and only observations/client sends otherwise. Unique model and response markers, accepted A digest, B physical send, saved B after rollback, applied A explain, A physical send and B zero-I/O refusal are present. Original catalog controls are restored with an automatic-apply barrier in `finally`. The existing catalog endpoint accepts these edits and notes apply once (`dashboard_v4/destination_catalog.rs:119-148`); edit enable flags are honored (`account_control.rs:438-445`). Routing `eligible` uses applied facts, while desired facts are separately exposed (`dashboard_v4/routing.rs:161-181`). These are source checks, not proof that rollback actually ran.
- The two public Antigravity compact cells use `/v1/responses` plus `compaction_trigger`, normal JSON and streamed SSE, with parent execute/stream labels and `internalChild: false`. The exact accepted 011f executor detects the trigger in execute at lines 42-43 and stream at lines 43-44. Stream compaction performs a summary Execute with the existing context (`antigravity_executor_stream.go:306-313`); normal compaction adds internal admission only without an existing attempt id (`antigravity_executor_execute.go:233-241`). The new capsule/completion checks match the frozen response helpers. No internal child credit is implied.
- SOURCE classification and stale-binding CAS now carry distinct evidence levels. The changed public wrong-mode/base paths invoke a request if a published model exists, otherwise report unavailable authority without substituted target or fabricated send. Unknown Antigravity alt invokes the public Gemini stream endpoint; Rust ingress forwards noncredential query fields (`gateway/cpa_ingress.rs:509-518`). Negative paths retain zero physical I/O checks, and a later unrelated Codex send is present. Their actual runtime refusal behavior remains UNRUN.
- Private SDK/host requirements produce OPEN rows (`:4347-4353`). They keep full native completion incomplete and exit 2 (`:5273-5279`), rather than turning successful operator work into a blanket throw or closing private gates. `closeout: false` and `wholeCli: incomplete` remain explicit. Existing transport/no-replay/cancellation/deadline/quota/reset/feature-off and the prior six archive corrections were not removed by this diff.

## Verification and residual limits

Permitted checks only, all exit 0:

- `node --check scripts/cli-cpa-acceptance.mjs`.
- `node scripts/cli-cpa-acceptance.mjs --list`, after inspecting the return before `prepareProfile` at `:5238-5245`.
- A source evaluation in memory with `main().catch` removed before import. It called only catalog/plan/request-shape and synthetic hash helpers; no fixture file or product path was started. It observed 137 cells, 90 callable cells, 78 network, 12 local-unsent, 10 SOURCE classifications, 42 pending SDK rows and 2 public compaction cells; no internal client sends were added.

All physical rollback, native compaction, public negatives, archive attribution, and separate exact current SDK/host receipts remain runtime-owner acceptance work. This review grants no runtime credit and cannot establish real OAuth, provider entitlement/refresh, native cross-platform operation, sustained operation or complete CLI acceptance.

## Recovery

The change is test-driver only and adds no production API, migration, store or authority. Reverting the assigned script to the preserved baseline removes these gates. Fixture control restoration uses existing CAS/apply barriers, and native cleanup remains in existing finally blocks. No irreversible action or publication is authorized by this review. Primary retains integration, Git and final acceptance.