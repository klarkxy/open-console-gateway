[简体中文](routing.zh-CN.md)

# Routing, Cost, And Failover

### Newly discovered OpenCode Go models

Refresh the model catalog on Providers, then explicitly enable newly discovered models and select the supported upstream protocol. Models in the saved Go catalog use their effective model contract even when no checked-in alias/protocol profile exists. Diagnostic planning cannot reject such models merely for being new. This does not enable unknown, disabled or removed models, and does not probe protocols during inference. A local catalog/protocol test is not proof that a live account has access to the model.

### Upstream rejection, quota reset, and local recovery

The current source uses one failure-facts and recovery policy. Static service-specific decoders identify cause, affected resource, quota window, upstream reset, and rule evidence; they do not each own a retry state machine. An unknown or transient 429 without a usable retry constraint only excludes the account from this request. It does not invent a five-minute account cooldown, quota exhaustion, or a replacement for an otherwise available global sticky target. Custom/dynamic, MiniMax, Kimi, Ollama, and CPA generic HTTP errors cannot borrow Go or GOAT quota phrases.

`Retry-After` is a separate not-before constraint, not evidence of an empty account or a quota reset. Without a known quota scope it is enforced for the exact endpoint, upstream model, protocol, and route; swapping Keys does not bypass that same-resource wait. A recognized plan-window reset is stored independently. Both constraints must be satisfied, even when Retry-After is longer. Invalid values do not invent a deadline; valid zero or past values add no future delay. Unrepresentably distant numeric waits remain blocked locally rather than being shortened.

These are source behavior notes, not a claim that an installed binary contains this work. The historical [2.4.2 candidate](../releases/v2.4.2.md) does not include PR #73's cross-request recovery.

## Account Selection And Failover

Accounts are tried in **list order**, which you can drag into shape and persist
from the Accounts view. The selector skips:

- Disabled accounts.
- Accounts that are cooling down.
- Accounts that have already failed during the current request (e.g. with a
  `429`).
- Accounts whose saved provider contract has no effective enabled upstream
  protocol for the resolved model.
- Keys whose inference binding is disabled, or whose binding `modelScope`
  does not include the requested public/routing model. A sibling Key on the
  same connection keeps its own allow-list.

Keys in a **declared quota pool** share restrictions only when decoded evidence identifies that pool. Known quota-window cooldown applies across its members. Insufficient-credit waiting is conservative and model-specific within the pool; success on another model does not certify recovery. Unknown status-only 429s do not prove shared-pool exhaustion. Matching names or URLs do not create a pool, and new independent credentials remain independent.

Only the matching service decoder may interpret quota-window reset evidence and persist the corresponding cooldown. Recognized exhaustion with unknown recovery instead uses process-local waiting and foreground reprobes. Admission skips a waiting resource without sending upstream, then tries the next compatible account within the shared request budget. `403` fails over without writing a
cooldown. Zen Free `401` is returned as-is. OpenCode Go structured
`CreditsError` 401 rotates to the next eligible card and persists `auth_error`;
re-saving the same Key clears that breaker after renewal. Its `ModelError`,
unknown, and malformed 401 responses remain passthrough because OpenCode also
uses 401 for unsupported models. Custom API `401` also rotates and persists
`auth_error`.
Managed-account Key verification and Custom **Verify connection** still
record `auth_error` when they get a 401. CLI `key ping` prints the real
upstream status without writing that field. A DNS/TCP/TLS connection
failure that proves the request was not sent is retried once on the same
account, including for streaming calls.

When **Conversation sticky** is on, a matching conversation key is tried
before the base routing mode. The header `X-OCG-Conversation-Id` wins when
present; otherwise the gateway fingerprints system / tools / the first user
message. No usable key means the selected strict-priority, global-sticky, or
round-robin mode runs unchanged.

The gateway does not replay `408`, `5xx`, post-connect send failures, or response-body timeouts. Ambiguous outcomes are reported as `upstream_outcome_unknown` and logged as `outcome_unknown`, because quota may already have been consumed. The existing narrow stream exception remains: an interrupted or incomplete stream before any downstream SSE output may retry the same account once, within the remaining budget. After output starts, no account replay or stream splicing is allowed.

All accounts and same-account retries share at most 32 attempt slots and one deadline. Locally rejected candidates also consume an attempt slot, but are not billed as upstream sends. Non-stream calls use the configured non-stream timeout; streams use the stream idle setting as their pre-output deadline. After stream handoff, the normal idle timer applies, so a healthy long stream can outlive the non-stream timeout. Known upstream error statuses are retained even if the bounded error-body read times out. A budget exhausted before send returns 503 without an additional request.

When no candidate can be used, a known persisted quota cooldown can produce 429 with its reset time. With only local waits and no known quota reset, the gateway returns 503 rather than manufacturing `resets_at`. Recovery waits are visible in request diagnostics as `resource_wait`, with no upstream status or cost for a skipped send.

## Cost Accounting

The 5-hour, weekly, and monthly bars are local estimates, driven by what the
gateway actually forwards — not by the upstream's authoritative billing. Token
rates, window limits, and each model's `Usage` (official **Monthly limit**)
all come from the active OpenCode Go USD snapshot.

- The official multiplier defaults to `account monthly window / model monthly
  limit`. A user can override it for a temporary promotion; subsequent
  requests use the active persisted value, and refresh never overwrites it
  without confirmation.
- Current official examples against the `$60` account monthly window: `$15`
  models such as `deepseek-v4-pro`, `mimo-v2.5-pro`, and Grok 4.6 use
  `60 / 15 = 4x`; `$30` models such as `deepseek-v4-flash` use `2x`.
- The applicable local MiniMax adjustment is applied last. No supplier API
  price, CNY value, or exchange rate participates in the calculation.

Edge cases in the log:

- Without a streaming usage chunk (after the gateway has requested
  `include_usage` on Chat streams), the row ends with `success_no_usage`.
- Models absent from the snapshot are still forwarded, but finish as
  `success_unpriced`, display no quota cost, and do not enter quota totals.
- Zen free models finish as `success` with `cost_state=free`: tokens are
  recorded, quota cost stays empty, and they do not enter Go quota totals.
- Custom API forwards finish with `cost_state=unknown`, display no quota
  cost, and do not debit any provider quota.
- Pre-snapshot successful rows retain their old value and are marked as a
  legacy estimate; they are never recalculated.
- A manually saved percentage becomes the baseline for that window. Official
  refresh (manual **Refresh quota** or adaptive sync) on a ready Key or managed
  account overwrites the baseline with official OpenCode usage percentages.
  Successful priced costs recorded afterward accumulate until the next manual
  calibration or official refresh. A real inference `429` is decoded into scoped restriction facts; it does not rewrite the usage baseline. Eligible OpenCode Go inference 429s additionally schedule the existing later official reconciliation, not an inline fetch.
- An `outcome_unknown` row means the upstream may have completed and charged
  the request while the gateway lost the response; its local cost stays unknown. The only stream retry exception is the bounded pre-output case described above.

Each bar is shown next to the account's cooldown state — the next section
explains what actually stops traffic.

## What Actually Stops Traffic

**A full local estimate is not a routing prohibition.** The gateway keeps using that account unless a separate authorization or upstream restriction prevents it. Usage calibration is not recovery verification and does not clear local recovery waiting.

**A known upstream quota reset is persistent evidence.** A recognized window and valid reset write the existing matching cooldown without changing the usage baseline. Its dashboard bar is forced to 100% until the persisted cooldown expires or is explicitly reset. Unknown 429s no longer get an invented five-minute cooldown.

**Known exhaustion without a reset is local waiting.** The first eligible retry is scheduled around 30 seconds later, with small jitter. Repeated failed probes back off exponentially, capped at 300 seconds. This is permission to check, not a promise that upstream quota has recovered. Only a real client request performs the single-flight probe; there is no new background or synthetic paid request. Other requests skip the waiting or probing resource. A complete protocol-valid response confirms only its leased restriction; malformed 200 responses, cancellation, and incomplete streams do not.

Local waiting clears on process restart; already stored quota deadlines remain. A credential/binding generation change prevents old credit state from attaching to a replacement Key. The existing CAS cooldown-reset operation also clears associated local waits and fences late replies, but an account reset does not reset the anonymous Free pool. No new waiting badge or recovery button is added in this change; historical generic cooldown rows are not automatically rewritten.

## Zen Free models

Zen Free is one credentialless account card with one enable switch. Disable
the card if you do not want Free traffic, or leave it enabled and let its
position in the account list decide its routing priority.

**Refresh model catalog** on **Providers** calls the official keyless Zen
model directory only on user request. The backend keeps only IDs ending in
`-free` and saves the successful snapshot. The original ID is always available
as an exact raw pin. Stripping the official `-free` suffix publishes that
shorter name as an Alias, whether or not the Go table already has it. For
`mimo-v2.5-free`, both `mimo-v2.5-free` and `mimo-v2.5` work; a shared Alias
follows account-card order across Go and Zen. A Zen-only row such as
`muse-spark-1.3-contributor-free` likewise publishes `muse-spark-1.3-contributor`. **Providers**
shows the saved catalog and each model contract. A failed or empty refresh
leaves the last saved snapshot active.

Free and Go cooldowns are **independent**. Zen Free sends no authentication
headers. Each Free inference attempt identifies the official anonymous
channel with the same OpenCode client headers the TUI uses (`User-Agent`,
`x-opencode-session`, `x-opencode-client`, `x-opencode-request`,
`x-opencode-project`) so session stickiness and the shared egress-IP free
pool apply. Client-supplied OpenCode values win; otherwise the gateway fills
them in. Its promo quota is shared per egress IP, so a Free `429` restricts the anonymous Free channel rather than rotating Keys. A known reset is persisted; unknown recovery uses the same local single-flight waiting policy. Routing continues to later compatible cards in saved order. A Free-only request with no known reset returns local unavailability rather than a fictitious quota deadline. Successful Free rows keep token counts, use `cost_state=free`, and
do not enter Go quota totals. Free models are promotional and may use request
data to improve models — do not submit confidential content.

### GOAT credit rejection without a reset time

An exact GOAT `400` / `BAD_REQUEST` / `invalid_request_error` declaring insufficient credits is a resource rejection, not a malformed prompt. It enters the common model-specific quota-pool waiting path and the current request tries another eligible account. New requests skip that resource until one foreground reprobe is admitted; a healthy sticky A with a transient 429 can still return on the next request while a credit-exhausted H is skipped. Logs retain H's real 400 for the send and `resource_wait` for subsequent local skips. No account disablement, authentication error, or invented quota reset is written. Ordinary 400s (context, model, reasoning validation), 413s, and lookalike messages from another service do not gain retry permission.

---

[User guide index](../USER.md) · [简体中文](routing.zh-CN.md) · [Docs index](../README.md)
