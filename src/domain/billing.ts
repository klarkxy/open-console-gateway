import type {
  BillingModel,
  BillingSource,
  BillingStatus,
  CreditBalanceCorrection,
  CreditBucket,
  CreditConfigurationWrite,
  CreditMeterView,
  CreditPreset,
  MonthlyCredits,
  ProviderUsage,
} from "../api/billing.ts";
import {
  presentProviderUsage,
  type ProviderQuotaWindow,
  type ProviderUsageResponse,
} from "../api/providers.ts";
import type { ObservedUsageWindow, UsageKey } from "./accounts-usage.ts";
import type { MessageKey } from "../i18n/index.ts";

export const CHINA_OFFSET_MINUTES = 480;
export const CREDIT_DISPLAY_SCALE = 1_000_000;
export const TOPUP_QUICK_AMOUNTS = [400_000_000, 1_600_000_000] as const;
export const TOPUP_DEFAULT_TTL_MS = 30 * 24 * 60 * 60 * 1000;

export type CreditUnitId = "1" | "k" | "m";

export const CREDIT_UNIT_FACTORS: Record<CreditUnitId, number> = {
  1: 1,
  k: 1_000,
  m: CREDIT_DISPLAY_SCALE,
};

export const BILLING_SOURCE_KEYS = {
  official: "官网",
  local_estimate: "本地估算",
  unavailable: "不可用",
} as const satisfies Record<BillingSource, MessageKey>;

export const BILLING_MODEL_KEYS = {
  quota: "额度窗口",
  cash: "现金余额",
  credits: "点数",
} as const satisfies Record<BillingModel, MessageKey>;

export const BILLING_ERROR_KEYS = {
  conflict: "账号绑定已变更",
  load_failed: "用量加载失败",
} as const satisfies Record<BillingClientError, MessageKey>;

export type BillingClientError = "conflict" | "load_failed";

export type BillingSurfaceKind = BillingStatus["surfaceKind"];

export type BillingPanelMode = "initial_loading" | "initial_error" | "ready";
export type CashRefreshKind = "official_balance" | "provider_usage";
export type CreditAmountIssue = "missing" | "invalid";

export const CREDIT_AMOUNT_ISSUE_KEYS = {
  missing: "填写金额",
  invalid: "金额无效",
} as const satisfies Record<CreditAmountIssue, MessageKey>;

export const CREDIT_DATE_ISSUE_KEY = "日期无效" as const satisfies MessageKey;

const WINDOW_KIND: Record<UsageKey, string> = {
  window_5h: "five_hours",
  window_week: "week",
  window_month: "month",
};

/**
 * One acknowledged percent window. It is not a billing status: no cash,
 * credit, CAS, provider, or sync metadata, and no copied migration offset.
 */
export interface ManualQuotaWindow {
  windowKind: string;
  used: number;
  limitValue: 100;
  unit: "percent";
  source: "manual";
  observedAt: string;
  resetsAt: string | null;
  updatedAt: string;
}

/** Quota-only receipt for one binding. Missing kinds stay absent, not zero. */
export interface ManualQuotaReceipt {
  windows: ManualQuotaWindow[];
}

export interface QuotaWindowsView {
  quota_windows: ProviderQuotaWindow[];
}

export function billingBinding(accountVersion: string, endpointUrl: string | null | undefined): string {
  return `${accountVersion}\0${endpointUrl ?? ""}`;
}

export function creditsToScaled(raw: number, factor = CREDIT_DISPLAY_SCALE): number | null {
  if (!Number.isFinite(raw) || !Number.isFinite(factor) || factor <= 0) return null;
  const scaled = raw / factor;
  return Number.isFinite(scaled) ? scaled : null;
}

export function formatScaledCredits(
  raw: number,
  localeName: string,
  factor = CREDIT_DISPLAY_SCALE,
): string {
  const scaled = creditsToScaled(raw, factor);
  if (scaled == null) return "";
  return scaled.toLocaleString(localeName, { maximumFractionDigits: 4 });
}

export function scaledToCredits(scaled: number, factor = CREDIT_DISPLAY_SCALE): number | null {
  if (!Number.isFinite(scaled) || scaled < 0 || !Number.isFinite(factor) || factor <= 0) return null;
  const raw = Math.round(scaled * factor);
  return Number.isFinite(raw) ? raw : null;
}

export function parseCreditAmount(
  scaled: number | null | undefined,
  factor = CREDIT_DISPLAY_SCALE,
): { amount: number } | { issue: CreditAmountIssue } {
  if (scaled === null || scaled === undefined) return { issue: "missing" };
  const amount = scaledToCredits(scaled, factor);
  if (amount == null) return { issue: "invalid" };
  return { amount };
}

export function creditDisplayFactor(presetCount: number): number {
  return presetCount > 0 ? CREDIT_DISPLAY_SCALE : 1;
}

export type CreditSetupIssue = CreditAmountIssue | "date";

export const CREDIT_SETUP_ISSUE_KEYS = {
  missing: CREDIT_AMOUNT_ISSUE_KEYS.missing,
  invalid: CREDIT_AMOUNT_ISSUE_KEYS.invalid,
  date: CREDIT_DATE_ISSUE_KEY,
} as const satisfies Record<CreditSetupIssue, MessageKey>;

export function meterNextResetAt(meter: CreditMeterView): string | null {
  return meter.nextResetAt;
}

export function nextCalendarMonthStart(nowMs: number, offsetMinutes = CHINA_OFFSET_MINUTES): Date {
  const wall = new Date(nowMs + offsetMinutes * 60_000);
  return new Date(Date.UTC(wall.getUTCFullYear(), wall.getUTCMonth() + 1, 1) - offsetMinutes * 60_000);
}

export function pad2(value: number): string {
  return String(value).padStart(2, "0");
}

export function formatOffsetDateTime(
  iso: string,
  offsetMinutes: number,
): string | null {
  const ms = Date.parse(iso);
  if (!Number.isFinite(ms)) return null;
  const wall = new Date(ms + offsetMinutes * 60_000);
  const sign = offsetMinutes >= 0 ? "+" : "-";
  const abs = Math.abs(offsetMinutes);
  const zone = `${sign}${pad2(Math.floor(abs / 60))}${abs % 60 === 0 ? "" : `:${pad2(abs % 60)}`}`;
  return `${wall.getUTCFullYear()}-${pad2(wall.getUTCMonth() + 1)}-${pad2(wall.getUTCDate())} ${pad2(wall.getUTCHours())}:${pad2(wall.getUTCMinutes())} UTC${zone}`;
}

export function toDatetimeLocalValue(iso: string, offsetMinutes: number): string {
  const ms = Date.parse(iso);
  if (!Number.isFinite(ms)) return "";
  const wall = new Date(ms + offsetMinutes * 60_000);
  return `${wall.getUTCFullYear()}-${pad2(wall.getUTCMonth() + 1)}-${pad2(wall.getUTCDate())}T${pad2(wall.getUTCHours())}:${pad2(wall.getUTCMinutes())}`;
}

export function fromDatetimeLocalValue(value: string, offsetMinutes: number): string | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})$/.exec(value.trim());
  if (!match) return null;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  const hour = Number(match[4]);
  const minute = Number(match[5]);
  if (month < 1 || month > 12 || day < 1 || day > 31 || hour > 23 || minute > 59) return null;
  if (!Number.isFinite(offsetMinutes)) return null;
  const utcMs = Date.UTC(year, month - 1, day, hour, minute);
  const wall = new Date(utcMs);
  if (
    wall.getUTCFullYear() !== year
    || wall.getUTCMonth() !== month - 1
    || wall.getUTCDate() !== day
    || wall.getUTCHours() !== hour
    || wall.getUTCMinutes() !== minute
  ) {
    return null;
  }
  return new Date(utcMs - offsetMinutes * 60_000).toISOString();
}

export function offsetMinutesOrDefault(
  value: number | null | undefined,
  fallback = CHINA_OFFSET_MINUTES,
): number {
  return value ?? fallback;
}

export function billingSurfaceKind(status: BillingStatus): BillingSurfaceKind {
  return status.surfaceKind;
}

export function billingPanelMode(slot: {
  status: BillingStatus | null;
  loaded: boolean;
  loading: boolean;
  error: BillingClientError | null;
}): BillingPanelMode {
  if (slot.status) return "ready";
  if (slot.error) return "initial_error";
  return "initial_loading";
}

export function billingPanelOverlayError(slot: {
  status: BillingStatus | null;
  error: BillingClientError | null;
}): BillingClientError | null {
  return slot.status && slot.error ? slot.error : null;
}

export function cashRefreshKind(status: BillingStatus): CashRefreshKind {
  return status.surfaceKind === "cash" ? "official_balance" : "provider_usage";
}

export function billingManualCalibration(status: BillingStatus): boolean {
  return status.quotaManualCalibration;
}

export type CreditCalibrationBlock = "pending";

export function creditCalibrationBlock(
  meter: Pick<CreditMeterView, "calibrationBlock"> | null | undefined,
): CreditCalibrationBlock | null {
  return meter?.calibrationBlock ?? null;
}

export function presentedUsageOf(status: BillingStatus | null | undefined): ProviderUsageResponse | null {
  return status?.usage ? presentProviderUsage(status.usage) : null;
}

function observedPercent(used: number | null | undefined): number | null {
  return typeof used === "number" && Number.isFinite(used) ? used : null;
}

function blankObserved(accountId: string): ObservedUsageWindow {
  return {
    account_id: accountId,
    window_5h: null,
    window_week: null,
    window_month: null,
    resets_in_5h: null,
    resets_in_week: null,
    resets_in_month: null,
  };
}

function quotaKindMatches(actual: string, kind: string): boolean {
  return actual === kind || (kind === "month" && actual === "monthly");
}

function acknowledgedManualWindow(
  key: UsageKey,
  window: ObservedUsageWindow,
): { used: number; resetsAt: string | null; kind: string } | null {
  const used = observedPercent(window[key]);
  if (used === null) return null;
  const resetsAt = key === "window_5h"
    ? window.resets_in_5h
    : key === "window_week"
      ? window.resets_in_week
      : window.resets_in_month;
  return { used, resetsAt, kind: WINDOW_KIND[key] };
}

function manualQuotaWindow(
  kind: string,
  used: number,
  resetsAt: string | null,
  updatedAt: string,
): ManualQuotaWindow {
  return {
    windowKind: kind,
    used,
    limitValue: 100,
    unit: "percent",
    source: "manual",
    observedAt: updatedAt,
    resetsAt,
    updatedAt,
  };
}

export function usageWindowFromProviderUsage(
  usage: ProviderUsage | null | undefined,
  accountId: string,
): ObservedUsageWindow {
  if (!usage) return blankObserved(accountId);
  const byKind = new Map(usage.quotaWindows.map((window) => [window.windowKind, window]));
  const five = byKind.get("five_hours");
  const week = byKind.get("week");
  const month = byKind.get("month") ?? byKind.get("monthly");
  return {
    account_id: usage.accountId || accountId,
    window_5h: observedPercent(five?.used),
    window_week: observedPercent(week?.used),
    window_month: observedPercent(month?.used),
    resets_in_5h: five?.resetsAt ?? null,
    resets_in_week: week?.resetsAt ?? null,
    resets_in_month: month?.resetsAt ?? null,
  };
}

/**
 * Percent windows from a quota-only receipt. Kinds the receipt does not
 * mention stay null; an entered 0 stays 0.
 */
export function usageWindowFromManualReceipt(
  receipt: ManualQuotaReceipt | null | undefined,
  accountId: string,
): ObservedUsageWindow {
  if (!receipt || receipt.windows.length === 0) return blankObserved(accountId);
  const byKind = new Map(receipt.windows.map((window) => [window.windowKind, window]));
  const five = byKind.get("five_hours");
  const week = byKind.get("week");
  const month = byKind.get("month") ?? byKind.get("monthly");
  return {
    account_id: accountId,
    window_5h: observedPercent(five?.used),
    window_week: observedPercent(week?.used),
    window_month: observedPercent(month?.used),
    resets_in_5h: five?.resetsAt ?? null,
    resets_in_week: week?.resetsAt ?? null,
    resets_in_month: month?.resetsAt ?? null,
  };
}

/** Maps a receipt to the quota-window rows the summary renders. */
export function manualReceiptQuotaView(
  receipt: ManualQuotaReceipt | null | undefined,
  accountId: string,
  canonical?: QuotaWindowsView | null,
): QuotaWindowsView | null {
  if (!receipt || receipt.windows.length === 0) return null;
  const acknowledged: ProviderQuotaWindow[] = receipt.windows.map((window) => ({
      account_id: accountId,
      window_kind: window.windowKind,
      used: window.used,
      limit_value: window.limitValue,
      started_at: null,
      resets_at: window.resetsAt,
      calibration_offset: 0,
      unit: window.unit,
      source: window.source,
      observed_at: window.observedAt,
      updated_at: window.updatedAt,
    }));
  const rows = canonical?.quota_windows ?? [];
  const quota_windows = rows.map((row) => acknowledged.find((window) =>
    quotaKindMatches(row.window_kind, window.window_kind)
    || quotaKindMatches(window.window_kind, row.window_kind)) ?? row);
  for (const window of acknowledged) {
    if (!rows.some((row) => quotaKindMatches(row.window_kind, window.window_kind)
      || quotaKindMatches(window.window_kind, row.window_kind))) quota_windows.push(window);
  }
  return { quota_windows };
}

/**
 * Records one acknowledged window on the receipt. A null percent does not
 * insert 0 and does not drop sibling windows.
 */
export function mergeManualQuotaReceipt(
  current: ManualQuotaReceipt | null,
  key: UsageKey,
  window: ObservedUsageWindow,
  updatedAt: string,
): ManualQuotaReceipt | null {
  const acknowledged = acknowledgedManualWindow(key, window);
  const existing = current?.windows ?? [];
  if (!acknowledged) return existing.length > 0 ? { windows: existing } : null;
  let matched = false;
  const windows = existing.map((row) => {
    if (!quotaKindMatches(row.windowKind, acknowledged.kind)) return row;
    matched = true;
    return manualQuotaWindow(row.windowKind, acknowledged.used, acknowledged.resetsAt, updatedAt);
  });
  if (!matched) {
    windows.push(manualQuotaWindow(
      acknowledged.kind,
      acknowledged.used,
      acknowledged.resetsAt,
      updatedAt,
    ));
  }
  return { windows };
}

export function applyUsageCalibration(
  usage: ProviderUsage,
  key: UsageKey,
  window: ObservedUsageWindow,
  updatedAt: string,
): ProviderUsage {
  const acknowledged = acknowledgedManualWindow(key, window);
  if (!acknowledged) return usage;
  let matched = false;
  const quotaWindows = usage.quotaWindows.map((row) => {
    if (!quotaKindMatches(row.windowKind, acknowledged.kind)) return row;
    matched = true;
    // The acknowledged percent replaces the old row. Legacy usd_credits,
    // a non-100 limit, and a migration offset are not reused as the observation.
    return {
      accountId: row.accountId,
      calibrationOffset: 0,
      limitValue: 100,
      observedAt: updatedAt,
      resetsAt: acknowledged.resetsAt,
      source: "manual",
      startedAt: null,
      unit: "percent",
      updatedAt,
      used: acknowledged.used,
      windowKind: row.windowKind,
    };
  });
  if (!matched) {
    quotaWindows.push({
      accountId: usage.accountId,
      calibrationOffset: 0,
      limitValue: 100,
      observedAt: updatedAt,
      resetsAt: acknowledged.resetsAt,
      source: "manual",
      startedAt: null,
      unit: "percent",
      updatedAt,
      used: acknowledged.used,
      windowKind: acknowledged.kind,
    });
  }
  return { ...usage, quotaWindows };
}

export function monthlyWithReset(
  monthly: MonthlyCredits | null | undefined,
  nextResetAt: string,
  offsetMinutes = CHINA_OFFSET_MINUTES,
): MonthlyCredits | null {
  if (!monthly) {
    return {
      amount: 0,
      nextResetAt,
      timezoneOffsetMinutes: offsetMinutes,
      renewalEndsAt: null,
    };
  }
  return {
    ...monthly,
    nextResetAt,
    timezoneOffsetMinutes: offsetMinutesOrDefault(monthly.timezoneOffsetMinutes, offsetMinutes),
  };
}

export function presetById(presets: readonly CreditPreset[], id: string): CreditPreset | undefined {
  return presets.find((preset) => preset.id === id);
}

export function configurationFromPreset(
  preset: CreditPreset,
  nextResetAt: string,
): CreditConfigurationWrite {
  return {
    name: preset.configuration.name,
    currency: preset.configuration.currency,
    monthly: monthlyWithReset(preset.configuration.monthly, nextResetAt, CHINA_OFFSET_MINUTES),
    sourceUrl: preset.configuration.sourceUrl,
  };
}

export function initialMonthlyBucket(
  configuration: { name: string; monthly: MonthlyCredits | null },
  remaining: number,
  startsAt: string,
): CreditBucket {
  return {
    id: "monthly",
    kind: "monthly",
    label: configuration.name,
    granted: configuration.monthly?.amount ?? remaining,
    remaining,
    startsAt,
    expiresAt: configuration.monthly?.nextResetAt ?? null,
  };
}

export function buildInitialCreditConfigure(input: {
  name: string;
  currency: string;
  remaining: number;
  monthlyEnabled: boolean;
  monthlyAmount: number | null;
  nextResetAt: string | null;
  timezoneOffsetMinutes: number;
  sourceUrl: string | null;
  startsAt: string;
}): { configuration: CreditConfigurationWrite; initialBuckets: CreditBucket[] } | { issue: CreditSetupIssue } {
  if (input.monthlyEnabled) {
    if (input.monthlyAmount == null) return { issue: "missing" };
    if (!Number.isFinite(input.monthlyAmount) || input.monthlyAmount < 0) return { issue: "invalid" };
    if (!input.nextResetAt) return { issue: "date" };
    const configuration: CreditConfigurationWrite = {
      name: input.name,
      currency: input.currency,
      monthly: {
        amount: input.monthlyAmount,
        nextResetAt: input.nextResetAt,
        timezoneOffsetMinutes: input.timezoneOffsetMinutes,
        renewalEndsAt: null,
      },
      sourceUrl: input.sourceUrl,
    };
    return {
      configuration,
      initialBuckets: [{
        id: "monthly",
        kind: "monthly",
        label: configuration.name,
        granted: input.monthlyAmount,
        remaining: input.remaining,
        startsAt: input.startsAt,
        expiresAt: input.nextResetAt,
      }],
    };
  }
  const configuration: CreditConfigurationWrite = {
    name: input.name,
    currency: input.currency,
    monthly: null,
    sourceUrl: input.sourceUrl,
  };
  return {
    configuration,
    initialBuckets: [{
      id: "manual",
      kind: "manual",
      label: configuration.name,
      granted: input.remaining,
      remaining: input.remaining,
      startsAt: input.startsAt,
      expiresAt: null,
    }],
  };
}

export function buildCreditSettingsConfiguration(
  current: {
    name: string;
    currency: string;
    sourceUrl?: string | null;
    monthly: MonthlyCredits | null;
  },
  input: {
    name: string;
    currency: string;
    monthlyEnabled: boolean;
    monthlyAmount: number | null;
    nextResetAt: string | null;
    timezoneOffsetMinutes: number;
  },
): CreditConfigurationWrite | { issue: CreditSetupIssue } {
  if (!input.monthlyEnabled) {
    return {
      name: input.name,
      currency: input.currency,
      monthly: null,
      sourceUrl: current.sourceUrl ?? null,
    };
  }
  if (input.monthlyAmount == null) return { issue: "missing" };
  if (!Number.isFinite(input.monthlyAmount) || input.monthlyAmount < 0) return { issue: "invalid" };
  if (!input.nextResetAt) return { issue: "date" };
  return {
    name: input.name,
    currency: input.currency,
    monthly: {
      amount: input.monthlyAmount,
      nextResetAt: input.nextResetAt,
      timezoneOffsetMinutes: input.timezoneOffsetMinutes,
      renewalEndsAt: current.monthly?.renewalEndsAt ?? null,
    },
    sourceUrl: current.sourceUrl ?? null,
  };
}

export function calibrationBalances(
  drafts: ReadonlyArray<{ bucketId: string; remainingScaled: number | null }>,
  factor: number,
): CreditBalanceCorrection[] | null {
  const balances: CreditBalanceCorrection[] = [];
  for (const draft of drafts) {
    const parsed = parseCreditAmount(draft.remainingScaled, factor);
    if ("issue" in parsed) return null;
    balances.push({ bucketId: draft.bucketId, remaining: parsed.amount });
  }
  return balances;
}

export function topupExpiryIso(nowMs: number, days = 30): string {
  return new Date(nowMs + days * 24 * 60 * 60 * 1000).toISOString();
}

export function meterOffsetMinutes(meter: CreditMeterView | null | undefined): number {
  return offsetMinutesOrDefault(meter?.configuration.monthly?.timezoneOffsetMinutes);
}
