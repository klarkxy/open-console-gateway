import { t } from "../i18n/index.ts";
import type { MessageKey } from "../i18n/index.ts";
import {
  ACCOUNT_MENU_LABEL_KEYS,
  ACCOUNT_TYPE_LABEL_KEYS,
  ROUTING_DRAFT_LABEL_KEYS,
} from "../domain/account-display.ts";
import type {
  AccountExpiry,
  AccountStatus,
  AccountTypeLabel,
  CooldownDetailSegment,
  CooldownRemaining,
  UsageSyncStatus,
} from "../domain/account-display.ts";
import type { ModelRestriction, QuotaShare } from "../domain/account-identity.ts";
import {
  PLATFORM_CREDENTIAL_TAG_KEYS,
  type PlatformCredentialTag,
} from "../domain/platform-accounts.ts";
import {
  CARD_QUOTA_AVAILABILITY_KEYS,
  QUOTA_RECOVERY_REASON_KEYS,
  QUOTA_RECOVERY_STATUS_KEYS,
  QUOTA_RECOVERY_WINDOW_KEYS,
  type CardQuotaAvailability,
  type QuotaRecoveryPresentation,
} from "../domain/quota-recovery.ts";
import { CPA_CARD_STATUS_KEYS, type CpaCardStatus } from "../domain/cpa-runtime.ts";

/**
 * Display composition for the account domain codes: every t() mapping for
 * account cards lives here, keeping src/domain free of localized copy.
 * Compound strings (e.g. disabled-unavailable) join t() results here too.
 */

export function formatCooldownRemainingText(remaining: CooldownRemaining | null): string {
  if (!remaining) return "";
  switch (remaining.unit) {
    case "seconds":
      return t("{seconds}秒", { seconds: remaining.seconds });
    case "minutes":
      return t("{minutes}分钟", { minutes: remaining.minutes });
    case "hours-minutes":
      return t("{hours}小时{minutes}分钟", { hours: remaining.hours, minutes: remaining.minutes });
    case "days-hours":
      return t("{days}天{hours}小时", { days: remaining.days, hours: remaining.hours });
  }
}

export function accountStatusText(status: AccountStatus): string {
  switch (status.kind) {
    case "enabled":
      return t("已启用");
    case "disabled":
      return t("已禁用");
    case "registering":
      return t("注册中");
    case "unavailable":
      return t("不可用");
    case "disabled-unavailable":
      return `${t("已禁用")} · ${t("不可用")}`;
    case "cooling":
      return t("冷却中·剩 {time}", { time: formatCooldownRemainingText(status.remaining) });
    case "draft":
      return t(ROUTING_DRAFT_LABEL_KEYS[status.state]);
  }
}

export function accountExpiryText(expiry: AccountExpiry): string {
  switch (expiry.kind) {
    case "unset":
      return t("未设置");
    case "today":
      return t("今天到期");
    case "remaining":
      return expiry.days === 1 ? t("剩 1 天") : t("剩 {days} 天", { days: expiry.days });
    case "expired":
      return expiry.days === 1 ? t("已到期 1 天") : t("已到期 {days} 天", { days: expiry.days });
  }
}

export function cooldownDetailsText(segments: readonly CooldownDetailSegment[]): string {
  return segments
    .map((segment) => {
      switch (segment.kind) {
        case "generic":
          return t("冷却中");
        case "free":
          return t("Free");
        case "window":
          return segment.label;
      }
    })
    .join(" · ");
}

export function usageSyncCaptionText(status: UsageSyncStatus): string {
  return status.kind === "never"
    ? t("尚未官方同步")
    : t("上次官方同步：{time}", { time: status.time });
}

/** Menu label key for a domain-owned option key; null for foreign options. */
export function accountMenuLabelKey(key: string | number): MessageKey | null {
  return (ACCOUNT_MENU_LABEL_KEYS as Record<string, MessageKey>)[String(key)] ?? null;
}

export function accountTypeLabelText(label: AccountTypeLabel): string {
  return label.kind === "plan" ? label.label : t(ACCOUNT_TYPE_LABEL_KEYS[label.kind]);
}

export function credentialCountText(count: number): string {
  return t("{count} 个凭据", { count });
}

export function quotaShareText(share: QuotaShare): string {
  return share.kind === "named"
    ? t("与 {name} 共享额度", { name: share.name })
    : t("与 {count} 个 Key 共享额度", { count: share.count });
}

export function modelRestrictionText(restriction: ModelRestriction): string {
  switch (restriction.kind) {
    case "restricted":
      return t("已限制模型");
    case "single":
      return t("仅 {model}", { model: restriction.model });
    case "count":
      return t("仅 {count} 个模型", { count: restriction.count });
  }
}

export function quotaRecoveryText(state: QuotaRecoveryPresentation): string {
  const balance = state.reason === "insufficient_balance"
    ? t(QUOTA_RECOVERY_REASON_KEYS.insufficient_balance)
    : null;
  if (state.kind === "ready" || state.kind === "probing") {
    return [t(QUOTA_RECOVERY_STATUS_KEYS[state.kind]), balance].filter(Boolean).join(" · ");
  }
  const cause = balance ?? t(QUOTA_RECOVERY_STATUS_KEYS.waiting);
  const window = state.window === "unknown" ? null : t(QUOTA_RECOVERY_WINDOW_KEYS[state.window]);
  const time = formatCooldownRemainingText(state.wait);
  return [cause, window, time].filter(Boolean).join(" · ");
}

export function cardQuotaAvailabilityText(kind: Exclude<CardQuotaAvailability, "available">): string {
  return t(CARD_QUOTA_AVAILABILITY_KEYS[kind]);
}

export function cpaCardStatusText(status: CpaCardStatus): string {
  return t(CPA_CARD_STATUS_KEYS[status]);
}

export function platformCredentialTagText(tag: PlatformCredentialTag): string {
  switch (tag.kind) {
    case "group":
      return t(PLATFORM_CREDENTIAL_TAG_KEYS.group, { name: tag.name });
    case "token":
      return t(PLATFORM_CREDENTIAL_TAG_KEYS.token, { name: tag.name });
    case "models":
      return t(PLATFORM_CREDENTIAL_TAG_KEYS.models, { count: tag.count });
  }
}
