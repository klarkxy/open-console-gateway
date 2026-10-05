[简体中文](model-metadata.zh-CN.md)

# Model Metadata And Reasoning Tiers In DSH

Use **Applications → DSH** to install the OCG provider. After upgrading OCG to a build containing this feature, install/replace the plugin once and reload the selected DSH runtime. Updating the gateway alone does not replace a previously installed plugin. Existing Key handoff and credential storage are unchanged.

The plugin loads the directory on first use. Reading the model list or resolving a model that is absent from the loaded directory fetches a fresh copy. Calls to known models reuse the loaded snapshot without a timed refresh. After changing published models, protocols, or capabilities in OCG, refresh the model list in DSH to load those changes. Authentication failures and invalid catalogs invalidate the cached directory. Concurrent refreshes share one request, and cancelling one caller's wait leaves other callers unaffected.

## What Is Reported

Authenticated `GET /v1/models` retains its OpenAI-compatible envelope and public IDs. Known capacities are added as `contextWindow` and `maxTokens`. The versioned `ocg` object uses `schemaVersion` 2. It contains the model name, context window, maximum output tokens, input/output modalities, reasoning support, explicit `reasoningEfforts`, tool-call facts, sources, and status. Every published row also contains a validated derived `protocols` object from the same qualified snapshot used to enrich the row. A name without that profile is omitted from this list. Missing capability fields mean unknown, not false. `status` `unknown` with a usable `protocols` profile is still listed. This GET never contacts an upstream.

`protocols` is derived for that response. It is not saved with the metadata record. `preferred` is `chat_completions`, `responses`, or `messages`. `supported` lists authorized upstream protocols in that fixed order. A protocol is listed when an eligible route can send it: the destination and model are enabled, the name resolves to that mapping, and a credential is enabled, ready, and binding-enabled, allows the model, has a Key when the route requires one, and holds the endpoint grant. Several mappings sort by routing rank, then destination id, then upstream model. `preferred` is the first mapping's saved preferred protocol when that protocol is authorized; otherwise it is the first saved protocol that is authorized. Cooldown, a probe flag, an auth error, and which credential last won do not change the object and do not omit a row that already has a usable profile. `supported` names those upstream protocols. It does not promise every feature on every protocol, and it does not choose the caller's client URL. A name whose derived `protocols` value is missing, or whose `preferred` is absent, illegal, or outside `supported`, is omitted from this list. That result is not Chat Completions.

`reasoning` and `reasoningEfforts` stay separate. `reasoning` records support. `reasoningEfforts` maps a selector level to an exact categorical `reasoning_effort` spelling. The same map may be carried unchanged on Chat Completions and on Responses. It is never a Messages thinking budget or an adaptive effort. A Responses vendor still applies its own contract to a historical Chat spelling.

```json
{
  "id": "my-model",
  "object": "model",
  "ocg": {
    "schemaVersion": 2,
    "name": "My model",
    "contextWindow": 262144,
    "maxOutputTokens": 32768,
    "inputModalities": ["text", "image"],
    "outputModalities": ["text"],
    "reasoning": true,
    "reasoningEfforts": {"low": "low", "high": "high", "xhigh": "max"},
    "toolCalling": true,
    "sources": ["operator"],
    "status": "declared",
    "protocols": {
      "preferred": "messages",
      "supported": ["chat_completions", "messages"]
    }
  }
}
```

The numbers and the Messages preference above illustrate the shape. They are not a specification for a real model. `status` is `declared` when any capability fact is present and `unknown` when the capability record is empty. A name with no eligible carrying route is omitted from the list rather than published without `protocols`.

The DSH plugin requires `ocg.schemaVersion` 2 and a `protocols.preferred` value listed in `protocols.supported`. It loads three APIs from the active DSH runtime — `openai-completions`, `openai-responses`, and `anthropic-messages` — and selects one per model from that preferred protocol. DSH 0.2.0-rc.2 with pi-ai 0.87.1 provides those APIs. The package does not vendor them, and installation is not gated by a version allowlist. Chat Completions and Responses keep the published `/v1` base. Messages drops a trailing `/v1` and keeps a deployment subpath. The Messages client sends the Gateway Key as `x-api-key` with `anthropic-version`; the plugin leaves those headers unchanged. A prepared call freezes the catalog metadata captured for that model before the prepare await returns. A missing or invalid profile is kept as an internal `ocg-rejected` placeholder and recorded on that model so exact resolve and prepare stay `INVALID_CONFIG`. It is not a selectable catalog row and is not registered as Chat. Mixed invalid entries are excluded from the advertised list; an all-invalid catalog is empty and does not fall back to Chat.

`reasoningEfforts` carries those exact categorical spellings on Chat Completions and on Responses. Messages keeps native reasoning support when `reasoning` is true. OCG does not write a Messages level menu or a thinking budget from that flag: those spellings are never a Messages budget or an adaptive effort. A native SDK may apply its own preset or default control. Omitting the OCG menu does not describe that control, and it does not mean the vendor accepts the SDK default. In the DSH plugin, selecting a Messages reasoning level the catalog does not declare is an explicit incompatibility. A protocol named in `supported` does not add a menu or guarantee a feature. A Responses vendor still applies its own contract to a historical Chat spelling. For example, `{"low":"low","high":"high","xhigh":"max"}` offers exactly Low, High, and Xhigh and sends those categorical spellings on Chat Completions and on Responses. Absent levels stay disabled, including Off. Declaring only `reasoning: true` does not invent selectable levels. `off: "none"` is an explicit wire declaration, not an implicit default.

Signed assistant history is sent only when the provider, API, and model id all match the model about to be called. Before the base adapter can turn a foreign signed assistant message into plain text, the Harness checks a present pi-ai envelope: kind `pi-ai`, version 2, aligned with the assistant content, and the same provider, API, and model when the envelope carries opaque native data. Missing replay metadata leaves ordinary text and tools portable. Gateway conversion of the request still follows the saved route described in [Protocol conversion](protocol-conversion.md).

DSH's existing native interface does not consume every capability. Additional facts are retained on the adapter descriptor under `ocg`; this does not add audio/video transports, hosted tools or an arbitrary-capability UI. Maximum output capability is not inserted into `configuredMaxTokens`, so it does not silently become a deployment's default per-request output budget.

For catalogs without a known context window, the plugin retains bounded internal compatibility defaults, but omits the entire public `context` descriptor and marks the fallback fields in `ocg.fallbacks`. DSH requires a positive `contextWindow` whenever `context` is present; an empty object would prevent model loading. Declared context windows remain visible. Malformed metadata is isolated to its model and fails that model's exact resolution instead of taking down unrelated models. Those rows are not advertised in the selectable list.

## Discovery And Explicit Declarations

This version captures explicitly supplied metadata during Go/GOAT catalog refresh and saved configurable HTTP destination refresh. It does not infer specifications from a model's name. Other adapter catalogs and upstreams that return only IDs can use the public catalog fallback below; add an operator declaration where that catalog has no usable facts. Refresh the provider directory to collect new metadata; merely opening DSH does not issue provider-directory requests.

Routes fall back to the public [models.dev](https://models.dev) catalog for
any field they never learned — no operator declaration, and no
upstream-observed value for that field. OCG downloads
`https://models.dev/api.json` in the background (never inside a `/v1/models`
request) and caches each provider's models with that provider's API address.
A download that fails, times out, or returns HTTP 200 without a usable
catalog leaves the previous cache in place and retries later. Offline use
stays on that last good copy. A current cache is reused for about a day. A
cache written by an older build, which only has the flat model index, still
answers and is refreshed on the next opportunity even when the file itself
is recent.

A saved route uses a provider row when the URL matches that provider: same
scheme, host, and port, and the provider path is a segment-boundary prefix
of the route. The longest matching path wins. A model entry may name its own
API; that address replaces the provider address for that model. A matched
provider that does not list the model, or lists it with empty reasoning
tiers, ends the search. A broader provider on the same host is not
substituted, and a canonical model is not substituted either.

A route that matches no provider uses the model id's generic baseline. That
baseline is an inference for an unrecognized proxy, not a verification of
the proxy. If the rows for that exact id share one canonical link and the
target exists, the target's facts are used, so a generic custom route can
still publish the canonical choices for ids such as `gpt-5.2`,
`gpt-5.3-codex`, and `o3` when unrelated providers simply omit those rows.
Conflicting links, a missing target, or no link keep only the facts those
rows share. An id written as `provider/model` reads that catalog row and
does not follow another canonical hop. The same id is also tried as the
upstream id, then its last path segment, then the public id. There is no
fuzzy name guess. Modalities outside `text`/`image`/`audio`/`video` are
dropped at ingestion. Effort-style `reasoning_options` become selectable
reasoning levels (the `none` spelling fills the `off` selector). A bare
`reasoning: true`, a toggle, or a budget does not invent tiers.

Effective facts keep their per-field priority: operator declaration >
upstream observation > models.dev > unknown. An operator declaration
replaces the whole record and is never filled from the public catalog. An
upstream observation that explicitly lists no reasoning tiers stays empty.
When models.dev fills gaps under an upstream observation, the row's
`sources` list credits both.

Declare metadata in the dashboard: open **Providers**, select a connection, and use a model row's **Model capabilities** action. The form shows the effective metadata and its source (`operator`, `upstream`, `modelsdev`, or `unknown`), applies the same validation rules as the server, saves the full declaration under CAS, and can clear a manual declaration to reveal discovered facts. Blank fields mean unknown, not false. The **Aliases** page shows every mapping's effective input modalities and their provenance, and its **Declare** link on unknown rows lands directly in this editor.

The same rules are available to scripts through the authenticated dashboard endpoint:

```
GET /dashboard/api/v4/destinations/{id}/model-metadata
```

`GET /dashboard/api/v4/model-metadata` returns the same entries for every destination in one aggregate read — prefer it over fanning the per-destination route out across many rows.

Use the destination ID from `GET /dashboard/api/v4/destinations`. The response includes the current revision, exact public and upstream IDs, effective metadata and its source (`operator`, `upstream`, `modelsdev`, or `unknown`). No inference Key is accepted in place of the dashboard session.

Declare metadata through the same route using `PUT` and the latest CAS tokens. The numbers below are examples, not specifications for any real model:

```json
{
  "expectedRevision": 123,
  "processGeneration": 456,
  "publicModel": "my-model",
  "metadata": {
    "name": "My model",
    "contextWindow": 262144,
    "maxOutputTokens": 32768,
    "inputModalities": ["text", "image"],
    "outputModalities": ["text"],
    "reasoning": true,
    "reasoningEfforts": {"low": "low", "high": "high", "xhigh": "max"},
    "toolCalling": true
  }
}
```

`publicModel` must match the exact saved catalog mapping. A declaration replaces that mapping's entire metadata record, not a global same-name model and not a field-by-field merge. Use `metadata: null` explicitly to remove the declaration and reveal discovered facts; an omitted member is rejected. A stale revision is rejected without changing metadata. The dashboard form and this endpoint share the same CAS behavior; the form is the default path and the endpoint remains for scripting.

Only declare effective capabilities supported by the actual gateway path. The operation changes no model routing, enabled protocol, credential grant, account status, or verification. Unknown optional facts should be omitted. Integers must be positive and safe; output cannot exceed the context window; tier keys must be one of `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`.

## Alias And Route Safety

For an alias that may use several enabled mappings, capacities are the minimum known limit, modalities are the intersection and tiers are retained only when every mapping agrees on the same wire spelling. Any unknown candidate prevents a positive guarantee. `ocg.protocols` uses this same eligible set, ordered by routing rank, destination id, and upstream model. A model enabled on more than one protocol route is held to the same rule: each route contributes its verified provider facts or, when the route matches no provider, the generic baseline. A route with neither is unknown and withdraws positive claims. This deliberately favors safety over advertising the largest backend's capacity; capability-aware fallback routing is not added here.

Facts and declarations bind to the destination's route, protocols and exact model mapping. Changing these invalidates the old binding. Model-specific upstream overrides are not populated from discovery of a different destination route. Operator declarations survive refresh of the unchanged route. Raw upstream payloads and credential echoes are not stored as metadata.

---

[User guide index](../USER.md) · [简体中文](model-metadata.zh-CN.md) · [Docs index](../README.md)
