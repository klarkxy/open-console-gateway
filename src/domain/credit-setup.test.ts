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
});
test("preset setup rejects missing or excessive remaining, missing tier and invalid reset", () => {
  for (const patch of [{ remaining: null }, { remaining: -1 }, { remaining: 1601 }, { presetId: "missing" }, { reset: "invalid" }]) {
    assert.equal(buildCreditSetup({ ...creditSetupDraft(presets, now), ...patch }, presets, now).valid, false);
  }
  assert.equal(buildCreditSetup({ ...creditSetupDraft(presets, now), remaining: 0 }, presets, now).valid, true);
});
test("generic credits retain a separate monthly capacity and reject invalid pricing", () => {
  const draft = { ...creditSetupDraft([], now), enabled: true, remaining: 40, monthly: true, monthlyAmount: 100 };
  const result = buildCreditSetup(draft, [], now);
  assert.equal(result.input?.initialBuckets[0]?.granted, 100);
  assert.equal(result.input?.initialBuckets[0]?.remaining, 40);
  assert.equal(buildCreditSetup({ ...draft, factor: 0 }, [], now).valid, false);
  assert.equal(buildCreditSetup({ ...draft, monthlyAmount: 20 }, [], now).valid, false);
  const rate = { model: "m", inputPerMillion: 1, outputPerMillion: 2, cacheReadPerMillion: null, cacheWritePerMillion: null };
  assert.equal(buildCreditSetup({ ...draft, rates: [rate, rate] }, [], now).valid, false);
  assert.equal(buildCreditSetup({ ...draft, rates: [{ ...rate, inputPerMillion: NaN }] }, [], now).valid, false);
});
