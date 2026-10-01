import type { CreditBucket, CreditConfigurationWrite, CreditPreset } from "../api/billing.ts";
import {
  CHINA_OFFSET_MINUTES, buildInitialCreditConfigure, configurationFromPreset,
  creditDisplayFactor, creditsToScaled, fromDatetimeLocalValue, initialMonthlyBucket,
  nextCalendarMonthStart, parseCreditAmount, toDatetimeLocalValue,
} from "./billing.ts";

export type CreditSetupInput = { configuration: CreditConfigurationWrite; initialBuckets: CreditBucket[] };
export interface CreditSetupDraft {
  enabled: boolean;
  presetId: string | null;
  remaining: number | null;
  reset: string;
  name: string;
  currency: string;
  monthly: boolean;
  monthlyAmount: number | null;
}

export function creditSetupDraft(presets: readonly CreditPreset[], now: number): CreditSetupDraft {
  const preset = presets[0];
  return {
    enabled: Boolean(preset), presetId: preset?.id ?? null,
    remaining: preset ? creditsToScaled(preset.initialGrant, creditDisplayFactor(presets.length)) : null,
    reset: toDatetimeLocalValue(nextCalendarMonthStart(now, CHINA_OFFSET_MINUTES).toISOString(), CHINA_OFFSET_MINUTES),
    name: "", currency: "CNY", monthly: false, monthlyAmount: null,
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
  const monthly = draft.monthly ? parseCreditAmount(draft.monthlyAmount, scale) : { amount: null };
  if ("issue" in monthly || (monthly.amount !== null && remaining.amount > monthly.amount)) return { input: null, valid: false };
  const built = buildInitialCreditConfigure({
    name: draft.name.trim() || "credits", currency: draft.currency.trim() || "CNY",
    remaining: remaining.amount, monthlyEnabled: draft.monthly,
    monthlyAmount: monthly.amount, nextResetAt: reset, timezoneOffsetMinutes: CHINA_OFFSET_MINUTES,
    sourceUrl: null, startsAt: new Date(now).toISOString(),
  });
  return "issue" in built ? { input: null, valid: false } : { input: built, valid: true };
}
