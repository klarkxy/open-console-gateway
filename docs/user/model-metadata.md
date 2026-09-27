[简体中文](model-metadata.zh-CN.md)

# Model metadata and reasoning tiers in DSH

Use **Applications → DSH** to install the OCG provider. After upgrading OCG to a build containing this feature, install/replace the plugin once and reload the selected DSH runtime. Updating the gateway alone does not replace a previously installed plugin. Model-list reads refresh the directory; requests reuse its snapshot for up to five seconds. Existing Key handoff and credential storage are unchanged.

## What is reported

Authenticated `GET /v1/models` retains its OpenAI-compatible envelope and public IDs. Known capacities are added as `contextWindow` and `maxTokens`. The versioned `ocg` object contains the model name, context window, maximum output tokens, input/output modalities, reasoning support, explicit `reasoningEfforts`, tool-call facts, sources and status. Missing fields mean unknown, not false. This GET never contacts an upstream.

The DSH plugin maps context and text/image input to native model descriptors, and translates every offered reasoning level into pi-ai's `thinkingLevelMap`. The wire format is explicitly OpenAI Chat Completions; OCG remains responsible for its configured protocol conversion. For example, `{"low":"low","high":"high","xhigh":"max"}` offers exactly Low, High and Xhigh and sends the declared spellings. Absent levels are disabled, including Off. Declaring only `reasoning: true` does not invent selectable levels. `off: "none"` is an explicit wire declaration, not an implicit default.

DSH's existing native interface does not consume every capability. Additional facts are retained on the adapter descriptor under `ocg`; this does not add audio/video transports, hosted tools or an arbitrary-capability UI. Maximum output capability is not inserted into `configuredMaxTokens`, so it does not silently become a deployment's default per-request output budget.

For legacy ID-only catalogs the plugin retains bounded internal compatibility defaults, but removes the fabricated context value from the public resolved descriptor and marks the fallback fields in `ocg.fallbacks`. Malformed metadata is isolated to its model and fails that model's resolution instead of taking down unrelated models.

## Discovery and explicit declarations

This version captures explicitly supplied metadata during Go/GOAT catalog refresh and saved configurable HTTP destination refresh. It does not infer specifications from a model's name. Other adapter catalogs, and upstreams that return only IDs, need an operator declaration until their metadata ingestion is implemented. Refresh the provider directory to collect new metadata; merely opening DSH does not issue provider-directory requests.

Read a connection's current metadata with the authenticated dashboard endpoint:

```
GET /dashboard/api/v4/destinations/{id}/model-metadata
```

Use the destination ID from `GET /dashboard/api/v4/destinations`. The response includes the current revision, exact public and upstream IDs, effective metadata and its source (`operator`, `upstream`, or `unknown`). No inference Key is accepted in place of the dashboard session.

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

`publicModel` must match the exact saved catalog mapping. A declaration replaces that mapping's entire metadata record, not a global same-name model and not a field-by-field merge. Use `metadata: null` explicitly to remove the declaration and reveal discovered facts; an omitted member is rejected. A stale revision is rejected without changing metadata. This first version exposes the declaration API, not a new dashboard form.

Only declare effective capabilities supported by the actual gateway path. The operation changes no model routing, enabled protocol, credential grant, account status or verification. Unknown optional facts should be omitted. Integers must be positive and safe; output cannot exceed the context window; tier keys must be one of `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max`.

## Alias and route safety

For an alias that may use several enabled mappings, capacities are the minimum known limit, modalities are the intersection and tiers are retained only when every mapping agrees on the same wire spelling. Any unknown candidate prevents a positive guarantee. This deliberately favors safety over advertising the largest backend's capacity; capability-aware fallback routing is not added here.

Facts and declarations bind to the destination's route, protocols and exact model mapping. Changing these invalidates the old binding. Model-specific upstream overrides are not populated from discovery of a different destination route. Operator declarations survive refresh of the unchanged route. Raw upstream payloads and credential echoes are not stored as metadata.
