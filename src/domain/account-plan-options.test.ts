import assert from "node:assert/strict";
import test from "node:test";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { buildPlanOptions, PLAN_OPTION_CREATION_HINT_KEYS, splitPlanOptionsByOffering } from "./account-plan-options.ts";
import { PLAN_CREATE_DISABLED_REASON_KEYS } from "./plans.ts";

function row(provider_id: string, extra: Partial<ProviderCatalogEntry> = {}): ProviderCatalogEntry {
  return {
    provider_id,
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: provider_id,
    display_family: provider_id,
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    verification_policy: "required",
    verification_runtime_availability: "available",
    routable: true,
    managed_registration: false,
    pricing_availability: "available",
    usage_availability: "available",
    manual_usage_calibration: false,
    quota_unit: "tokens",
    model_source: "invented_catalog",
    key_prefix: null,
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [{ id: "key", kind: "secret", required: true, immutable_after_create: false }],
    model_aliases: [],
    ...extra,
  };
}

test("failed catalog has only createable Go while successful empty catalog stays empty", () => {
  for (const catalog of [null, undefined] as const) {
    const options = buildPlanOptions(catalog);
    assert.deepEqual(options.map((option) => option.optionId), ["opencode"]);
    assert.equal(options[0]?.managed, true);
    assert.equal(options[0]?.disabled, false);
  }
  assert.deepEqual(buildPlanOptions([]), []);
});

test("an invented builtin row automatically drives order, form fields, name and capabilities", () => {
  const catalog = [
    row("future-plan", {
      display_name: "Future Plan",
      form_fields: [
        { id: "name", kind: "text", required: true, immutable_after_create: false },
        { id: "key", kind: "secret", required: true, immutable_after_create: false },
      ],
    }),
    row("custom", { offering: "api", display_name: "Custom API", pricing_availability: "unpriced" }),
  ];
  const options = buildPlanOptions(catalog);
  assert.deepEqual(options.map((option) => option.optionId), ["future-plan", "custom"]);
  assert.equal(options[0]?.label, "Future Plan");
  assert.deepEqual(options[0]?.plan.form_fields.map((field) => field.id), ["name", "key"]);
  assert.equal(options[0]?.plan.usage_availability, "available");
});

test("dynamic offering comes from its catalog row and never from preset inference", () => {
  const dynamicId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  const catalog = [
    row(dynamicId, {
      origin: "preset",
      offering: "plan",
      display_name: "Coding Plan",
      model_source: "dynamic_provider",
    }),
    row("custom", { offering: "api", display_name: "Custom API" }),
  ];
  const split = splitPlanOptionsByOffering(catalog, new Map([[dynamicId, "some-api-preset"]]));
  assert.deepEqual(split.plan.map((option) => option.optionId), [dynamicId]);
  assert.deepEqual(split.api.map((option) => option.optionId), ["custom"]);
  assert.equal(split.plan[0]?.source, "user-defined");
  assert.equal(split.plan[0]?.plan.dynamic, true);
});

test("catalog creation status and singleton state are enforced without family branches", () => {
  const options = buildPlanOptions([
    row("blocked", { creation_availability: "unavailable" }),
    row("singleton", { singleton: true, credential_kind: "none" }),
  ]);
  assert.equal(options.length, 1);
  assert.equal(options[0]?.optionId, "blocked");
  assert.equal(options[0]?.disabledReason, "creation_unavailable");
});

test("every disabled-reason and creation-hint code has a message key", () => {
  const reason = buildPlanOptions([row("blocked", { creation_availability: "unavailable" })])[0]
    ?.disabledReason;
  if (!reason) assert.fail("blocked option must carry a disabled-reason code");
  assert.ok(PLAN_CREATE_DISABLED_REASON_KEYS[reason]);
  assert.deepEqual(
    Object.keys(PLAN_CREATE_DISABLED_REASON_KEYS).sort(),
    ["catalog_entry_missing", "catalog_unavailable", "creation_unavailable", "singleton_managed"],
  );
  assert.deepEqual(Object.keys(PLAN_OPTION_CREATION_HINT_KEYS), ["missing_mappings"]);
});
