# Fallback current-credential diagnostic review

Verdict: **REVISE the test's final assertion; no production credential-admission bypass is established by this diagnostic.** The coherent previous-child transport remains available, while current credential authority is checked per attempt. Reinstating a blanket connection failure for any rotated credential would not follow the accepted requirement that unrelated current authority remains usable.

This is a bounded source and saved-diagnostic review. No source edits, builds, tests, runtime processes, network, Git, credentials or delegation were performed. Only this review artifact was written. No broader rollback audit or whole-CLI acceptance is implied.

## Actual failure and exact cause

The primary's receipt identifies `target/debug/deps/ocg_core-e49abade750f97cc.exe`, SHA-256 `08D2B53C7B965FC91134C838330EC0162BA07679FCECDE1FA16FC7523A1953DF`, exit 101. A read-only hash check matched those binary bytes. The saved `primary-fallback-diagnostic/test.log` records **0 passed, 1 failed, 1861 filtered**, 14.15 seconds, failing at `cpa_execution/tests.rs:3188` because `unwrap_err()` received a redacted OwnedInferenceConnection. This was the primary's execution, not this reviewer's. The assertions establishing the initial connection and coherent failure restoration were reached before that failure; they do not constitute real child/provider runtime proof because the test uses synthetic hooks and a ToggleHost.

Current `owned_inference_connection` (`cpa_execution.rs:1231-1268`) checks owned-running state, unavailable/poisoned state, verified applied child tuple, loopback origin/hop and unchanged in-memory applied view. It never queries canonical credentials. `applied_tuple_ready` (`:1478-1487`) checks verified/policy/applied generation-revision-digest coherence. `applied_view` and `same_applied_view` (`:1435-1507`) capture and compare stored applied auth/routes and capabilities, not current database credential versions/material. There is no current `applied_credentials_current` helper or `owned CPA applied credentials are not current` production branch in this source.

The test rotates the canonical credential at lines 3178-3187, then expects that absent connection-level check and error string at 3188-3191. Database mutation does not mutate the retained accepted stamp map or applied child tuple, so both in-memory views still match. Returning the existing verified loopback/hop is the direct result of those current checks. It is not proof that the old provider credential was authorized or sent.

## Rotation is real and targets the correct identity

This is not a wrong-ID fixture:

- `account_control::rotate_upstream_credential` validates the supplied CAS and finds the identity model record by the canonical credential id (`account_control.rs:296-303,335-339`). It encrypts the replacement and rotates that record's actual legacy account (`:355-368`), then advances settings revision (`:369`).
- The database operation is transactional (`db/identity.rs:2131-2139`), updates the source account material, synchronizes the inference credential projection (`:2163-2184`), and increments version/auth-state in the applicable identity storage path. The canonical path increments `credentials.credential_version` and `auth_state_version` and rereads the resulting canonical id/version (`:2218-2237`); the historical satellite path likewise increments its version/auth-state (`:2192-2215`).
- The policy reader loads canonical `credentials` by id, including credential_version, key_cipher, binding and current enabled/setup facts (`identity.rs:779-839`). It recomputes keyed material fingerprint/auth id using the current version/binding and clears the plaintext after use (`:666-675`).

The root warning about schema-v51 satellite repair is not evidence that rotation missed the canonical identity. The corrected test should explicitly assert the returned id/current canonical version increase rather than relying on the warning or connection outcome.

## Current per-attempt consequence

Public ingress obtains this connection and registers a fresh client correlation intent before proxying to the owned CPA (`gateway/cpa_ingress.rs:314-342`). A child attempt is not granted by obtaining that hop.

The policy callback uses the applied plane and current database revalidation (`cpa_execution/callback.rs:130-143`). `build_current` reads the current canonical row/material (`identity.rs:572-607`); adopted registration epoch requires exact credential/version/binding/auth/material matching (`:693-719`), so it cannot transplant an old stamp's epoch onto a rotated version.

During client intent confirmation, `locate` requires an applied stamp at the live canonical version (`identity.rs:1298-1303`) and matching binding/provider/auth/epoch identity (`:1305-1325`). A rotated old stamp has no current-version match, and this caller path maps the miss to `IdentityFence` (`:1253-1258`). Separately, the policy's current gate retains durable credential/version/provider/binding comparisons and material/auth/epoch comparisons (`cpa_policy/decide.rs:309-341`). No zero-epoch wildcard, old-material wildcard, version relaxation or restored desired grant is introduced by the connection function.

Thus the inspected source rejects a new admit using the rotated credential's old stamp; the shared connection can still serve a different unchanged authorized credential. Exact resulting callback action/reason and zero provider I/O need the meaningful test below. The saved diagnostic never invokes that post-rotation attempt, so it neither proves nor disproves the per-attempt outcome.

This matches the accepted bounded contract in `rollback-authority-finite-plan.md:52`: historical A after rotation/revocation cannot authorize stale material, while unrelated current authority remains usable. `rollback-backup-coherent-v1.md:12,22` also retains current product/native fences. Those requirements do not require global child unavailability merely because one selected row was rotated.

## Minimal correction and meaningful verification

Preserve the current initial/failure-restoration assertions. Replace the test's final connection-error assertion with the actual authority boundary:

1. Assert rotation returned the same canonical credential id and that the current canonical version is greater than the retained applied stamp's version; keep the accepted applied tuple unchanged.
2. Permit the coherent child connection to remain available, then register a fresh valid client intent and submit the old applied stamp through the real policy callback. Assert its exact refusal for this path (`identity_fence` from current-version stamp lookup), no Allow, and no new admitted pin. Do not merely remove `unwrap_err()` or change it to an unconditional success assertion.
3. Include a second, unchanged credential in the original accepted plane and show its separate fresh correlated admit still succeeds after the first credential's rotation. This guards against replacing the obsolete global refusal with an equally broad outage. Preserve existing stopped-child/tuple/hop rejection tests.
4. The primary's later owned-host operator fixture should send the rotated and unchanged routes with physical counters: the stale credential must cause zero provider sends, while the unchanged authorized route remains usable. A successful apply of the new credential should then permit its new exact stamp. No real credentials or external provider are required for that synthetic runtime check.

These are test corrections and existing boundary checks, not permission to relax production version/material/native fences. If the fresh old-stamp callback is actually allowed, or a stale credential physically sends, that is a separate concrete production defect requiring source correction. Do not infer such a bypass from an Ok transport connection alone. A global database-currentness gate would need a new outcome justification and must not disable unrelated accepted routes.

## Freeze, rollback and limits

Eight bounded source files had identical entry/closing hashes (paths relative to `crates/ocg-core/src/`):

```text
cpa_execution.rs 2ABE5C549AC731ACE9059E380699FC094B550BE3153582DB6D190152DFE59BCC
cpa_execution/identity.rs 0219B01E1129043EBA05006F624CA0983C75502D9BDF57A5FB7A7A9EBEE2A24F
cpa_execution/tests.rs 2323AA6078C8FAA07124C396AC71EBDF32B114A122BC5D7387F56A3BC06F9889
cpa_execution/ready.rs 8E1FF62A24AA8E4582076D5312231B72DA6AC2B249460855941F60F7D36BD6FF
account_control.rs B93D4686EE74BC8AA7D189190987580BEECF6F02DE33CE0C346A7BC60F1005C7
db/identity.rs 690E235061C002741A6E65E75F6EC01F1207C77E042CF0F5579A02A20C9BB20C
cpa_policy/decide.rs F5806CA430EE336E2EFADA1089AD56A8995A5FC422D8FA10D4BA26E0078AA58B
cpa_execution/callback.rs 34F9D35F1E57D536C647E14090F384D3CA4D258B961213D7F662DA6FE26950CA
```

The proposed test revision is reversible and has no storage migration, shipping artifact or rollback action. Preserve all records, grants, private data and unrelated WIP. The saved executable diagnostic does not prove a final-source build, full CLI, real native-provider behavior or sustained operation. Primary retains intent, routing decisions, integration, Git and final acceptance.
