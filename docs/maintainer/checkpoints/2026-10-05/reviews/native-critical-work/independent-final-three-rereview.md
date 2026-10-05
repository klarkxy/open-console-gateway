# Native critical adjunct final three source rereview

Verdict: **READY** for the bounded test-only source checkpoint to proceed to primary-controlled isolated compilation/tests. Mode: `change_review`. Reviewed only the three remaining fixture corrections and directly changed assertions. No remaining verified defect was found within this assigned scope. This is source readiness, **not a test PASS, product acceptance, or runtime grant**.

No Go/build/test/Cargo/runtime/listener/network/credential/Git/source edit/delegation was performed. Only this report was written. Both prior review files remain readonly. Earlier six structural closures retain the qualified conclusions in independent-finite-rereview.md; no whole-Go audit was repeated.

Abbreviations: H = sdk/cliproxy/auth/ocg_native_critical_http_helpers_test.go; A = sdk/cliproxy/auth/ocg_native_critical_http_test.go; N = host/native_critical_stream_refresh_test.go, within integration-work/native-critical-work. Packet paths are relative to tmp/ocg3-cli-delivery/orchestration-20261004; final hash paths below are repository-relative.

## Verified correction 1 — observed absolute URL

H:290-335 and N:159-204 construct the absolute observed URL from the receiving request's scheme (TLS/http fallback for ordinary server origin-form requests), Host, EscapedPath and RawQuery, preserving ForceQuery. They do not fill it from the expected target. Both record canonicalization errors instead of discarding them.

H:734-748 requires no observation error and a nonempty canonical value before comparing against the independently canonicalized expected URL. N:375-384 likewise requires no error and exact POST/path/query/nonempty canonical target. This removes the previous guaranteed empty-canonical failure for ordinary httptest server requests. The final matched pin is still independently checked against the expected target.

No external URL/transport check was run; these are verified source assertions for the intended direct loopback HTTP fixture.

## Verified correction 2 — intended A outranks eligible B

H:530-534 registers A priority 10 and B priority 1; N:325-329 and :513-517 use the same priorities in both host fixtures. B stays active, registered and callable, so its zero-arrival counter has an actual competing-account role.

This matches the previously inspected frozen selector's highest-numeric-priority behavior. H:677-687 requires the first selected credential/version to be oauth-a/4 and the first actual arrival bearer access-old; A applies it to refresh, bootstrap and held-first-request paths. Host positive assertions inspect bearer sequence and policy result/admit credential identity; host held refusal requires the actual first stream policy decision to identify oauth-a/4 before mutating anything (N:546-551).

This removes the previous B-first selection error without changing product selection behavior. All account selection/arrival assertions remain UNRUN.

## Verified correction 3 — required named refusal and expected context error

H:121-165 records the actual BeforeSend decision separately from AfterResult, including kind, new attempt ID, stop/current_authority_denied, captured identity and the real admission context. H:228-240 searches only decisions after the test's snapshot. H:656-674 now requires the intended newly observed kind/stop/reason, a nonempty distinct attempt ID, null matched pin, sent=false/unsent transport, and an ErrAttemptStop/RequestStopped error. It cannot pass merely because a generic refresh failure occurred before the armed boundary.

A:145-163 separates held current-authority stop from cancellation/deadline and removal. Current-authority calls requireNamedRefusal plus requireIntendedStop. Cancellation/deadline require errors.Is against the specific expected context error; the former err=nil/empty clean stream acceptance is removed. Removal requires a returned error and zero second generation I/O, without inventing an admission that may never occur.

The named seven-kind negatives call requireNamedRefusal on fresh decision snapshots: refresh-resend A:332-340, stream-refresh :376-385, bootstrap :402-411, internal :440-453; execute/stream/count/compact use the same requirement through denyNamedFollowup at :642-656. Positive result/pin assertions remain separate from these refusal decisions.

N:209-280 adds a separate policy-admit decision log recording the exact action/reason/attempt/identity returned by the test policy. N:430-449 compares replacement epoch against the **actual first stream policy admit**, preserving credential version. N:574-595 requires a newly observed named stream-refresh stop/current_authority_denied, a distinct attempt and the intended stop error. Its result checks reject any sent or matched-pin publication for that denied attempt if a result exists. A denied admission may emit no result; the source correctly avoids fabricating a result to satisfy a test.

Host cancellation/deadline check the specific returned context error at N:570-573. Removed auth remains a truthful no-second-I/O/error case. This does not claim an unconditional captured-epoch abort or a confirmed production bypass.

## Evidence boundaries carried forward

- Real SDK/host Codex execution still measures actual loopback server arrivals, refresh/token traffic and generation/result/pin assertions.
- `fx.transport` is a **test-local** counting RoundTripper, not an injected native Codex transport counter.
- Missing/wrong-pin native executor fixtures establish real executor guard behavior plus zero server arrivals; their separate local transport calls establish guard/invocation/pass-through contract behavior.
- Host mutation uses the exported guard and a conditional test-local transport path; it does not intercept native Codex's client.
- Local count establishes actual count result null-pin/unsent facts and zero generation server arrivals; it does not measure native RoundTripper invocation count.
- Synthetic SDK boundary, host policy wire and CLI/Rust authority remain distinct. Host bootstrap stays OPEN because the helper exposes no buffering seam. Public Antigravity compaction and CLI rollback remain other work.
- Real OAuth/provider login/entitlement, native Linux/macOS hardware and sustained operation are not established by this source checkpoint or eventual synthetic tests.

SOURCE-RECEIPT.md now states those distinctions and leaves every new case UNRUN. Existing 011f tests retain only their prior documented executed scope; this rereview does not add execution evidence.

## Verification, drift and recovery

The four new checkpoint hashes matched the primary's final-three packet at entry. Both prior review hashes remained unchanged. A bounded 11-file set includes those files, the supplied task result, relevant frozen priority/pin and host inputs, and root lock. Rehash before writing this report found **zero drift**, exit 0; after-report verification is performed by the reviewer as well. No Git status/diff operation was used.

The worker result's workspace snapshot was unavailable, so its self-reported files_changed list was not treated as a complete immutable workspace proof. Readiness is based on actual source reads and bounded hashes. No compiler/test exit is claimed.

Primary retains isolated source-copy construction, compiler/runtime allocation and final acceptance. This review authorizes no shipping Go patch, product export, unconditional epoch rule, binary rebuild/relock, publication or deployment. Recovery remains revision/removal of these unintegrated test adjuncts; there is no product data migration or shipping rollback to perform. No further source blocker is identified in these three corrections.

Closing repository-relative SHA-256 values:

```text
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/sdk/cliproxy/auth/ocg_native_critical_http_helpers_test.go 646726B1B35584F53BEBC6C49BA0B9B8C40B71156F1D6B3244355BB33C7E84E3
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/sdk/cliproxy/auth/ocg_native_critical_http_test.go A4023A8CF8A08295442A61C5FEBD4CACFA0934EFF8828EDBBFDE40F05711B962
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/host/native_critical_stream_refresh_test.go 9CE16AA05EC01EAB918B01328EABC2CFF51892B4CA660F4A413D06158C7CA530
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/SOURCE-RECEIPT.md D5A4170458E505202591283CB0E3AA3F335ED1EB2D7E02D4B0793474F5E866C2
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/independent-source-review.md F23920F70E30113F8EA43C41E5C9CE47F9EC38250534AD1FDE4644E2A22C3F46
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/independent-finite-rereview.md 1B31BFBDBD85E4DA881BB359DFFD5D3D918D57FB201B39E5BC008FC17541C825
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/task_7c29563778-source-result.json 4A39A4AD5A2ED3E513D15ADB3E979F5A7F375CC7B2724A4E759D3EE537526C62
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/selector.go 70898F93905F01D894CE7B9C48671603A6D13C09C0E43FE7747CA555E71800D5
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/ocg_endpoint_pin.go 98AED35CF738D7B02D10231760F0C62094B53587FF3D94110EF90CD006489874
runtime/cpa/host.go D0CE875934E7631CC0C054AA54A4C18FC74F7C1C70F4C961E5952C15DA3B3228
runtime/cpa/artifact-lock.json BF89D3A47323FC17281A42EE4D7874E03249CFE27DC765297055B65DDE3A7839
```

