import type { Account, AccountSetupStep } from "../api/dashboard";
import { isCooling, isFreeCooling, isWindowCooling } from "./accounts-usage.ts";
import type { UsageKey } from "./accounts-usage.ts";
import { daysUntilDate, expiryTagType } from "./account-lifecycle.ts";
import type { ExpiryTagType } from "./account-lifecycle.ts";
import { isCpaIntegrationAccount, isOllamaCloudAccount, isZenFreeAccount } from "./account-providers.ts";
import { isCustomApiAccount } from "./custom-account.ts";
import { t } from "../i18n/index.ts";
import type { MessageKey } from "../i18n/index.ts";

/**
 * Pure presentational helpers for the account list: status/expiry tags,
 * cooldown summaries, managed-registration step labels, the quota-sync
 * captions, and the per-card overflow menu. Everything takes `now` explicitly
 * so the view's 15s clock stays the single re-render driver.
 */

export type AccountStatusTagType = "success" | "warning" | "error" | "default";

export type AccountMenuOption = {
  key: string | number;
  label?: string;
  accountId: string;
  accountName: string;
};

export function accountIsReady(account: Pick<Account, "setup_step">): boolean {
  return account.setup_step === "ready";
}

/** Shared label for ready accounts that the backend keeps as unroutable drafts. */
export function accountRoutingDraftLabel(
  account: Pick<Account, "setup_step" | "plan_routable" | "verification_status">,
): MessageKey | null {
  if (!accountIsReady(account) || account.plan_routable) return null;
  if (account.verification_status === "pending") return "待验证";
  if (account.verification_status === "failed") return "验证失败";
  return "等待支持";
}

/** Shared explanatory copy for the same backend-owned draft state. */
export function accountRoutingDraftDescription(
  account: Pick<Account, "setup_step" | "plan_routable" | "verification_status">,
): MessageKey | null {
  if (!accountRoutingDraftLabel(account)) return null;
  if (account.verification_status === "pending") {
    return "该方案验证功能暂不可用，创建后保持禁用草稿。";
  }
  if (account.verification_status === "failed") {
    return "验证失败，请检查 Key 或等待该方案支持验证。";
  }
  return "该方案暂不可路由。";
}

export function formatCooldownRemainingUntil(until: string | null, now = Date.now()): string {
  if (!until) return "";
  const ms = new Date(until).getTime() - now;
  if (ms <= 0) return t("{seconds}秒", { seconds: 0 });
  const seconds = Math.ceil(ms / 1000);
  if (seconds < 60) return t("{seconds}秒", { seconds });
  const min = Math.floor(ms / 60000);
  if (min < 60) return t("{minutes}分钟", { minutes: min });
  const hr = Math.floor(min / 60);
  if (hr < 24) return t("{hours}小时{minutes}分钟", { hours: hr, minutes: min % 60 });
  const day = Math.floor(hr / 24);
  return t("{days}天{hours}小时", { days: day, hours: hr % 24 });
}

export function formatCooldownRemaining(
  account: Pick<Account, "cooldown_until">,
  now = Date.now(),
): string {
  return formatCooldownRemainingUntil(account.cooldown_until, now);
}

export function accountStatusLabel(account: Account, now = Date.now()): string {
  if (isZenFreeAccount(account)) {
    if (!account.enabled) return t("已禁用");
    if (isFreeCooling(account, now)) {
      return t("冷却中·剩 {time}", {
        time: formatCooldownRemainingUntil(account.cooldown_free_until, now),
      });
    }
    return t("已启用");
  }
  if (!accountIsReady(account)) return t("注册中");
  const draftLabel = accountRoutingDraftLabel(account);
  if (draftLabel) return t(draftLabel);
  if (account.auth_error) {
    return account.enabled
      ? t("不可用")
      : `${t("已禁用")} · ${t("不可用")}`;
  }
  if (!account.enabled) return t("已禁用");
  if (isCooling(account, now)) return t("冷却中·剩 {time}", { time: formatCooldownRemaining(account, now) });
  // Ready + enabled is a configuration state, not a verified-availability
  // claim: no connection test evidence is implied.
  return t("已启用");
}

export function accountStatusTagType(account: Account, now = Date.now()): AccountStatusTagType {
  if (isZenFreeAccount(account)) {
    if (!account.enabled) return "error";
    return isFreeCooling(account, now) ? "warning" : "success";
  }
  if (!accountIsReady(account)) return "warning";
  const draftLabel = accountRoutingDraftLabel(account);
  if (draftLabel) return draftLabel === "验证失败" ? "error" : "warning";
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

export function accountExpiryLabel(account: Pick<Account, "expires_on">, now = Date.now()): string {
  const days = accountExpiryDays(account, now);
  if (!Number.isFinite(days)) return t("未设置");
  if (days === 1) return t("剩 1 天");
  if (days > 0) return t("剩 {days} 天", { days });
  if (days === 0) return t("今天到期");
  if (days === -1) return t("已到期 1 天");
  return t("已到期 {days} 天", { days: Math.abs(days) });
}

export function cooldownDetails(
  account: Account,
  now: number,
  limits: Array<{ key: UsageKey; label: string }>,
): string {
  const active = limits
    .filter((limit) => isWindowCooling(account, limit.key, now))
    .map((limit) => limit.label);
  if (
    account.cooldown_generic_until
    && Date.parse(account.cooldown_generic_until) > now
  ) {
    active.unshift(t("冷却中"));
  }
  if (isFreeCooling(account, now)) {
    active.push(t("Free"));
  }
  return active.length > 0 ? active.join(" · ") : t("冷却中");
}

export function managedStepLabel(step: AccountSetupStep): string {
  switch (step) {
    case "google_account": return t("待完成：登录身份");
    case "opencode_registration": return t("待完成：邀请注册");
    case "payment": return t("待完成：支付");
    case "key_verification": return t("待完成：验证 Key");
    case "ready": return t("注册完成");
  }
}

function formatUsageSyncTime(value: string | null | undefined): string {
  if (!value) return t("尚未官方同步");
  const ts = Date.parse(value);
  if (!Number.isFinite(ts)) return value;
  return new Date(ts).toLocaleString();
}

export function usageSyncCaption(account: Account): string {
  return account.usage_sync_last_success_at
    ? t("上次官方同步: {time}", { time: formatUsageSyncTime(account.usage_sync_last_success_at) })
    : t("尚未官方同步");
}

export function accountMenuOptions(account: Account, now = Date.now()): AccountMenuOption[] {
  const options: AccountMenuOption[] = [];
  // CPA is a static external-integration singleton. Account ordering and its
  // enabled switch stay here; all other controls live on the CPA page.
  if (isCpaIntegrationAccount(account)) {
    options.push({ key: "open-cpa", label: t("前往 CPA"), accountId: account.id, accountName: account.name });
    return options;
  }
  // The built-in Zen Free singleton has no Key/profile/console actions.
  if (isZenFreeAccount(account)) return options;
  if (isCustomApiAccount(account)) {
    // Custom API has no OpenCode console, browser profile, or managed setup;
    // keep only the generic lifecycle actions.
    if (accountIsReady(account)) {
      options.push({ key: "edit", label: t("编辑账号"), accountId: account.id, accountName: account.name });
    }
    if (accountIsReady(account) && isCooling(account, now)) {
      options.push({
        key: "reset",
        label: t("重置冷却"),
        accountId: account.id,
        accountName: account.name,
      });
    }
    options.push({
      key: "delete",
      label: t("删除账号"),
      accountId: account.id,
      accountName: account.name,
    });
    return options;
  }
  // The console link, managed setup, and browser-profile actions are
  // OpenCode-only semantics; other sealed families stop at the generic
  // lifecycle actions.
  const isOpencodeGo = account.provider_id === "opencode";
  if (isOpencodeGo && !accountIsReady(account)) {
    options.push({
      key: "continue-setup",
      label: t("继续注册"),
      accountId: account.id,
      accountName: account.name,
    });
    options.push({
      key: "reset-profile",
      label: t("重置官网登录状态"),
      accountId: account.id,
      accountName: account.name,
    });
    options.push({
      key: "delete",
      label: t("删除账号"),
      accountId: account.id,
      accountName: account.name,
    });
    return options;
  }
  if (accountIsReady(account)) {
    if (isOpencodeGo) {
      options.push({
        key: "open-console",
        label: t("打开 OpenCode 官网"),
        accountId: account.id,
        accountName: account.name,
      });
    }
    if (isOllamaCloudAccount(account)) {
      options.push({
        key: "open-site",
        label: t("打开 Ollama 官网"),
        accountId: account.id,
        accountName: account.name,
      });
    }
    options.push({ key: "edit", label: t("编辑账号"), accountId: account.id, accountName: account.name });
    if (isCooling(account, now)) {
      options.push({
        key: "reset",
        label: t("重置冷却"),
        accountId: account.id,
        accountName: account.name,
      });
    }
    if (isOpencodeGo) {
      options.push({
        key: "reset-profile",
        label: t("重置官网登录状态"),
        accountId: account.id,
        accountName: account.name,
      });
    }
  } else {
    // Non-OpenCode families have no setup flow; keep the draft editable.
    options.push({ key: "edit", label: t("编辑账号"), accountId: account.id, accountName: account.name });
  }
  options.push({
    key: "delete",
    label: t("删除账号"),
    accountId: account.id,
    accountName: account.name,
  });
  return options;
}
