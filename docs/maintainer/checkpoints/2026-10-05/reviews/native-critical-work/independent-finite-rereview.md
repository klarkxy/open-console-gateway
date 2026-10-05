# Native critical adjunct finite source rereview

Verdict: **REVISE**. Mode: `change_review`, limited to the six prior fixture blockers and their directly changed assertions. Reviewed current actual files, not the worker verdict. All runtime evidence remains **UNRUN**. No build/test/Go/Cargo/runtime/listener/network/credential/Git/source edit/delegation. Only this new review artifact is written; independent-source-review.md stays readonly.

Abbreviations: `H` = sdk/cliproxy/auth/ocg_native_critical_http_helpers_test.go, `A` = sdk/cliproxy/auth/ocg_native_critical_http_test.go, `N` = host/native_critical_stream_refresh_test.go under integration-work/native-critical-work. `T` is the frozen runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b source tree. These are relative to tmp/ocg3-cli-delivery/orchestration-20261004, except explicitly repository paths.

## Minimal remaining fixture blockers

### P1 — full-URL arrival assertions canonicalize a relative server request

H:225 and N:169 call CanonicalDispatchURL(r.URL.String()) inside httptest handlers and discard the error. Ordinary direct Go HTTP server requests have origin-form URLs such as `/responses`, with no URL Scheme or Host. Frozen T/sdk/cliproxy/auth/ocg_endpoint_pin.go:311-326 rejects empty URL Host/scheme and returns an empty canonical string. Consequently H's requireExactArrival (:610 onward) and N's full canonical equality assertion compare empty to a nonempty `http://127.0.0.1:<port>/responses`; the SDK refresh/bootstrap/compact and host positive assertions cannot pass on their intended direct request.

Revision: construct the observed absolute URL using the actual receiving scheme, r.Host and r.URL's escaped path/query, preserving exact path/query and force-query semantics; handle canonicalization errors. Do not fill the recorded field from the expected URL, which would make the check tautological. Existing canonical pin fingerprints remain the final-dispatch evidence. This is a source-supported fixture defect, not a runtime failure observed here.

### P1 — registered B outranks A, so the new fixture selects the wrong initial account

H:430 registers A priority `1`; H:434 registers B priority `2`. Both advertise the same callable model and are active. FillFirstSelector at T/sdk/cliproxy/auth/selector.go:812-820 uses getSelectorAvailableAuths; selector.go:553-577 narrows to the **highest** numeric priority tier. The scheduler agrees (scheduler.go:1595-1596 sorts priority descending). Thus SDK manager tests select B first. Their boundary gives only A endpoint pins, so the real B executor target is refused before reaching held A; positive A tests fail and held-A waits time out.

N repeats A=1/B=2 at :208-213 and :387-392. Repository runtime/cpa/host.go:833 copies binding.Priority directly into the auth attribute. Host tests therefore also select B, and strict policy can authorize B's own target, causing the explicit B error counter rather than the intended A first 401. Existing accepted pinned/replay fixtures use A=10/B=1, for example runtime/cpa/validation_bridge_test.go:344 and native_xai_replay_test.go:44-49.

Revision: give intended initial A a higher priority than competing B (or use an existing explicit pin if that is the intended contract), while keeping B registered and eligible. Retain actual initial credential/arrival assertions so a selection mistake cannot be mislabeled as resumed denial. This is a fixture input defect; production priority behavior must not change.

### P2 — refusal must be observed; arming it and seeing one hit/error is insufficient

The revised source configures the correct named refusal, but assertions still permit a return before that refusal is ever evaluated:
- A:135-141 accepts generic stream termination and one first hit; its stream-refresh result check is conditional on a result existing. It does not require a new stream-refresh admit or the current_authority_denied decision.
- A:318-329,366-375,393-402 arm refresh-resend/stream-refresh/stream-bootstrap denial and check only an error plus one extra initial hit. A refresh/token/bootstrap failure before the armed boundary would pass those checks. The lastResult test at :326 can simply see the previous successful attempt and skip its assertion.
- N's runHostHeldDenial final loop (:457 onward) only rejects a success+sent result *if* a stream-refresh admission exists. No admission at all passes. The heldCurrentAuthority callback at :303 onward captures a live auth snapshot rather than checking its identity against the actual first policy admission.
- H:533-557 requireStreamTerminated permits err=nil with a cleanly closed stream containing neither a chunk error nor response.completed. N uses a similar “no completed frame” test. Such an empty stream is not proof that the intended refusal caused termination.

Revision: for held current-authority and named resumed-kind negatives, require the actual newly observed named boundary/policy decision to be stop/current_authority_denied, with distinct attempt identity and captured/live epoch/version as applicable. Assert its context has null matched pin and sent=false, and require the returned error to indicate the intended refusal. Cancellation/deadline should instead require their expected context error; removed auth may terminate before refreshed admission, so do not fabricate a denied-admit record for it. Compare host captured identity to the actual first policy admission. This can use test-local decision/refusal records plus existing exported facts; no product API or synthetic result publication is required. Preserve the honest distinction between a refused admission that emits no result and an actual executor result.

These are repairs within prior blockers 4-6 and direct changed assertions, not an expanded Go audit.

## Six prior blockers: bounded disposition

1. **Import cycle: corrected at source level.** H/A now use `auth_test`, and the calls use exported Manager/Auth/GenerationBoundary/pin/result symbols. The native executor may import auth without cycling through auth's package implementation. Inspected frozen ProviderExecutor signatures in conductor.go:16-31; observeCodex implements Identifier, Execute, ExecuteStream, Refresh, CountTokens and HttpRequest with matching types. Result.Success/epoch/version/material fields and GetByID/RequestStopped/pin/facts/internal helpers exist. No additional missing-symbol issue was identified in this bounded read. This is API source feasibility, not compiler acceptance.

2. **Explicit empty/count pins: corrected at source level.** H:pinsFor returns empty for count, setEmpty removes execute pins, and A reads the real ExecuteCount admission context and AfterResult. The later direct native Execute with that unpinned count context is expected to refuse; no second unrelated count context substitutes for the result.

3. **Internal parent/child stop semantics: corrected at source level.** A:190-284 first performs allowed child Execute+PublishInternalGeneration and parent Execute, then separately refuses an admitted child's final wrong-pin dispatch and expects the parent to stay unconsumed/stopped. It no longer expects parent success after terminal child refusal. Result/pin checks are now present. The A/B priority error prevents actual execution of this intended branch until corrected.

4. **Authority replacement attribution: corrected claim; observation remains incomplete.** Replacement now asserts a changed registration epoch and unchanged credential version and arms an existing boundary/policy stop. The receipt correctly retracts confirmed product-bypass/unconditional captured-epoch-abort claims. Fresh authorized replacement and ordinary OAuth refresh remain separate. Require the actual refusal observation described above before crediting this gate.

5. **Named-kind/results/transport: materially corrected, with scoped limits.** The seven negative triggers now target their named kind, positive AfterResult/pin/sent checks exist, explicit internal publication is present, and B is registered. Correct priority/canonical capture and strengthen observed decisions as above. The count fixture checks actual unsent/null state and server zero-I/O; it does not observe a native transport invocation counter.

6. **Held sequencing: corrected setup order.** The 25ms setup-before-call deadlines and 40ms timing sleeps are removed. Tests finish setup, wait for a real first request, then mutate/cancel/expire/revoke and release it. sequencedContext/hostSequencedContext are test-local contexts injecting cancellation/deadline state after first arrival; this is synthetic SDK/host context evidence, not persisted Rust deadline or sustained-runtime proof. Termination/refusal assertions still need the finite correction above.

## Guard / transport evidence classification

The worker's stated Codex RoundTripper limit is honored. `fx.transport` is a **test-local** counting RoundTripper; it is not installed into Codex's NewUtlsHTTPClient. In missing/wrong cases, the manager calls the real native executor and measures server arrivals, then separately calls this transport on the captured, already stopped context. In the Host mutation case, observeCodex.beforeExecute builds a mutated request and uses that local transport; the real native Execute is deliberately skipped after the hook.

Accordingly:
- Native executor missing/wrong-pin paths: real executor/guard and zero **server arrivals**.
- Local RoundTripper: explicit guard/invocation/pass-through contract counters.
- Host mutation: exported final guard + conditional local transport, **not native executor transport interception**.
- Actual count: local CountTokens/result facts and zero generation server arrivals, **not a measured Codex RoundTripper invocation count**.

Receipt/runtime reporting must keep these levels distinct. No product export is needed or authorized to manufacture a stronger label. The file's name cannot certify a native transport counter; its current OPEN paragraph accurately states this limitation.

## Verification, stability and recovery

No test/build command was run; Go compile feasibility was checked only by public signatures and package dependencies. Supplied task result had unavailable workspace snapshot provenance, so its files_changed list was not treated as immutable-tree proof. Actual source hashes matched the primary packet. Four adjunct hashes were captured at entry and rechecked; the original review hash is unchanged. A bounded 18-file hash set includes the four checkpoint files, original review, finite packet/result, relevant API/priority/pin sources, host helper input and root lock. Closing rehash reports zero drift, exit 0. The report is the sole write.

Frozen Go/root lock/shipping artifacts stay unchanged. Recovery is another finite test-only correction or removal of the unintegrated adjuncts; no product rollback/data migration/relock is needed. Root owns later isolated-copy grant and execution. This verdict establishes neither SDK/host PASS nor full CLI acceptance. Real OAuth/provider entitlements, Linux/macOS hardware and sustained behavior remain outside these synthetic fixtures.

Repository-relative SHA-256 values observed at closing:

```text
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/sdk/cliproxy/auth/ocg_native_critical_http_helpers_test.go 877FE5A075B8CCAAD80145A73E58C70D85C6DFEC9245F3C49F1EC4AA6CC52961
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/sdk/cliproxy/auth/ocg_native_critical_http_test.go 838ED53E057D35016B6A3BB629D53EEF9D9891F284D09152C511F940418C798E
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/host/native_critical_stream_refresh_test.go 3E63B02682EDFB47C5871D9134B813BA3881D8C123961DBEC1D968CF6098E63E
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/SOURCE-RECEIPT.md 5054925FB1E36E8A75116E1402F63965C89285F061BF1B082ECA80D2C9B276A7
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/independent-source-review.md F23920F70E30113F8EA43C41E5C9CE47F9EC38250534AD1FDE4644E2A22C3F46
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-sdk-finite-revision.md A1961ACB791774C0F43DB04E1A3DA14C433E874AAE50B8241C6829F41BA67971
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/task_8b6a4d323b-source-result.json 1CDC361E464407F861478AF78BA01C04003619D748E8023BAA1150969C46848E
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/conductor.go 39E0F512BA1225594863EAC5F3362C2411F64CF9C15CF08D5095EC67058D33C6
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/selector.go 70898F93905F01D894CE7B9C48671603A6D13C09C0E43FE7747CA555E71800D5
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/scheduler.go BF3BF049617C517030443CDA2CBE886762430B772F33773B9284E5B6C8EAFACE
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/conductor_selection.go B1904D111DA02EBAA13337ADC67D822D0EC711D9583138D9375CF5A2FB20166B
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/conductor_stream.go 58729DAE40583393E92CC52C566393AFAC282D3903F8147DC97ABA99F60146B3
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/ocg_attempt_boundary.go 35D31E8F4735924463DF20D178ED538E175EA603D50905F35959AB9F746B7965
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/ocg_endpoint_pin.go 98AED35CF738D7B02D10231760F0C62094B53587FF3D94110EF90CD006489874
runtime/cpa/host.go D0CE875934E7631CC0C054AA54A4C18FC74F7C1C70F4C961E5952C15DA3B3228
runtime/cpa/native_endpoint_test.go 1CA66AB8F6826A952D0D14317D0F803D7E413AE853C6463DBF199E72EA228C34
runtime/cpa/native_registration_fence_test.go B66C187C759C0167D0D4C391177523F4532288EAE7F656603AAA52168637AEB6
runtime/cpa/artifact-lock.json BF89D3A47323FC17281A42EE4D7874E03249CFE27DC765297055B65DDE3A7839
```
