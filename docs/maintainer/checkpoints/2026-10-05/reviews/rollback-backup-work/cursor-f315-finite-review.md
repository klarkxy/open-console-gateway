# Cursor f315 finite change review

**READY for the assigned source checkpoint.** No remaining verified defect was established in the four corrections or the revised fallback fixture. This is not build, test, physical runtime, or full CLI acceptance.

Reviewed only the immutable six-file copies in `integration-work/cursor-f315-review-candidate`, comparing their actual bytes with `primary-interrupted-rollback-source` and the prior finite-source-review.md and fallback-current-credential-review.md. References below are relative to the candidate's `crates/ocg-core/src/`. No moving live source was used for this verdict. No source edits, builds, tests, runtime, network, credentials, Git operations, or delegation were performed.

## Closure of the four source findings

1. **Accepted recovery envelope and map:** `cpa_execution.rs:1800` restores the retained accepted snapshot before recovery launch, and `restore_accepted_envelope` at line 1888 installs its listen port, owned/public origin, applied auth/routes, and applied generation/revision/digest together. Recovery launch uses the restored port at lines 1803–1825; Ready acceptance uses that restored Record at line 1839. The snapshot is the one captured from the matching accepted current YAML before the candidate fields change at lines 1610–1645. Failed desired state stays separate. Thus the previously demonstrated candidate-port publication path is corrected.

   The new test `failed_apply_restores_the_accepted_port_not_the_candidate` at `cpa_execution/tests.rs:3262` holds an actual loopback listener on A's port during candidate reservation, forcing a different candidate port. It asserts restored YAML/report/Record/connection agree on A's port, applied auth/routes include A and exclude B, and A's callback remains eligible while B's is refused. Its synthetic host and callbacks do not prove a real child send, but the fixture meaningfully exposes the original field mismatch.

2. **Persistence before Ready:** `cpa_execution.rs:1864` checks the coherent recovery save before publishing verified_ready. On failure it marks apply_failed, clears policy_ready, sets unavailable, clears coherence, and publishes Failed with verified_ready=false at lines 1875–1884. A best-effort second failure-record save does not convert that state back to Ready. The new test at `cpa_execution/tests.rs:3361` targets recovery through the test-only `persist` hook: initial pending persistence still goes through `store::save` directly at `cpa_execution.rs:1650`, so enabling the hook before the second start does not merely fail the initial candidate write. The fixture asserts durable apply_pending, unavailable/non-ready memory, and connection rejection. HookGuard now resets the new fault flag on entry and drop.

3. **Snapshot validation:** `cpa_execution/store.rs:628` rejects contradictory credential/version/binding/material fields when a route set shares a keyed stamp's auth_id. It does not require native-only route sets to have keyed stamps. The new matching/contradictory and native-only/None fixtures at `store/tests.rs:300` and line 349 reflect that distinction. The existing accepted-empty representation is unchanged. `cpa_execution.rs:566` compares the matching historical snapshot artifact SHA with the current Record's permitted artifact before returning a rollback source; mismatch returns RollbackUnavailable before cancellation or mutation. The foreign-artifact fixture at `cpa_execution/tests.rs:4647` checks both current and previous file bytes and the durable Record remain unchanged. `store::parse` at line 291 now checks MAX_CONFIG_BYTES before deserialization; staged rebase also rejects oversized input at line 794. The oversized parse/rebase fixture is at `store/tests.rs:390`.

4. **Private YAML Debug:** `cpa_execution/project.rs:682` implements manual Debug, replacing yaml with a fixed redacted marker and retaining only digest/generation/revision metadata. The formatting fixture at `project/tests.rs:174` checks provider, hop, Ready, policy secrets and the full YAML are absent. This closes the source-level diagnostic formatter issue; no observed real credential leak was claimed.

## Fallback fixture and boundaries

`coherent_apply_fallback_serves_the_previous_child_until_credentials_move` retains the initial connection and coherent failure-restoration assertions at `cpa_execution/tests.rs:3159–3188`. It now adds an unchanged credential, asserts rotation returned the same canonical id and increased credential/auth-state versions at lines 3212–3215, and checks the retained applied tuple/map stayed unchanged. It permits the coherent transport connection, then submits a fresh correlated old-stamp callback: exact identity_fence, no eligible/Allow response, and no admitted pin are asserted at lines 3247–3253. The unchanged credential has a separate eligible callback at line 3255. This is the requested meaningful replacement for the obsolete blanket connection-error expectation and does not weaken production version/material/epoch gates.

The withdrawn native epoch hypothesis stays withdrawn. No native-only stamp restriction, new authority, service, store, production API, or outer retry was added by these corrections. The actual production edits in the six-file diff are bounded to the reviewed recovery, snapshot validation, and formatter changes; other observed changes are sibling fixtures, test-only fault controls, and formatting.

## Verification and remaining acceptance

The new fixtures are meaningful source constructions, not executed evidence from this review. A zero-case filtered command or shell pipeline exit cannot count as a test pass. This report credits neither compilation nor worker PASS claims without the primary's exact matched-test counts and command exits. Real restored-port inference, rotated stale route zero physical sends with unchanged-route success, A/B rollback/reopen/failure behavior, backup inference and native execution remain the primary's runtime acceptance work. The G fake-browser driver under active construction is outside these copies and this verdict.

These changes are local and reversible, retain the existing storage schema and one-slot history, and leave failures explicitly unavailable rather than reporting Ready after an unsuccessful recovery save. No publication or deployment is authorized. A later live Rust revision needs comparison/resynchronization against this checkpoint before this verdict can apply to it.

## Immutable evidence

All six entry and exit SHA-256 values matched the candidate inventory; no drift was observed. Full digests and copied paths are recorded in `integration-work/cursor-f315-review-candidate/inventory.json`:

| Candidate file | Entry = exit SHA-256 |
| --- | --- |
| cpa_execution.rs | E54A91BC30F2130A761FA9AC62F83762C13A4B0617BF317CE1C3182C9BE9EAF7 |
| cpa_execution/store.rs | 0C526A45E4DCB01AD6D3B4D58F7DF99B1D9D053148005E6AB3A97A64102F0257 |
| cpa_execution/store/tests.rs | 2745B46C91E07D70B9883ACA24614749C2C5D25BF5803EE5FB47CB45B4845F5C |
| cpa_execution/project.rs | 76148F0DC2D75D25ED2410AEE7DC38B331DC29E052A7AA8D01BAB8946ABA2E2E |
| cpa_execution/project/tests.rs | 58D66EFB332DF58C16067BDD3CD2781CE5FC991FC04A8B6BD8A75A7099C8E3FE |
| cpa_execution/tests.rs | 58EAE6FBDA49B191A67220FCEF3B458F09A563AA4C470492B6EABE0262E4572E |

