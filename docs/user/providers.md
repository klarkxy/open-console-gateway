[简体中文](providers.zh-CN.md)

# Providers

The rail lists the V4 connection projection: built-in Providers with at least one account, every user-defined Provider (with or without a Key, including persisted onboarding drafts), and each Custom API account as its own row, grouped by Plan/API. Unused built-in templates stay off the rail and remain in the **Add Provider** catalog only. Row and detail-header status comes from server-side projection fields only: **Draft** (lifecycle `draft`), **Missing credential** (authorization `missing` on a configured connection), **Disabled** (lifecycle `disabled` or all credentials disabled), **Invalid credential** (authorization `invalid`), **No enabled model** (eligibility reason `no_enabled_target`), **Cooling down** (eligibility `cooling`). These are local eligibility projections, never upstream health; `unknown` authorization shows no badge and is not verified. A draft stays on the rail with **Continue setup** and is not routed; it does not open **Add Key** merely because a credential is missing. **Continue setup** reloads that Provider's current definition and pairs save with that view's revision; a none-auth draft is not treated as having a saved Key. A configured keyed Provider with no Key stays on the rail as **Missing credential**: it is saved, has no Key, and does not participate in routing; it is not tested automatically. **Add Key** on a configured detail opens the same credential editor used on **Accounts**, prefilled for that Provider. A Custom API account row is a read-only summary (endpoint, protocol, mapped models); **Edit on Accounts** opens that account's editor on **Accounts**. Custom API configuration remains account-owned. The page selects by `connection=<id>`; older `provider=<id>` bookmarks still resolve to the matching built-in or user-defined connection. **Add Provider** in the rail footer opens the preset browser in the main pane, keeping available templates separate from configured Providers; existing preset bookmarks open the same embedded creation form. That form can **Save draft** once name and URL are valid (Key and models may be omitted) or **Complete setup** (models required, and a Key for keyed auth). Fetch models and Test model stay explicit. Saving or completing a user-defined Provider from **Providers → Add Provider** or **Accounts → Add account** → preset commits once through onboarding. A draft saved from Accounts continues on Providers. Reopening a draft uses the same connection ID, keeps a blank Key field to retain saved material, and rotates only when a new Key is provided. Completing with a saved Key shows the current destination Origin/URL and an unchecked authorize-current-address control; opening or editing never grants. If the network drops before a response, the form reports an unknown outcome, locks the fields, and retries the same payload and operation on explicit Retry. A conflicting revision reloads tokens for review and keeps the input without replaying. Configured Providers still use ordinary edit, never turning a live Provider into a draft. Saved connections retain their preset brand where provenance is known, without certifying an edited address as official. The model catalog has its own model search and enabled-state filter; searching the Provider list does not search models. Mapping tables keep both public and upstream names accessible on narrow screens.

Enabling a model force-enables every available protocol; it does not merely restore `auto`. Available upstreams always use the same chips: a visible chip can connect, and blue is the conversion default. Clicking a chip sets that default. The preference is remembered independently of enablement and travels in node migration packages. Restoring the official baseline clears it. Model and connection tests never enable a model or change its protocol choice.

## Protocol defaults and connection tests

An empty draft can be removed with **Delete Provider**. If it already has accounts, remove those accounts first; deleting a Provider does not delete accounts for you.

Each user-defined Provider supplies the default endpoint, protocol and authentication. A model inherits them unless its mapping has an explicit protocol and endpoint override. Clearing the override restores inheritance. Authentication remains Provider-owned. Each keyed route requires the selected Key's saved endpoint and Origin grants. Editing the Provider URL or a model override does not grant access to the new destination. Different public aliases for one upstream model must resolve to the same route.

Routing passes the client protocol through when that protocol is enabled. Otherwise the gateway converts to the model's preferred protocol, then to the first remaining enabled protocol in adapter fallback order. CPA is unchanged: it preserves supported Chat, Responses and Messages client formats; Gemini clients are converted to Chat.

Official presets initialize new Providers from documented defaults. Manual configuration starts with unverified Chat. There is no automatic protocol scan, and template updates never rewrite saved choices. New API and Sub2API are site types with independent instances; their linked Custom accounts retain account-owned configuration.

**Test model** sends a minimal request using the selected model's effective configuration. It does not guess alternate URLs, enable protocols or change preference. Draft tests need an explicit temporary Key; account tests use that account's saved Key. Model discovery is separate and does not certify inference support.

Defaults reviewed on **2026-09-09**: xAI uses [Responses](https://docs.x.ai/developers/model-capabilities/text/comparison); MiniMax uses [Messages with Bearer auth](https://platform.minimax.io/docs/api-reference/text-chat-anthropic). Other presets retain documented compatible defaults. Built-in model profiles and manual disabled states remain in effect.

Preset creation groups searchable [channel presets](provider-presets.md) under Plan and API, then vendor and regional variant. These templates stay separate from the default list of configured connections. Fixed presets show the exact connection summary and seed an editable chat model. Azure and Bedrock still require their resource/regional address and deployment/model information. **Save draft** from **Providers** or **Accounts → Add account** keeps the connection as **Draft** (not routed). **Complete setup** requires models and, for keyed auth, a Key — or retains a saved Key on resume. Both entry points use the same onboarding commit. Saved configurations are never rewritten by template changes.

Want to connect another upstream or contribute a built-in integration? Start with [Add a Provider](add-provider.md), which includes user-defined Providers, Custom API, and the sealed Adapter Registry path.

**Providers** is the supplier control plane — the page you land on when an old
bookmark still ends in `?view=pricing`.

The Adapter Registry stays static and sealed. Built-in Providers and
user-defined Providers share this page, labelled **Built-in**, **Official
preset**, or **Custom** by origin. Custom API is a Configurable HTTP adapter
used as an account-owned path. Scopes are split like this:

- `Provider(contract_scope_id)` for one exact built-in Provider contract.
  Existing scope IDs keep their historical Provider-shaped values.
- User-defined Providers persist as typed definitions and bind Configurable
  HTTP. Their Endpoint, protocol, auth kind, and mappings are edited here.
- `CustomEndpoint(account_id)` scopes keep Custom mappings account-owned.
  Edit those mappings on **Accounts**.

Built-in and user-defined Providers open the same detail shell with up to three tabs. **Models**
is the default: built-in scopes show the model catalog (source line, refresh,
official protocol baseline, and the protocol matrix), while user-defined
Providers show their read-only model mappings with an edit entry. **Pricing**
appears only when the Provider has pricing. **Settings** shows the connection
facts; built-in rows are read-only (provided by the official adapter),
user-defined rows offer edit/delete, and the **OpenCode Go** scope keeps the
managed-signup **invite URL** here. It is a user-owned `opencode.ai` /
`console.opencode.ai` HTTPS link (not a sealed origin). Fresh installs may
ship a demo default; replace it with your own link before a real signup.
Creating a managed draft can also edit and write this value back. A Custom
API account row is a read-only summary (endpoint, protocol, mapped models);
**Edit on Accounts** opens the account editor on **Accounts**. User-defined
Providers are unpriced.

**Aliases** is a separate core page because its read-only table spans every
Provider contract, user-defined Provider mapping, and Custom account instead of
the selected Provider. It aggregates existing contracts and account capabilities
into public names with their configuration state, enabled-account counts, and
exact upstream identities. These are configuration facts, not a guarantee that
a request will succeed. Overlapping public names and upstream IDs are flagged
for inspection. Search by public name, upstream ID, or Provider; **Edit mappings**
opens the relevant Custom account editor on **Accounts**.

**Model catalog** is local. Each scope renders one row per current catalog model with columns: model (alias plus raw upstream ID), upstream protocol, enable, and a row action. Every model uses the same chips for its available upstreams; a visible chip can connect, and blue is the conversion default. MiniMax CN and Kimi Code CN start with Chat Completions and Messages; neither advertises Responses. The enable switch turns the model on or off for routing: on force-enables every available protocol, off removes the model from routing and from `GET /v1/models`. The switch updates immediately while the CAS-protected save runs in the background; only the affected row shows saving progress. Batch actions move out of per-column controls into a single scope-level toolbar with **Enable all** and **Disable all** that apply to every model in the scope.

Underlying static, preset, and probe evidence remains in the contract, but is not surfaced as a separate badge in the per-model list. `auto` remains the stored default until an explicit switch writes an override. Connection tests record observations only. A Key rotate or Endpoint/protocol change on a Custom or user-defined connection drops that account's probe observations so the old result cannot speak for the new Key or URL. Failed account attempts are reported and retained as evidence, but never pin the shared protocol `force_off`; only an explicit switch can do that.

For the built-in **OpenCode Go**, **Zen Free**, **Command Code GOAT**,
**MiniMax CN**, and **Kimi Code CN** scopes, the catalog header offers
**Restore official protocol baseline**. It makes no upstream request, keeps the
current model catalog, clears manual switches and probe evidence, and restores
the development-time official baseline reviewed on **2026-09-06**. OpenCode
Go and known Zen rows default to the one upstream endpoint documented for each
model. GOAT uses Messages for Anthropic model IDs and Chat Completions for the
other Provider families, with newly discovered non-preset models still off by
default. MiniMax CN and Kimi Code CN both default to Chat Completions and
Messages; neither advertises Responses. The current effective target protocol
stays in effect until an explicit row change rewrites it.

The compact source line, refresh action, and model list share one content panel.
Every refreshable scope uses the same action. OpenCode Go refreshes from the
official authenticated model endpoint with a backend-selected eligible Go
account, Zen Free uses the fixed keyless directory
`https://opencode.ai/zen/v1/models`, and Command Code uses its fixed public
official `/models` directory without selecting an account. Refresh is always
explicit.

MiniMax and Kimi require an eligible account Key. MiniMax refreshes
`https://api.minimaxi.com/v1/models`; Kimi refreshes
`https://api.kimi.com/coding/v1/models`. Their saved rows activate only
code-owned sealed mappings; unmatched rows remain exact raw model IDs. MiniMax
maps M3, M2.7/M2.5/M2.1 standard and highspeed variants, and M2 to matching
lowercase kebab Aliases. Kimi maps `kimi-for-coding` → `kimi-k2.7-code`,
`kimi-for-coding-highspeed` → `kimi-k2.7-code-highspeed`, `k3` → `kimi-k3`,
and `k3-256k` → `kimi-k3-256k`. Forwarding retains every exact upstream ID.

Before the first successful refresh, the built-in static catalog is the initial
preset. After success, the saved official snapshot is authoritative and
replaces that preset. Models newly added by a refresh appear in the list. For
OpenCode Go, Zen Free, and Command Code, the new rows default to off (the
upstream protocol is the documented default and the enable switch is off) until
you explicitly turn one on. A refreshed model the checked-in preset does not
know still falls back to the provider's official default protocol — Chat
Completions for OpenCode Go and Zen Free — so the row stays operable instead of
showing "No protocol available". MiniMax CN and Kimi Code CN new rows default to
their sealed target protocol; Responses stays unsupported. Existing overrides
and probe results for surviving models are preserved. A failed or empty refresh
keeps the previous snapshot.

Custom API continues to use account-owned public-name → upstream-ID mappings;
discovery never silently replaces them. The account form **Fetch models** action
is an unsaved-form helper that returns upstream IDs only. Selecting one imports
an exact `public name = upstream ID` row. Command Code uses its public official
`/models` directory: the GOAT preset starts enabled, while additional models
discovered later start disabled until you enable their supported protocol in
the list.

Local catalogs feed resolution without another request-time upstream call.
Built-in Alias authority is static and code-owned: the original OpenCode Go
table supplies Go names, while sealed MiniMax CN, Kimi CN, and selected GOAT
long-name maps supply provider aliases without creating Go routes. Command
removes the Provider namespace and reuses an existing code-owned Alias; known
plan suffixes are removed only when the shorter name is already authorized.
For example, `nvidia/nemotron-3-ultra-550b-a55b` uses Alias
`nemotron-3-ultra`. Saved CN rows activate only their exact sealed map.
Unmatched Command/MiniMax/Kimi rows remain exact raw model IDs and are not advertised as
new Aliases; CN mappings keep the upstream ID's exact spelling. A Zen Free row
publishes its suffix-stripped Alias from the official `-free` suffix;
the original `-free` ID remains an exact raw pin,
as described under
[Zen Free models](routing.md#zen-free-models).

If every model's enable switch is off, that Provider contributes no route. Authenticated downstream `GET /v1/models` publishes only routeable public names. It omits raw-only identities and raw-name conflicts; an ambiguous raw identity fails as `ambiguous_model_id` without an upstream request.

Built-in Provider rows whose adapter exposes connection testing have a **Test** button. The dashboard tests the effective configured protocol.
For the configured protocol the provider automatically tries its eligible
accounts in saved routing order and stops at the first success. OpenCode Go and
Zen Free use their constructable protocol set. GOAT tests only its sealed native
family path: Messages for Anthropic IDs and Chat Completions otherwise. MiniMax
CN and Kimi Code CN test their sealed Chat Completions and Messages paths.
Custom endpoint tests remain account-owned. Models must belong to the current provider
catalog, including newly fetched models not yet in the static table. A
Popconfirm warns that these real minimal requests may consume quota. Each protocol
result is shown above the list with its success, failure, or skipped state,
HTTP status, readable upstream message, and a safe upstream help/billing link
when one is supplied. Every actual account attempt is recorded as a
redacted request log; probe traffic never enters Runtime Logs. One account
failure never disables a protocol that another eligible account can serve.

**Pricing** is scoped to the selected provider. **Refresh price table** only
hits the official source owned by that Provider. OpenCode and Command Code
keep separate revisions and last-good snapshots; one failing does not touch
the other. If a Provider later owns several priced Plans, the same action
refreshes those Plans only. Refresh stays manual:

- OpenCode Go shows revision, documentation timestamp, token rates, `Usage`,
  and the quota-debit multiplier, and can fetch
  `https://opencode.ai/docs/go/` after you press refresh. A failed fetch or
  validation keeps the last successful snapshot. The allowance is not a quota
  pool and does not route requests: it only derives that debit multiplier
  (`monthly limit / Usage`). Saving a temporary override creates a new
  persistent revision for later estimates.
- Command Code GOAT shows its saved official rate snapshot from
  `https://commandcode.ai/docs/plans/goat`. Models with scheduled pricing retain
  the official daily peak windows (01:00–04:00 and 06:00–10:00 UTC) and their
  separate input, output, and cache-read rates. Each priced model's applied
  multiplier can be edited and saved. The saved provider revision prices later
  requests; missing or ambiguous rows stay unpriced. A refresh asks before
  replacing edited multipliers. This remains separate from OpenCode Go. GOAT
  account cards use those priced OCG request logs for a local `$14 / $35 / $70`
  window estimate with manual baseline correction. Command Code exposes no
  machine-readable usage API.
- Zen Free is unpriced (egress-IP-shared free quota).
- Custom API is unpriced: successful forwards log `cost_state=unknown` with
  no quota debit and no official usage refresh.
- Ollama Cloud refreshes the public keyless directory `https://ollama.com/v1/models` without selecting an account. Discovered ids enable Chat Completions immediately; Responses and Messages are unsupported, and there is no protocol-probe entry. A refreshed catalog may append one routeable Ollama mapping to a Go-owned alias only when stripping the `:` tag leaves exactly one catalog match. Date-tagged snapshot ids come from the runtime catalog. Manual pricing refresh reads `https://ollama.com/pricing` (Model / Input / Cached input / Output) into the provider snapshot with quota multiplier `1.0`. New accounts require Pro/Max/Team plus a purchase date. Account cards estimate one monthly USD-Credits window from official per-request usage against that tier; used credit may exceed the soft limit, the bar clamps at 100%, and the meter never writes cooldown or changes routing. Migrated accounts with no billing row stay routeable without a meter.
- MiniMax CN and Kimi Code CN are unpriced in OCG, but their account cards can
  manually read the official subscription windows (`/token_plan/remains` and
  `/usages`). These snapshots are display-only and do not gate inference.

Request-time flow: Alias → account eligibility → adapter ceiling → saved
contract → per-model/per-protocol effective state → passthrough or conversion.
Protocol selection uses the saved contract. Authenticated `GET /v1/models` and
protected `GET /dashboard/api/v3/application-models` publish only currently
routable public names that have an effective enabled protocol. `application-models` stays Go aliases ∩ active pricing and excludes Custom.

---

[User guide index](../USER.md) · [简体中文](providers.zh-CN.md) · [Docs index](../README.md)
