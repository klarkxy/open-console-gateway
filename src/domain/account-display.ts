import type { AccountCapabilitySource } from "./account-capabilities.ts";
import type { Account, AccountSetupStep } from "../api/dashboard";
import type { Destination } from "../api/destinations.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { isCooling, isFreeCooling, isWindowCooling } from "./accounts-usage.ts";
import type { UsageKey } from "./accounts-usage.ts";
import { daysUntilDate, expiryTagType } from "./account-lifecycle.ts";
import type { ExpiryTagType } from "./account-lifecycle.ts";
import { accountCapabilities, destinationCapabilities } from "./account-capabilities.ts";
import { planLabel } from "./plans.ts";
import type { MessageKey } from "../i18n/index.ts";

/**
 * Pure presentational helpers for the account list: status/expiry tags,
 * cooldown summaries, managed-registration steps, the quota-sync caption,
 * and the per-card overflow menu. Domain functions return semantic codes and
 * raw data only — never MessageKey or t() output. The view layer maps codes
 * through the exported `*_KEYS` tables (see src/views/account-status-text.ts).
 * Everything takes `now` explicitly so the view's deadline-driven clock
 * stays the single re-render driver.
 */

export type AccountStatusTagType = "success" | "warning" | "error" | "default";

export type AccountMenuOption = {
  key: string | number;
  label?: string;
  accountId: string;
  accountName: string;
  disabled?: boolean;
};

/** Menu labels by option key; the view renders t(ACCOUNT_MENU_LABEL_KEYS[key]). */
export const ACCOUNT_MENU_LABEL_KEYS = {
  "open-cpa": "前往 CPA",
  "continue-setup": "继续注册",
  "reset-profile": "重置官网登录状态",
  "open-console": "打开 OpenCode 官网",
  "open-site": "打开 Ollama 官网",
  edit: "编辑账号",
  reset: "重置冷却",
  delete: "删除账号",
  "move-up": "上移",
  "move-down": "下移",
  "move-to-card": "移到卡片",
  "fetch-models": "获取模型",
  "edit-key": "编辑",
  unlink: "取消关联",
} as const satisfies Record<string, MessageKey>;

/** Neutral type-tag code, or the catalog plan label when the card is a billed family. */
export type AccountTypeLabel =
  | { kind: "cpa" }
  | { kind: "keyless" }
  | { kind: "plan"; label: string };

export const ACCOUNT_TYPE_LABEL_KEYS = {
  cpa: "CPA 订阅池",
  keyless: "免费通道",
} as const satisfies Record<"cpa" | "keyless", MessageKey>;

export function accountTypeLabel(
  account: Pick<Account, "id" | "provider_id" | "account_type">,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
  destination?: AccountCapabilitySource | null,
): AccountTypeLabel {
  const caps = accountCapabilities(account, catalog, destination);
  if (caps.externalIntegration) return { kind: "cpa" };
  if (caps.keylessSingleton) return { kind: "keyless" };
  return { kind: "plan", label: planLabel(account, catalog) };
}

export function destinationTypeLabel(
  destination: Pick<
    Destination,
    | "account_controls"
    | "adapter"
    | "auth_scheme"
    | "brand_family"
    | "capabilities"
    | "max_credentials"
    | "name"
    | "plan"
  >,
): AccountTypeLabel {
  const caps = destinationCapabilities(destination);
  if (caps.externalIntegration) return { kind: "cpa" };
  if (caps.keylessSingleton) return { kind: "keyless" };
  return { kind: "plan", label: destination.brand_family ?? destination.name };
}

/**
 * Reorder actions for a credential row inside one routing card: move within
 * the card, plus a move-to-card action that opens the card picker when the
 * same supplier has (or can have) another card.
 */
export function groupMoveMenuOptions(
  account: Pick<Account, "id" | "name">,
  index: number,
  count: number,
  options: { canMoveToCard?: boolean } = {},
): AccountMenuOption[] {
  const moves: AccountMenuOption[] = [
    {
      key: "move-up",
      accountId: account.id,
      accountName: account.name,
      disabled: index <= 0,
    },
    {
      key: "move-down",
      accountId: account.id,
      accountName: account.name,
      disabled: count <= 0 || index >= count - 1,
    },
  ];
  if (options.canMoveToCard) {
    moves.push({
      key: "move-to-card",
      accountId: account.id,
      accountName: account.name,
      disabled: false,
    });
  }
  return moves;
}

export function accountIsReady(account: Pick<Account, "setup_step">): boolean {
  return account.setup_step === "ready";
}

/** Backend-owned unroutable draft state for ready accounts. */
export type RoutingDraftState = "pending" | "failed" | "unsupported";

export const ROUTING_DRAFT_LABEL_KEYS: Record<RoutingDraftState, MessageKey> = {
  pending: "待验证",
  failed: "验证失败",
  unsupported: "等待支持",
};

export const ROUTING_DRAFT_DESCRIPTION_KEYS: Record<RoutingDraftState, MessageKey> = {
  pending: "该方案暂不支持验证，创建后仍为禁用草稿。",
  failed: "验证失败，请检查 Key 或等待该方案支持验证。",
  unsupported: "该方案暂不可路由。",
};

/** The draft state a ready but unroutable account is in; null when routable. */
export function accountRoutingDraftState(
  account: Pick<Account, "setup_step" | "plan_routable" | "verification_status">,
): RoutingDraftState | null {
  if (!accountIsReady(account) || account.plan_routable) return null;
  if (account.verification_status === "pending") return "pending";
  if (account.verification_status === "failed") return "failed";
  return "unsupported";
}

/** Structured cooldown remainder; the view formats it via t() unit keys. */
export type CooldownRemaining =
  | { unit: "seconds"; seconds: number }
  | { unit: "minutes"; minutes: number }
  | { unit: "hours-minutes"; hours: number; minutes: number }
  | { unit: "days-hours"; days: number; hours: number };

export function cooldownRemainingUntil(
  until: string | null,
  now = Date.now(),
): CooldownRemaining | null {
  if (!until) return null;
  const ms = new Date(until).getTime() - now;
  if (ms <= 0) return { unit: "seconds", seconds: 0 };
  const seconds = Math.ceil(ms / 1000);
  if (seconds < 60) return { unit: "seconds", seconds };
  const min = Math.floor(ms / 60000);
  if (min < 60) return { unit: "minutes", minutes: min };
  const hr = Math.floor(min / 60);
  if (hr < 24) return { unit: "hours-minutes", hours: hr, minutes: min % 60 };
  const day = Math.floor(hr / 24);
  return { unit: "days-hours", days: day, hours: hr % 24 };
}

export function cooldownRemaining(
  account: Pick<Account, "cooldown_until">,
  now = Date.now(),
): CooldownRemaining | null {
  return cooldownRemainingUntil(account.cooldown_until, now);
}

/** The single status an account card presents right now. */
export type AccountStatus =
  | { kind: "enabled" }
  | { kind: "disabled" }
  | { kind: "registering" }
  | { kind: "unavailable" }
  | { kind: "disabled-unavailable" }
  | { kind: "cooling"; remaining: CooldownRemaining }
  | { kind: "draft"; state: RoutingDraftState };

const ZERO_REMAINING: CooldownRemaining = { unit: "seconds", seconds: 0 };

export function accountStatus(
  account: Account,
  now = Date.now(),
  catalog: readonly ProviderCatalogEntry[] | null | undefined = null,
  destination?: AccountCapabilitySource | null,
): AccountStatus {
  if (accountCapabilities(account, catalog, destination).freeCooldownOnly) {
    if (!account.enabled) return { kind: "disabled" };
    if (isFreeCooling(account, now)) {
      return {
        kind: "cooling",
        remaining: cooldownRemainingUntil(account.cooldown_free_until, now) ?? ZERO_REMAINING,
      };
    }
    return { kind: "enabled" };
  }
  if (!accountIsReady(account)) return { kind: "registering" };
  const draftState = accountRoutingDraftState(account);
  if (draftState) return { kind: "draft", state: draftState };
  if (account.auth_error) {
    return account.enabled ? { kind: "unavailable" } : { kind: "disabled-unavailable" };
  }
  if (!account.enabled) return { kind: "disabled" };
  if (isCooling(account, now)) {
    return { kind: "cooling", remaining: cooldownRemaining(account, now) ?? ZERO_REMAINING };
  }
  // Ready + enabled is a configuration state, not a verified-availability
  // claim: no connection test evidence is implied.
  return { kind: "enabled" };
}

export function accountStatusTagType(
  account: Account,
  now = Date.now(),
  catalog: readonly ProviderCatalogEntry[] | null | undefined = null,
  destination?: AccountCapabilitySource | null,
): AccountStatusTagType {
  if (accountCapabilities(account, catalog, destination).freeCooldownOnly) {
    if (!account.enabled) return "error";
    return isFreeCooling(account, now) ? "warning" : "success";
  }
  if (!accountIsReady(account)) return "warning";
  const draftState = accountRoutingDraftState(account);
  if (draftState) return draftState === "failed" ? "error" : "warning";
  if (account.auth_error) return "error";
  if (!account.enabled) return "error";
  if (isCooling(account, now)) return "warning";
  return "success";
}

function accountExpiryDays(account: Pick<Account, "expires_on">, now = Date.now()): number {
  return daysUntilDate(account.expires_on, now);
}

export function accountExpiryTagType(account: Pick<Account, "expires_on">, now = Date.now()): ExpiryTagType {
  return expiryTagType(accountExpiryDays(account, now));
}

/** Expiry state with the raw day count; singular/plural copy stays in the view. */
export type AccountExpiry =
  | { kind: "unset" }
  | { kind: "remaining"; days: number }
  | { kind: "today" }
  | { kind: "expired"; days: number };

export function accountExpiry(account: Pick<Account, "expires_on">, now = Date.now()): AccountExpiry {
  const days = accountExpiryDays(account, now);
  if (!Number.isFinite(days)) return { kind: "unset" };
  if (days > 0) return { kind: "remaining", days };
  if (days === 0) return { kind: "today" };
  return { kind: "expired", days: Math.abs(days) };
}

/**
 * One segment of the cooldown tooltip. Window labels are caller-provided
 * display text; "generic"/"free" are codes the view maps through t().
 */
export type CooldownDetailSegment =
  | { kind: "generic" }
  | { kind: "window"; label: string }
  | { kind: "free" };

export function cooldownDetails(
  account: Account,
  now: number,
  limits: Array<{ key: UsageKey; label: string }>,
): CooldownDetailSegment[] {
  const segments: CooldownDetailSegment[] = [];
  if (
    account.cooldown_generic_until
    && Date.parse(account.cooldown_generic_until) > now
  ) {
    segments.push({ kind: "generic" });
  }
  for (const limit of limits) {
    if (isWindowCooling(account, limit.key, now)) {
      segments.push({ kind: "window", label: limit.label });
    }
  }
  if (isFreeCooling(account, now)) {
    segments.push({ kind: "free" });
  }
  if (segments.length === 0) segments.push({ kind: "generic" });
  return segments;
}

/** Managed-registration step labels; the step id itself is the semantic code. */
export const MANAGED_STEP_LABEL_KEYS: Record<AccountSetupStep, MessageKey> = {
  google_account: "待完成：登录身份",
  opencode_registration: "待完成：邀请注册",
  payment: "待完成：支付",
  key_verification: "待完成：验证 Key",
  ready: "注册完成",
};

/** Quota-sync caption state; `time` is locale-formatted data, not copy. */
export type UsageSyncStatus = { kind: "never" } | { kind: "synced"; time: string };

function formatUsageSyncTime(value: string): string {
  const ts = Date.parse(value);
  if (!Number.isFinite(ts)) return value;
  return new Date(ts).toLocaleString();
}

export function usageSyncStatus(account: Account): UsageSyncStatus {
  return account.usage_sync_last_success_at
    ? { kind: "synced", time: formatUsageSyncTime(account.usage_sync_last_success_at) }
    : { kind: "never" };
}

export function accountMenuOptions(
  account: Account,
  now = Date.now(),
  catalog: readonly ProviderCatalogEntry[] | null | undefined = null,
  destination?: AccountCapabilitySource | null,
): AccountMenuOption[] {
  const options: AccountMenuOption[] = [];
  const caps = accountCapabilities(account, catalog, destination);
  // CPA is a static external-integration singleton. Account ordering and its
  // enabled switch stay here; all other controls live on the CPA page.
  if (caps.externalIntegration) {
    options.push({ key: "open-cpa", accountId: account.id, accountName: account.name });
    return options;
  }
  // The built-in Zen Free singleton has no Key/profile/console actions.
  if (caps.toggleWrite === "provider_settings") return options;
  if (caps.endpointOnAccount) {
    // Custom API has no OpenCode console, browser profile, or managed setup;
    // keep only the generic lifecycle actions.
    if (accountIsReady(account)) {
      options.push({ key: "edit", accountId: account.id, accountName: account.name });
    }
    if (accountIsReady(account) && isCooling(account, now)) {
      options.push({
        key: "reset",
        accountId: account.id,
        accountName: account.name,
      });
    }
    options.push({
      key: "delete",
      accountId: account.id,
      accountName: account.name,
    });
    return options;
  }
  const opencodeActions = caps.consoleLink === "opencode" || caps.browserProfile || caps.managedSignup;
  if (opencodeActions && !accountIsReady(account)) {
    if (caps.managedSignup) {
      options.push({
        key: "continue-setup",
        accountId: account.id,
        accountName: account.name,
      });
    }
    if (caps.browserProfile) {
      options.push({
        key: "reset-profile",
        accountId: account.id,
        accountName: account.name,
      });
    }
    options.push({
      key: "delete",
      accountId: account.id,
      accountName: account.name,
    });
    return options;
  }
  if (accountIsReady(account)) {
    if (caps.consoleLink === "opencode") {
      options.push({
        key: "open-console",
        accountId: account.id,
        accountName: account.name,
      });
    }
    if (caps.consoleLink === "ollama") {
      options.push({
        key: "open-site",
        accountId: account.id,
        accountName: account.name,
      });
    }
    options.push({ key: "edit", accountId: account.id, accountName: account.name });
    if (isCooling(account, now)) {
      options.push({
        key: "reset",
        accountId: account.id,
        accountName: account.name,
      });
    }
    if (caps.browserProfile) {
      options.push({
        key: "reset-profile",
        accountId: account.id,
        accountName: account.name,
      });
    }
  } else {
    // Non-OpenCode families have no setup flow; keep the draft editable.
    options.push({ key: "edit", accountId: account.id, accountName: account.name });
  }
  options.push({
    key: "delete",
    accountId: account.id,
    accountName: account.name,
  });
  return options;
}
