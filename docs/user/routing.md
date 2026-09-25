[简体中文](routing.zh-CN.md)

# Routing, Cost, And Failover

A request resolves model identity from the saved destination catalog, then selects credentials in their global order. Supplier, credential and model enablement, protocol selection, model scope and explicit endpoint grants all constrain sending. Invalid configuration fails explicitly; there is no fallback to reconstructed legacy accounts. Each logical request freezes model mappings and transport configuration, while every send rechecks current authorization, Key version, cooldown and any persisted quota-recovery state. Changes invalidate an old candidate rather than silently redirecting it.

On **Accounts**, card order followed by Key order inside each card is the saved routing priority. Drag a card to move its Keys together, or move a Key within its card or to another card of the same supplier. Create another card for that supplier to arrange `A1 → B1 → A2`; both A cards use the same saved supplier configuration. Adjacent cards remain separate. Priority, round-robin and sticky routing retain their existing policies.

### Newly discovered OpenCode Go models

Refresh the model catalog on Providers. Newly discovered models are enabled automatically when a supported upstream protocol is known; models without protocol evidence remain unavailable until that evidence exists. Models in the saved Go catalog use their effective model contract even when no checked-in alias/protocol profile exists. Diagnostic planning cannot reject such models merely for being new. This does not re-enable models you explicitly disabled or removed, and does not probe protocols during inference. A local catalog/protocol test is not proof that a live account has access to the model.

### 429 cooldowns and official observation

Every upstream `429` starts a temporary cooldown for the exact Key that
received it: 30 seconds when there is no usable constraint. A valid
`Retry-After` delay or HTTP date can extend that wait and never shortens an
already longer wait. The cooldown is not a displayed quota-reset time and does
not spread through a declared quota pool. Zen Free retains its existing anonymous egress-IP recovery scope instead of a Key scope.

The gateway never infers persistent quota or balance exhaustion from an HTTP
status, an error body, or text in that body. A `403` is request-local and does
not persist `auth_error`. MiniMax still validates its structured error envelope,
including an error envelope delivered with HTTP 200, but its codes and message
do not create persistent quota or balance state.

After a `429`, the gateway can queue an asynchronous, coalesced and throttled
official-usage refresh for an adapter that supports it; the client request does
not wait for that work. Fetched authoritative OpenCode Go usage can establish or
clear Go quota state. GOAT and CN usage snapshots remain display-only unless an
adapter explicitly declares a different authority. A failed or unsupported
refresh retains the last observation, or remains unknown. Existing persisted
quota episodes are retained rather than bulk-purged; new error responses do not
create them.

## Account Selection And Failover

On **Aliases**, each mapping row carries a **routing order** column: the global routing ranks of the Keys that can serve that public name at the configuration level (the same order you drag into shape on the Accounts view). Rows within a model are sorted by ascending rank; a plan backed by several eligible Keys lists each rank in turn; "—" means no enabled Key currently serves the mapping. This is the configured order — runtime states such as cooldowns or quota waits are not reflected here, and it is not a delivery guarantee. Custom Keys linked from a platform (new-api/sub-api) also show the platform name as a tag next to the plan name, so rows serving the same model are easy to tell apart by origin.

Accounts are tried in **list order**, which you can drag into shape and persist
from the Accounts view. The selector skips:

- Disabled accounts.
- Accounts that are cooling down.
- Keys with an active temporary `429` wait or a quota-recovery deadline that has not elapsed.
- Accounts that have already failed during the current request (e.g. with a
  `429`).
- Accounts whose saved provider contract has no effective enabled upstream
  protocol for the resolved model.
- Keys whose inference binding is disabled, or whose binding `modelScope`
  does not include the requested public/routing model. Matching trims and
  ignores ASCII case; `/`, `_`, spaces, and `-` stay different models. A
  sibling Key on the same connection keeps its own allow-list.
- For an otherwise eligible Key, the gateway chooses one upstream protocol
  from those that are enabled, have a configured route, and are granted to
  that Key. It prefers the client protocol when that path is granted and the
  request can be converted, then the saved preference, then the other granted
  protocols. It does not send a request to discover which protocol is
  authorized. The pre-send check still re-reads the current grants.

Keys that share a **declared quota pool** can share stored ordinary cooldowns.
The temporary cooldown created by a `429` remains on the receiving Key.
Matching names do not create a shared pool. Switching Keys on the same identity
does not invent a fresh pool.

A `429` cools the receiving Key temporarily and the gateway tries the next
eligible Key. Other failures follow their observed scope and retry constraints,
without treating arbitrary error text as quota evidence. A `403` fails over
for this request without writing a cooldown or `auth_error`, including Kimi
responses. Any rejected Zen Free HTTP response briefly cools its anonymous
channel and tries the next compatible card. OpenCode Go structured
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
If every account is cooling or has persisted quota-recovery state, the gateway returns
`429` with the next known eligibility time. Purely process-local resource waits
without a known eligibility time return `503`.

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
  calibration or official refresh. A real inference `429` does not rewrite the
  usage baseline, but can queue a later asynchronous official refresh when that
  adapter supports it.
- An `outcome_unknown` row means the upstream may have completed and charged
  the request while the gateway lost the response; its local cost stays unknown. The only stream retry exception is the bounded pre-output case described above.

Each bar is shown next to the account's cooldown state — the next section
explains what actually stops traffic.

## True And False Circuit Breakers

A full local quota bar or zero estimated credit balance never disables an
account. Calibration changes the display baseline, not routing eligibility.

An upstream `429` uses the temporary cooldown above. Existing ordinary
cooldowns remain effective until their stored deadline or an explicit reset.
Resetting an ordinary cooldown does not rewrite persisted quota-recovery state.

No background or synthetic inference is sent to clear a temporary cooldown.
Restart clears process-local waits while retained persisted quota episodes
remain available for authoritative Go usage to reconcile.

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
them in. Its promo quota is shared per egress IP. Any Free HTTP error, malformed
JSON, or explicit error body temporarily restricts the anonymous channel rather
than rotating Keys; `429` also honors a valid `Retry-After`. A connection
failure known to precede sending follows the same fallback. Routing continues
to later compatible cards in saved order.
An exact `-free` raw pin stays on Free and cannot silently switch to a different
model. With no other compatible route, the gateway returns a local unavailable
or rate-limited response. Once SSE output has begun, it cannot switch sources.
Successful Free rows keep token counts, use `cost_state=free`, and
do not enter Go quota totals. Free models are promotional and may use request
data to improve models — do not submit confidential content.

## OpenRouter Free

OpenRouter and OpenRouter Free are separate, reorderable preset connections that
use an OpenRouter API Key. The Free preset starts with `openrouter/free` on Chat
Completions; OpenRouter chooses the underlying free model for each request. To
use a particular free model, add its exact catalog ID ending in `:free`. A paid
model does not become free merely by appending that suffix. The Free preset
does not refresh OpenRouter's mixed paid/free catalog into enabled routes.

Free-model HTTP errors, explicit error bodies and malformed JSON temporarily
pause that model route and try another compatible route for the same public
name. A `429` instead waits on the receiving Key and honors a valid
`Retry-After`. These temporary waits do not mark the paid balance exhausted or
cool Zen Free. An exact `openrouter/free` or `:free` public model remains pinned;
switching to a paid model requires a shared public alias configured by you.
Free attempts do not debit a configured local paid Credit estimate. OCG leaves
their total cost unknown and does not provide an official daily-limit meter;
check OpenRouter's bill for any optional charged features. Once
streaming output begins, the gateway cannot change providers mid-response.

### GOAT credit errors

A GOAT response that reports insufficient credits is an upstream error for the
current request; its body does not create persistent quota or balance state.
Other 400s (context, model, reasoning validation), 413s, and similar errors
remain request-local as well. Only a `429` starts the temporary cooldown and
optional asynchronous official refresh described above.

MiniMax reports cache counters unchanged through JSON, SSE, and request
accounting. The gateway does not rewrite those counters from a model prefix.

---

[User guide index](../USER.md) · [简体中文](routing.zh-CN.md) · [Docs index](../README.md)
