import type { Account, UsageWindow } from "../api/dashboard";
import type { ProviderQuotaWindow, ProviderUsageResponse } from "../api/providers.ts";

export type UsageKey = "window_5h" | "window_week" | "window_month";

const PROVIDER_WINDOW_KIND: Record<UsageKey, string> = {
  window_5h: "five_hours",
  window_week: "week",
  window_month: "month",
};

export function mergeCalibratedProviderUsage(
  current: ProviderUsageResponse | undefined,
  key: UsageKey,
  usage: UsageWindow,
  updatedAt: string,
): ProviderUsageResponse | undefined {
  if (!current) return undefined;
  const windowKind = PROVIDER_WINDOW_KIND[key];
  const resetsAt = key === "window_5h"
    ? usage.resets_in_5h
    : key === "window_week"
      ? usage.resets_in_week
      : usage.resets_in_month;
  let matched = false;
  const quotaWindows = current.quota_windows.map((window) => {
    if (window.window_kind !== windowKind) return window;
    matched = true;
    return {
      ...window,
      used: usage[key],
      resets_at: resetsAt,
      updated_at: updatedAt,
    };
  });
  return matched ? { ...current, quota_windows: quotaWindows } : current;
}

export interface ProviderWindowLabels {
  fiveHours: string;
  week: string;
  month: string;
  hours: (count: number) => string;
}

function humanizeWindowPart(value: string): string {
  const normalized = value.replaceAll(/[_:-]+/g, " ").trim();
  return normalized ? normalized[0]!.toUpperCase() + normalized.slice(1) : normalized;
}

/**
 * Pure display adapter for provider-defined quota windows. Known historical
 * wire names remain friendly; unknown names get a safe humanized fallback.
 */
export function providerQuotaWindowLabel(
  window: Pick<ProviderQuotaWindow, "window_kind" | "started_at" | "resets_at">,
  labels: ProviderWindowLabels,
): string {
  const kind = window.window_kind.trim();
  const normalized = kind.toLowerCase();
  if (normalized === "five_hours" || normalized === "5h" || normalized === "kimi_5h") {
    return labels.fiveHours;
  }
  if (normalized === "week" || normalized === "weekly" || normalized === "kimi_usage") {
    return labels.week;
  }
  if (normalized === "month" || normalized === "monthly") return labels.month;

  const scope = kind.includes(":") ? humanizeWindowPart(kind.slice(kind.indexOf(":") + 1)) : "";
  if (normalized.startsWith("minimax_weekly:")) {
    return scope ? `${labels.week} · ${scope}` : labels.week;
  }

  const started = window.started_at ? Date.parse(window.started_at) : Number.NaN;
  const resets = window.resets_at ? Date.parse(window.resets_at) : Number.NaN;
  const hours = Math.round((resets - started) / 3_600_000);
  if (Number.isFinite(hours) && hours > 0) {
    const period = hours === 5 ? labels.fiveHours : hours === 168 ? labels.week : labels.hours(hours);
    return scope ? `${period} · ${scope}` : period;
  }
  return humanizeWindowPart(kind) || kind;
}

export type UsageEditState = {
  draft: number;
  saved: number;
  saving: boolean;
  error: string | null;
  /// 手动校准的"距上游重置还剩多少分钟"。仅 5h/周窗口使用；月窗口始终为 null。
  resets_in_minutes_draft: number | null;
  /// 最近一次从后端读到的绝对重置时刻。未手动改时间时，保存前由它重新计算剩余分钟。
  resets_at_saved: string | null;
  resets_dirty: boolean;
};

/// 5h/周窗口的满窗分钟数。月窗口无法手动校准时间。
export const WINDOW_FULL_MINUTES: Record<UsageKey, number | null> = {
  window_5h: 5 * 60,
  window_week: 7 * 24 * 60,
  window_month: null,
};

/// 根据当前 `resets_in_*` 推断手动校准的默认剩余分钟数。
/// `resets_in_*` 为 null（窗口未开始）时返回满窗分钟数。
export function defaultResetsInMinutes(usage: Pick<UsageWindow, "resets_in_5h" | "resets_in_week" | "resets_in_month">, key: UsageKey, now = Date.now()): number | null {
  const full = WINDOW_FULL_MINUTES[key];
  if (full === null) return null;
  const until = windowResetsAt(usage, key);
  if (!until) return full;
  const remainingMs = Date.parse(until) - now;
  return Math.max(0, Math.ceil(remainingMs / 60000));
}

export function resetsInMinutesForSave(
  edit: Pick<UsageEditState, "resets_in_minutes_draft" | "resets_at_saved" | "resets_dirty">,
  key: UsageKey,
  now = Date.now(),
): number | null {
  const full = WINDOW_FULL_MINUTES[key];
  if (full === null) return null;
  if (edit.resets_dirty) return edit.resets_in_minutes_draft;
  if (!edit.resets_at_saved) return full;
  const remainingMs = Date.parse(edit.resets_at_saved) - now;
  if (!Number.isFinite(remainingMs) || remainingMs <= 0) return full;
  // The backend starts a fresh integer-minute deadline when it receives the
  // calibration. Keep a positive remainder at one minute so a percent-only
  // save in the final seconds is not immediately discarded as an expired
  // window; an already-expired deadline starts a fresh full window above.
  return Math.max(1, Math.floor(remainingMs / 60000));
}

const cooldownFields: Record<UsageKey, keyof Pick<Account, "cooldown_5h_until" | "cooldown_week_until" | "cooldown_month_until">> = {
  window_5h: "cooldown_5h_until",
  window_week: "cooldown_week_until",
  window_month: "cooldown_month_until",
};

const resetsFields: Record<UsageKey, keyof Pick<UsageWindow, "resets_in_5h" | "resets_in_week" | "resets_in_month">> = {
  window_5h: "resets_in_5h",
  window_week: "resets_in_week",
  window_month: "resets_in_month",
};

export function isWindowCooling(
  account: Pick<Account, "cooldown_5h_until" | "cooldown_week_until" | "cooldown_month_until">,
  key: UsageKey,
  now = Date.now(),
): boolean {
  const until = account[cooldownFields[key]];
  return until !== null && Date.parse(until) > now;
}

export function resetTimeForWindow(
  account: Pick<Account, "cooldown_5h_until" | "cooldown_week_until" | "cooldown_month_until">,
  key: UsageKey,
): string | null {
  return account[cooldownFields[key]];
}

/// 固定窗口的清零时刻（来自后端 `resets_in_*`）；`null` 表示窗口尚未开始（无成功请求）或月窗口无购买日期。
export function windowResetsAt(
  usage: Pick<UsageWindow, "resets_in_5h" | "resets_in_week" | "resets_in_month">,
  key: UsageKey,
): string | null {
  return usage[resetsFields[key]];
}

export function isFreeCooling(
  account: Pick<Account, "cooldown_free_until">,
  now = Date.now(),
): boolean {
  return account.cooldown_free_until !== null && Date.parse(account.cooldown_free_until) > now;
}

export function isCooling(
  account: Pick<Account, "cooldown_until" | "cooldown_5h_until" | "cooldown_week_until" | "cooldown_month_until" | "cooldown_free_until">,
  now = Date.now(),
): boolean {
  return (
    (account.cooldown_until !== null && Date.parse(account.cooldown_until) > now) ||
    isWindowCooling(account, "window_5h", now) ||
    isWindowCooling(account, "window_week", now) ||
    isWindowCooling(account, "window_month", now) ||
    isFreeCooling(account, now)
  );
}

export function isUsageLimitReached(
  account: Pick<Account, "cooldown_5h_until" | "cooldown_week_until" | "cooldown_month_until">,
  key: UsageKey,
  now = Date.now(),
): boolean {
  return isWindowCooling(account, key, now);
}

export function normalizeUsagePercent(value: number): number {
  return Math.min(100, Math.max(0, Math.round(value * 10) / 10));
}

export function usagePercentFromCost(cost: number, limit: number): number {
  return normalizeUsagePercent((cost / limit) * 100);
}

export function mergeUsageEdit(
  edit: UsageEditState | undefined,
  saved: number,
  force: boolean,
): UsageEditState {
  if (!edit) {
    return {
      draft: saved,
      saved,
      saving: false,
      error: null,
      resets_in_minutes_draft: null,
      resets_at_saved: null,
      resets_dirty: false,
    };
  }
  if (!force && (edit.saving || edit.draft !== edit.saved)) {
    return { ...edit, saved };
  }
  return { ...edit, draft: saved, saved, error: null };
}

export function usageProgressStatus(
  account: Pick<Account, "cooldown_5h_until" | "cooldown_week_until" | "cooldown_month_until">,
  key: UsageKey,
  percent: number,
  now = Date.now(),
): "success" | "warning" | "error" {
  if (isUsageLimitReached(account, key, now)) return "error";
  return percent >= 80 ? "warning" : "success";
}

export function usageProgressPercentage(
  account: Pick<Account, "cooldown_5h_until" | "cooldown_week_until" | "cooldown_month_until">,
  key: UsageKey,
  percent: number,
  now = Date.now(),
): number {
  return isUsageLimitReached(account, key, now) ? 100 : normalizeUsagePercent(percent);
}

// 用户直接编辑"天+小时"或"小时+分钟"，而不是分钟总数。
// 5h 窗口（<1天）显示 [小时][分钟]；周窗口（≥1天）显示 [天][小时]。
export function resetsFirstFieldMax(key: UsageKey): number {
  if (key === "window_5h") return 5;
  if (key === "window_week") return 7;
  return 0;
}

export function resetsSecondFieldMax(key: UsageKey): number {
  if (key === "window_5h") return 59;
  if (key === "window_week") return 23;
  return 0;
}

export function resetsFirstFieldValue(
  edit: Pick<UsageEditState, "resets_in_minutes_draft" | "resets_at_saved" | "resets_dirty"> | undefined,
  key: UsageKey,
  now = Date.now(),
): number {
  if (!edit) return 0;
  const m = resetsInMinutesForSave(edit, key, now);
  if (m === null) return 0;
  if (key === "window_5h") return Math.floor(m / 60);
  if (key === "window_week") return Math.floor(m / (24 * 60));
  return 0;
}

export function resetsSecondFieldValue(
  edit: Pick<UsageEditState, "resets_in_minutes_draft" | "resets_at_saved" | "resets_dirty"> | undefined,
  key: UsageKey,
  now = Date.now(),
): number {
  if (!edit) return 0;
  const m = resetsInMinutesForSave(edit, key, now);
  if (m === null) return 0;
  if (key === "window_5h") return m % 60;
  if (key === "window_week") return Math.floor((m % (24 * 60)) / 60);
  return 0;
}

export function resetsFieldsToMinutes(first: number, second: number, key: UsageKey): number {
  if (key === "window_5h") return first * 60 + second;
  if (key === "window_week") return first * 24 * 60 + second * 60;
  return 0;
}
