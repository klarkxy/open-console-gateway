# Native-critical SDK/host adjunct finite revision

STATUS: SOURCE CONSTRUCTION ONLY. Every new case is UNRUN. This receipt is not a PASS, not CLI acceptance, and not an 011f SDK/host execution proof. Independent-source-review.md remains readonly. No confirmed product bypass is claimed.

Primary owns review, isolated-copy grant, runtime, Git, and acceptance. Cursor wrote only this owned subtree. No product Go/overlay/011f tree/host/shipping binary/root lock/Rust/script/docs/dep/Git change. No compile, go test, Cargo, listener, network, credential, or profile.

## Provenance

Current accepted build `011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b` from `runtime-build/primary-registration-mutation-suite-exits.json` `D93733429F8E500939F839B41FAA6E74DC6D39C22C527219BBF1A627061C79D6`. Production/fixture exits 0; unfiltered host 85.060/78.033; SDK `TestOCGNativeDispatchGuard` 0.141/0.123.

Packets (SHA-256):

```text
native-acceptance-gates-plan.md        1E11AC7736B4AB192414C09DDFC8EECCED5257ECC36E8A6F5D4A2A9BCC4A54B9
native-acceptance-gates-review.md      2A7625B0D4C6A4E4A0FD6D20B5905475DEC9F7371310D8E3BA27E66AFD9ADFF2
native-critical-sdk-source-v1.md       403FBDC8AEEB9FFB76D14C2B1090D42B1D4936EDD13334DF776645B6DC44AF53
native-critical-sdk-finite-revision.md A1961ACB791774C0F43DB04E1A3DA14C433E874AAE50B8241C6829F41BA67971
independent-source-review.md           F23920F70E30113F8EA43C41E5C9CE47F9EC38250534AD1FDE4644E2A22C3F46
independent-finite-rereview.md         1B31BFBDBD85E4DA881BB359DFFD5D3D918D57FB201B39E5BC008FC17541C825
cursor-routing-20261005.md             361C6AD3A98EE17B076CED07BADA0EB3D78EF8FAF9AF97B403226A18F431826C
```

Frozen 011f / current host inputs inspected (not rewritten):

```text
011f sdk/cliproxy/auth/ocg_attempt_boundary.go   35D31E8F4735924463DF20D178ED538E175EA603D50905F35959AB9F746B7965
011f sdk/cliproxy/auth/ocg_endpoint_pin.go       98AED35CF738D7B02D10231760F0C62094B53587FF3D94110EF90CD006489874
011f sdk/cliproxy/auth/ocg_dispatch_test.go      E64BFF3C89B740790D60EEC29C70D5A24A9410E1DFDEBD27D0047A33D6FA4F9F
host review_gaps_test.go                        8BC7DCA7546F4D1BDA883635D391A6E5CD8EAD8EAED3F47F3A2774E23D623AC2
host native_registration_fence_test.go          B66C187C759C0167D0D4C391177523F4532288EAE7F656603AAA52168637AEB6
host validation_bridge_test.go                  56D95CE6817F167F5DE5923B7114D9E3713DF708298C04A6309DA2437F9E8391
```

Historical b903 / f388 maps are not current proof.

## Existing 011f credit (do not rename)

| Case | Evidence | Level |
|---|---|---|
| JSON 401 → token refresh → `/responses` resend, material change, version held | `TestNativeRefreshResendAndFacts` | host real native HTTP |
| Ordinary refresh keeps registration epoch | `TestNativeOrdinaryRefreshKeepsRegistrationEpoch` | host real native HTTP |
| Fresh authorized replacement resend + stale refresh fence | `TestNativeReplacementResendObeysEpoch` | host real native HTTP; left unchanged |
| Validated pin stays on A; B/evil 0 through JSON 401 refresh | `TestNativeValidatedPinRefreshStaysOnPinnedAuth` | host real native HTTP |
| XAI warmed drop noReplay | `TestNativeXAIWarmedConnectionDoesNotReplay` | host physical TCP |
| Codex local count unsent / null pin | `TestNativeCodexCountStaysLocal` | host manager ExecuteCount |
| Dispatch consume-once, parent/internal permits, host/missing/wrong pin, cancel/deadline, local count facts | `TestOCGNativeDispatchGuard` | SDK guard only; no native executor HTTP |

Those tests were not copied.

## Six adopted fixture corrections

These are fixture/test-construction blockers. They are not product defects and do not authorize a shipping patch.

1. SDK files are `package auth_test` and use only exported manager/boundary/pin/result APIs plus a test-local `observeCodex` and recording `GenerationBoundary`. Isolated copy stays beside `sdk/cliproxy/auth` so `auth_test` → `auth` and `auth_test` → `executor` → `auth` is not a package cycle.
2. Count admissions return an explicit empty pin vector. Missing execute pins use `setEmpty`, not nil-preserve. Assertions read `ExecuteCount` / `AfterResult` / `MatchedEndpointPin` / `GenerationFacts` on the real admission context.
3. Internal: allowed parent and child consume independently (child `Execute` + `PublishInternalGeneration`, then parent `Execute`). Terminal child refusal then asserts parent unconsumed and unable to send on the stopped request, with zero extra server arrivals.
4. Held current-authority requires a newly observed named `stop`/`current_authority_denied` decision, distinct attempt ID, unsent/null-pin context, and intended stop error. Ordinary OAuth refresh remains the always-allow stream-refresh fixture. Fresh authorized replacement remains `TestNativeReplacementResendObeysEpoch`.
5. Each named kind has an allowed result with attempt/pin/sent facts, then a denied follow-up of that same kind recorded as a decision (denied admission may emit no result). Competing B is registered and eligible with lower numeric priority than A (A=10, B=1). Observed arrivals build an absolute URL from the receiving scheme, `r.Host`, and escaped path/query; canonical errors are reported, not filled from the expected URL.
6. Cancel/deadline require the actual context error. Removed auth may stop before a refresh admission and does not fabricate a denied-admit record. Held tests finish setup, wait for the first A request, then mutate and release.

## Three remaining rereview repairs

1. httptest origin-form `r.URL` is not passed to `CanonicalDispatchURL`. The recorded absolute URL is `scheme://Host + EscapedPath + query`, using the receiving request; canonicalization errors stay on the observation.
2. Intended initial A has higher numeric priority than eligible competing B. First selected credential/arrival must be A.
3. Held current-authority and named resumed negatives require a new named stop/current_authority_denied decision, distinct attempt, unsent/null pin, and intended error. Decisions are recorded separately from AfterResult/policy results.

## Owned adjuncts (post-gofmt hashes)

```text
sdk/cliproxy/auth/ocg_native_critical_http_helpers_test.go  646726B1B35584F53BEBC6C49BA0B9B8C40B71156F1D6B3244355BB33C7E84E3
sdk/cliproxy/auth/ocg_native_critical_http_test.go          A4023A8CF8A08295442A61C5FEBD4CACFA0934EFF8828EDBBFDE40F05711B962
host/native_critical_stream_refresh_test.go                 9CE16AA05EC01EAB918B01328EABC2CFF51892B4CA660F4A413D06158C7CA530
```

Later isolated copy only after primary runtime grant, into an exact 011f checkout (not shipping binaries, not overlay rebuild, not root lock):

```text
sdk/cliproxy/auth/ocg_native_critical_http_helpers_test.go   package auth_test
sdk/cliproxy/auth/ocg_native_critical_http_test.go           package auth_test
runtime/cpa/native_critical_stream_refresh_test.go           package main
```

## Case / evidence-level map

| New test | Real HTTP | Mock | Guard-only | SDK | Host | CLI | Notes | Runtime |
|---|---|---|---|---|---|---|---|---|
| `TestOCGNativeStreamRefreshHTTP` | yes Codex ExecuteStream JSON 401 + token_url + SSE | no | no | yes | no | no | first identity A; observed absolute `/responses`; kinds `stream` then `stream-refresh` | UNRUN |
| `TestNativeStreamRefreshResendKeepsPinnedCredential` | yes host `CoreAuthManager().ExecuteStream` | no | no | no | yes | no | A=10/B=1; competing B eligible; observed absolute POST `/responses`; result/pin/sent facts | UNRUN |
| `TestOCGNativeStreamResumeDeniedAfterAuthorityLoss` | yes first stream 401 held | no | no | yes | no | no | heldCurrentAuthority requires new stream-refresh stop decision; cancel/deadline require context err; revoke may stop before admit | UNRUN |
| `TestNativeStreamResumeDeniedAfterAuthorityLoss` | yes same host path | no | no | no | yes | no | first policy admit compared for epoch; named stop recorded separately from results | UNRUN |
| `TestOCGNativeStreamBootstrapHTTP` | yes HTTP 200 SSE `authentication_error` + buffering | no | no | yes | no | no | first identity A; kinds `stream` then `stream-bootstrap`; not stream-refresh | UNRUN |
| host bootstrap | — | — | — | — | not constructed | no | `startNative` has no buffering seam; OPEN | UNRUN / not built |
| `TestOCGNativeInternalAdmitHTTP` | yes child and parent Codex Execute | no | no | yes | no | no | `AdmitInternalGeneration` + `PublishInternalGeneration`; stopped-request parent refusal | UNRUN |
| public Antigravity compaction | — | — | — | — | — | C leaf | parent permit; not automatic internal child | not this leaf |
| `TestOCGNativeSevenKindsFollowupDenyHTTP` | yes per named kind except count | no | no | yes | no | no | allowed result + new named stop decision; count local null/unsent, not a native RoundTripper count | UNRUN |
| `TestOCGNativeCompactAltHTTP` | yes manager `Options.Alt=responses/compact` | no | no | yes | no | no | kind `execute`; observed absolute `/responses/compact` pin | UNRUN |
| `TestOCGNativePinDenialBeforeCountingTransport` | native executor: zero server arrivals | no | local test RoundTripper only | yes | no | no | missing/wrong: real Codex guard + A-server 0; host mutation: exported guard + test-local transport, not Codex client interception | UNRUN |
| `TestOCGNativeDispatchGuard` | no | no | yes | credited | no | no | not expanded by rename | executed 011f |

## OPEN public-API limits

- Codex `NewUtlsHTTPClient` has no exported RoundTripper injection. `fx.transport` is a test-local counting RoundTripper only. It is not a native executor transport counter. Missing/wrong-pin native paths credit zero server arrivals. Host mutation credits the exported guard plus conditional local `Do`.
- Host stream-bootstrap remains unconstructed: `startNative` has no buffering seam.
- No public API expresses an unconditional captured-epoch abort of an expressly allowed replacement identity. That rule was not selected. Fresh authorized replacement stays the existing passed host test.

## Later runtime commands (ungranted)

After primary review and an isolated 011f+adjunct copy, not against shipping artifacts:

```text
go test -count=1 -timeout 15m -run TestOCGNativeStreamRefreshHTTP\|TestOCGNativeStreamResumeDeniedAfterAuthorityLoss\|TestOCGNativeStreamBootstrapHTTP\|TestOCGNativeInternalAdmitHTTP\|TestOCGNativeSevenKindsFollowupDenyHTTP\|TestOCGNativeCompactAltHTTP\|TestOCGNativePinDenialBeforeCountingTransport ./sdk/cliproxy/auth

go test -p 1 -count=1 -timeout 15m -run TestNativeStreamRefreshResendKeepsPinnedCredential\|TestNativeStreamResumeDeniedAfterAuthorityLoss .
```

Do not run these in this leaf. A zero-match command or file presence is not a pass. Full workspace tests remain ungranted.

## Limits

- Synthetic loopback Codex HTTP and `token_url` refresh only. Not real OAuth/login/provider entitlement.
- SDK fixtures use a recording `GenerationBoundary`, not the Rust policy path. Host fixtures use the existing host policy wire plus a test-local admit-stop.
- Public Antigravity compaction and CLI rollback remain the C leaf.
- Websocket fail-closed stays unclaimed.
- Construction checkpoint, not reduced full CLI scope.
