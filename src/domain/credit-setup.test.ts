import assert from "node:assert/strict";
import test from "node:test";
import type { CreditPreset } from "../api/billing.ts";
import { buildCreditSetup, creditSetupDraft } from "./credit-setup.ts";
const now = Date.parse("2026-09-22T00:00:00Z");
const presets: CreditPreset[] = [{
  id: "plus", initialGrant: 1_600_000_000,
  configuration: { name: "Plus", currency: "CNY", creditsPerCurrency: 1_000_000, rates: [], sourceUrl: null,
    monthly: { amount: 1_600_000_000, nextResetAt: "2026-09-30T16:00:00Z", timezoneOffsetMinutes: 480, renewalEndsAt: null } },
}];
test("cash setup is opt-in; a preset builds scaled remaining with its original monthly capacity", () => {
  assert.deepEqual(buildCreditSetup(creditSetupDraft([], now), [], now), { input: null, valid: true });
  const draft = creditSetupDraft(presets, now);
  draft.remaining = 750.5;
  const result = buildCreditSetup(draft, presets, now);
  assert.equal(result.valid, true);
  assert.equal(result.input?.initialBuckets[0]?.remaining, 750_500_000);
  assert.equal(result.input?.initialBuckets[0]?.granted, 1_600_000_000);
  assert.equal(result.input?.configuration.monthly?.nextResetAt, "2026-09-30T16:00:00.000Z");
  assert.equal(result.input?.configuration.monthly?.amount, 1_600_000_000);
  assert.equal("rates" in (result.input?.configuration ?? {}), false);
  assert.equal(JSON.stringify(result.input).includes("PerMillion"), false);
});
test("preset setup rejects missing or excessive remaining, missing tier and invalid reset", () => {
  for (const patch of [{ remaining: null }, { remaining: -1 }, { remaining: 1601 }, { presetId: "missing" }, { reset: "invalid" }]) {
    assert.equal(buildCreditSetup({ ...creditSetupDraft(presets, now), ...patch }, presets, now).valid, false);
  }
  assert.equal(buildCreditSetup({ ...creditSetupDraft(presets, now), remaining: 0 }, presets, now).valid, true);
});
test("generic credits retain a separate monthly capacity and do not emit token rates", () => {
  const draft = { ...creditSetupDraft([], now), enabled: true, remaining: 40, monthly: true, monthlyAmount: 100 };
  const result = buildCreditSetup(draft, [], now);
  assert.equal(result.valid, true);
  assert.equal(result.input?.initialBuckets[0]?.granted, 100);
  assert.equal(result.input?.initialBuckets[0]?.remaining, 40);
  assert.equal(result.input?.configuration.monthly?.amount, 100);
  assert.equal("rates" in (result.input?.configuration ?? {}), false);
  assert.equal("creditsPerCurrency" in (result.input?.configuration ?? {}), false);
  assert.equal(JSON.stringify(result.input).includes("PerMillion"), false);
  assert.equal(buildCreditSetup({ ...draft, monthlyAmount: 20 }, [], now).valid, false);
  assert.equal(buildCreditSetup({ ...draft, remaining: null }, [], now).valid, false);
});
