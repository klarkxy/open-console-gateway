[简体中文](routing.zh-CN.md)

# Routing, Cost, And Failover

### Newly discovered OpenCode Go models

Refresh the model catalog on Providers, then explicitly enable newly discovered models and select the supported upstream protocol. Models in the saved Go catalog use their effective model contract even when no checked-in alias/protocol profile exists. Diagnostic planning cannot reject such models merely for being new. This does not enable unknown, disabled or removed models, and does not probe protocols during inference. A local catalog/protocol test is not proof that a live account has access to the model.

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

Keys that share a **declared quota pool** also share cooldown: exhausting the
pool through one Key blocks the others. Matching names do not create a shared
pool. Switching Keys on the same identity does not invent a fresh pool.

A `429` with a recognized `Resets in …` phrase writes `cooldown_until` and
the gateway tries the next account. `403` fails over without writing a
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

The gateway does not replay `408`, `5xx`, post-connect transport failures,
response-body timeouts, or interrupted streams. Ambiguous failures are
reported as `upstream_outcome_unknown` and logged as `outcome_unknown`,
because the upstream may already have consumed quota. When every enabled
account is cooling down, the gateway returns `429` with the soonest reset
time.

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
  independent cooldown and affects account selection; it does not rewrite the
  usage baseline, but it does schedule a later official reconciliation.
- An `outcome_unknown` row means the upstream may have completed and charged
  the request while the gateway lost the response; the request is not
  retried and its local cost stays unknown.

Each bar is shown next to the account's cooldown state — the next section
explains what actually stops traffic.

## True And False Circuit Breakers

- **False circuit breaker (local estimate).** The local estimate is a
  *signal*, not a stop sign. When it reaches the limit, the gateway **keeps
  sending** requests with that account. Local accounting and upstream
  billing/reset boundaries may not match, so a full local bar is a warning,
  not proof that the upstream account is blocked.
- **True circuit breaker (upstream 429).** The gateway stores the upstream
  error, parses the `Resets in …` phrase from the response, writes
  `cooldown_until`, and tries the next available account. The known 5-hour,
  weekly, and monthly limit messages use the reset duration reported by the
  upstream for that cooldown only; they do not rewrite the matching usage
  baseline. During cooldown the matching bar is forced to 100% in the
  dashboard; after cooldown, local priced costs continue from the existing
  baseline until the next manual calibration or official refresh. An
  unrecognized 429 falls back to a five-minute cooldown without changing
  any usage baseline.
- **No account available.** If every enabled account is cooling down, the
  gateway returns `429` with the soonest reset time.
- **Dashboard display.** While a true circuit breaker is active, the matching
  5-hour, weekly, or monthly bar is forced to 100% and marked as an error,
  even when the local estimate is lower. The account becomes eligible
  automatically after `cooldown_until`, or immediately after you reset its
  cooldown in the dashboard.

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
them in. Its promo quota is shared per egress IP, so a Free `429` cools the
whole Free channel and does not rotate keys. Routing then continues to later
compatible account cards in saved order; a Free-only model returns the shared
cooldown. Successful Free rows keep token counts, use `cost_state=free`, and
do not enter Go quota totals. Free models are promotional and may use request
data to improve models — do not submit confidential content.

### GOAT credit rejection without a reset time

An exact GOAT `400` / `BAD_REQUEST` / `invalid_request_error` declaring insufficient credits is an account-level rejection, not a malformed prompt. The current request tries the next eligible account once per account. Without an upstream reset time, OCG does not invent a cooldown, disable the Key, or mark it as an authentication failure. New requests may try that account again; logs retain the upstream 400 and fallback action. Other 400s (context, model, reasoning validation), 413s, and similar messages from other Providers do not gain retry permission.

---

[User guide index](../USER.md) · [简体中文](routing.zh-CN.md) · [Docs index](../README.md)
