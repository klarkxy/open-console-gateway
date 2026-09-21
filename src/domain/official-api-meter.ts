import type { OfficialApiStatus, OfficialBalance, OfficialSpend } from "../api/generated/dashboard-v4.ts";
import type { MessageKey } from "../i18n/index.ts";

/** Why the remaining-balance figure has no amount. */
export type OfficialApiMeterEmpty = "unavailable" | "not_queried";

export const OFFICIAL_API_METER_EMPTY_KEYS = {
  unavailable: "无余额接口",
  not_queried: "尚未刷新",
} as const satisfies Record<OfficialApiMeterEmpty, MessageKey>;

export interface OfficialApiMeterRemaining {
  currency: string;
  total: number;
  gift: number | null;
  observedAt: string;
}

export interface OfficialApiAccountMeter {
  remainingEmpty: OfficialApiMeterEmpty | null;
  remaining: OfficialApiMeterRemaining[];
  monthSpend: OfficialSpend[];
  lifetimeSpend: OfficialSpend[];
  unpriced: number;
}

function finiteAmount(value: number): boolean {
  return Number.isFinite(value);
}

export function officialApiAccountMeter(
  status: Pick<
    OfficialApiStatus,
    "balanceAvailable" | "balances" | "monthSpend" | "lifetimeSpend" | "unpricedRequests"
  >,
): OfficialApiAccountMeter {
  const monthSpend = status.monthSpend.filter((row) => finiteAmount(row.amount));
  const lifetimeSpend = (status.lifetimeSpend ?? []).filter((row) => finiteAmount(row.amount));
  const unpriced = Math.max(0, status.unpricedRequests);
  if (!status.balanceAvailable) {
    return {
      remainingEmpty: "unavailable",
      remaining: [],
      monthSpend,
      lifetimeSpend,
      unpriced,
    };
  }
  const remaining = status.balances.flatMap((row) => remainingFromBalance(row) ?? []);
  return {
    remainingEmpty: remaining.length === 0 ? "not_queried" : null,
    remaining,
    monthSpend,
    lifetimeSpend,
    unpriced,
  };
}

function remainingFromBalance(row: OfficialBalance): OfficialApiMeterRemaining | null {
  if (!finiteAmount(row.total)) return null;
  const gift = finiteAmount(row.granted) && row.granted > 0 ? row.granted : null;
  return {
    currency: row.currency,
    total: row.total,
    gift,
    observedAt: row.observedAt,
  };
}

export function joinOfficialApiSpend(
  rows: readonly OfficialSpend[],
  formatAmount: (value: number, currency: string) => string,
): string | null {
  const parts = rows
    .filter((row) => finiteAmount(row.amount))
    .map((row) => formatAmount(row.amount, row.currency));
  return parts.length > 0 ? parts.join(" · ") : null;
}
