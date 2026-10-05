# Failed-apply explain fixture triage

**BLOCKED on identifying the firing exclusion from the supplied diagnostic.** Source establishes that the fixed-date clock hypothesis does not explain this posture failure. It does not establish a production defect or a particular fixture correction. No source edits, tests, Cargo/Go commands, runtime, listener, Git, network, credentials, or delegation were performed. The six-file f315 source verdict is unchanged.

The primary reports `cpa_execution::explain::tests::failed_apply_keeps_applied_client_route_distinct_from_desired` failed at `crates/ocg-core/src/cpa_execution/explain/tests.rs:955`, receiving Excluded where Client was expected. The diagnostic contains no structured exclusion list. This reviewer did not rerun the binary.

## What the source proves

`World::explain` passes the fixed `clock()` explicitly at explain/tests.rs:93–103. `explain::read_snapshot` constructs desired and applied configuration authority at explain.rs:469–472 without a time argument. Time is used later by quota decoration at lines 698–702. Route posture is copied directly from the authority proof at line 733; quota flags affect client_configuration_eligible, not posture. In `identity/authority.rs:670`, Excluded means the authority proof's exclusions vector is nonempty. Therefore expiry of the fixture's `future()` date under the actual October 5 wall clock cannot itself produce this Excluded-vs-Client assertion.

The inspected native grant reader (`identity.rs:1744–1768`) reads only kind/value and has no expiry or wall-clock test. Its coverage check is endpoint/origin membership. The separate callback deadline code is not used by this read-only static proof.

`build_record` starts from Record::empty and supplies owned_origin, applied generation/revision/digest, capabilities, applied auth/routes and native OAuth presence at explain/tests.rs:481–568. It leaves artifact/listen port/public origin/runtime verification absent. Those omissions can prevent runtime readiness; they are not inputs to the static posture branch. Runtime/coherence processing does not rewrite Client to Excluded: the facade's suppression at explain.rs:347–350 changes eligibility, while static posture remains the proof's value.

The expected native identity is populated consistently in the inspected fixture: canonical row and applied stamp/set use credential `cred-native`, version 3, binding `bind-native`; auth id is `native-auth`, both auth/OAuth epochs are 4, OAuth presence is Present with native provider codex, and the endpoint-pin capability is supplied. The catalog and explicit endpoint/origin grants are seeded at explain/tests.rs:318–332. Those values satisfy the visible identity/presence preconditions of `authority::aligned_stamp` and `identity::native_face_block` when the seeded row and retained record are read unchanged. The deliberately differing material strings in the helper are not compared by the inspected native-face alignment branch (`identity.rs:2174–2180`) and do not by themselves explain this failure.

The posture can still be excluded by the current canonical row/binding/setup/scope, route/catalog/protocol, native presence/alignment, target/mode, or grant branches. Naming one without the actual structured proof would be speculative. The result could also come from a binary/source mismatch or a changed fixture state; neither is established by the supplied assertion.

## Minimal next evidence and correct contract

The active source/runtime owner should preserve the assertion and expose the already nonsecret native RouteFact on failure (at least exclusions, current_version, material, grants_cover, and native operation dispositions), then run this exact named test with a verified nonzero matched-test count and record the binary/source identity. This requires no production API, clock workaround, credential access, or wider kernel audit. This reviewer has not edited that diagnostic or requested a test run.

An applied static Client proof remains valid only if current version/binding, setup/scope, catalog/protocol, native presence/identity, targets and grants are coherent. A failed desired apply does not erase a still-valid applied proof, but it does not confer runtime readiness or revive invalid authority. Preserve distinct desired/applied assertions. Do not move the dates or relax native/version/grant fences as a guessed correction. If the diagnostic shows one of those facts is actually malformed, fix the fixture to supply the accepted nonsecret facts required by that existing branch; if it shows a valid fact rejected, investigate that specific consumer as a bounded production defect.

The exact minimal proposed test-only replacement for the line 955 assertion is below; it is **not applied**:

```rust
assert_eq!(
    native.posture,
    RoutePosture::Client,
    "native exclusions={:?}; current_version={:?}; material={:?}; grants_cover={}; native_operations={:?}",
    native.exclusions,
    native.current_version,
    native.material,
    native.grants_cover,
    native.native_operations,
);
```

One exact-case command for the primary's later hermetic fixture grant is below; it is **not executed** and does not imply a current build lease:

```powershell
cargo test -p ocg-core --lib --features ollama-cloud-loopback-test cpa_execution::explain::tests::failed_apply_keeps_applied_client_route_distinct_from_desired -- --exact --nocapture
```

Record the actual test result/count and exit directly; do not pipe it through a success-returning filter. Supply the established fixture environment isolation before execution rather than adding process-global changes here.

## Source status

The three initial bounded source hashes matched on closing read:

```text
cpa_execution/explain/tests.rs 30D199516EF1300605A452858A4546204354BFA89F4EA105EEBDA9F2FB631D05
cpa_execution/explain.rs 35E7E3270E3B68E886D4549A49EE3C2BB99F3D2DC3221B12FBB4CC79E91ACE97
cpa_execution/identity/authority.rs 3E6F607B10F7BAF067920E0B8A326F80632195EC8DCC0059F88B0B44AFB510BE
```

Additional targeted consumers had the same hashes at their first recorded hash and closing read: identity.rs `0219B01E1129043EBA05006F624CA0983C75502D9BDF57A5FB7A7A9EBEE2A24F`; cpa_projection/native_targets.rs `74EB769A2FA2CA9FE00840FAECD27D4A8CFAFEA3879FDB222A44CB4063192FC0`. Other supporting initialization/store reads were source-only. No unexpected drift was observed in the hashed inputs. No rollback or publication action is proposed; runtime acceptance remains primary-owned.
