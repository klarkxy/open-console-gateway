/**
 * Native (original-currency) platform cost estimates on forward logs.
 *
 * Platform-linked Keys get a token estimate in the platform's own currency
 * from the frozen platform price; official providers may also populate
 * `native_cost_*` from their USD cost fields, so presence alone does not mark
 * a platform estimate (see `forwardLogNativeEstimate` for the gate). The
 * figure is an estimate — never a quota debit and never an observed wallet
 * charge — so it renders separately from the USD/quota columns and is never
 * summed across currencies. Pure helpers only; no i18n runtime import.
 */

export interface NativeCostEstimate {
  /** Finite amount in the platform's original currency; 0 is a real estimate, distinct from null upstream. */
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
 * Display gate: official Go/GOAT/Ollama rows also carry `native_cost_*` (from
 * `usd_fields_from_cost`) and official priced rows have a nonempty
 * `pricing_revision_id`, so a revision string is NOT platform-lineage proof.
 * The estimate renders only for Custom-provider Keys with a finite,
 * nonnegative native amount — exactly the rows the platform attribution
 * writes. Anything else hides the estimate entirely.
 */
export function forwardLogNativeEstimate(row: NativeCostLogRow): NativeCostEstimate | null {
  const value = row.native_cost_value;
  if (row.provider_id !== "custom") return null;
  if (value === null || !Number.isFinite(value) || value < 0) return null;
  return {
    value,
    currency: row.native_cost_currency || null,
    unit: row.native_cost_unit || null,
  };
}

/**
 * Original-currency amount with significant-digit precision so tiny per-token
 * totals stay inspectable. Non-ISO currency labels fall back to a plain
 * suffix; a missing currency renders the bare number. The unit is appended
 * only when it differs from the currency label (the backend currently mirrors
 * currency into unit).
 */
export function formatNativeCostEstimate(estimate: NativeCostEstimate, locale: string): string {
  const { value, currency, unit } = estimate;
  let amount: string;
  if (currency) {
    try {
      amount = new Intl.NumberFormat(locale, {
        style: "currency",
        currency,
        currencyDisplay: "narrowSymbol",
        maximumSignificantDigits: 6,
      }).format(value);
    } catch {
      amount = `${new Intl.NumberFormat(locale, { maximumSignificantDigits: 6 }).format(value)} ${currency}`;
    }
  } else {
    amount = new Intl.NumberFormat(locale, { maximumSignificantDigits: 6 }).format(value);
  }
  return unit && unit !== currency ? `${amount} ${unit}` : amount;
}
