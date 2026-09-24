import type { CreditBucket, CreditConfiguration, CreditPreset, CreditRate } from "../api/billing.ts";
import {
  CHINA_OFFSET_MINUTES, buildInitialCreditConfigure, configurationFromPreset,
  creditDisplayFactor, creditsToScaled, fromDatetimeLocalValue, initialMonthlyBucket,
  nextCalendarMonthStart, parseCreditAmount, toDatetimeLocalValue,
} from "./billing.ts";

export type CreditSetupInput = { configuration: CreditConfiguration; initialBuckets: CreditBucket[] };
export interface CreditSetupDraft {
  enabled: boolean;
  presetId: string | null;
  remaining: number | null;
  reset: string;
  name: string;
  currency: string;
  factor: number | null;
  monthly: boolean;
  monthlyAmount: number | null;
  rates: CreditRate[];
}

export function creditSetupDraft(presets: readonly CreditPreset[], now: number): CreditSetupDraft {
  const preset = presets[0];
  return {
    enabled: Boolean(preset), presetId: preset?.id ?? null,
    remaining: preset ? creditsToScaled(preset.initialGrant, creditDisplayFactor(presets.length)) : null,
    reset: toDatetimeLocalValue(nextCalendarMonthStart(now, CHINA_OFFSET_MINUTES).toISOString(), CHINA_OFFSET_MINUTES),
    name: "", currency: "CNY", factor: 1, monthly: false, monthlyAmount: null, rates: [],
  };
}

export function buildCreditSetup(draft: CreditSetupDraft, presets: readonly CreditPreset[], now: number):
  { input: CreditSetupInput | null; valid: boolean } {
  if (!draft.enabled) return { input: null, valid: true };
  const scale = creditDisplayFactor(presets.length);
  const remaining = parseCreditAmount(draft.remaining, scale);
  if ("issue" in remaining) return { input: null, valid: false };
  const reset = fromDatetimeLocalValue(draft.reset, CHINA_OFFSET_MINUTES);
  if (presets.length) {
    const preset = presets.find(row => row.id === draft.presetId);
    if (!preset || !reset || remaining.amount > preset.initialGrant) return { input: null, valid: false };
    const configuration = configurationFromPreset(preset, reset);
    return { input: { configuration, initialBuckets: [initialMonthlyBucket(configuration, remaining.amount, new Date(now).toISOString())] }, valid: true };
  }
  if (!draft.factor || !Number.isFinite(draft.factor) || draft.factor <= 0) return { input: null, valid: false };
  const monthly = draft.monthly ? parseCreditAmount(draft.monthlyAmount, scale) : { amount: null };
  if ("issue" in monthly || (monthly.amount !== null && remaining.amount > monthly.amount)) return { input: null, valid: false };
  const rates = draft.rates.map(rate => ({ ...rate, model: rate.model.trim() }));
  if (rates.some(rate => !rate.model || rate.inputPerMillion == null || rate.outputPerMillion == null || [rate.inputPerMillion, rate.outputPerMillion, rate.cacheReadPerMillion, rate.cacheWritePerMillion]
    .some(value => value !== null && (!Number.isFinite(value) || value < 0)))
    || new Set(rates.map(rate => rate.model)).size !== rates.length) return { input: null, valid: false };
  const built = buildInitialCreditConfigure({
    name: draft.name.trim() || "credits", currency: draft.currency.trim() || "CNY", creditsPerCurrency: draft.factor,
    rates, remaining: remaining.amount, monthlyEnabled: draft.monthly,
    monthlyAmount: monthly.amount, nextResetAt: reset, timezoneOffsetMinutes: CHINA_OFFSET_MINUTES,
    sourceUrl: null, startsAt: new Date(now).toISOString(),
  });
  return "issue" in built ? { input: null, valid: false } : { input: built, valid: true };
}
