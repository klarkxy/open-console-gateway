# Finite archive bearer change re-review

Verdict: **READY** for the finite correction of the prior known-seeded-bearer false-pass finding. This is change_review only. The other source-reviewed gates retain the findings recorded in independent-source-review.md; they were not re-audited. Full CLI acceptance and all physical runtime evidence remain pending with the primary/runtime owner.

Reviewed current C SHA256 before and after checks: `0DEEA0B34B95451F881E77A9A90106AA63C2DF742C5AEE6E324347FF843BDCDC`. The assigned script remained `?? scripts/cli-cpa-acceptance.mjs` before and after; no source or Git mutation occurred. Only this normal report artifact was written. No product, Cargo, Go, listeners, network, credentials or delegation were used.

The former P2 finding is resolved:

- `physicalBearerAttribution` now distinguishes a matching known token from a known mismatch, including an empty physical bearer (`scripts/cli-cpa-acceptance.mjs:4105-4143`). Unknown fixture mapping remains explicitly ambiguous.
- `archiveBearerVerdict` permits PASS and a bearer claim only for `kind: match` plus `matched: true`; known mismatch yields FAIL and ambiguity yields OPEN (`:4145-4158`). Diagnostics contain hash prefixes and classification, not raw fixture or bearer values.
- The archive workflow checks that verdict before any success row. A mismatch throws FAIL at `:4313`. Ambiguity preserves the already verified archive operations without a bearer claim and creates a separate `native-oauth-archive-bearer` OPEN row with `required-incomplete` (`:4314-4328`). The positive row carries the actual match detail (`:4330-4335`). Cleanup remains in the finally block (`:4336-4337`).
- Main completion includes both required SDK/host and required-incomplete OPEN rows in its incomplete calculation (`:5323-5329`); ambiguity cannot yield overall pass/exit 0. A thrown mismatch becomes the existing FAIL stage outcome. No new policy or receipt framework was added.

Permitted verification: Node syntax check and --list both exited 0; --list returns before prepareProfile (`:5288-5295`). A no-start evaluation removed main invocation before import and invoked only the synthetic attribution/verdict helpers. It asserted and observed match -> PASS, known different bearer -> FAIL, empty bearer -> FAIL, unknown mapping -> OPEN; claimBearer was true only for PASS. The corrected evaluation exited 0. The first reviewer-authored evaluation command had a shell quoting syntax error (exit 1), was corrected, and did not execute source or start anything; that error is not a product defect. No token was printed or persisted.

No further concrete blocker remains in this finite correction. Runtime success, actual archive identity mapping, private provider contracts and complete CLI acceptance are not established by these checks. Recovery remains reverting the test-driver correction; no production migration, release or irreversible action is involved.