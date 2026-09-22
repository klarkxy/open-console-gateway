[简体中文](providers.zh-CN.md)

# Providers

The rail lists the V4 connection projection: built-in Providers with at least one account and every configurable HTTP connection (with or without a Key, including persisted onboarding drafts), in one searchable list. The list defaults to name A–Z; use the sort selector to switch to Z–A. Existing Custom API records remain separate connections; equal names or URLs are never merged. Unused built-in templates stay off the rail and remain in the **Add Provider** catalog only. Row and detail-header status comes from server-side projection fields only: **Draft** (lifecycle `draft`), **Missing credential** (authorization `missing` on a configured connection), **Disabled** (lifecycle `disabled` or all credentials disabled), **Invalid credential** (authorization `invalid`), **No enabled model** (eligibility reason `no_enabled_target`), **Cooling down** (eligibility `cooling`). These are local eligibility projections, never upstream health; `unknown` authorization shows no badge and is not verified. A draft stays on the rail with **Continue setup** and is not routed; it does not open **Add Key** merely because a credential is missing. **Continue setup** reloads that Provider's current definition and pairs save with that view's revision; a none-auth draft is not treated as having a saved Key. A configured keyed connection with no Key stays on the rail as **Missing credential**: it is saved, has no Key, and does not participate in routing; it is not tested automatically. **Add Key** opens the same credential editor used on **Accounts**, prefilled for that connection. Endpoint, authentication, protocol, and model mappings are connection-owned and edited on **Providers** for every configurable HTTP connection; Accounts owns each Key, its scope, enablement, quota relation, and order. The page selects by `connection=<id>`; older `provider=<id>` bookmarks still resolve to the matching built-in or user-defined connection. **Add Provider** in the rail footer opens the same **Accounts → Add account** chooser (`add=1`, or `preset:<id>` for a preset bookmark), keeping available templates off the configured-Providers rail. That form can **Save draft** once name and URL are valid (Key and models may be omitted) or **Complete setup** (models required, and a Key for keyed auth). Fetch models and Test model stay explicit. Saving or completing a user-defined Provider from **Providers → Add Provider** or **Accounts → Add account** → preset commits once through onboarding. A draft saved from Accounts continues on Providers. Reopening a draft uses the same connection ID, keeps a blank Key field to retain saved material, and rotates only when a new Key is provided. Completing with a saved Key shows the current destination Origin/URL and an unchecked authorize-current-address control; opening or editing never grants. Editing an address or model override lists affected Keys and adds grants only for Keys the operator explicitly selects. If the network drops before a response, the form reports an unknown outcome, locks the fields, and retries the same payload and operation on explicit Retry. A conflicting revision reloads tokens for review and keeps the input without replaying. Configured Providers use ordinary edit, never turning a live Provider into a draft. Saved connections retain their preset brand where provenance is known, without certifying an edited address as official. The model catalog has its own model search and enabled-state filter; searching the Provider list does not search models. Mapping tables keep both public and upstream names accessible on narrow screens.

Enabling a model force-enables every available protocol; it does not merely restore `auto`. Available upstreams always use the same chips: a visible chip can connect, and blue is the conversion default. Clicking a chip sets that default. The preference is remembered independently of enablement and travels in node migration packages. Model and connection tests never enable a model or change its protocol choice.

For configurable HTTP connections, **Providers** edits one legacy route or one to three explicit protocol routes as a set. Each explicit route stores its complete endpoint and authentication. Existing single-route connections keep their saved legacy address and authentication. Editing routes or model overrides lists affected Keys and adds access only for credential IDs the operator explicitly authorizes; catalog refresh never grants access.

## Protocol defaults and connection tests

An empty draft can be removed with **Delete Provider**. If it already has accounts, remove those accounts first; deleting a Provider does not delete accounts for you.

Each user-defined Provider has a legacy default route or one to three explicit protocol routes. A model inherits the route for its protocol unless its mapping has a single explicit upstream override. Clearing the override restores inheritance. Authentication remains route-owned. Each keyed route requires the selected Key's saved endpoint and Origin grants. Editing a route or model override does not grant access to the new destination. Different public aliases for one upstream model must resolve to the same route.

Routing passes the client protocol through when that protocol is enabled. Otherwise the gateway converts to the model's preferred protocol, then to the first remaining enabled protocol in adapter fallback order. CPA preserves its supported Chat, Responses and Messages client formats; Gemini clients are converted to Chat.

Official presets initialize or explicitly update their documented route set. Manual configuration declares only the routes the operator saves; there is no automatic protocol scan, and template updates never rewrite saved choices. New API and Sub2API remain independently managed site types; their site and management-credential boundary is unchanged.

**Test model** sends a bounded request through the selected model's exact saved route and an already authorized ready Key. It does not guess alternate URLs, add grants, enable protocols, or change preference. The receipt is tied to that scope, Key, and connection configuration. Model discovery is separate and does not certify inference support.

Presets retain only documented routes. MiniMax CN/API and Global save Chat Completions and Responses with Bearer authentication plus Messages with `x-api-key`; MiMo API and Token Plan save all three protocols with Bearer authentication. Kimi remains Chat Completions plus Messages. Built-in model profiles and all manually saved disabled states remain in effect.

Preset creation shows searchable [channel presets](provider-presets.md) in one list without Plan/API sections. Custom API comes first; the remaining choices follow name order. Vendor variants remain selectable in the detail pane. These templates stay separate from the default list of configured connections. Fixed presets show the exact connection summary and seed an editable chat model. Azure and Bedrock still require their resource/regional address and deployment/model information. **Save draft** from **Providers → Add Provider** or **Accounts → Add account** (the same chooser) keeps the connection as **Draft** (not routed). **Complete setup** requires models and, for keyed auth, a Key — or retains a saved Key on resume. Both entry points use the same onboarding commit. Saved configurations are never rewritten by template changes.

Want to connect another upstream or contribute a built-in integration? Start with [Add a Provider](add-provider.md), which includes user-defined Providers, Custom API, and the sealed Adapter Registry path.

**Providers** is the supplier control plane — the page you land on when an old
bookmark still ends in `?view=pricing`.

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

Provider-preset and user-defined Providers open the same detail shell with up to three tabs. **Models**
is the default: provider-preset scopes show the model catalog (source line, refresh,
and the protocol matrix), while user-defined
Providers show their read-only model mappings with an edit entry. **Pricing**
always shows the selected Provider's catalog state (`available`, `unpriced`,
`unavailable`, or not applicable) without inventing rows. **Settings** shows the connection
facts; provider-preset rows are read-only (provided by the official adapter),
user-defined rows offer edit/delete, and the **OpenCode Go** scope keeps the
managed-signup **invite URL** here. It is a user-owned `opencode.ai` /
`console.opencode.ai` HTTPS link (not a sealed origin). Fresh installs may
ship a demo default; replace it with your own link before a real signup.
Creating a managed draft can also edit and write this value back. Configurable
HTTP connection settings are edited here; **Accounts** edits only the attached
credentials and their routing bindings. User-defined Providers stay unpriced until an official preset or a per-account credit configuration supplies the rates.

**Aliases** is a separate core page because its table spans every
currently enabled account instead of the selected Provider. It lists only
Providers that have at least one enabled account, including CPA as its own
Provider. Public names and exact upstream identities come from those
Providers' contracts, user-defined mappings, Custom capabilities, and the
selected CPA catalog. Overlapping public names and upstream IDs are flagged
for inspection. Search by public name, upstream ID, or Provider.
The switch to the left of each public name controls downstream listing:
on (the default) advertises that name on authenticated `GET /v1/models`;
off omits it from that list. Hidden names stay on this page and remain
routable if a client already knows them. Provider catalog enablement still
gates routing.

**Model catalog** is local. Each scope renders one row per current catalog model with columns: model (alias plus raw upstream ID), upstream protocol, enable, and row actions. Every model uses the same chips for its available upstreams; a visible chip can connect, and blue is the conversion default. MiniMax CN/API and Global expose Chat Completions, Responses, and Messages; Kimi Code CN exposes Chat Completions and Messages. The enable switch turns the model on or off for routing: on enables every declared available protocol, off removes the model from routing and from `GET /v1/models`. The switch updates immediately while the CAS-protected save runs in the background; only the affected row shows saving progress. **Select** opens multi-select: a checkbox column appears, and the toolbar trailing slot becomes **On**, **Off**, and **Delete** for the checked rows. Each row can also delete that model from the persisted local catalog. A deleted ID stops routing; **Refresh model catalog** may add official IDs back, on by default.

Deleting the last Zen Free model leaves an empty catalog across reloads and restarts. Only an explicit catalog refresh can bring official IDs back. If a deletion is saved but the subsequent runtime reload fails, removed IDs still stop accepting new requests; the operation reports the reload error.

Underlying static, preset, and probe evidence remains in the contract, but is not surfaced as a separate badge in the per-model list. A protocol keeps its ordinary inherited state until you change its switch. Connection tests record observations only. A Key rotate or Endpoint/protocol change on a Custom or user-defined connection drops that account's probe observations so the old result cannot speak for the new Key or URL. Failed account attempts are reported and retained as evidence, but never turn off a shared protocol; only an explicit switch can do that.

Every refreshable scope takes its model list from that Provider's official `/models` catalog when you **Refresh model catalog**. Protocols come from official documentation or, for configurable HTTP connections, from the saved routes. **OpenCode Go** reads public `https://opencode.ai/zen/go/v1/models` without a Key and uses the per-model endpoint table at `https://opencode.ai/docs/go/`; `mimo-v2.6-flash` is Chat Completions only. **Command Code GOAT** reads its public `https://api.commandcode.ai/provider/v1/models` directory. Its per-model `supported_endpoints` and the official documentation are authoritative; `xiaomi/mimo-v2.6-flash` currently has Chat Completions and Responses evidence only. Do not infer an omitted capability.

The compact source line, refresh action, and model list share one content panel. A catalog refresh is a control-plane action. It preserves existing switches and probe observations, never expands grants, and uses an already authorized ready Key only when the directory requires one. MiniMax CN sealed inference/catalog routes use `https://api.minimax.cn/v1` plus the documented `/anthropic` path; its older usage endpoint is unchanged. Kimi refreshes `https://api.kimi.com/coding/v1/models` with a ready Key. Kimi's rolling product IDs `kimi-for-coding` and `kimi-for-coding-highspeed` are published unchanged; OCG does not relabel them as fixed model versions. Their saved rows activate only code-owned sealed mappings; unmatched rows remain exact raw model IDs.

Before the first successful refresh the catalog is empty. After success, the saved official snapshot is authoritative. Newly discovered models appear enabled with their official known or configured protocols. An existing model remains off only when it is confirmed supported but disabled, or when you explicitly turned it off; a model with no protocol evidence waits for official documentation and can be enabled when refresh adds that declaration. Existing preferences, overrides, and probe results for surviving models are preserved. A failed or empty refresh keeps the previous snapshot.

Migrated Custom API connections retain public-name → upstream-ID mappings and
`public_only` lookup; discovery never silently replaces them. Ordinary new
configurable HTTP connections may also accept a unique exact upstream ID. Command Code uses its public official
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
Command ids that contain `/` publish a unique last-segment lowercase kebab Alias
(for example `google/gemini-3.5-flash` → `gemini-3.5-flash`). Slash-free unmatched
Command rows and unmatched MiniMax/Kimi rows remain exact raw model IDs and are
not advertised as new Aliases; CN mappings keep the upstream ID's exact spelling. A Zen Free row
publishes its suffix-stripped Alias from the official `-free` suffix;
the original `-free` ID remains an exact raw pin,
as described under
[Zen Free models](routing.md#zen-free-models).

If every model's enable switch is off, that Provider contributes no route. Authenticated downstream `GET /v1/models` publishes only routeable public names. It omits raw-only identities and raw-name conflicts; an ambiguous raw identity fails as `ambiguous_model_id` without an upstream request.

Each supported model row has a **Test** action. It probes the exact selected saved route with one already authorized ready Key; it does not fall back to another route or account. Models must belong to the current provider catalog, including newly fetched models not yet in a static table. A confirmation warns that the minimal real request may consume quota. The receipt shows success, failure, or skipped state together with its scope, Key, configuration, protocol, and safe upstream detail when supplied. A probe never changes enablement, preference, grants, or route configuration.

**Pricing** is scoped to the selected provider. The default catalog projection includes every `offering=plan` row whose V3 `pricingAvailability` is `available`, in catalog order. Snapshots are requested and cached by `provider_id`; the renderer selects the returned `models` or `values` structure, derives token-range/time-window/adjustment variants, and preserves unknown adjustment labels. Source links come only from the returned snapshot. Refresh requires V3 pricing availability, while multiplier editing additionally requires V4 `pricingMultiplierEditable=true`; if V4 is unavailable, pricing is read-only.

**Refresh price table** only hits the official source owned by that Provider. OpenCode and Command Code
keep separate revisions and last-good snapshots; one failing does not touch
the other. If a Provider later owns several priced Plans, the same action
refreshes those Plans only. Refresh stays manual:

- OpenCode Go shows revision, documentation timestamp, token rates, `Usage`
  (labeled **Monthly limit** in the official docs),
  and the quota-debit multiplier, and can fetch
  `https://opencode.ai/docs/go/` after you press refresh. A failed fetch or
  validation keeps the last successful snapshot. The allowance is not a quota
  pool and does not route requests: it only derives that debit multiplier
  (`account monthly window / model monthly limit`). Saving a temporary
  override creates a new persistent revision for later estimates.
- Command Code GOAT shows its saved official rate snapshot from
  `https://commandcode.ai/docs/plans/goat`. Models with scheduled pricing retain
  the official daily peak windows (01:00–04:00 and 06:00–10:00 UTC) and their
  separate input, output, and cache-read rates. Each priced model's applied
  multiplier can be edited and saved. The saved provider revision prices later
  requests; missing or ambiguous rows stay unpriced. A refresh asks before
  replacing edited multipliers. This remains separate from OpenCode Go. GOAT
  account cards can explicitly **Refresh quota** to read the `$14 / $35 / $70`
  windows from Command Code's first-party `/alpha/billing/credits` account
  endpoint; that action also refreshes the GOAT model catalog. The official CLI uses this endpoint, although the public Provider
  API does not document it. Priced OCG logs continue accumulating between
  snapshots. The official snapshot is the baseline, and the timed windows can
  be calibrated by hand afterwards. There is no automatic GOAT usage sync.
- Zen Free is unpriced (egress-IP-shared free quota).
- Custom API keeps USD cost unknown: successful forwards log `cost_state=unknown` with
  no quota debit. Configured credit meters record native credits separately. There is no generic official usage window. Known-host current-balance
  reads (DeepSeek / Moonshot / StepFun API) are display-only.
- Ollama Cloud refreshes the public keyless directory `https://ollama.com/v1/models` without selecting an account. Discovered ids enable Chat Completions immediately; Responses and Messages are unsupported, and there is no protocol-probe entry. A refreshed catalog may append one routeable Ollama mapping to a Go-owned alias only when stripping the `:` tag leaves exactly one catalog match. Date-tagged snapshot ids come from the runtime catalog. Manual pricing refresh reads `https://ollama.com/pricing` (Model / Input / Cached input / Output) into the provider snapshot with quota multiplier `1.0`. New accounts require Pro/Max/Team plus a purchase date. Account cards estimate one monthly USD-Credits window from official per-request usage against that tier; used credit may exceed the soft limit, the bar clamps at 100%, and the meter never writes cooldown or changes routing. Existing accounts with no billing row stay routeable without a meter.
- MiniMax CN and Kimi Code CN are unpriced in OCG, but their account cards can
  manually read the official subscription windows (`/token_plan/remains` and
  `/usages`). These snapshots are display-only and do not gate inference.
- Custom API and user-defined Provider cards whose stored Endpoint host is
  exactly `api.deepseek.com`, `api.moonshot.cn`, or `api.moonshot.ai` can
  manually read that official current balance. Other Custom hosts are not
  probed.

Request-time flow: Alias → account eligibility → adapter ceiling → saved
contract → per-model/per-protocol effective state → passthrough or conversion.
Protocol selection uses the saved contract. Authenticated `GET /v1/models` and
protected `GET /dashboard/api/v4/application-models` publish only currently
routable public names that have an effective enabled protocol. `application-models` stays Go aliases ∩ active pricing and excludes Custom.

---

[User guide index](../USER.md) · [简体中文](providers.zh-CN.md) · [Docs index](../README.md)
