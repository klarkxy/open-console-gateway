[简体中文](routing.zh-CN.md)

# Routing And Failover

A request resolves model identity from the saved destination catalog, then selects credentials in their global order. Supplier, credential and model enablement, protocol selection, model scope and explicit endpoint grants all constrain sending. Invalid configuration fails explicitly; there is no fallback to reconstructed legacy accounts. Each logical request freezes model mappings and transport configuration, while every send rechecks current authorization, Key version, cooldown and any persisted quota-recovery state. Changes invalidate an old candidate rather than silently redirecting it.

On **Accounts**, card order followed by Key order inside each card is the saved routing priority. Drag a card to move its Keys together, or move a Key within its card or to another card of the same supplier. Create another card for that supplier to arrange `A1 → B1 → A2`; both A cards use the same saved supplier configuration. Adjacent cards remain separate. Priority, round-robin and sticky routing retain their existing policies.

### Newly discovered OpenCode Go models

Refresh the model catalog on Providers. Newly discovered models are enabled automatically when a supported upstream protocol is known; models without protocol evidence remain unavailable until that evidence exists. Models in the saved Go catalog use their effective model contract even when no checked-in alias/protocol profile exists. Diagnostic planning cannot reject such models merely for being new. This does not re-enable models you explicitly disabled or removed, and does not probe protocols during inference. A local catalog/protocol test is not proof that a live account has access to the model.

### 429 cooldowns and official observation

An unrecognized upstream `429` starts a temporary cooldown for the exact Key that
received it: 30 seconds when there is no usable constraint. A valid
`Retry-After` delay or HTTP date can extend that wait and never shortens an
already longer wait. That temporary cooldown is not a displayed quota-reset time and does
not spread through a declared quota pool. Zen Free retains its existing anonymous egress-IP recovery scope instead of a Key scope.

Command Code GOAT has one exception. It is an HTTP 429 whose `error.code` is `RATE_LIMITED`, whose `error.type` is `rate_limit_error`, and whose `error.message` is the complete sentence `You've reached your weekly usage limit for your plan. Your limit resets at <RFC3339>. Please wait for the window to reset or upgrade your plan to continue.` Only `5-hour`, `weekly`, and `monthly` may stand in the window position, and the reset must be strictly in the future. That deadline is saved only on the Key that received it. Each window keeps the later time. Accounts show the later of that Key-local deadline and any ordinary cooldown. The gateway does not invent a balance, a usage figure, or a quota-recovery episode from it, and it does not change sign-in state or the enable switch. It does not fan out through a shared pool and is not written into the ordinary cooldown columns. A pool join, a shared ordinary cooldown, or resetting a sibling Key does not copy or clear it. Replacing the Key — rotation, a bulk replace, managed verification, or an import of a different Key — clears only that map, then applies a valid map from the incoming file. Saving the same Key keeps it, and an older backup that omits the field leaves the deadlines already on this node in place. A move to another routing card keeps it. A manual cooldown reset clears it and fences a response that was already in flight. A longer `Retry-After` stays an in-memory wait on that Key. An unrelated catalog refresh does not drop it. It is not saved as the plan reset, is not part of a node export, and does not survive a restart. A declared reset shorter than 30 seconds is not raised to 30 seconds. When the latest active deadline passes, the Key is eligible again, and sticky routing is not reset. A wrong profile, an incomplete sentence, an unknown window, a non-future reset, or a `429` missing the exact code or type stays on the temporary cooldown. A GOAT insufficient-credits `400` is the existing in-memory wait on that Key and model, 30 to 300 seconds by default. It does not write the map.

Aside from that exact sentence, the gateway never infers persistent quota or balance exhaustion from an HTTP
status, an error body, or text in that body. Unknown bodies stay opaque. A `403` is request-local and does
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

Each mapping row also carries a **capabilities** column: the effective input modalities of the route behind that mapping (text/image/audio/video) plus their provenance (operator declaration, upstream discovery, or the models.dev catalog). "Unknown" means no source has reported modalities for that route yet; its **Declare** link opens the model capabilities editor on the matching Providers row. Declarations stay route-scoped there — the Aliases page only reads them. CPA rows have no per-route metadata and show "—".

Accounts are tried in **list order**, which you can drag into shape and persist
from the Accounts view. The selector skips:

- Disabled accounts.
- Accounts that are cooling down.
- Keys with an active temporary `429` wait, a GOAT plan-window deadline, a longer in-memory `Retry-After`, a local-policy wait (for example a GOAT insufficient-credits wait on that Key and model), or a quota-recovery deadline that has not elapsed.
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
The temporary cooldown created by a `429` remains on the receiving Key. A declared GOAT plan window is also stored only on the receiving Key.
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
If every account is cooling, waiting on a GOAT plan-window deadline, waiting on a longer in-memory `Retry-After`, or has persisted quota-recovery state, the gateway returns
`429` with the next known eligibility time. Purely process-local resource waits
without a known eligibility time, including local-policy waits, return `503`.

## Usage windows

OpenCode Go and GOAT show 5-hour, weekly, and monthly windows from the official percentage and reset when that reading exists. You can also save a manual percentage. The window uses that percentage against a full window of 100. Later requests do not add a price on top. A window with no official reading and no manual percentage stays unavailable. It is not shown as 0.

A full window does not disable the account. Logs still record tokens when the upstream sends them. The gateway does not estimate a price for a new request. A cost that was not recorded stays unknown and is not shown as zero or free. Older log rows keep a previously stored cost and are not recalculated. If the gateway loses the response, the outcome stays unknown and no local price is invented. The only stream retry exception is the bounded pre-output case described above.

Each bar is shown next to the account's cooldown state — the next section explains what actually stops traffic.

## True And False Circuit Breakers

A full usage window, an unavailable window, or an empty manual credit balance never disables an account. Calibration changes the displayed baseline, not routing eligibility.

An upstream `429` uses the temporary cooldown above. Existing ordinary
cooldowns remain effective until their stored deadline or an explicit reset.
Resetting an ordinary cooldown on the selected Key also clears that Key's saved GOAT plan deadlines and leaves a sibling Key's deadlines in place. It does not rewrite persisted quota-recovery state.

No background or synthetic inference is sent to clear a temporary cooldown.
The same holds for local-policy waits. Restart clears process-local waits, including a longer in-memory `Retry-After`, while the saved GOAT plan map and retained persisted quota episodes remain available for authoritative Go usage to reconcile.

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
Successful Free rows keep token counts. They are not given a local price, and they do not enter Go quota totals. Free models are promotional and may use request
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
Free attempts do not change a manual credit balance. Their cost stays unknown and is not shown as zero. OCG does not provide an official daily-limit meter;
check OpenRouter's bill for any optional charged features. Once
streaming output begins, the gateway cannot change providers mid-response.

### GOAT credit errors

A classified GOAT insufficient-credits response is an upstream error on that
send. The current request may still fall over to the next eligible Key (the
existing first fallback). The Key that reported insufficient credits, plus the
actual upstream model, starts the existing process-local wait (30 to 300
seconds by default), so later requests skip it
until a real client request on that same route is due. The same Key is not
retried on this request. The body
does not create persistent quota, a saved plan deadline, or balance state, change enablement, or
record `auth_error`.

Other 400s (context, model, reasoning validation), unknown 400s, 413s, and
similar errors remain request-local: this policy does not add a first
fallback for them. A custom matcher on Settings can still install a later
skip; the current unknown-400 request fails as it did before. Configure
global and per-connection rules in [Temporary unavailability](temporary-unavailability.md).

An unrecognized `429` starts the Retry-After temporary cooldown described
above. The exact GOAT plan-limit `429` uses that Key's declared deadline
instead. Either may queue the supported optional asynchronous official
refresh described above. That refresh never clears or rewrites inference
cooldown or the plan-window map.

MiniMax reports cache counters unchanged through JSON, SSE, and request
accounting. The gateway does not rewrite those counters from a model prefix.

---

[User guide index](../USER.md) · [简体中文](routing.zh-CN.md) · [Docs index](../README.md)
