[简体中文](provider-models.zh-CN.md)

# Manage individual supplier models

On **Providers**, select a configurable HTTP supplier, including a preset-derived HTTP supplier or a legacy Custom API connection. The model toolbar offers **Add model** and a searchable model selector with **Edit**. These controls also work before the first catalog refresh, including an empty catalog. The existing table still supports single-model and selected-model deletion.

## Model fields

- **Upstream model ID** is the exact model identifier sent to the supplier. It is required.
- **Public model name** is the callable alias clients put in their `model` field, not just a display label. Leave it blank to use the upstream ID verbatim. Names must be unique within this supplier, ignoring ASCII case and surrounding whitespace. Multiple distinct public names may point to one upstream ID.
- **Upstream protocols** selects the configured upstream routes this model may use. **Preferred protocol** must be one of those selections. These settings do not reject a client protocol that the gateway can convert. Add or change endpoint URLs in **Edit connection**, not in this model form.
- **Allow routing** enables the mapping. An enabled model needs at least one selected protocol; disabling a model does not delete it. Actual eligibility still depends on the destination, its Keys, model scopes, grants, and upstream availability.

An existing per-model endpoint override is preserved and shown read-only. Only its protocol can be selected. Editing an alias replaces that row: the old alias is not retained as an additional name. Cross-supplier name conflicts continue to follow the gateway's existing fail-closed resolution rules.

## Saving and deleting

Saving is one CAS-protected V4 destination PATCH. It keeps all other model mappings, their enablement and protocol selections, and the supplier's endpoint configuration. It does not authorize Keys, expand their model scopes, discover models, or send a paid test. Existing backend verification invalidation rules still apply when a mapping changes.

An open form captures its configuration revision. A conflicting edit or process restart cannot be silently overwritten. **Retry** on a stale form reloads the latest saved fields; re-enter the change and save again. The old mutation is never automatically replayed.

Use the table's delete action to remove one row, or select rows for batch deletion. Deleting the last model is supported by that existing catalog operation. Deletion is local: an explicit later catalog refresh may discover the same upstream ID again. An HTTP model rediscovered after deletion starts enabled. Disable a row instead when its disabled state should survive discovery.

## Scope

This editor does not change sealed built-in adapters or CPA. Their existing model/protocol controls and local deletion behavior remain unchanged; this change does not add manual models or custom aliases to them. A rare legacy HTTP destination with multiple implicit protocols must first be configured with explicit protocol routes in **Edit connection**; the individual-model editor refuses to silently collapse that transport configuration.

Related: [Add a provider](add-provider.md) · [Model catalog refresh](model-catalog-refresh.md).
