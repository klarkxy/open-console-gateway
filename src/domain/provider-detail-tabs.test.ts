import assert from "node:assert/strict";
import test from "node:test";
import { providerDetailTabs } from "./provider-detail-tabs.ts";

const miniMax = {
  provider_id: "minimax-cn",
  origin: "builtin" as const,
  editable: false,
  deletable: false,
  managed_registration: false,
};

test("fixed builtin plans expose only models", () => {
  assert.deepEqual(providerDetailTabs(miniMax), ["models"]);
  assert.deepEqual(providerDetailTabs(null), ["models"]);
});

test("settings follow registration and editability", () => {
  assert.deepEqual(providerDetailTabs({ ...miniMax, managed_registration: true }), ["models", "settings"]);
  assert.deepEqual(providerDetailTabs({ ...miniMax, provider_id: "custom" }), ["models", "settings"]);
  for (const origin of ["custom", "preset"] as const) {
    assert.deepEqual(providerDetailTabs({ ...miniMax, origin, editable: true }), ["models", "settings"]);
    assert.deepEqual(providerDetailTabs({ ...miniMax, origin, deletable: true }), ["models", "settings"]);
    assert.deepEqual(providerDetailTabs({ ...miniMax, origin }), ["models"]);
  }
});

test("official API presets keep models and settings", () => {
  assert.deepEqual(providerDetailTabs({
    ...miniMax, origin: "preset", editable: true,
  }), ["models", "settings"]);
});
