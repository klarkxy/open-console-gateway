import type { Account } from "../api/dashboard.ts";
import type { DestinationCredential } from "../api/destinations.ts";
import type { ProviderQuotaWindow } from "../api/providers.ts";
import { quotaRetryRequestNeeded } from "./quota-recovery.ts";

/**
 * Deadline scheduling for the Accounts view clock. The view passes `now` to
 * every card and Key row; a tick only matters when some `now`-driven display
 * changes at a known moment. This module finds that moment so the view can
 * idle at a coarse fallback instead of forcing a full-list re-render on a
 * fixed interval.
 *
 * Included (states the render layer actually derives from `now`):
 * - account cooldown fields (`isCooling` / `cooldownDetails` in
 *   account-display.ts, window progress in accounts-usage.ts);
 * - credential cooldown stamps, conservatively all five windows — a
 *   destination consults a subset via `credentialHasActiveCooldown`, and the
 *   unused ones are null, so the superset only ever ticks earlier, never late;
 * - `quota_recovery.next_retry_at` while the recovery is waiting
 *   (`quotaRecoveryPresentation` shows the retry countdown only then);
 * - usage quota window `resets_at` (the "{time}后重置" caption).
 *
 * Day-granularity states (account expiry, purchase-date comparisons) flip at
 * midnight and are covered by the idle fallback; they stay out so a
 * far-future date cannot pin the clock at the fast tick.
 */

/** Fastest tick while any time-sensitive state exists; matches the old fixed clock. */
export const ACCOUNTS_TICK_MAX_MS = 15_000;

/**
 * Shortest gap between ticks. Countdown captions render at second
 * granularity, so sub-second ticks only re-render without visible change;
 * dense cooldown windows would otherwise tick several times per second.
 */
export const ACCOUNTS_TICK_MIN_MS = 1_000;

/** Idle fallback when nothing is time-sensitive; bounds date-flip staleness. */
export const ACCOUNTS_TICK_IDLE_MS = 5 * 60_000;

export type TickAccount = Pick<
  Account,
  | "cooldown_until"
  | "cooldown_generic_until"
  | "cooldown_5h_until"
  | "cooldown_week_until"
  | "cooldown_month_until"
  | "cooldown_free_until"
>;

export type TickCredential = Pick<DestinationCredential, "cooldowns" | "quota_recovery">;

export type TickQuotaWindow = Pick<ProviderQuotaWindow, "resets_at">;

const ACCOUNT_COOLDOWN_FIELDS = [
  "cooldown_until",
  "cooldown_generic_until",
  "cooldown_5h_until",
  "cooldown_week_until",
  "cooldown_month_until",
  "cooldown_free_until",
] as const;

const CREDENTIAL_COOLDOWN_FIELDS = [
  "generic_until",
  "five_hour_until",
  "week_until",
  "month_until",
  "free_until",
] as const;

function futureTimestamp(value: string | null, now: number): number | null {
  if (value === null) return null;
  const ts = Date.parse(value);
  return Number.isFinite(ts) && ts > now ? ts : null;
}

function nearer(current: number | null, candidate: number | null): number | null {
  if (candidate === null) return current;
  return current === null || candidate < current ? candidate : current;
}

/** Nearest future moment a `now`-driven display changes; null when none is pending. */
export function accountsNextDeadline(input: {
  accounts: readonly TickAccount[];
  credentials: readonly TickCredential[];
  quotaWindows: readonly TickQuotaWindow[];
  now: number;
}): number | null {
  let nearest: number | null = null;
  for (const account of input.accounts) {
    for (const field of ACCOUNT_COOLDOWN_FIELDS) {
      nearest = nearer(nearest, futureTimestamp(account[field], input.now));
    }
  }
  for (const credential of input.credentials) {
    for (const field of CREDENTIAL_COOLDOWN_FIELDS) {
      nearest = nearer(nearest, futureTimestamp(credential.cooldowns[field], input.now));
    }
    const recovery = credential.quota_recovery;
    if (recovery && quotaRetryRequestNeeded(recovery)) {
      nearest = nearer(nearest, futureTimestamp(recovery.next_retry_at, input.now));
    }
  }
  for (const window of input.quotaWindows) {
    nearest = nearer(nearest, futureTimestamp(window.resets_at, input.now));
  }
  return nearest;
}

/**
 * Milliseconds until the next clock tick for a resolved deadline. The
 * deadline is rounded up to the next whole second so deadlines packed into
 * one second coalesce into a single tick, and the gap never drops below
 * ACCOUNTS_TICK_MIN_MS — sub-second ticks cannot change any caption, which
 * renders at second granularity.
 */
export function accountsTickDelay(deadline: number | null, at: number): number {
  if (deadline === null) return ACCOUNTS_TICK_IDLE_MS;
  const quantized = Math.ceil(deadline / 1_000) * 1_000;
  return Math.min(ACCOUNTS_TICK_MAX_MS, Math.max(ACCOUNTS_TICK_MIN_MS, quantized - at));
}
