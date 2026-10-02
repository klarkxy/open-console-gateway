import type { MessageKey } from "../i18n/index.ts";

/** Remaining-balance label. Observation time is formatted separately. */
export const PAY_GO_METER_LABEL_KEYS = {
  remaining: "余额",
} as const satisfies Record<"remaining", MessageKey>;

export const PAY_GO_METER_EMPTY = "—";

/** Compact observation time for a balance or site quota figure. */
export function formatPayGoObservedAt(value: string | number, locale: string): string {
  const ms = typeof value === "number" ? value * 1000 : Date.parse(value);
  if (!Number.isFinite(ms) || ms <= 0) return "";
  return new Intl.DateTimeFormat(locale, {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(ms));
}
