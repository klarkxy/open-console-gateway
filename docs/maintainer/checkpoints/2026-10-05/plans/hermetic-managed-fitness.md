# Hermetic managed verification fitness

VERDICT: READY - revised queued contract only. The primary-selected section resolves both finite fixture-lifecycle findings. No source/build/runtime acceptance.

## Finite plan revision verification

OUTCOME COVERAGE: The selected map, exact Zen adoption, typed disabled-binding response and two-phase CAS contract retain the requested managed/builtin behavior.

NECESSARY CHANGES: No further plan corrections. The queued implementation must install the existing usage fetch/reactive seams at state construction before gateway startup; install or seal map absence before first render; reject duplicate/late/racing installation under existing synchronization; and use fresh states or a stable listener for successive-origin cases.

UNNECESSARY COMPLEXITY: None added. The revision reuses existing instance-local usage controls and the pure parser, avoids unrelated usage signatures, and retires the old no-op managed override registry after migration.

TRADEOFFS AND DEFERRALS: Dedicated usage assertions retain controlled scoped fetches rather than blanket suppression. CLI child-env behavior and feature-off official URLs remain; no feature-off positive official-provider sends are authorized. Active 29742 ownership remains a construction sequencing boundary.

ACCEPTANCE IMPACT: Retain the initial review's zero-external-I/O, immutable installation, path separation, exact Zen negatives, held/stale/concurrent CAS and narrow typed-refusal checks. Their implementation and actual CPA traces are still required.

UNKNOWNS: This rereview read only the revised plan and prior report. No moving source was rescanned and no Cargo/runtime/network/Git/credentials/delegation occurred. The source evidence below is the initial bounded snapshot, not newly verified code.

The remaining sections preserve that initial source analysis and required implementation checks; their two findings are resolved in the revised contract.

## OUTCOME COVERAGE

Keep actual owned CPA execution for builtin/managed verification, shared projection/live/explanation/validated URL authority, exact Zen None stamp adoption, and R0/Rstage/Rdone assertions. Source confirms the old generation-keyed managed URL installer no longer participates in the hop (managed_key_verify.rs:64-69,424), so reusing it or merely changing its map cannot produce truthful positive CPA verification. Projection/live currently rewrite Go/GOAT but omit Zen. Identity adopt_stamp_for (724-737) adopts configured HTTP None but not the legitimate Zen singleton, while the host registers that keyless auth with a nonzero epoch. A narrow Zen adoption correction is necessary; generic empty-cipher or Native Pending/Absent authority is not.

The two-phase CAS plan preserves current source semantics. stage_managed_candidate writes Pending and bumps its own revision (299-338); completion fences that revision/generation/CID/version/binding and bumps again (495-613). The old concurrent same-R0 test expecting two sends and one bump is incompatible. Keep one-stage/one-send/one-completion for the shared entry receipt and a separately staged fresh-CAS A/B test for meaningful stale completion coverage.

The disabled-binding model-test correction is justified by an existing observable contract (dashboard_v3_account_model_test.rs:244-285). Use a typed preflight refusal returning 200/success=false/httpStatus=null only for that expected no-send condition. Infrastructure/authority/bridge failures remain service errors; do not broaden 200 handling by matching text.

## NECESSARY CHANGES

1. Close current non-generation official I/O before starting the harness, not just before first CPA apply. managed_key_verify.rs:631-633 spawns reactive usage refresh after an auth-valid 429. usage_sync/reactive.rs:65-104 dispatches Go official refresh, and usage_sync.rs:1137 falls back to go_usage::fetch_accepted, whose official_usage_endpoint (go_usage.rs:149-153) reads the process environment, not the proposed CoreState fixture map. gateway::start_gateway_on starts the scheduler before the current harness installs later fixtures. The inspected managed harness does not install usage seams.

Use the existing instance-local UsageSyncRuntime test APIs for this bounded lane: install a non-network set_fetch_for_test seam before gateway startup to cover scheduled/manual official Go fetches; disable optional reactive refresh with set_reactive_refresh_enabled_for_test(false) when that behavior is outside the assertion. Do not invent new callback, global registry or fake trusted quota evidence. Tests explicitly covering reactive/official usage should instead keep that behavior active against their existing controlled fetch/local responder with real scoped fixture facts. The generation-only map is not sufficient proof of zero official network. Retain deny-external isolation as defense and report scheduler/usage coverage separately. No need to thread the new map through unrelated usage APIs merely to close these existing test callers.

2. Freeze immutable installation and adapt actual harness lifetimes. The map must be parsed/installed (or explicitly sealed absent) before any owned apply can render authority; installation after the first render/apply and a second installation must fail under the existing synchronization, including races. Do not fall back to process-env mapping for a missing participant when this instance map is installed. Preserve the separate CLI child-env path without mutating global env in parallel Rust tests.

The managed success/auth/429/5xx loop currently creates successive listeners on the same harness (dashboard_v3_managed_key_verify.rs:779-875), and its old guard is repeatedly installed. With an immutable instance map, use a fresh CoreState/complete map per outcome, or one stable mapped listener with scenario responses; do not reinstall the map, restart/reapply inside a held stale-response window, or add another override registry. Likewise adapt the dead-origin/proxy/timeout cases to their own stable mapped state. Install usage isolation at state construction before gateway bind. Preserve phase-specific measured revisions, exact request/body/auth/path and zero sibling/protocol sends.

## UNNECESSARY COMPLEXITY

A feature-only immutable field associated with CoreState and one parsed map is justified by parallel independent harnesses. Reuse the existing pure parser/rewrite implementation. Thread it through existing projection and authority contexts only where URLs/grants/fingerprints are actually computed; avoid independent map copies or a new routing/policy facade. No environment lock, thread/task local, generation registry, serialized test suite, stored fixture metadata, HTTP API or product endpoint setting is needed.

After all callers migrate, remove the obsolete MANAGED_KEY_VERIFY_TARGET_OVERRIDES/guard plumbing or clearly retire it; retaining a no-op global fixture control invites false positive test claims. The existing usage-runtime seams are simpler than widening all usage signatures for this lane.

## TRADEOFFS AND DEFERRALS

Deferring coherent fixture authority leaves positive managed/provider checks unable to reach their controlled server and tempts transport-only bypass or official sends. Deferring the exact Zen epoch predicate leaves keyless builtin acceptance unexplained despite plausible explanation DTOs. Neither is a harmless test-only omission in the full CLI outcome. The bounded map supports Go/GOAT/Zen; other/native fixture keys and CLI child-env behavior remain intact through their existing lane, not implicitly expanded here.

Keep Go and Zen path classification explicit: Go /zen/go (including its selected legitimate paths) must never be accepted by a generic Zen /zen prefix; Zen should match its canonical base /zen and protocol /zen/v1 paths, excluding /zen/go. Preserve Goat's source protocol paths. Required map absence/malformed or crossover rejects before physical generation; feature-off stays official without parsing fixture state. Real official network is never a feature-off positive test.

## ACCEPTANCE IMPACT

Add startup/scheduled/reactive usage zero-external-I/O proof, late/duplicate/racing installer rejection, and parallel states with distinct mapped origins. Verify projection, stored/applied grants, live identity and validated pins agree physically on exact Go/Zen/Goat prefixes; explanation cannot claim admission merely from an epoch-looking stamp. Preserve HTTP None, counterfeit singleton/provider, keyed missing material, Native Pending/Absent, duplicate/stale auth/binding/version and current scope/setup/enablement negatives.

Keep held Rstage encrypted-candidate checks and exact additional Rdone receipt; shared R0 loser must reject before hop; fresh post-stage replacement must retain two physical sends and reject A's late completion without modifying B. Preserve auth-valid 429 temporary/no fabricated durable quota, malformed/error/redaction/redirect/proxy/deadline/body-limit/actual-session tests, plus typed disabled-binding no-send response. Scheduling/retry/no-replay assertions remain open until actual CPA traces pass; do not delete them or recreate a Rust outer retry to satisfy them. Stop owned child/listener/workers before deleting profiles, including setup/panic failures.

## UNKNOWNS / SOURCE SNAPSHOT

No source edits, Cargo/build/tests, runtime/network, credentials, Git or delegation were performed. Current actors own rollback/backup/CPA source; freeze disjoint ownership before implementation. Snapshot SHA256 under crates/ocg-core:

- src/cpa_test_endpoints.rs: abdc56dd58a0e709a7ea4a6010de53e5ef93686c509ecc542b3b862dd348edaf
- src/dashboard_v3/managed_key_verify.rs: ccb9e4c8a02662c9c50a6b57e542ccd2af0d97ef5a67a15c73ad98611a720655
- tests/dashboard_v3_managed_key_verify.rs: ddd8317160e6b162c1468f74d2d4ac399e133d4093d3ed19619806bab29780ee
- src/cpa_execution/identity.rs: bdbe0614f69e132a2a8321d2055e6fcf24b5b1c2ec71a6cc496897be04da3e69
- src/protocol_probe.rs: db431a0673e2920b499c11cf8621af45ea59f156500ae05e751f92a24f575638

The primary-selected revised contract incorporates both corrections above. READY applies to that queued plan, not runtime hermeticity or complete CLI acceptance.
