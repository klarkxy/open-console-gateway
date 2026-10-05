# Native critical adjunct independent source change review

Verdict: **REVISE**. Mode: `change_review`. This reviews the actual four-file source checkpoint, against native-critical-sdk-source-v1.md and the corrected gates plan. Every adjunct is **UNRUN**. No Go/Cargo command, build, test, runtime, listener, network, credential, Git operation, delegation, or shipping-source mutation was performed. The only write is this report. Review does not grant isolated-copy execution or a product patch.

Use these path abbreviations:
- `A`: integration-work/native-critical-work/sdk/cliproxy/auth/ocg_native_critical_http_test.go
- `H`: integration-work/native-critical-work/sdk/cliproxy/auth/ocg_native_critical_http_helpers_test.go
- `N`: integration-work/native-critical-work/host/native_critical_stream_refresh_test.go
- `T`: runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b

Paths above are relative to tmp/ocg3-cli-delivery/orchestration-20261004 unless explicitly repository-root paths below.

## Verified findings, ordered by severity

### P1 — SDK in-package tests have an import cycle

H:1 declares `package auth`; H:16 imports `internal/runtime/executor`. That package imports the same `sdk/cliproxy/auth`, for example T/internal/runtime/executor/codex_executor_execute.go:13 and codex_executor_stream.go:16. Putting these files at the receipt's proposed SDK destination creates `auth [auth.test] -> executor -> auth`. This prevents the package from being tested; private `generationFacts`, `beforeGenerationSend`, and manager mutex use prevent fixing this by changing the package declaration alone.

Revision: place real native executor HTTP fixtures in a test package/location that does not import its own package transitively, using the existing public APIs and test-local capture seams. Keep any required private guard-only checks separate. No production exports or patch are needed merely to accommodate these tests. Update destinations and eventual package/filter commands accordingly. This is a source-supported compile blocker, not a compiler run.

### P1 — missing-pin and local-count fixtures retain valid HTTP pins

H:145-148 installs both /responses and /responses/compact pins. H:289-293 only replaces pins when `pins != nil`. A:372 passes nil for the “missing” case, preserving both valid pins; the actual Codex Execute target will therefore be allowed, contradicting A:375's expected ErrAttemptStop/no HTTP.

The same error affects A:300-322: recordingBoundary always returns its HTTP pins for count-tokens. The actual ExecuteCount remains local, but the separately created count context also retains pins, so `len(facts.allowed) != 0` fails. T/sdk/cliproxy/auth/ocg_endpoint_pin.go:161-174 explicitly remembers supplied pins even for a local count. The host boundary rejects HTTP pins for local count (repository runtime/cpa/boundary.go:124-130). This SDK fixture does not model that contract.

Revision: represent explicit empty pins separately from “retain previous pins”; return no HTTP pins for actual count admissions; assert the actual ExecuteCount result's null matched pin and unsent facts rather than inspecting a separate count admission. Do not change production guard behavior to satisfy the malformed fixture.

### P1 — denied child stops the shared request; fixture then expects parent success

A:188-200 admits an internal child with a wrong pin, checks final-dispatch refusal, then calls Execute with the original parent context and expects success. In the frozen SDK, final-dispatch refusal calls `stopDispatch` (T/sdk/cliproxy/auth/ocg_endpoint_pin.go:261-266), which calls `noteRotationHalted`. T/sdk/cliproxy/auth/ocg_attempt_boundary.go:151-187 retains and stops the shared requestStop/internalGate; child admission inherits those objects. The parent is consequently stopped as well. Its permit remaining unconsumed does not authorize continued I/O after request termination.

Revision: prove independent child and parent consumption in the allowed case before a terminal child refusal, or in distinct fresh requests. In the refusal case assert parent remains unconsumed and also cannot send within the stopped request. Preserve the zero-extra-I/O check. The present required parent-success assertion conflicts with current intentional request-stop behavior.

### P2 — “replacement authority loss” and product-fence attribution are not established

A:61-66 and N:248-254 use Manager.Update with replaced access/refresh tokens. This is a real credential replacement, not ordinary UpdateRefreshedAuth: T/sdk/cliproxy/auth/conductor_lifecycle.go:225-277 selects updateModeReplace and advances RegistrationEpoch on CredentialsChanged. However, neither fixture captures/asserts that epoch, changes the credential version/current endpoint grant, or tells its boundary to refuse authority after replacement.

H:40-51 always returns allow and fresh attempt IDs. Repository runtime/cpa/lifecycle_more_test.go:306-321 shows strictPolicy always allows every valid admit with allowPins; the callback in N controls only result disposition. The resumed conductor receives the live replacement auth and invokes a new stream-refresh admission (T/sdk/cliproxy/auth/conductor_stream.go:281-298). The refresh shortcut is indeed present: tryRefreshAfterUnauthorized calls refreshAuthForRequest without a captured epoch (conductor_refresh.go:514-532,557-559), and atEpoch accepts the current different token when passed epoch 0 (:598-606). This identifies the source mechanism, but does not prove that a newly and explicitly allowed current-authority attempt is unauthorized.

The existing executed host TestNativeReplacementResendObeysEpoch (repository runtime/cpa/native_registration_fence_test.go:325-416) explicitly expects fresh-authority resend after replacement and proves stale refresh cannot overwrite it. Its replacement occurs before the request, so it is not identical to the proposed mid-401 case; neither case alone proves the missing OCG authority-loss gate.

Classification: **verified fixture/attribution defect; no test basis for a confirmed product bypass**. A source-supported investigation question remains whether the primary wants an unconditional captured-epoch abort even when current per-attempt policy explicitly authorizes the replacement. That is not established by these fixtures and must not be smuggled into shipping Go.

Revision: assert captured/live epochs and version; arrange a genuine current authority loss/refusal during the held first attempt through existing boundary/policy seams, then require observed resumed refusal, no second generation dispatch, an error/terminated stream, and truthful unsent refusal facts. Keep ordinary concurrent OAuth refresh and fresh authorization separate. Remove the receipt's definitive “product fence gap” attribution until actual authority-denial evidence exists. No shipping patch/relock is authorized by this review.

### P2 — kind-specific follow-up, exact results and counting transport claims exceed assertions

A:413-420 always creates a new **execute** admission and calls Codex Execute. Every non-count seven-kind case uses that helper. Thus allowed refresh/stream/bootstrap/internal kinds are scheduled, but their negative counterpart is always execute; it does not prove denied resumed dispatch at each named boundary.

H:54-56 discards AfterResult entirely. None of the SDK positive tests assert `MatchedEndpointPin`, exact final pin/attempt/result correspondence, or actual result sent/outcome facts. Direct internal Execute at A:178 and :293 does not call PublishInternalGeneration, so it establishes child executor HTTP, not the required internal result publication. Count assertions inspect another context, as above. N:118-122 asserts only the first two outcome labels, without requiring exact count, result attempt linkage, matched pins, sent/body/stream facts, or terminal stream content. N:80-85 also silently accepts nil stream/chunks.

No counting RoundTripper is installed. H's `hits` counts HTTP server arrivals. A:400 directly invokes BeforeOCGHTTPDispatch and never calls a transport regardless of outcome, making its zero transport/server observation tautological. A wrong request could invoke a transport and fail before arrival without changing these counters. The name “BeforeCountingTransport” and receipt's transport-level credit are therefore unsupported. Existing guard-only evidence may still be credited as guard-only.

B and evil servers in H:133-144 and N:38-49 are never connected to a registered alternate credential or a stale captured endpoint/material. Their idle counters do not prove selection stayed off an available B or a replaced evil target. They are merely unrelated idle listeners.

Revision: deny the actual relevant named follow-up before its dispatch, capture boundary result contexts/attempt IDs/pins with existing exported accessors, publish explicit internal results, and require local count's actual unsent/null result. Use a real counting transport at the final-dispatch boundary, keeping server-arrival counters separate; make Host mutation's potential Do conditional on successful guard so the counter can detect bypass. For the pin-retention claim, give B/stale evil a real competing/captured role, or retain the existing meaningful passed pinned test and narrow the new claim. Require exact POST/path/query/canonical target and result pin checks instead of contains/suffix-only assertions. These are necessary evidence repairs, not new production machinery.

### P2 — deadline/refusal tests do not deterministically establish held resumed-send denial

N:206 starts a 25ms deadline before servers, policy and startNative are constructed; ExecuteStream is only invoked at :237. The context can expire before initial generation, yielding hits=0 rather than testing a held first 401. SDK A:116 has the same timing issue before fixture setup. Forty-millisecond sleeps do not establish the intended sequencing.

Several negative tests discard both returned stream and error (A:71; N:161,199,237,283), and retain no result log. SDK cancel A:110 accepts err=nil whenever the context is cancelled. One upstream hit alone cannot prove truthful unsent resumed-result facts or absence of a successful stream.

Revision: finish setup first, hold the first observed generation request with a bounded channel barrier, expire/cancel/revoke while held, then release the 401. Assert initial hit is observed, no subsequent dispatch/transport invocation, expected error/stream termination, and exact refusal/result facts. Keep finite bounded waits. These are source-visible reliability/evidence defects; no flaky execution was claimed.

## What is source-feasible and correctly scoped

- The normal stream refresh fixture uses actual manager ExecuteStream and Codex native HTTP 401/token refresh/SSE paths, not a mock executor. It separates stream and stream-refresh kind assertions.
- Bootstrap's first HTTP-200 SSE event is authentication_error with no prior payload. Frozen codex_executor_terminal.go:178-195 maps it to 401; codex_executor_stream.go:204-227,338-354 returns its pre-payload error chunk; conductor_stream.go:113-118,374-395 can choose stream-bootstrap. The buffering configuration and source event are appropriate. Still UNRUN.
- Compact uses manager Execute Options.Alt=responses/compact and the real Codex executor, keeping execute distinct from internal. Its result/pin assertions need strengthening as above.
- Explicit AdmitInternalGeneration uses a real manager gate and real child Codex HTTP. It correctly avoids claiming public Antigravity compaction automatically created that child; the terminal-denial/parent/result assertions require correction.
- The seven constants are the actual frozen seven kinds. No new kind DTO, compact enum, API, store or outer retry was introduced.
- No host bootstrap is constructed because startNative has no buffering seam. That is an accurately stated limit, not a fabricated normal-host pass.

## Existing execution evidence credited

Read current primary-registration-mutation-suite-exits.json and primary-registration-mutation-production.log directly. The receipt identifies build 011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b, production/fixture exit 0, host 85.060/78.033 seconds and SDK 0.141/0.123. Log lines 3,859-861 show an unfiltered host package suite and SDK **only** `-run TestOCGNativeDispatchGuard ./sdk/cliproxy/auth`.

Accordingly existing host native refresh/material/replacement/validated-pin/local-count and XAI no-replay tests retain their actual executed host scope. Existing SDK dispatch-guard tests retain guard scope. Upstream mock stream tests, direct executor compact tests, and these new adjuncts are not promoted to executed coverage. Historical f388/b903 maps are not current evidence. No new SDK/host/CLI PASS or full CLI acceptance is established.

## Required disposition and recovery

Correct the test-only package cycle, nil/count pin semantics, terminal child/parent expectation, authority-loss attribution, kind-specific physical/refusal/results assertions, and deadline sequencing before granting this checkpoint's isolated execution. Update SOURCE-RECEIPT.md hashes, destinations and truthful case coverage. A product patch is not an approved remedy for malformed tests.

The appropriate recovery is to revise or remove these unintegrated adjuncts. Frozen production Go, host files, root lock and shipping artifacts require no rollback; none was changed here. Root retains integration, isolated test tree/runtime slot, trust/relock authority and full CLI acceptance. This source review does not close CLI import/apply/grants/public compaction/rollback or live provider behavior.

Residual limits remain synthetic Codex/token_url OAuth only, synthetic SDK boundary versus host policy wire versus actual Rust authority, real provider login/entitlement/refresh, native Linux/macOS hardware, and sustained operation. Compilation/tests alone will not close those limits.

## Frozen status and hashes

Four adjunct/checkpoint hashes were captured before detailed source tracing and match the constructor receipt (receipt itself separately hashed). A bounded 27-file baseline covers the adjuncts, reviewed frozen sources/current host inputs, current execution receipts, root lock and supplied packet. The same 27 paths were rehashed before this report: **zero drift**. Git was prohibited and not consulted. Source reads exited successfully except explicitly corrected nonexistent-path exploratory reads; no check/test exit was invented. Hashing exit 0. The bounded list is not a claim that every unrelated workspace file was immutable.

All paths below are repository-relative; SHA-256 is shown. These same values were observed at closing:

```text
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/sdk/cliproxy/auth/ocg_native_critical_http_helpers_test.go DB0767B80AAA947137C582B9E900C002A782C1ED3D3BD5702A680202F9EFD859
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/sdk/cliproxy/auth/ocg_native_critical_http_test.go EE163ED9F5F3314E6E7A2261C7EADCD8E6F31D9F192A56618EF856B8AED03FC1
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/host/native_critical_stream_refresh_test.go 71DF4BB1C4C563E6766844141BE55295A738AD3505D7E9EDEB57C1C6D349762C
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-work/SOURCE-RECEIPT.md 5AA3B939AD0FFD6319F9DC85442DBAC52E5AB896D12594CEB4157184510FB08A
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/ocg_attempt_boundary.go 35D31E8F4735924463DF20D178ED538E175EA603D50905F35959AB9F746B7965
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/ocg_endpoint_pin.go 98AED35CF738D7B02D10231760F0C62094B53587FF3D94110EF90CD006489874
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/ocg_dispatch_test.go E64BFF3C89B740790D60EEC29C70D5A24A9410E1DFDEBD27D0047A33D6FA4F9F
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/conductor_refresh.go 12D6EBC130A33544E08FF2A6876D7C12BD643B0315D3C6F5FFBBA45A0C947E5C
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/conductor_lifecycle.go 48D7B8E100064F4E462A34007FC24397813AE79DE725DE3C67434A70E2E4499E
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/sdk/cliproxy/auth/conductor_stream.go 58729DAE40583393E92CC52C566393AFAC282D3903F8147DC97ABA99F60146B3
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/internal/runtime/executor/codex_executor_execute.go 97731071E2874E1D20995E2833E64EC9D4CD8831DE7E8362ED182360E60DAEF7
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/internal/runtime/executor/codex_executor_stream.go 08F5B965D7FC00840C70021983721E269706507126DD5C759542182D097C07EE
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/trees/011f360291c4c026cee9a57c3707ebe5a975b100f4fe54a500fdb0fb6aa6fe4b/internal/runtime/executor/codex_executor_terminal.go 563A19AF1291D40D28C78ADE9BBDB19C8FFD22CF90644ADE1AEB8F3B3B663CB7
runtime/cpa/native_registration_fence_test.go B66C187C759C0167D0D4C391177523F4532288EAE7F656603AAA52168637AEB6
runtime/cpa/lifecycle_more_test.go 52182F50689214BCCB1A7DDA72132221B80E360C11F763A0B3A9CDB2CBEC8F89
runtime/cpa/native_endpoint_test.go 1CA66AB8F6826A952D0D14317D0F803D7E413AE853C6463DBF199E72EA228C34
runtime/cpa/review_gaps_test.go 8BC7DCA7546F4D1BDA883635D391A6E5CD8EAD8EAED3F47F3A2774E23D623AC2
runtime/cpa/validation_bridge_test.go 56D95CE6817F167F5DE5923B7114D9E3713DF708298C04A6309DA2437F9E8391
runtime/cpa/native_xai_replay_test.go 02D651328A7BE2770EF96E5C0965896891B94D200A21BAC2FB4BE4B486A6C9B5
runtime/cpa/boundary.go D1C6F50D96593FB347654B6250998242EF51AA2BF66FFF5CDD618F52B7B0412C
runtime/cpa/policy.go 3AF972E61EFCFCD9E555750263AD223965C82631A7846FFEC9BF3C019FFF7983
runtime/cpa/artifact-lock.json BF89D3A47323FC17281A42EE4D7874E03249CFE27DC765297055B65DDE3A7839
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/primary-registration-mutation-suite-exits.json D93733429F8E500939F839B41FAA6E74DC6D39C22C527219BBF1A627061C79D6
tmp/ocg3-cli-delivery/orchestration-20261004/runtime-build/primary-registration-mutation-production.log 84FC022E568B0B834D5624CDAC436F26477BF45566451B189E9ABA18B6002CB1
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-critical-sdk-source-v1.md 403FBDC8AEEB9FFB76D14C2B1090D42B1D4936EDD13334DF776645B6DC44AF53
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-acceptance-gates-plan.md 1E11AC7736B4AB192414C09DDFC8EECCED5257ECC36E8A6F5D4A2A9BCC4A54B9
tmp/ocg3-cli-delivery/orchestration-20261004/integration-work/native-acceptance-gates-review.md 2A7625B0D4C6A4E4A0FD6D20B5905475DEC9F7371310D8E3BA27E66AFD9ADFF2
```

