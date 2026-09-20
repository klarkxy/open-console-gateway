[简体中文](add-provider.zh-CN.md)

# Add a Provider

Use this guide when you want Open Console Gateway to route to another upstream service. There are three different integration paths:

| Goal | Path | Repository change |
| --- | --- | --- |
| Add a named Provider this node can reuse across accounts | **Providers** → **Add Provider** (user-defined) | No |
| Connect one OpenAI- or Anthropic-compatible service and attach one or more Keys | Add a **Custom API** / manual HTTP connection | No |
| Ship a named built-in Provider (the product's Provider/Plan identity) to every Open Console Gateway user | Add a sealed built-in Provider | Yes, reviewed code and tests |

The **Adapter Registry** stays static and sealed. User-defined Providers are typed persisted definitions; every one binds the code-owned Configurable HTTP adapter. OCG never loads user scripts, plugins, or binaries. Unknown `provider_id` values fail closed unless they match a saved definition. Migrated Custom API rows are normal configurable HTTP connections: endpoint, auth, protocol, and model mappings are edited on Providers, while Accounts manages one or more Keys.

## Create from a preset

**Providers → Add Provider** and **Accounts → Add account** open the same Accounts chooser. Choose a [Plan or API preset](provider-presets.md) there to create the Provider and its first Key together. Fixed-address presets already set protocol, authentication, endpoint and a default chat model. Optional settings expose names and models. Azure and Bedrock also need their customer-specific address and model/deployment information. Switching presets clears the previous Key and mappings, then supplies the new preset's default model. Completing a preset requires a Key; **Save draft** may omit it. Save from either button commits through `POST /dashboard/api/v4/onboarding/commit`.

## Create a user-defined Provider manually

1. Open **Providers** or **Accounts**, choose **Add Provider** / **Add account**,
   then pick **Manual setup** in the shared chooser.
2. Enter a name, one API Endpoint, one upstream protocol (Chat Completions, Responses, or Messages), and one auth kind (Bearer, `x-api-key`, or none).
3. Add at least one public-model → exact-upstream-ID mapping. **Fetch models** and **Test model** stay on this form but need a Key; for keyed auth they remain disabled until you enter one.
4. Save. Keyed auth may include an optional Key: filling it creates the first account in the same write; leaving it empty saves the definition only (shown as **Missing credential** until you use **Add Key**). A no-auth Provider creates one singleton account without a Key. The write goes through `POST /dashboard/api/v4/onboarding/commit` and does not require a successful probe. If the network drops before a response, the dashboard retries the same commit automatically; saving the unchanged draft again replays the stored result instead of creating a second Provider.

Edit replaces the whole Provider configuration through `PATCH /dashboard/api/v4/providers/{id}`. The Provider id is immutable. Changing no-auth to keyed auth requires an explicit replacement Key, written only to that singleton account. An already-keyed Provider rejects any Key on the Provider update; rotate Keys on **Accounts**. Delete is allowed only after every referencing account is removed; there is no cascade.

Provider-owned fields stay on **Providers**. Account **Key**, enablement, order, notes, cooldown, and tests stay on **Accounts**. User-defined Providers are always unpriced: no official usage, quota estimate, or pricing rows. Request logs still attribute provider, account, and model.

Node backups export payload V8 with destinations and credentials, model-resolution policy, and per-model route overrides. Imports accept V4 through V8 bundles. The current SQLite schema (v58) stores configurable HTTP Providers on destinations and `destination_models`; legacy Custom connections remain distinct and may hold multiple Keys. Sealed builtins stay compiled-in.

## Connect a compatible upstream now

1. Open **Accounts** and choose **Add account** → **Custom API**.
2. Enter a name, the upstream API Key, one API URL, and one upstream protocol: **Chat Completions**, **Responses**, or **Messages**.
3. Add at least one mapping: a public model name clients request and the exact upstream model ID. **Fetch models** can fill the draft from upstream IDs when the upstream exposes the optional model-list interface below.
4. Save the account. A ready Key account starts enabled. **Test connection** is an optional real, potentially billable request through that exact account and does not change the switch.
5. Call authenticated `GET /v1/models` on Open Console Gateway and confirm the routeable public name is published, then send one inference request.

One Custom account uses one upstream protocol for every mapping on that card. Matching client traffic passes through; other supported client formats are converted to the selected upstream protocol. **Fetch models** returns upstream IDs only; importing one makes `public model = upstream ID` exactly, without suffix stripping or generated Aliases. You may then edit the public name while retaining the exact upstream ID.

## Upstream HTTP interface

OCG resolves common base URLs consistently for model discovery, verification, and production traffic:

| Configured API URL | Inference URL | Optional model-list URL |
| --- | --- | --- |
| `https://api.example.com` | Adds `/v1/chat/completions`, `/v1/responses`, or `/v1/messages` | `https://api.example.com/v1/models` |
| `https://api.example.com/v1` | Adds `/chat/completions`, `/responses`, or `/messages` | `https://api.example.com/v1/models` |
| A complete standard inference URL | Used exactly as entered | The sibling `/models` |
| A non-standard complete path | Used exactly as entered | Not guessed; enter model IDs manually |

The configured URL must be HTTP or HTTPS and have a host. Embedded credentials, query strings, and fragments are rejected. A trusted administrator may deliberately select a loopback, LAN, or public destination. Metadata, link-local, and opaque IPv4-trick hosts are rejected. A per-model override to another Origin does not inherit the Provider Key. OCG does not follow redirects on secret-bearing requests.

The selected protocol defines the wire contract:

| Protocol | Standard path | Authentication sent upstream | Required behavior |
| --- | --- | --- | --- |
| OpenAI Chat Completions | `/v1/chat/completions` | `Authorization: Bearer <upstream-key>` | Accept Chat request JSON and return Chat JSON or Chat SSE |
| OpenAI Responses | `/v1/responses` | `Authorization: Bearer <upstream-key>` | Accept Responses request JSON and return Responses JSON or Responses SSE |
| Anthropic Messages | `/v1/messages` | `x-api-key: <upstream-key>` plus `anthropic-version: 2023-06-01` | Accept Messages request JSON and return Messages JSON or Messages SSE |

OCG derives authentication from the protocol. It never sends both auth styles, retries a `401` with another header, or forwards a dashboard/client Key upstream. The response must follow the selected protocol closely enough for OCG's parser and converter, including standard error bodies and `text/event-stream` framing when streaming.

### Optional model discovery

**Fetch models** sends an authenticated `GET` to the resolved model-list URL. Return an OpenAI/Anthropic-style object with a `data` array:

```json
{
  "data": [
    { "id": "model-a" },
    { "id": "model-b" }
  ],
  "has_more": false
}
```

Each usable row needs a non-empty string `id`. For pagination, set `has_more: true`, return `last_id` (or ensure the last usable row has an ID), and accept the next request's `after_id` query parameter. Discovery only updates the unsaved form; it does not save, verify, or enable an account.

## Add a built-in Provider

A built-in integration is appropriate only when the Provider needs product-owned identity, catalog, account lifecycle, routing, pricing/usage, or other semantics that Custom API cannot express. Start from the current code.

1. Define one stable `provider_id`, its Provider row, credential/quota semantics, and an exhaustive `ProviderAdapterKind` mapping in `crates/ocg-domain/src/ids.rs` and `provider.rs`. Provider and Plan share `provider_id`.
2. Add only verified protocol facts to `crates/ocg-domain/src/protocol.rs`. Request routing uses the saved contract.
3. Add code-owned client Alias mappings in `crates/ocg-gateway/src/alias.rs`. Preserve exact upstream IDs and reject ambiguous raw IDs; a discovered row must not silently invent a public Alias.
4. Implement the host route resolver in `ocg-core`. The adapter returns an `AttemptSpec`; database access, Key decryption, proxy selection, and outbound HTTP remain host-owned.
5. Add the account and **Providers** control-plane/UI workflow, including catalog refresh, enablement, verification, errors, cooldown, pricing, and usage only where the Provider actually supports them. All dashboard writes use CAS on `/dashboard/api/v4`: user-defined Provider creation goes through the onboarding commit; Provider definition edits, account operations, Key rotation, binding edits, and additional identity credentials use remounted or native V4 routes. `/dashboard/api/v3` is a 410 tombstone.
6. Update the paired user guides and tests. Run the checks in [Development](../maintainer/development.md) for the crates and UI you touched.

Before opening a contribution, write down the upstream origin, auth scheme, catalog source, supported model/protocol pairs, streaming behavior, error semantics, quota/price source, and a non-billable validation plan. Keep the new family fail-closed until its complete routing and control-plane path exists.

For repository architecture details, continue with [Extending Open Console Gateway](../maintainer/extending.md) and [Runtime invariants](../maintainer/runtime-invariants.md).

---

[User guide index](../USER.md) · [简体中文](add-provider.zh-CN.md) · [Add an application](add-application.md) · [Docs index](../README.md)
