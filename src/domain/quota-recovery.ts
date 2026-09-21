import type {
  Destination,
  DestinationCredential,
  QuotaRecovery,
  QuotaRecoveryReason,
  QuotaRecoveryStatus,
  QuotaRecoveryWindow,
} from "../api/destinations.ts";
import type { MessageKey } from "../i18n/index.ts";
import { destinationCapabilities } from "./account-capabilities.ts";
import {
  cooldownRemainingUntil,
  type CooldownRemaining,
} from "./account-display.ts";

export type { QuotaRecovery, QuotaRecoveryReason, QuotaRecoveryStatus, QuotaRecoveryWindow };

export type RouteAvailableCredential = Pick<
  DestinationCredential,
  | "enabled"
  | "auth_state"
  | "quota_recovery"
  | "has_secret"
  | "onboarding_task"
  | "scope"
  | "cooldowns"
>;

export type RouteAvailableDestination = Pick<
  Destination,
  "enabled" | "auth_scheme" | "adapter" | "capabilities" | "max_credentials" | "plan"
>;

/** Backend `status` is the presentation source; `next_retry_at` is only a time label. */
export type QuotaRecoveryPresentation =
  | { kind: "waiting"; reason: QuotaRecoveryReason; window: QuotaRecoveryWindow; wait: CooldownRemaining | null }
  | { kind: "ready"; reason: QuotaRecoveryReason }
  | { kind: "probing"; reason: QuotaRecoveryReason };

export type CardQuotaAvailability =
  | "available"
  | "quota_exhausted"
  | "no_available_keys"
  | "no_keys";

export const QUOTA_RECOVERY_STATUS_KEYS = {
  waiting: "额度耗尽",
  ready: "等待请求验证",
  probing: "验证中",
} as const satisfies Record<QuotaRecoveryStatus, MessageKey>;

export const QUOTA_RECOVERY_REASON_KEYS = {
  quota_exhausted: "额度耗尽",
  insufficient_balance: "余额不足",
} as const satisfies Record<QuotaRecoveryReason, MessageKey>;

export const QUOTA_RECOVERY_WINDOW_KEYS = {
  five_hours: "5 小时",
  week: "本周",
  month: "本月",
} as const satisfies Record<Exclude<QuotaRecoveryWindow, "unknown">, MessageKey>;

export const CARD_QUOTA_AVAILABILITY_KEYS = {
  quota_exhausted: "额度耗尽",
  no_available_keys: "无可用 Key",
  no_keys: "未添加 Key",
} as const satisfies Record<Exclude<CardQuotaAvailability, "available">, MessageKey>;

export function credentialHasQuotaRecovery(
  credential: Pick<RouteAvailableCredential, "quota_recovery">,
): boolean {
  return credential.quota_recovery != null;
}

function cooldownActive(until: string | null, now: number): boolean {
  return until !== null && Date.parse(until) > now;
}

/**
 * Channel-relevant ordinary cooldown for this Key. Matches runtime
 * `cooldown_ends_at_for`: Go uses generic+5h+week+month and ignores free;
 * Zen/Free uses generic+free and ignores Go windows. Not quota-recovery evidence.
 */
export function credentialHasActiveCooldown(
  credential: Pick<RouteAvailableCredential, "cooldowns">,
  destination: RouteAvailableDestination,
  now: number,
): boolean {
  const cooldowns = credential.cooldowns;
  if (destinationCapabilities(destination).freeCooldownOnly) {
    return cooldownActive(cooldowns.generic_until, now)
      || cooldownActive(cooldowns.free_until, now);
  }
  return cooldownActive(cooldowns.generic_until, now)
    || cooldownActive(cooldowns.five_hour_until, now)
    || cooldownActive(cooldowns.week_until, now)
    || cooldownActive(cooldowns.month_until, now);
}

function scopeAllowsRouting(credential: Pick<RouteAvailableCredential, "scope">): boolean {
  return credential.scope.kind !== "only" || credential.scope.models.length > 0;
}

function onboardingReady(
  credential: Pick<RouteAvailableCredential, "onboarding_task">,
): boolean {
  return credential.onboarding_task?.state !== "in_progress";
}

function secretSatisfied(
  credential: Pick<RouteAvailableCredential, "has_secret">,
  destination: Pick<RouteAvailableDestination, "auth_scheme" | "adapter">,
): boolean {
  // CPA keeps the Inference Key on the integration, not the pool credential.
  if (destination.adapter === "cpa") return true;
  return destination.auth_scheme === "none" || credential.has_secret;
}

/**
 * Local routing usability for one Key. Confirmed quota recovery stays a
 * separate overlay; ordinary cooldown can make the Key unusable without
 * becoming exhaustion. Unknown auth is allowed; missing usage or expiry is
 * not consulted.
 */
export function credentialIsRouteAvailable(
  credential: RouteAvailableCredential,
  destination: RouteAvailableDestination,
  now: number,
): boolean {
  return destination.enabled
    && credential.enabled
    && credential.auth_state !== "invalid"
    && !credentialHasQuotaRecovery(credential)
    && secretSatisfied(credential, destination)
    && onboardingReady(credential)
    && scopeAllowsRouting(credential)
    && !credentialHasActiveCooldown(credential, destination, now);
}

/**
 * Accounts enablement lives on the V3 account switch. Overlay it onto the
 * V4 credential so row/card gray follows the switch immediately, even when
 * the destination projection has not reloaded yet.
 */
export function withAccountEnablement<T extends RouteAvailableCredential>(
  credential: T,
  accountEnabled: boolean | null | undefined,
): T {
  if (typeof accountEnabled !== "boolean") return credential;
  return { ...credential, enabled: accountEnabled };
}

export function quotaRetryRequestNeeded(
  recovery: QuotaRecovery | null | undefined,
): boolean {
  return recovery?.status === "waiting";
}

export function quotaRecoveryPresentation(
  recovery: QuotaRecovery | null | undefined,
  now: number,
): QuotaRecoveryPresentation | null {
  if (!recovery) return null;
  if (recovery.status === "ready") return { kind: "ready", reason: recovery.reason };
  if (recovery.status === "probing") return { kind: "probing", reason: recovery.reason };
  const remaining = cooldownRemainingUntil(recovery.next_retry_at, now);
  const wait = remaining && !(remaining.unit === "seconds" && remaining.seconds === 0)
    ? remaining
    : null;
  return { kind: "waiting", reason: recovery.reason, window: recovery.window, wait };
}

/**
 * Availability for one routing card's saved membership. Callers must pass
 * the unfiltered card rows; a status-filtered display subset can hide a
 * still-usable sibling. Separate cards of the same destination stay
 * independent; quota pools do not fan out.
 */
export function cardQuotaAvailability(
  membership: readonly RouteAvailableCredential[],
  destination: RouteAvailableDestination,
  now: number,
): CardQuotaAvailability {
  if (membership.length === 0) return "no_keys";
  if (membership.every(credentialHasQuotaRecovery)) return "quota_exhausted";
  if (membership.some((credential) => credentialIsRouteAvailable(credential, destination, now))) {
    return "available";
  }
  return "no_available_keys";
}
