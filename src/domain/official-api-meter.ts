import type { OfficialApiStatus } from "../api/generated/dashboard-v4.ts";
import type { MessageKey } from "../i18n/index.ts";

/** Why the remaining-balance figure has no amount. */
export type OfficialApiMeterEmpty = NonNullable<OfficialApiStatus["meter"]["remainingEmpty"]>;

export const OFFICIAL_API_METER_EMPTY_KEYS = {
  unavailable: "无余额接口",
  not_queried: "尚未刷新",
} as const satisfies Record<OfficialApiMeterEmpty, MessageKey>;

export type OfficialApiMeterRemaining = OfficialApiStatus["meter"]["remaining"][number];
export type OfficialApiAccountMeter = OfficialApiStatus["meter"];

export function officialApiAccountMeter(
  status: Pick<OfficialApiStatus, "meter">,
): OfficialApiAccountMeter {
  return status.meter;
}
