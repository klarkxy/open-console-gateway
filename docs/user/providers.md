[简体中文](providers.zh-CN.md)

# Providers

The connection list and selected connection's models load in pages from the
local Rust core. Search covers the complete saved list or model catalog.
Editing loads the selected connection's complete configuration and grants;
bulk selection covers all matching models, including other pages. Alias
links locate the exact model's page. Revisiting keeps visible content while
revalidating it.

Opening **Add Provider** from Providers keeps the origin: Cancel returns to that selection, and a successful setup selects the new connection on its Models tab. A saved draft continues on Providers. Alias rows link back to their provider with the exact public model selected in the model search; account-owned mappings open that account. New API/Sub2API platform parents are managed on Accounts and remain there after creation; they are not provider model rows.

The rail lists saved V4 Destinations as canonical rows, plus unmatched
onboarding draft Connections, in one searchable list. The list defaults to
name A–Z; use the sort selector to switch to Z–A. Existing Custom API records
remain separate Destinations; equal names or URLs are never merged. Unused
built-in templates stay off the rail and remain in the **Add Provider**
catalog only. Row and detail-header status comes from server-side projection
fields only: **Draft** (lifecycle `draft`), **Missing credential**
(authorization `missing` on a configured connection), **Disabled** (lifecycle
`disabled` or all credentials disabled), **Invalid credential** (authorization
`invalid`), **No enabled model** (eligibility reason `no_enabled_target`),
**Cooling down** (eligibility `cooling`). These are local eligibility
projections, never upstream health; `unknown` authorization shows no badge
and is not verified.

A draft stays on the rail with **Continue setup** and is not routed; it does
not open **Add Key** merely because a credential is missing. **Continue setup**
reloads that Provider's current definition and pairs save with that view's
revision; a none-auth draft is not treated as having a saved Key. A configured
keyed connection with no Key stays on the rail as **Missing credential**: it
is saved, has no Key, and does not participate in routing; it is not tested
automatically. **Add Key** opens the same credential editor used on
**Accounts**, prefilled for that connection. Endpoint, authentication,
protocol, and model mappings are connection-owned and edited on **Providers**
for every configurable HTTP connection; Accounts owns each Key, its scope,
enablement, quota relation, and order. Normal selection writes
`destination=<id>`; an unmatched draft writes `connection=<id>`. Older
`connection=<id>`, `provider=<id>`, and `scope_kind`/`scope_id` links still
resolve at entry.

**Add Provider** in the rail footer opens the same **Accounts → Add account**
chooser (`add=custom`, opening New services with Custom API selected; an
exact preset bookmark uses `preset:<id>`; a manual user-defined Provider
bookmark uses `manual` or `preset:manual`), keeping available templates off the
configured-Providers rail. That form can **Save draft** once name and URL are
valid (Key and models may be omitted) or **Complete setup** (models required,
and a Key for keyed auth). Fetch models and Test model stay explicit. Saving
or completing a user-defined Provider from **Providers → Add Provider** or
**Accounts → Add account** → preset commits once through onboarding. A draft
saved from Accounts continues on Providers.

Reopening a draft uses the same connection ID, keeps a blank Key field to
retain saved material, and rotates only when a new Key is provided. Completing
with a saved Key shows the current destination Origin/URL and an unchecked
authorize-current-address control; opening or editing never grants. Editing an
address or model override lists affected Keys and adds grants only for Keys
the operator explicitly selects. If the network drops before a response, the
form reports an unknown outcome, locks the fields, and retries the same
payload and operation on explicit Retry. A conflicting revision reloads
tokens for review and keeps the input without replaying. Configured Providers
use ordinary edit, never turning a live Provider into a draft. Saved
connections retain their preset brand where provenance is known, without
certifying an edited address as official. The model catalog has its own model
search and enabled-state filter; searching the Provider list does not search
models. Mapping tables keep both public and upstream names accessible on
narrow screens.

Enabling a model force-enables every available protocol; it does not merely restore `auto`. Available upstreams always use the same chips: a visible chip can connect, and blue is the conversion default. Clicking a chip sets that default. The preference is remembered independently of enablement and travels in node migration packages. Model and connection tests never enable a model or change its protocol choice.

For configurable HTTP connections, **Providers** edits one legacy route or one to three explicit protocol routes as a set. Each explicit route stores its complete endpoint and authentication. Existing single-route connections keep their saved legacy address and authentication. Editing routes or model overrides lists affected Keys and adds access only for credential IDs the operator explicitly authorizes; catalog refresh never grants access.

While a form is fetching models, testing a model, or loading its saved configuration, you can close it or switch to another choice. Results from the abandoned form cannot overwrite the next one. Closing stops the browser waiting; an upstream test may already have run. An actual save retains its submission guard until the service answers.

## Protocol Defaults And Connection Tests

An empty draft can be removed with **Delete Provider**. If it already has accounts, remove those accounts first; deleting a Provider does not delete accounts for you.

Each user-defined Provider has a legacy default route or one to three explicit protocol routes. A model inherits the route for its protocol unless its mapping has a single explicit upstream override. Clearing the override restores inheritance. Authentication remains route-owned. Each keyed route requires the selected Key's saved endpoint and Origin grants. Editing a route or model override does not grant access to the new destination. Different public aliases for one upstream model must resolve to the same route.

Each attempt uses the saved preferred protocol when it is enabled, configured, and granted, then the same client protocol when that protocol is also available, then the remaining granted protocols in saved order. Preservation is checked locally before the send. The gateway does not probe another protocol or switch protocol after an upstream HTTP 400. Credential and provider retries keep their existing policy. CPA uses that same saved order across Chat, Responses, and Messages. Gemini remains a client format.

Official presets initialize or explicitly update their documented route set. Manual configuration declares only the routes the operator saves; there is no automatic protocol scan, and template updates never rewrite saved choices. New API and Sub2API remain independently managed site types; their site and management-credential boundary is unchanged.

**Test model** sends a bounded request through the selected model's exact saved route and an already authorized ready Key. It does not guess alternate URLs, add grants, enable protocols, or change preference. The receipt is tied to that scope, Key, and connection configuration. Model discovery is separate and does not certify inference support.

Presets retain only documented routes. MiniMax CN/API and Global save Chat Completions and Responses with Bearer authentication plus Messages with `x-api-key`; MiMo API and Token Plan save all three protocols with Bearer authentication. Kimi remains Chat Completions plus Messages. Built-in model profiles and all manually saved disabled states remain in effect.

Preset creation shows searchable [channel presets](provider-presets.md) in one list without Plan/API sections. Custom API comes first; the remaining choices follow name order. Vendor variants remain selectable in the detail pane. These templates stay separate from the default list of configured Destinations. Fixed presets show the exact connection summary and seed an editable chat model. Azure and Bedrock still require their resource/regional address and deployment/model information. **Save draft** from **Providers → Add Provider** or **Accounts → Add account** (the same chooser) keeps the connection as **Draft** (not routed). **Complete setup** requires models and, for keyed auth, a Key — or retains a saved Key on resume. Both entry points use the same onboarding commit. Saved configurations are never rewritten by template changes.

Want to connect another upstream or contribute a built-in integration? Start with [Add a Provider](add-provider.md), which includes user-defined Providers, Custom API, and the sealed Adapter Registry path.

**Providers** is the supplier control plane. An old bookmark that still ends
in `?view=pricing` opens this page. A leftover provider detail tab `pricing`
opens **Models**.

The Adapter Registry stays static and sealed. Sealed adapters and
user-defined Providers share this page, labelled **Provider preset** or
**Custom**. Custom API is a Configurable HTTP adapter used as an
account-owned path. Scopes are split like this:

- `Provider(contract_scope_id)` for one exact sealed Provider contract;
  the scope ID is the Provider's own ID.
- User-defined Providers persist as typed definitions and bind Configurable
  HTTP. Their Endpoint, protocol, auth kind, and mappings are edited here.
- Legacy `CustomEndpoint(account_id)` scopes remain as compatibility evidence,
  while their endpoint and mappings are edited on the owning connection.

Provider-preset and user-defined Providers open the same detail shell. **Models**
is the default: provider-preset scopes show the model catalog (source line, refresh,
and the protocol matrix), while user-defined
Providers show their read-only model mappings with an edit entry. There is no price-table tab, no price refresh, and no multiplier editor. Official balances stay on the account card. **Settings** appears only for edit/delete actions, account configuration, or managed signup; fixed adapters with no settings omit it. The **OpenCode Go** scope keeps the
managed-signup **invite URL** here. It is a user-owned `opencode.ai` /
`console.opencode.ai` HTTPS link (not a sealed origin). Fresh installs may
ship a demo default; replace it with your own link before a real signup.
Creating a managed draft can also edit and write this value back. Configurable
HTTP connection settings are edited here; **Accounts** edits only the attached
credentials and their routing bindings. A user-defined Provider does not estimate a request price. A manual credit balance, when you keep one on the account, is not reduced by a completed request.

**Aliases** is a separate core page because its table spans every
currently enabled account instead of the selected Provider. It lists only
Providers that have at least one enabled account, including CPA as its own
Provider. Disabled model mappings are hidden; a public name disappears from
this page when none of its mappings is enabled. Public names and exact upstream identities come from those
Providers' contracts, user-defined mappings, Custom capabilities, and the
selected CPA catalog. Overlapping public names and upstream IDs are flagged
for inspection. Search by public name, upstream ID, or Provider.
The switch to the left of each public name controls downstream listing:
on (the default) advertises that name on authenticated `GET /v1/models`;
off omits it from that list. Hidden names stay on this page and remain
routable if a client already knows them. Provider catalog enablement still
gates routing.

Publication changes show progress per public name. A failed change restores only that name; other pending or saved changes retain their own state.

**Model catalog** is local. Each scope renders one row per current catalog model with columns: model (alias plus raw upstream ID), upstream protocol, enable, and row actions. Every model uses the same chips for its available upstreams; a visible chip can connect, and blue is the conversion default. MiniMax CN/API and Global expose Chat Completions, Responses, and Messages; Kimi Code CN exposes Chat Completions and Messages. The enable switch turns the model on or off for routing: on enables every declared available protocol, off removes the model from routing and from `GET /v1/models`. The switch updates immediately while the CAS-protected save runs in the background; only the affected row shows saving progress. The checkbox column is always visible: checking any row turns the toolbar trailing slot into a bulk bar with the selected count plus **On**, **Off**, and **Delete** for the checked rows; the ✕ button or Esc clears the selection. The header checkbox selects the currently shown rows, and when more models match the filters than are rendered, a banner above the list offers selecting every matching row. Bulk actions clear the selection once applied. Each row can also delete that model from the persisted local catalog. A deleted ID stops routing; **Refresh model catalog** may add official IDs back, on by default.

Deleting the last Zen Free model leaves an empty catalog across reloads and restarts. Only an explicit catalog refresh can bring official IDs back. If a deletion is saved but the subsequent runtime reload fails, removed IDs still stop accepting new requests; the operation reports the reload error.

Underlying static, preset, and probe evidence remains in the contract, but is not surfaced as a separate badge in the per-model list. A protocol keeps its ordinary inherited state until you change its switch. Connection tests record observations only. A Key rotate or Endpoint/protocol change on a Custom or user-defined connection drops that account's probe observations so the old result cannot speak for the new Key or URL. Failed account attempts are reported and retained as evidence, but never turn off a shared protocol; only an explicit switch can do that.

Every refreshable scope takes its model list from that Provider's official `/models` catalog when you **Refresh model catalog**. Protocols come from official documentation or, for configurable HTTP connections, from the saved routes. **OpenCode Go** reads public `https://opencode.ai/zen/go/v1/models` without a Key and uses the per-model endpoint table at `https://opencode.ai/docs/go/`; `mimo-v2.6-flash` is Chat Completions only. **Command Code GOAT** reads its public `https://api.commandcode.ai/provider/v1/models` directory. Its per-model `supported_endpoints` and the official documentation are authoritative; `xiaomi/mimo-v2.6-flash` currently has Chat Completions and Responses evidence only. Do not infer an omitted capability.

The compact source line, refresh action, and model list share one content panel. A catalog refresh is a control-plane action. It preserves existing switches and probe observations, never expands grants, and uses an already authorized ready Key only when the directory requires one. MiniMax CN sealed inference/catalog routes use `https://api.minimax.cn/v1` plus the documented `/anthropic` path; its older usage endpoint is unchanged. Kimi refreshes `https://api.kimi.com/coding/v1/models` with a ready Key. Kimi's rolling product IDs `kimi-for-coding` and `kimi-for-coding-highspeed` are published unchanged; OCG does not relabel them as fixed model versions. Their saved rows activate only code-owned sealed mappings; unmatched rows remain exact raw model IDs.

Before the first successful refresh the catalog is empty. After success, the saved official snapshot is authoritative. Newly discovered models appear enabled with their official known or configured protocols, except that GOAT's first snapshot enables only models included in its plan. Other GOAT models in that first snapshot stay off until you turn them on; models first discovered in later refreshes use the normal enabled default. An existing model remains off when its saved switch is off; a model with no protocol evidence waits for official documentation and can be enabled when refresh adds that declaration. Existing preferences, overrides, and probe results for surviving models are preserved. A failed or empty refresh keeps the previous snapshot.

Migrated Custom API connections retain public-name → upstream-ID mappings and
`public_only` lookup; discovery never silently replaces them. Ordinary new
configurable HTTP connections may also accept a unique exact upstream ID. Command Code uses its public official
`/models` directory: the initial GOAT cohort follows the plan's included-model
list, while genuinely new models discovered by later refreshes enable once
their supported protocol is documented. Saved switches remain in effect.

Local catalogs feed resolution without another request-time upstream call.
Built-in Alias authority is static and code-owned: the original OpenCode Go
table supplies Go names, while sealed MiniMax CN, Kimi CN, and selected GOAT
long-name maps supply provider aliases without creating Go routes. Command
removes the Provider namespace and reuses an existing code-owned Alias; known
plan suffixes are removed only when the shorter name is already authorized.
For example, `nvidia/nemotron-3-ultra-550b-a55b` uses Alias
`nemotron-3-ultra`. Saved CN rows activate only their exact sealed map.
Command ids that contain `/` publish a unique last-segment lowercase kebab Alias
(for example `google/gemini-3.5-flash` → `gemini-3.5-flash`). Slash-free unmatched
Command rows and unmatched MiniMax/Kimi rows remain exact raw model IDs and are
not advertised as new Aliases; CN mappings keep the upstream ID's exact spelling. A Zen Free row
publishes its suffix-stripped Alias from the official `-free` suffix;
the original `-free` ID remains an exact raw pin,
as described under
[Zen Free models](routing.md#zen-free-models).

If every model's enable switch is off, that Provider contributes no route. Authenticated downstream `GET /v1/models` publishes only qualified public names that carry a validated derived protocol profile from the same snapshot used to enrich the row. It omits raw-only identities and raw-name conflicts; an ambiguous raw identity fails as `ambiguous_model_id` without an upstream request.

Each supported model row has a **Test** action. It probes the exact selected saved route with one already authorized ready Key; it does not fall back to another route or account. Models must belong to the current provider catalog, including newly fetched models not yet in a static table. A confirmation warns that the minimal real request may consume quota. The receipt shows success, failure, or skipped state together with its scope, Key, configuration, protocol, and safe upstream detail when supplied. A probe never changes enablement, preference, grants, or route configuration.

There is no price table on this page. The gateway does not fetch a provider price list, apply a multiplier, or estimate a price for a new request. A cost that was not recorded stays unknown and is not shown as zero or free. Older stored prices stay on disk and are not recalculated.

- OpenCode Go account cards replace the 5-hour, weekly, and monthly windows with the official percentage and reset. A missing percentage stays unavailable and is not shown as 0. You can save a manual percentage on those three windows. A blank or null percentage is not stored as 0. Later requests do not add a price onto that percentage.
- Command Code GOAT account cards can explicitly **Refresh quota** to read official percentage windows from Command Code's first-party `/alpha/billing/credits` endpoint. That action also refreshes the GOAT model catalog. The official CLI uses this endpoint, although the public Provider API does not document it. When the reading includes a percentage, the window uses it against a full window of 100 and keeps its reset. A dollar amount is not relabeled as a percentage. You can save a manual percentage afterwards. With no official reading and no manual percentage, the window stays unavailable and is not shown as 0. There is no automatic GOAT usage sync.
- Zen Free uses an egress-IP-shared free quota. Successful requests keep token counts and are not given a local price.
- Custom API keeps a missing cost unknown. It is not shown as zero or free, and it does not debit a timed quota window. A manual credit balance is recorded separately and is not reduced by the request. There is no generic official usage window. Known-host current-balance reads (DeepSeek / Moonshot / StepFun API) are display-only.
- Ollama Cloud refreshes the public keyless directory `https://ollama.com/v1/models` without selecting an account. Discovered ids enable Chat Completions immediately; Responses and Messages are unsupported, and there is no protocol-probe entry. A refreshed catalog may append one routeable Ollama mapping to a Go-owned alias only when stripping the `:` tag leaves exactly one catalog match. Date-tagged snapshot ids come from the runtime catalog. Ollama has no official usage API in this product. The gateway does not fetch a price list or estimate a monthly credit meter from request prices. The account form still presents Pro, Max, or Team and a purchase date. A manual percentage does not require a price. A month percentage can be saved before a tier is chosen. A week percentage is not accepted. Without an official or manual usage observation, usage stays unavailable. Existing accounts stay routeable. Previously stored billing rows stay on disk and are not recalculated.
- MiniMax CN and Kimi Code CN do not price requests in OCG, but their account cards can manually read the official subscription windows (`/token_plan/remains` and `/usages`). These snapshots are display-only and do not gate inference.
- Custom API and user-defined Provider cards whose stored Endpoint host is exactly `api.deepseek.com`, `api.moonshot.cn`, or `api.moonshot.ai` can manually read that official current balance. Other Custom hosts are not probed.

Request-time flow, for each attempt: Alias → account eligibility → adapter
ceiling → saved contract → per-model/per-protocol effective state → one local
protocol choice before send. That choice is the saved preferred protocol,
then the client protocol, then the remaining granted protocols. The choice
does not replace credential or provider retry, and an HTTP 400 does not
switch protocol. Authenticated `GET /v1/models` publishes currently qualified
public names that carry a validated derived protocol profile: the destination
and model are enabled, the name resolves to that mapping, and a credential is
enabled, ready, and binding-enabled, allows the model, has a Key when the
route requires one, and holds the endpoint grant. Catalog enablement alone is
not enough. Cooldown, a probe flag, and auth-error history do not omit a
usable profile. Protected `GET /dashboard/api/v4/application-models` lists Go
names that resolve in the saved catalog and have an enabled protocol. It does
not consult a price snapshot and excludes Custom.

---

[User guide index](../USER.md) · [简体中文](providers.zh-CN.md) · [Docs index](../README.md)
