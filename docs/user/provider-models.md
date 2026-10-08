[简体中文](provider-models.zh-CN.md)

# Manage Individual Supplier Models

The model table is paged. Search and **Enabled only** apply to the complete
catalog, and selecting all matching models includes matches on other pages.
Editors load complete configuration and credential grants before saving.

On **Providers**, select a built-in provider or a configurable HTTP supplier, including preset-derived HTTP suppliers and legacy Custom API connections. All provider model tables use the same toolbar: **Add model**, one model/alias search field, and **Enabled only**. Use the pencil in the row’s **Actions** column to edit that model. **Add model** remains available before the first catalog refresh, including an empty catalog. The existing table still supports single-model and selected-model deletion.

## Built-in Providers

MiniMax CN Token Plan, Kimi Code CN, OpenCode Go, Zen Free, Command Code GOAT, and Ollama Cloud use the full add/edit form: upstream model ID, public callable alias, upstream protocols, preferred protocol, and routing enablement. Empty catalogs support additions. Built-ins retain one mapping per upstream model; rename its existing row to change the alias. HTTP suppliers continue to allow several aliases per upstream.

The public alias participates in client model lists and routing; the outbound model remains the exact upstream ID. A custom alias replaces that model's generated public name without removing another provider's mapping under a shared name. Exact raw IDs keep the existing ambiguity checks.

Manually added built-in rows start disabled; catalog refresh uses the Provider's documented evidence and preserves saved ForceOff choices. Protocol selections must fit the sealed adapter's capabilities. Endpoints and authentication remain fixed. Saving sends no upstream request, grants no Key access, and does not expand model scopes. Check a Key's scope after renaming if it was restricted to the old alias.

Saved aliases, protocol selections, preferences, and switches survive restart. **Refresh model catalog** rebuilds the snapshot from the official response: upstream models still present retain these settings; fresh MiniMax CN, Kimi CN, and GOAT aliases derive from the complete saved catalog, while absent manual IDs may be removed. Kimi's `kimi-for-coding` and `kimi-for-coding-highspeed` IDs remain unchanged. A manually created catalog is labeled as such without a fabricated official refresh timestamp.

## Model Fields

- **Upstream model ID** is the exact model identifier sent to the supplier. It is required.
- **Public model name** is the callable alias clients put in their `model` field, not just a display label. For built-in catalog rows, the generated name comes from the complete saved catalog's normalized final slash-separated leaf; an explicit name replaces that generated name. Names must be unique within this supplier, ignoring ASCII case and surrounding whitespace. Multiple distinct public names may point to one upstream ID.
- **Upstream protocols** selects the configured upstream routes this model may use. **Preferred protocol** must be one of those selections. These settings do not reject a client protocol that the gateway can convert. For HTTP suppliers, add or change endpoint URLs in **Edit connection**, not in this model form.
- **Allow routing** enables the mapping. An enabled model needs at least one selected protocol; disabling a model does not delete it. Actual eligibility still depends on the destination, its Keys, model scopes, grants, and upstream availability.

An existing per-model endpoint override is preserved and shown read-only. Only its protocol can be selected. Editing an alias replaces that row: the old alias is not retained as an additional name. Cross-supplier name conflicts continue to follow the gateway's existing fail-closed resolution rules.

## Saving And Deleting

Saving uses the CAS-protected V4 model editor for built-ins or destination PATCH for HTTP suppliers. It keeps all other model mappings, their enablement and protocol selections, and the supplier's endpoint configuration. It does not authorize Keys, expand their model scopes, discover models, or send a paid test. Existing backend verification invalidation rules still apply when a mapping changes.

An open form captures its configuration revision. A conflicting edit or process restart cannot be silently overwritten. **Retry** on a stale form reloads the latest saved fields; re-enter the change and save again. The old mutation is never automatically replayed.

Use the table's delete action to remove one row, or select rows for batch deletion. Deleting the last model is supported by that existing catalog operation. Deletion is local: an explicit later catalog refresh may discover the same upstream ID again. An HTTP model rediscovered after deletion starts enabled. Disable a row instead when its disabled state should survive discovery.

## Scope

Built-in model mappings and protocols are editable while their sealed adapters, endpoints, and authentication remain fixed. CPA remains an externally managed catalog and does not offer manual additions. A rare legacy HTTP destination with multiple implicit protocols must first be configured with explicit protocol routes in **Edit connection**; the individual-model editor refuses to silently collapse that transport configuration.

Related: [Add a provider](add-provider.md) · [Model catalog refresh](model-catalog-refresh.md).

---

[User guide index](../USER.md) · [简体中文](provider-models.zh-CN.md) · [Docs index](../README.md)
