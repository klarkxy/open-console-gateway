[简体中文](accounts.zh-CN.md)

# Accounts

**Add account** first distinguishes an existing connection from a new service. Choose an existing Provider to add another Key using its saved address, protocol and models — including a user-defined Provider saved from **Providers** that still has no Key. Choose a new service to browse Plan/API presets or add a platform site; saving a preset creates a Provider and its first account together. Saving a preset from **Add account** uses the same onboarding commit as **Providers**. Connection summaries remain visible before entering a Key. Regional variants use a compact picker. Keys are stored by the account service; you can add one here or from a Provider's detail with **Add Key**.

**Enabled** describes configuration, not a successful upstream authentication or model test. Test results stay in the test dialog. User-defined Providers have no modeled subscription period, including those created from Plan presets: their accounts do not show an inferred purchase date, expiry countdown, or expiry alert. Existing stored purchase anchors are preserved for compatibility, but are not presented as confirmed billing facts.

Accounts still edit through the same forms. Card status and relations come from the identity projection: a declared relation is not a verified wallet, and dynamic or Custom dates stay unknown unless V3 already stored a real purchase date.

A ready Key card can **Rotate Key**, **Add Key**, and **Edit binding** from the overflow menu. Rotate replaces only the Key this console will send on later requests for that card's credential (same credential id; version numbers increase). It is a local replacement: the provider-side credential is not revoked and stays under your control. **Add Key** creates another inference Key on the same identity, defaulting to that card's connection. Quota is independent unless you explicitly share with a selected inference Key on that identity; belonging to the same identity is not enough. After save, cards that actually share a stored quota pool show that relationship (naming the sibling Key when possible). A third Key on the same identity stays independent when it has its own pool or none. Custom API, Zen Free, CPA, no-auth, and observer credentials do not expose Add Key (Custom API still uses its dedicated account editor). If create does not return a definite result, the form keeps the submitted contents and operation: retry the same body, or cancel; do not change the form and submit again (that can create a duplicate Key).

Edit binding changes that credential's enabled state, model scope (all models, or only the exact names you list), and — when you change it — destination consent: which configured endpoint this Key may be sent to (protocol and URL). Saved destination grants are facts; if a provider URL later changes, the saved Origin is shown and that endpoint stays unchecked until you explicitly allow the new destination. Changing only scope or enabled leaves destinations unchanged. Clearing both destination lists revokes access. Sealed official endpoints with no URL stay locked destinations and do not invent Origin strings. A disabled binding is shown on that card and does not flip the account enable switch. Zen Free, CPA, no-auth, and observer credentials do not expose rotate or binding. An identity can hold more than one Key; each card uses the credential whose legacy account id matches that card.

Accounts is the tenant list. A Provider and a Plan are the same product
identity (`provider_id` only), and every card belongs to one Provider with one
credential when that Provider requires it. Quota authority is Provider-specific:
OpenCode Go counts usage by account **Key**, Zen Free shares free cooldown by
egress IP, and Custom API keeps no provider-side quota. All cards share one
manually persisted global order; capability filtering runs first, then strict priority, global
sticky, and round-robin all read from that order. There is no per-model quota
pool.

**Accounts** owns identity, the account **Key**, verification, enabled state,
card order, managed registration, and available usage / cooldown state.
Catalogs, protocol probes, per-model protocol overrides, user-defined Provider
Endpoint/protocol/mappings, and scoped pricing live on **Providers**.
A user-defined Provider account only stores Key (when auth requires it), notes,
enablement, and runtime state. Custom API is the exception: that account still
owns Endpoint, protocol, and model mappings. No-auth user-defined Providers
expose one singleton account and reject a second. GOAT cards show a clearly
labelled local estimate: priced OCG request logs accumulate against the public
`$14 / $35 / $70` windows. Command Code exposes no machine-readable usage API.
Traffic outside OCG and unpriced rows are not included; manual calibration can
correct the displayed baseline. Paid Ollama Cloud cards (Pro / Max / Team) show
one monthly USD-Credits window from locally priced request logs against
`$60 / $300 / $1000`. Ollama Cloud exposes no official usage API in this
product. The meter is a soft estimate — used credit may exceed the limit, the
bar clamps at 100%, and fullness never writes cooldown or changes routing. New
accounts must choose Pro, Max, or Team plus a purchase date. Existing accounts
with no billing row stay unconfigured until edited and remain routeable.

The Adapter Registry is sealed. Built-in Provider families are:

| Family | Provider ID | Live routing | Notes |
| --- | --- | --- | --- |
| OpenCode Go | `opencode` | Yes | One officially distributable API key per card; managed signup remains Beta |
| Zen Free | `opencode-zen-free` | Yes | One credentialless, anonymous singleton; sortable and enableable, not deletable; quota shared by egress IP |
| Command Code GOAT | `command-code` | Yes | Public Provider catalog; GOAT preset models default on, additional models default off in the Providers matrix; no account-level GOAT/All or Max mode |
| MiniMax CN Token Plan | `minimax` | Yes | Dedicated `sk-cp` Key; fixed official Chat and Messages routes, authenticated model directory, and manual official Token Plan usage refresh |
| Kimi Code CN | `kimi` | Yes | Dedicated Kimi Code Key; fixed official Chat and Messages routes, authenticated model directory, and manual official weekly/rate-window usage refresh |
| Ollama Cloud | `ollama` | Yes | Fixed-origin Chat Completions only (`https://ollama.com`, Bearer); public keyless catalog refresh; account billing tier (Pro $60 / Max $300 / Team $1000 USD Credits per billing month) plus purchase date; local monthly soft-credit estimate from official per-request usage and the manual `https://ollama.com/pricing` table; unconfigured existing accounts stay routeable with no meter |
| Custom API | `custom` | Yes | Trusted-administrator destination; one API URL, one account-wide upstream protocol, and public-name → upstream-ID mappings per account; common base URLs are completed automatically; new accounts default on; eligible public names appear on `/v1/models`; unpriced/unknown cost, no quota debit |

## Move a node configuration

Use **Export** on the Accounts toolbar to create a password-encrypted
`.ocgbackup` file, then use **Import** on the destination node to preview and
confirm the merge. Choose a migration password of at least 12 characters and
transfer it separately from the file; Open Console Gateway cannot recover it. The
operation remains available only from the node's loopback dashboard; forwarded
scheme headers do not grant access to a remote dashboard.

The current V6 payload moves usable ordinary accounts and their stable IDs,
ready account Keys, Custom Endpoint/public-model → upstream-ID mappings and verification state,
user-defined Providers (`providerId` only),
the primary and active sub Access Keys, portable routing/proxy settings, Zen
Free enablement/catalog, Provider catalogs, evidence, and protocol
overrides, and an explicit identity / credential / binding / quota-pool snapshot.
Shared identities, a second credential on the same identity, binding model
restrictions and enabled flags, and quota-pool membership and declared/unknown
evidence are restored as stored. Matching stable IDs are merged with package-owned portable fields;
same-Plan or same-name rows with different IDs coexist and independent same-URL
accounts are not merged. Existing destination
accounts keep their current order and position; source-only accounts append in
package order. Destination-only Access Keys and Provider scopes are retained.

Browser profiles/cookies, third-party login passwords, referral codes, logs,
and usage history do not move. V6 carries source cooldown deadlines without
shortening a later destination deadline; V4/V5 retain their older host-local
cooldown behavior. Existing destination usage history and browser data stay in
place; stale authentication and last-error flags are cleared when package
account fields replace the stored credential.
Machine-local listener/root URL, auto-start, and Dock settings also stay with
the destination. Ready managed accounts keep their Key, but their browser login
does not move; unfinished managed drafts are skipped. Import accepts payload
V4, V5, and V6. V4/V5 packages rebuild one identity, credential, All-scope
binding, and identity quota pool per account. Payload V1–V3 and future V7
backups are rejected with an explicit unsupported-version error. A V4/V5 file
that already contains V6 identity fields is rejected rather than silently
dropping them. The outer encrypted envelope remains version 1 and
is distinct from the portable payload version.

Every persistent mutation path rejects `enabled=true` for a catalogued
`routable=false` Provider before it mutates the row, revision, or timestamps.
GOAT catalog refresh updates the model directory; Key auth is observed from
inference 401/403. An enabled, ready account with a non-empty Key can route
models enabled in the Provider matrix. A newly created, routable Custom API
account defaults to enabled. Editing the Endpoint, capabilities, Key, or
protocol preserves its enabled state. Disabled drafts remain saveable.

Use only the official provider API **Key** for OpenCode Go, Command Code GOAT,
MiniMax Token Plan, or Kimi Code. Browser cookies and reverse-proxy credentials
are not account Keys. GOAT is a separate provider mapping and its Key is sent
only to the fixed Command Code Provider API, never to OpenCode. Custom API is a
separate trusted-administrator destination and must not send its key to an
OpenCode endpoint.

MiniMax and Kimi keys are also origin-bound: MiniMax CN uses
`https://api.minimaxi.com/v1`; Kimi Code CN uses
`https://api.kimi.com/coding/v1`. Model and usage refreshes are explicit
dashboard actions. Usage display never changes routing eligibility. Before the
first successful usage refresh, the account card still shows a neutral **Not
yet refreshed** quota bar; official windows replace it after refresh.

Command Code's official `GET /models` is public and refreshes one
Provider-level catalog. The Providers matrix is the model-supply control: GOAT
preset rows default on, newly discovered rows default off.

Custom API is a live trusted-administrator destination. **Accounts** is the
only editor for its mappings: each row pairs a public model name (what the
client requests) with the exact upstream model ID (what OCG sends). The card
stores one API URL, one upstream protocol (Chat Completions, Responses, or
Messages), and at least one mapping. That protocol is uniform across every
mapping on the account and is the effective preferred
protocol: matching client traffic passes through, while other supported client
formats convert to it. See [Protocol conversion](protocol-conversion.md).
Entering an origin root is recommended:
OCG appends `/v1` and the selected protocol path. A base already ending in
`/v1` is used without duplicating that segment. Existing complete standard
Endpoints remain exact. **Fetch models** derives `/v1/models` from a root or
versioned base, or the sibling `/models` from a complete standard Endpoint.
Non-standard complete paths remain exact for inference and retain manual model
entry instead of guessing a directory URL. Discovery returns upstream IDs only.
Choosing one imports a row with the public name and upstream ID exactly equal.
Fetching does not save, verify, or enable the account.

A trusted administrator may configure a public, LAN, or loopback HTTP or HTTPS
origin. Metadata, link-local, and opaque IPv4-trick hosts (for example
`169.254.169.254` or `metadata.google.internal`) are rejected. URL-embedded
credentials, query strings, and fragments are rejected. The gateway
rejects redirects and does not forward dashboard or client authentication.
A model or endpoint override to another Origin does not inherit the stored Key.
Chat Completions and Responses use `Authorization: Bearer <key>`; Messages uses
`x-api-key: <key>`. A 401 does not retry with a different auth header. Root and
`/v1` bases resolve through the same rule for discovery, verification, and
production inference; legacy complete Endpoints are requested verbatim.
Custom HTTP uses the same process-wide Direct / Manual / Auto proxy policy;
connect and request timeouts are bounded from the configured connect timeout
(clamped 5–60 seconds).

Every ready account card has the same **Test connection** action. It opens an
account-scoped, searchable model table with single-model and sequential
**Test all** controls. Each test sends one minimal real request through that
exact account and its current effective protocol. Tests stay on that account:
they do not switch accounts, run gateway fallback, change enablement or
cooldown, or write Provider protocol evidence. Results live only in the open
dialog; closing it stops dispatching queued tests. A request already sent may finish, and testing
may consume provider quota. Provider-page tests remain the separate,
low-frequency control for validating newly added Provider model/protocol
capabilities and may use eligible-account fallback.

Eligible accounts (enabled + ready + non-empty key) expose only their routeable
public names on authenticated `GET /v1/models`. A Custom public name resolves
to its paired exact upstream ID. A public name never steals a published
built-in Alias. Raw identity
conflicts are excluded from publication and resolve as `ambiguous_model_id`
without an upstream call. Undeclared names stay unknown (`400`). Changing the
Endpoint, Key, mappings, or protocol leaves the account enabled. Endpoint and
upstream protocol can be edited after create; the config and complete mapping
set are replaced in one CAS transaction. Disabling the declared protocol makes
the model unroutable; no fixed-priority fallback or override can enable an
undeclared protocol. Custom traffic is
unpriced: logs record `cost_state=unknown` with no quota debit, and Custom has
no provider usage refresh. `MODEL_PROTOCOLS` remains Go-specific; Custom
converts the client protocol to the account's single upstream protocol.

Use the existing-connection choices to add another Key without creating a second Provider. New-service choices contain Plan/API presets, Custom API and platform types. Search matches vendor, variant, preset name and endpoint host. Selecting a result retains its exact variant when the search clears. Zen Free is a backend-owned singleton, managed only from the account list; OpenCode Go retains its optional managed-registration action where the host supports it.

- A **Key account** stores one officially distributable OpenCode Go API key.
- A **managed account** immediately creates a disabled, recoverable draft, then
  runs the wizard through optional sign-in identity, invite registration,
  payment, and key verification. The draft and current step are persisted to
  SQLite, so closing the page or restarting the service does not lose the flow.
  Pending accounts cannot be selected by the gateway and do not expose usage,
  verify, or enable controls.

Managed signup and isolated browser profiles are **Beta** features. They have
not been thoroughly tested; do not rely on them in production.

When you create a managed draft, the form shows the **invite URL** (prefilled
from the OpenCode Go provider; fresh installs may ship a demo default). Edit it
in place: it must be an HTTPS URL no longer than 2,048 characters, contain no
username or password, and use exactly `opencode.ai` or `console.opencode.ai` as
its host. If it differs from the saved value, it is written back to
**Providers → OpenCode Go → Settings**. Changes affect later invite-page opens
only; they do not rewrite completed accounts. Replace the demo default with your
own invite link before a real signup, or referral credit goes to the link owner.

The managed wizard is intentionally manual (no password autofill, no payment
clicks, no automatic key extraction):

1. **Sign-in identity (optional).** Sign up for Google or GitHub only if you
   need a new account; otherwise **skip this step**. OpenCode sign-in can also
   finish on the next step.
2. **Invite registration.** Open the invite URL in the same isolated profile and
   complete OpenCode sign-in/registration with Google or GitHub.
3. **Payment.** Confirm the plan and amount in the console; only you complete
   payment on the page.
4. **Verify Key.** Copy the key from the console, paste it, and run a real
   upstream probe.

Click an earlier finished step in the step bar to **rewind**; forward progress
still uses each step's primary button. A `2xx` verification completes and enables
the account. A `429` also proves that the key is valid, completes the account,
and records the current cooldown. `401`/`403`, network errors, and `5xx`
responses leave the account at key verification so you can correct it and retry.

Every account has a durable, isolated browser profile. Desktop builds launch an
external Chromium-family browser: Windows prefers Edge and then Chrome; macOS
checks Chrome, Edge, and Chromium; Linux desktop searches `PATH` for Chrome,
Chromium, or Edge. It uses only `browser-profiles/<account_id>`, first-run
suppression, and a new window; it does not enable CDP, automation,
`--no-sandbox`, or weakened web security. Older `profiles/<account_id>` WebView
data is deliberately not imported, so the first open after upgrading requires
another login.

Every completed account has **Open OpenCode console**
(`https://opencode.ai/auth`). A legacy account starts with a blank isolated
profile the first time; sign in once and its cookies remain available.
Google/GitHub and OpenCode cookies belong to different domains, but both stay in
the same account profile.

Resetting browser identity first closes that account's browser and removes both
new and legacy profile directories. A completed account keeps its key and is only
signed out of the console; a pending managed account also returns to the sign-in
identity step. Deleting an account likewise deletes its cookies/profile, and the
confirmation states this explicitly. That login state can then be recovered only
from a backup or by signing in again.

Each ready OpenCode Go or GOAT card shows the account name, cooldown state, and
5-hour / weekly / monthly usage bars. OpenCode Go periodically calibrates the
local accounting against its official endpoint. GOAT bars remain a local
projection of priced OCG logs. Zen Free has its own anonymous, egress-IP-shared
free cooldown rather than a key quota.

- **Usage baselines.** Type a percentage or drag a bar to set its current
  real-world usage baseline. After the value is saved, successful request cost
  recorded by Open Console Gateway continues to accumulate above that baseline. Reaching
  100% is still only a warning; it does not stop the gateway from selecting the
  account. Manual calibration is shown only when the Plan declares it; GOAT
  uses it to correct for traffic that OCG cannot observe.
- **Refresh quota (ready Key and managed accounts).** Official OpenCode usage
  (`/zen/go/v1/usage`) is only a periodic calibration baseline; local forward-log
  costs stay the live estimator. Active ready accounts reconcile about hourly,
  inactive ones about daily; disabled, unfinished, or empty-key accounts are
  never auto-refreshed. Opening this page or starting the gateway does not force
  a fetch: new schedules are spread across the first 0–15 minutes. **Refresh
  quota** runs the same path on demand with a 15-second per-account server
  throttle (Retry-After / next-allowed). The card shows the last successful
  official sync time and any temporary retry wait. Local estimates that reach
  ≥80% may trigger one expedited sync per 15 minutes. A real inference `429`
  still writes the existing cooldown/selector state and additionally schedules
  an official reconciliation about 1–2 minutes later; official failures or
  `status=rate-limited` never write inference cooldown. Failures keep the
  previous baseline and last-success timestamp. The request uses the same global
  outbound proxy as other dashboard fetches.
- **Identity and credentials.** The name is the account's required primary
  display label. The login account field is optional; on Key-account creation,
  entering it first copies it into the name until you edit the name yourself.
  Optional freeform notes live in **Edit account**. They can stay empty and do
  not affect routing or quota. The dashboard stores the account key but does not
  collect or manage third-party login passwords.
- **Purchase date.** New lifecycle-bearing accounts default to the browser's
  current date. Click the expiry tag on a card to choose another purchase date
  or set it directly to today; the full edit form remains available. The managed
  wizard also writes the purchase date when
  payment advances to key verification. Expiry is the same day in the next
  natural month, clamped to that month's last day when necessary:
  `2026-01-31` expires on `2026-02-28`. Accounts and Dashboard show days
  remaining, due today, or days expired. This is informational only and never
  disables an account or prevents the gateway from selecting it. Zen Free and
  Custom API have no purchase-cycle expiry and show no expiry tag or alert.
- **Priority order.** Use the drag handle on an account card to persist its
  priority with a mouse, touchscreen, or pen. When the handle has keyboard focus,
  the Up and Down arrow keys move the account as well. Dashboard, the Logs
  account filter, CLI listings, and the gateway selector all consume this same
  SQLite-backed order.
- **Cooldown reset.** You can reset a cooldown manually from this view. The bar
  snaps back to its local estimate as soon as the cooldown is cleared.

---

[User guide index](../USER.md) · [简体中文](accounts.zh-CN.md) · [Docs index](../README.md)
