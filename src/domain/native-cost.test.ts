import assert from "node:assert/strict";
import test from "node:test";
import {
  forwardLogNativeEstimate,
  formatNativeCostEstimate,
} from "./native-cost.ts";

function row(overrides: Partial<Parameters<typeof forwardLogNativeEstimate>[0]> = {}) {
  return {
    native_cost_value: null,
    native_cost_currency: null,
    native_cost_unit: null,
    provider_id: "custom",
    pricing_revision_id: null,
    ...overrides,
  };
}

test("missing, negative, or non-finite native amounts never render", () => {
  assert.equal(forwardLogNativeEstimate(row()), null);
  assert.equal(forwardLogNativeEstimate(row({ native_cost_value: Number.NaN })), null);
  assert.equal(forwardLogNativeEstimate(row({ native_cost_value: Number.POSITIVE_INFINITY })), null);
  assert.equal(forwardLogNativeEstimate(row({ native_cost_value: -0.5 })), null);
});

test("official rows with native USD fields and a real pricing revision stay hidden", () => {
  // Official Go rows populate native_cost_* from usd_fields_from_cost and carry
  // an official pricing revision; a nonempty revision is NOT platform lineage.
  assert.equal(forwardLogNativeEstimate(row({
    native_cost_value: 0.0123,
    native_cost_currency: "USD",
    native_cost_unit: "USD",
    provider_id: "opencode",
    pricing_revision_id: "official:opencode:2026-09-01",
  })), null);
  // Any non-custom provider is excluded on provider id alone.
  assert.equal(forwardLogNativeEstimate(row({
    native_cost_value: 1,
    provider_id: "goat",
    pricing_revision_id: "uuid-1:2:vip:gpt-4o:1700000000:storefront:1699999999",
  })), null);
});

test("custom Keys with a native amount show the estimate, revision or not", () => {
  assert.ok(forwardLogNativeEstimate(row({ native_cost_value: 1, pricing_revision_id: null })));
  assert.ok(forwardLogNativeEstimate(row({
    native_cost_value: 0.0123,
    pricing_revision_id: "uuid-1:2:vip:gpt-4o:1700000000:storefront:1699999999",
  })));
});

test("zero is a real estimate, never treated as missing", () => {
  const estimate = forwardLogNativeEstimate(row({
    native_cost_value: 0,
    native_cost_currency: "CNY",
    native_cost_unit: "CNY",
  }));
  assert.ok(estimate);
  assert.equal(estimate.value, 0);
  assert.equal(formatNativeCostEstimate(estimate, "zh-CN"), "¥0");
});

test("tiny per-token totals keep significant digits", () => {
  const text = formatNativeCostEstimate({ value: 0.00000025, currency: "USD", unit: "USD" }, "en-US");
  assert.match(text, /0\.0+25/);
  assert.match(text, /\$/);
});

test("non-ISO currencies fall back to a plain suffix", () => {
  assert.equal(
    formatNativeCostEstimate({ value: 0.5, currency: "credits", unit: "credits" }, "en-US"),
    "0.5 credits",
  );
});

test("missing currency renders the bare number; a distinct unit is appended", () => {
  assert.equal(
    formatNativeCostEstimate({ value: 12.5, currency: null, unit: null }, "en-US"),
    "12.5",
  );
  assert.equal(
    formatNativeCostEstimate({ value: 12.5, currency: "USD", unit: "points" }, "en-US"),
    "$12.5 points",
  );
});
