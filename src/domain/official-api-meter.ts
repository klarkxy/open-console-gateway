import type { OfficialApiStatus, OfficialBalance } from "../api/generated/dashboard-v4.ts";
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
}

function finiteAmount(value: number): boolean {
  return Number.isFinite(value);
}

export function officialApiAccountMeter(
  status: Pick<OfficialApiStatus, "balanceAvailable" | "balances">,
): OfficialApiAccountMeter {
  if (!status.balanceAvailable) {
    return { remainingEmpty: "unavailable", remaining: [] };
  }
  const remaining = status.balances.flatMap((row) => remainingFromBalance(row) ?? []);
  return {
    remainingEmpty: remaining.length === 0 ? "not_queried" : null,
    remaining,
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
