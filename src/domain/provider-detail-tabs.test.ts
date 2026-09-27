import assert from "node:assert/strict";
import test from "node:test";
import { providerDetailTabs } from "./provider-detail-tabs.ts";

const miniMax = {
  provider_id: "minimax-cn",
  origin: "builtin" as const,
  editable: false,
  deletable: false,
  managed_registration: false,
  pricing_availability: "unpriced" as const,
  model_source: "static",
};

test("unpriced fixed plans expose only models", () => {
  for (const pricing_availability of ["unpriced", "unavailable", "not_applicable"] as const) {
    assert.deepEqual(providerDetailTabs({ ...miniMax, pricing_availability }), ["models"]);
  }
  assert.deepEqual(providerDetailTabs(null), ["models"]);
});

test("pricing and settings are independent capabilities", () => {
  assert.deepEqual(providerDetailTabs({ ...miniMax, pricing_availability: "available" }), ["models", "pricing"]);
  assert.deepEqual(providerDetailTabs({ ...miniMax, managed_registration: true }), ["models", "settings"]);
  assert.deepEqual(providerDetailTabs({ ...miniMax, provider_id: "custom" }), ["models", "settings"]);
  for (const origin of ["custom", "preset"] as const) {
    assert.deepEqual(providerDetailTabs({ ...miniMax, origin, editable: true }), ["models", "settings"]);
    assert.deepEqual(providerDetailTabs({ ...miniMax, origin, deletable: true }), ["models", "settings"]);
    assert.deepEqual(providerDetailTabs({ ...miniMax, origin }), ["models"]);
  }
});

test("official API presets retain their balance and pricing panel before prices load", () => {
  assert.deepEqual(providerDetailTabs({
    ...miniMax, origin: "preset", editable: true, model_source: "official_api_preset",
  }), ["models", "pricing", "settings"]);
});
