import assert from "node:assert/strict";
import test from "node:test";
import type { PricingSnapshot } from "../api/dashboard.ts";
import type { ProviderCatalogEntry, ProviderNeutralPricingSnapshot } from "../api/providers.ts";
import {
  buildPlanPricingGroups,
  buildScopedPlanPricingGroups,
  PLAN_PRICING_MESSAGE_KEYS,
  resolvePlanPricingDisplay,
} from "./pricing-plans.ts";

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
    model_source: "future",
    key_prefix: null,
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
    ...extra,
  };
}

const modelSnapshot: PricingSnapshot = {
  revision: "models-r1",
  activated_at: "2026-09-14T00:00:00Z",
  document_updated_at: null,
  source_url: "https://prices.example/models",
  content_hash: "models",
  adjustment_policy_version: "1",
  limits: { window_5h: 1, window_week: 2, window_month: 3 },
  models: [{
    model_id: "future-1",
    display_name: "Future 1",
    input: 1,
    output: 2,
    cache_read: null,
    cache_write: null,
    usage: 1,
    quota_multiplier: 1,
    adjustments: [],
  }],
};

const valueSnapshot: ProviderNeutralPricingSnapshot = {
  revision: "values-r1",
  activated_at: "2026-09-14T00:00:00Z",
  document_updated_at: null,
  source_url: "https://prices.example/values",
  content_hash: "values",
  evidence: "test",
  values: [{
    model_id: "future-2",
    display_name: "Future 2",
    input_per_million: 1,
    output_per_million: 2,
    cache_read_per_million: null,
    cache_write_per_million: null,
    plan_limit: null,
    model_allowance: null,
    quota_multiplier: 1,
    paid_plan_price: null,
    currency: "USD",
  }],
};

test("default pricing is catalog ordered, plan-only, and available-only", () => {
  const catalog = [
    row("future-models", { display_name: "Future Models" }),
    row("future-api", { offering: "api" }),
    row("future-unpriced", { pricing_availability: "unpriced" }),
    row("future-values"),
  ];
  const groups = buildPlanPricingGroups(catalog, null, {
    "future-models": {
      provider_id: "future-models",
      availability: "available",
      snapshot: modelSnapshot,
      revision: 1,
      process_generation: 1,
      pricing_revision: "r",
      provider_pricing_revision: "models-r1",
    },
    "future-values": {
      provider_id: "future-values",
      availability: "available",
      snapshot: valueSnapshot,
      revision: 1,
      process_generation: 1,
      pricing_revision: "r",
      provider_pricing_revision: "values-r1",
    },
  });
  assert.deepEqual(groups.map((group) => group.plan.provider_id), ["future-models", "future-values"]);
  assert.equal(groups[0]?.content.kind, "models");
  assert.equal(groups[1]?.content.kind, "values");
  assert.equal(resolvePlanPricingDisplay(groups[0]!).messageCode, "models_note");
  assert.equal(resolvePlanPricingDisplay(groups[1]!).messageCode, "estimate_note");
});

test("provider detail preserves unpriced state and never invents content", () => {
  const groups = buildScopedPlanPricingGroups(
    "future-unpriced",
    [row("future-unpriced", { pricing_availability: "unpriced" })],
    null,
    {},
  );
  assert.equal(groups[0]?.pricingAvailability, "unpriced");
  assert.equal(groups[0]?.content.kind, "none");
  assert.equal(resolvePlanPricingDisplay(groups[0]!).state, "unpriced");
  assert.equal(resolvePlanPricingDisplay(groups[0]!).messageCode, "unpriced");
});

test("error and empty states resolve message codes and every code has a message key", () => {
  const priced = row("priced");
  const groups = buildPlanPricingGroups([priced], null, {
    priced: {
      provider_id: "priced",
      availability: "available",
      snapshot: modelSnapshot,
      revision: 1,
      process_generation: 1,
      pricing_revision: "r",
      provider_pricing_revision: "models-r1",
    },
  });
  assert.equal(resolvePlanPricingDisplay(groups[0]!, "boom").state, "error");
  assert.equal(resolvePlanPricingDisplay(groups[0]!, "boom").messageCode, "load_failed");

  const emptyGroups = buildPlanPricingGroups(
    [row("empty-plan")],
    null,
    {
      "empty-plan": {
        provider_id: "empty-plan",
        availability: "available",
        snapshot: { ...modelSnapshot, models: [] },
        revision: 1,
        process_generation: 1,
        pricing_revision: "r",
        provider_pricing_revision: "models-r1",
      },
    },
  );
  assert.equal(resolvePlanPricingDisplay(emptyGroups[0]!).state, "available-empty");
  assert.equal(resolvePlanPricingDisplay(emptyGroups[0]!).messageCode, "empty");

  assert.deepEqual(
    Object.keys(PLAN_PRICING_MESSAGE_KEYS).sort(),
    ["empty", "estimate_note", "load_failed", "models_note", "not_applicable", "unavailable", "unpriced"],
  );
});

test("catalog failure keeps only the existing Go snapshot fallback", () => {
  assert.deepEqual(
    buildPlanPricingGroups(null, modelSnapshot, {}).map((group) => group.plan.provider_id),
    ["opencode"],
  );
  assert.deepEqual(buildPlanPricingGroups([], modelSnapshot, {}), []);
});
