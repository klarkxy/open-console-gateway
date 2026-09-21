[简体中文](routing.zh-CN.md)

# Routing, Cost, And Failover

A request resolves model identity from the saved destination catalog, then selects credentials in their global order. Supplier, credential and model enablement, protocol selection, model scope and explicit endpoint grants all constrain sending. Invalid configuration fails explicitly; there is no fallback to reconstructed legacy accounts. Each logical request freezes model mappings and transport configuration, while every send rechecks current authorization, Key version, cooldown and confirmed quota recovery. Changes invalidate an old candidate rather than silently redirecting it.

On **Accounts**, card order followed by Key order inside each card is the saved routing priority. Drag a card to move its Keys together, or move a Key within its card or to another card of the same supplier. Create another card for that supplier to arrange `A1 → B1 → A2`; both A cards use the same saved supplier configuration. Adjacent cards remain separate. Priority, round-robin and sticky routing retain their existing policies.

### Newly discovered OpenCode Go models

Refresh the model catalog on Providers, then explicitly enable newly discovered models and select the supported upstream protocol. Models in the saved Go catalog use their effective model contract even when no checked-in alias/protocol profile exists. Diagnostic planning cannot reject such models merely for being new. This does not enable unknown, disabled or removed models, and does not probe protocols during inference. A local catalog/protocol test is not proof that a live account has access to the model.

### Recognized quota versus temporary 429

Confirmed per-Key quota exhaustion dims that Key and skips routing. Usage at 100%, unknown, or failed never infers it. This exhaustion state does not copy to other members of a declared quota pool. Existing cooldowns remain valid; Zen Free uses its separate shared-channel recovery.

These errors are recognized quota:

- Kimi: an explicit `403` naming the 5-hour, weekly, or monthly window.
- OpenCode Go: an explicit 5-hour, weekly, or monthly window only. Structured `CreditsError` 401 may still mean an inactive subscription and keeps the existing `auth_error` path.
- GOAT: strict plan-window errors even when no reset deadline is given, plus known insufficient-credits errors.
- MiniMax: `1008` and `2056`.
- Configurable HTTP: structured `insufficient_quota` or `insufficient_balance`.

An unrecognized temporary `429` does not prove quota exhaustion. Without a usable retry constraint it excludes the account only from that request and keeps an otherwise healthy sticky target. No five-minute account cooldown is invented. `Retry-After` constrains the relevant endpoint/model; when accompanying confirmed per-Key exhaustion, it constrains that Key independently of its quota reset. It never becomes a displayed quota-reset time or spreads that Key's exhaustion to siblings.

A known reset waits until that time. An unknown window waits 15 minutes, then 1 hour, then 6 hours (capped); that step advances only after another proven quota error. When due, the next request that would normally select that Key is the single trial. The gateway does not send a synthetic probe, run a scheduler, or hold other traffic waiting for validation. Manual retry only makes the Key eligible. Only a completed successful JSON or SSE response restores it, including success with no usage. Timeout, cancel, or 5xx keep exhaustion, release the trial, and allow another try after at least 15 minutes without increasing the unknown-window step. A success from an old Key or an old exhaustion episode cannot clear the current state.

## Account Selection And Failover

On **Aliases**, expand a model to request a read-only routing explanation. It shows the resolved mapping, eligible service connections and Keys, upstream protocol, global order, structured exclusion reasons, and the base policy's expected first pick. This preview sends no upstream request, decrypts no Key, writes no cooldown, quota recovery or log, and does not advance sticky, round-robin or quota-trial state. It does not simulate a conversation binding, retry-time exclusions, state changes after the snapshot, or an upstream result, so it is an explanation of the observed base policy rather than a delivery guarantee.

Accounts are tried in **list order**, which you can drag into shape and persist
from the Accounts view. The selector skips:

- Disabled accounts.
- Accounts that are cooling down.
- Keys in confirmed quota exhaustion, except the single due trial when that
  Key is the ordinary next pick.
- Accounts that have already failed during the current request (e.g. with a
  `429`).
- Accounts whose saved provider contract has no effective enabled upstream
  protocol for the resolved model.
- Keys whose inference binding is disabled, or whose binding `modelScope`
  does not include the requested public/routing model. A sibling Key on the
  same connection keeps its own allow-list.

Keys that share a **declared quota pool** can share stored ordinary cooldowns.
Confirmed quota exhaustion stays on that Key and does not fan out. Matching
names do not create a shared pool. Switching Keys on the same identity does
not invent a fresh pool.

A recognized quota error persists per-Key recovery and the gateway tries the
next Key. Other failures follow their observed scope and retry constraints,
without treating arbitrary error text as quota evidence. A generic `403` fails over without
writing a quota cooldown; Kimi's explicit plan-window `403` is quota recovery, not
that generic path. Zen Free `401` is returned as-is. OpenCode Go structured
`CreditsError` 401 rotates to the next eligible card and persists `auth_error`
(it may still be an inactive subscription, not quota recovery);
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

The gateway does not replay `408`, `5xx`, or ambiguous send/body failures.
An incomplete or interrupted stream before any downstream output may retry
once on the same account within the original deadline. After output starts,
there is no replay or cross-account splicing. All attempts share a 32-attempt
budget and one pre-output deadline. Unresolved outcomes are reported as
`upstream_outcome_unknown` because the upstream may already have charged.
If every account is cooling or quota-exhausted, the gateway returns `429`
with the next known eligibility time. Purely process-local resource waits
without a known quota reset return `503`.

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
  calibration or official refresh. A real inference `429` only writes an
  scope-specific recovery or, when recognized as quota, per-Key recovery; it
  does not rewrite the usage baseline, but a temporary 429 does schedule a
  later official reconciliation.
- An `outcome_unknown` row means the upstream may have completed and charged
  the request while the gateway lost the response; its local cost stays unknown. The only stream retry exception is the bounded pre-output case described above.

Each bar is shown next to the account's cooldown state — the next section
explains what actually stops traffic.

## True And False Circuit Breakers

A full local quota bar or zero estimated credit balance never disables an
account. Calibration changes the display baseline, not routing eligibility.

Confirmed per-Key exhaustion follows the persistent wait and single-trial
rules above. Existing ordinary cooldowns remain effective until their stored
deadline or an explicit reset. Their matching usage bars stay at 100% while
active. Resetting ordinary cooldown does not clear per-Key quota recovery.

Other observed endpoint restrictions and anonymous Free exhaustion use
process-local waiting. When recovery is unknown, a real client request may
probe after about 30 seconds with small jitter; failed probes back off to at
most 300 seconds. No background or synthetic inference is sent. A complete,
protocol-valid response can clear only its own probe lease. Invalid 200 JSON,
partial streams and cancellation do not establish recovery. Restart clears
these local waits while persisted per-Key quota state remains.

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

An exact GOAT `400` / `BAD_REQUEST` / `invalid_request_error` declaring insufficient credits is an account-level quota rejection, not a malformed prompt. The current request tries the next eligible account once per account. That Key persists as exhausted (`insufficient_balance`) even when the upstream gives no reset time; Enabled does not change and it is not marked as an authentication failure. Recovery follows the per-Key wait and single trial above. Other 400s (context, model, reasoning validation), 413s, and similar unrecognized messages from other Providers do not gain this path.

---

[User guide index](../USER.md) · [简体中文](routing.zh-CN.md) · [Docs index](../README.md)
