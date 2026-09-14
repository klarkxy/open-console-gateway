import assert from "node:assert/strict";
import test from "node:test";
import {
  buildProviderDefinitionUpdateBody,
  completeDynamicTestTargets,
  dynamicAuthRequiresKey,
  dynamicMappingOverrideError,
  dynamicProviderActionNeedsConfirm,
  emptyProviderDefinitionDraft,
  isDynamicCatalogEntry,
  normalizeDynamicMappings,
  resolveDynamicMappingRoute,
  sanitizeProviderDefinitionDraft,
  validateProviderDefinitionDraft,
  type ProviderDefinitionMapping,
} from "./dynamic-provider.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";

function entry(extra: Partial<ProviderCatalogEntry> = {}): ProviderCatalogEntry {
  return {
    provider_id: "opencode",
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: "OpenCode Go",
    display_family: "OpenCode",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    verification_policy: "not_required",
    verification_runtime_availability: "not_applicable",
    routable: true,
    managed_registration: false,
    pricing_availability: "available",
    usage_availability: "available",
    manual_usage_calibration: false,
    quota_unit: "usd",
    model_source: "opencode_get_models",
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
    ...extra,
  };
}

test("source labels distinguish built-in catalog rows from user-defined Providers", () => {
  assert.equal(isDynamicCatalogEntry(entry()), false);
  assert.equal(isDynamicCatalogEntry(entry({ model_source: "dynamic_provider" })), true);
});

test("mapping validation requires one unique public model and allows repeated upstream IDs", () => {
  assert.equal(normalizeDynamicMappings([]), "missing_mappings");
  assert.equal(
    normalizeDynamicMappings([
      { public_model: "Opus", upstream_model: "a" },
      { public_model: "opus", upstream_model: "b" },
    ]),
    "duplicate_public_model",
  );
  assert.deepEqual(
    normalizeDynamicMappings([
      { public_model: "opus", upstream_model: "vendor/a" },
      { public_model: "sonnet", upstream_model: "vendor/a" },
    ]),
    [
      { public_model: "opus", upstream_model: "vendor/a", upstream_override: null },
      { public_model: "sonnet", upstream_model: "vendor/a", upstream_override: null },
    ],
  );
});

test("create validation requires a Key only when asked; none-auth is not keyed", () => {
  const draft = emptyProviderDefinitionDraft();
  draft.name = "Lab";
  draft.endpoint_url = "http://127.0.0.1:9";
  draft.models = [{ public_model: "lab-opus", upstream_model: "vendor/opus" }];
  assert.equal(validateProviderDefinitionDraft(draft, { mode: "create" }), null);
  assert.equal(
    validateProviderDefinitionDraft(draft, { mode: "create", requireKey: true }),
    "missing_key",
  );
  draft.key = "sk-lab";
  assert.equal(validateProviderDefinitionDraft(draft, { mode: "create", requireKey: true }), null);
  draft.auth_kind = "none";
  draft.key = "should-not-send";
  assert.equal(validateProviderDefinitionDraft(draft, { mode: "create" }), null);
  draft.key = "";
  assert.equal(validateProviderDefinitionDraft(draft, { mode: "create" }), null);
  assert.ok(dynamicAuthRequiresKey("bearer"));
  assert.equal(dynamicAuthRequiresKey("none"), false);
});

test("edit from none to keyed requires an explicit replacement Key", () => {
  const draft = emptyProviderDefinitionDraft();
  draft.name = "Lab";
  draft.endpoint_url = "http://127.0.0.1:9";
  draft.auth_kind = "bearer";
  draft.models = [{ public_model: "lab-opus", upstream_model: "vendor/opus" }];
  assert.equal(
    validateProviderDefinitionDraft(draft, { mode: "edit", previousAuthKind: "none" }),
    "missing_replacement_key",
  );
  draft.key = "sk-now";
  const body = buildProviderDefinitionUpdateBody(draft, "none");
  assert.equal(body.key, "sk-now");
});

test("ordinary keyed edit omits a discover/test Key", () => {
  const draft = emptyProviderDefinitionDraft();
  draft.name = "Lab";
  draft.endpoint_url = "http://127.0.0.1:9";
  draft.auth_kind = "bearer";
  draft.models = [{ public_model: "lab-opus", upstream_model: "vendor/opus" }];
  draft.key = "sk-probe";
  const update = buildProviderDefinitionUpdateBody(draft, "bearer");
  assert.equal("key" in update, false);
  assert.equal(update.key, undefined);
});

test("sanitization drops the write-only Key from a draft and keeps the draft name", () => {
  const draft = emptyProviderDefinitionDraft();
  draft.key = "sk-secret";
  draft.name = "Lab";
  const sanitized = sanitizeProviderDefinitionDraft(draft);
  assert.equal(sanitized.key, "");
  assert.equal(sanitized.name, "Lab");
});

test("paid tests and deletes require confirmation; Enter submits save", () => {
  assert.equal(dynamicProviderActionNeedsConfirm("test"), true);
  assert.equal(dynamicProviderActionNeedsConfirm("delete"), true);
  assert.equal(dynamicProviderActionNeedsConfirm("save"), false);
  assert.equal(dynamicProviderActionNeedsConfirm("discover"), false);
});

test("only complete trimmed mappings become selectable test targets, overrides included", () => {
  assert.deepEqual(completeDynamicTestTargets([
    { public_model: "  pub-a ", upstream_model: " up-a " },
    {
      public_model: "pub-b",
      upstream_model: "up-b",
      upstream_override: { protocol: "messages", endpoint_url: " https://up.example.com/v1/messages " },
    },
    { public_model: "", upstream_model: "up-c" },
    { public_model: "pub-d", upstream_model: "   " },
    { public_model: "", upstream_model: "" },
  ]), [
    { public_model: "pub-a", upstream_model: "up-a", upstream_override: null },
    {
      public_model: "pub-b",
      upstream_model: "up-b",
      upstream_override: { protocol: "messages", endpoint_url: "https://up.example.com/v1/messages" },
    },
  ]);
  assert.deepEqual(completeDynamicTestTargets([]), []);
});

test("any present override wins verbatim, even unfinished; only absence inherits the supplier default", () => {
  const supplier = { endpoint_url: "https://api.example.com/v1/responses", upstream_protocol: "responses" as const };
  assert.deepEqual(resolveDynamicMappingRoute(supplier, { upstream_override: null }), supplier);
  assert.deepEqual(resolveDynamicMappingRoute(supplier, {}), supplier);
  // An explicit but empty override never silently falls back to the supplier:
  // the empty route is returned so the pre-request validation can reject it.
  assert.deepEqual(
    resolveDynamicMappingRoute(supplier, { upstream_override: { protocol: "messages", endpoint_url: "  " } }),
    { endpoint_url: "", upstream_protocol: "messages" },
  );
  assert.deepEqual(
    resolveDynamicMappingRoute(supplier, {
      upstream_override: { protocol: "messages", endpoint_url: "https://other.example.com/anthropic/v1/messages" },
    }),
    { endpoint_url: "https://other.example.com/anthropic/v1/messages", upstream_protocol: "messages" },
  );
});

test("the selected-row override gate rejects an empty explicit override before any request", () => {
  // Presence is enough to fail: the test must not run against the supplier.
  assert.equal(
    dynamicMappingOverrideError({ upstream_override: { protocol: "messages", endpoint_url: " " } }),
    "missing_override_endpoint",
  );
  assert.equal(
    dynamicMappingOverrideError({ upstream_override: { protocol: "messages", endpoint_url: "not a url" } }),
    "invalid_override_endpoint",
  );
  // Inherit rows and well-formed overrides pass without touching other rows.
  assert.equal(dynamicMappingOverrideError({ upstream_override: null }), null);
  assert.equal(dynamicMappingOverrideError({}), null);
  assert.equal(
    dynamicMappingOverrideError({
      upstream_override: { protocol: "messages", endpoint_url: "https://up.example.com/v1/messages" },
    }),
    null,
  );
});

test("an override requires an explicit well-formed endpoint and never guesses siblings", () => {
  const rows = (override: ProviderDefinitionMapping["upstream_override"]) => [
    { public_model: "opus", upstream_model: "vendor/a", upstream_override: override },
  ];
  assert.equal(
    normalizeDynamicMappings(rows({ protocol: "messages", endpoint_url: "" })),
    "missing_override_endpoint",
  );
  assert.equal(
    normalizeDynamicMappings(rows({ protocol: "messages", endpoint_url: "not a url" })),
    "invalid_override_endpoint",
  );
  assert.equal(
    normalizeDynamicMappings(rows({ protocol: "messages", endpoint_url: "ftp://up.example.com/v1/messages" })),
    "override_endpoint_not_http",
  );
  assert.equal(
    normalizeDynamicMappings(rows({ protocol: "messages", endpoint_url: "https://user:pw@up.example.com/v1/messages" })),
    "override_endpoint_with_credentials",
  );
  assert.deepEqual(
    normalizeDynamicMappings(rows({ protocol: "messages", endpoint_url: " https://up.example.com/v1/messages " })),
    [{
      public_model: "opus",
      upstream_model: "vendor/a",
      upstream_override: { protocol: "messages", endpoint_url: "https://up.example.com/v1/messages" },
    }],
  );
});

test("edit body roundtrips the override; null on edit clears it", () => {
  const draft = emptyProviderDefinitionDraft();
  draft.name = "Lab";
  draft.endpoint_url = "https://api.example.com/v1/responses";
  draft.upstream_protocol = "responses";
  draft.auth_kind = "none";
  draft.models = [
    { public_model: "inherit-row", upstream_model: "vendor/a" },
    {
      public_model: "override-row",
      upstream_model: "vendor/b",
      upstream_override: { protocol: "messages", endpoint_url: "https://up.example.com/v1/messages" },
    },
  ];
  // Edit sends the full model list: a preserved override rides through,
  // and null explicitly returns the row to the supplier default.
  const updated = buildProviderDefinitionUpdateBody(draft, "none");
  assert.deepEqual(updated.models, [
    { publicModel: "inherit-row", upstreamModel: "vendor/a", upstreamOverride: null },
    {
      publicModel: "override-row",
      upstreamModel: "vendor/b",
      upstreamOverride: { protocol: "messages", endpointUrl: "https://up.example.com/v1/messages" },
    },
  ]);
  const cleared = buildProviderDefinitionUpdateBody({
    ...draft,
    models: draft.models.map((model) => ({ ...model, upstream_override: null })),
  }, "none");
  assert.deepEqual(cleared.models.map((model) => model.upstreamOverride), [null, null]);
});
