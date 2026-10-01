/**
 * Stored native-currency amount already recorded on a forward log.
 *
 * The figure is historical storage. A missing, zero, or negative amount stays
 * hidden, and the value is never summed across currencies. Official rows can
 * carry the same columns, so `forwardLogNativeEstimate` is the display gate.
 * Pure helpers only; no i18n runtime import.
 */

import { numberFormatter } from "../utils/intl-cache.ts";

export interface NativeCostEstimate {
  /** Positive finite stored amount in the original currency. */
  value: number;
  currency: string | null;
  unit: string | null;
}

export interface NativeCostLogRow {
  native_cost_value: number | null;
  native_cost_currency: string | null;
  native_cost_unit: string | null;
  provider_id: string | null;
  pricing_revision_id: string | null;
}

/**
 * Display gate for a stored native amount. A pricing revision string does not
 * decide visibility. Only a Custom-provider row with a positive finite stored
 * amount is shown; missing, zero, and negative amounts stay hidden.
 */
export function forwardLogNativeEstimate(row: NativeCostLogRow): NativeCostEstimate | null {
  const value = row.native_cost_value;
  if (row.provider_id !== "custom") return null;
  if (value === null || !Number.isFinite(value) || value <= 0) return null;
  return {
    value,
    currency: row.native_cost_currency || null,
    unit: row.native_cost_unit || null,
  };
}

/**
 * Formats a stored native amount. Non-ISO currency labels fall back to a plain
 * suffix; a missing currency renders the bare number. The unit is appended
 * only when it differs from the currency label.
 */
export function formatNativeCostEstimate(estimate: NativeCostEstimate, locale: string): string {
  const { value, currency, unit } = estimate;
  let amount: string;
  if (currency) {
    try {
      amount = numberFormatter(locale, {
        style: "currency",
        currency,
        currencyDisplay: "narrowSymbol",
        maximumSignificantDigits: 6,
      }).format(value);
    } catch {
      amount = `${numberFormatter(locale, { maximumSignificantDigits: 6 }).format(value)} ${currency}`;
    }
  } else {
    amount = numberFormatter(locale, { maximumSignificantDigits: 6 }).format(value);
  }
  return unit && unit !== currency ? `${amount} ${unit}` : amount;
}
