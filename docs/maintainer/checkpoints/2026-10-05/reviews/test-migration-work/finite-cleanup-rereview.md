# Finite cleanup and exact-grant correction re-review

Verdict: **READY — source-only**, after task6f9's exact one-file correction to taskccd3891b64. Bounded source review against `reviewed-source-before-finite-revisions` and the prior 21-file freeze. No Cargo, runtime, source edit, credentials, Git or delegation. Candidate remains uncompiled/unrun.

## Resolved P2: BlackBox unwind closure now owns the harness

`crates/ocg-core/tests/fixtures/blackbox/harness.rs:577` now moves the harness into a local `_harness` inside the caught closure. `fail_setup` borrows that local at `:578`; its panic unwinds the local and invokes harness Drop before returning to the post-catch assertions. External state/path clones remain available for those assertions. The former borrowed-outer-harness defect is resolved in source.

The source correction changes only this ownership transfer and the borrow target. Existing assertions and `fail_setup` are unchanged. It preserves the caller Drop listener-ownership contract and matches the V3 fixture's moved `OwnedProfile` pattern at `v3_runtime_invariants.rs:1186`. No compile or runtime success is claimed.

## Prior findings resolved in source

- BlackBox normal shutdown and Drop now share one child → listener → fake-upstream → profile cleanup path (`fixtures/blackbox/harness.rs:386-414`). Idempotence flags prevent a normal explicit cleanup followed by Drop from repeating operator Stop. Existing users do not move out a non-Copy harness field; no direct compile defect was found by bounded caller inspection.
- `fixtures/v3/harness.rs:39-67` introduces an owned profile guard. All 15 previous V3 teardown sites now stop child/listener before their existing fake stop/profile delete. Constructor returns and destructuring agree. Its Drop covers unwind and repeats no product Stop after explicit cleanup.
- `gateway_fallback.rs:3963-3983` captures account/binding identity, enabled flag, scope, endpoint/origin lists and credential version. It seeds and verifies nonempty endpoint/origin sentinels before taking the snapshot, then compares the full tuple after refusal (`:4070-4086`, `:4118`). Existing integration/catalog/cipher/CAS equality assertions remain. No retired table, fake authority, live activation or remote send is introduced.
- The keep-directory host-exit path and previously verified run-intent preservation remain unchanged. No production lifecycle implementation was edited.

## Fixture limits and pending work

The four new cleanup fixtures register a real owned host but do not install/start an accepted child. Their running=false assertions therefore cover empty-host cleanup and listener/profile ownership, not termination of a previously running child. Later accepted-child teardown evidence remains required; this review does not upgrade them to that proof. Listener stop is signal-only in these synchronous Drop paths; actual listener/task exit and Windows directory deletion still need the granted behavior run. No assertion was removed to avoid those checks.

The other 16 files were independently hash-verified unchanged from `remaining-source-independent-review.md`. After task6f9, the other four correction files also match the prior finite correction hashes. All 21 entry/exit hashes matched during this final bounded read; no scoped drift occurred. Only these five hashes differ from the initial remaining-source freeze:

| Path under crates/ocg-core/tests | SHA256 |
| --- | --- |
| fixtures/blackbox/harness.rs | 2be006ed8a829d6ee6a2eb53e121472f19d3680783c2c721eb184232aa9a23ee |
| fixtures/owned_cpa.rs | 9655b2781dbbf979f62d6cf750c82bdf37d5be552e07c01fe622f14a5a7bf779 |
| fixtures/v3/harness.rs | 3710d8bf77967416ae084f0e5b97a64a551841be72d347c8ef865c5e7a8b0c1d |
| gateway_fallback.rs | bb94993a0568335be1fad42250d74bab6ba3607fc641a0a8fd30e3c9b188355e |
| v3_runtime_invariants.rs | 2051d04d39d4052b025de593155bc532ea823656785cb0232b06f1783d298a95 |

Hermetic official-endpoint mapping, managed staging/completion CAS semantics, scheduling/no-replay/quota compatibility, coherent compilation, both variants' tests, and full CLI acceptance remain separate primary-owned work. Original before-byte snapshots remain recovery evidence; this test-only revision introduces no product migration.
