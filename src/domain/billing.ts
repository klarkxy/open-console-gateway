import type { UsageWindow } from "../api/dashboard.ts";
import type {
  BillingModel,
  BillingSource,
  BillingStatus,
  CreditBalanceCorrection,
  CreditBucket,
  CreditConfiguration,
  CreditMeterView,
  CreditPreset,
  CreditRate,
  MonthlyCredits,
  ProviderUsage,
} from "../api/billing.ts";
import { presentProviderUsage, type ProviderUsageResponse } from "../api/providers.ts";
import type { UsageKey } from "./accounts-usage.ts";
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

export type BillingSurfaceKind =
  | "cash"
  | "cash_balances"
  | "quota"
  | "credits_meter"
  | "credits_setup"
  | "credits_usd_month"
  | "empty";

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

export function isBucketExpired(bucket: CreditBucket, nowMs: number): boolean {
  if (!bucket.expiresAt) return false;
  const expires = Date.parse(bucket.expiresAt);
  return Number.isFinite(expires) && expires <= nowMs;
}

export function partitionCreditBuckets(
  buckets: readonly CreditBucket[],
  nowMs: number,
): { active: CreditBucket[]; expired: CreditBucket[] } {
  const active: CreditBucket[] = [];
  const expired: CreditBucket[] = [];
  for (const bucket of buckets) {
    if (isBucketExpired(bucket, nowMs)) expired.push(bucket);
    else active.push(bucket);
  }
  return { active, expired };
}

export function distinctExpiryIso(buckets: readonly CreditBucket[]): string[] {
  const seen = new Set<string>();
  const ordered: string[] = [];
  for (const bucket of buckets) {
    if (!bucket.expiresAt || seen.has(bucket.expiresAt)) continue;
    seen.add(bucket.expiresAt);
    ordered.push(bucket.expiresAt);
  }
  return ordered;
}

export function usdCreditMonthWindow(
  usage: ProviderUsage | null | undefined,
): boolean {
  if (!usage) return false;
  return usage.quotaWindows.some((window) => (
    (window.windowKind === "month" || window.windowKind === "monthly")
    && window.unit === "usd_credits"
  ));
}

export function billingSurfaceKind(status: BillingStatus): BillingSurfaceKind {
  if (status.model === "cash") return status.cash ? "cash" : "cash_balances";
  if (status.model === "quota") return "quota";
  if (status.credits) return "credits_meter";
  if (usdCreditMonthWindow(status.usage)) return "credits_usd_month";
  if (status.configurableCredits) return "credits_setup";
  return "empty";
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
  return status.cash ? "official_balance" : "provider_usage";
}

export function cashCreditConfigureAvailable(status: BillingStatus): boolean {
  return status.model === "cash" && status.configurableCredits && !status.credits;
}

export function billingManualCalibration(status: BillingStatus): boolean {
  if (!status.manualCalibration) return false;
  if (status.credits) return false;
  if (status.model === "quota") return true;
  return usdCreditMonthWindow(status.usage);
}

export type CreditCalibrationBlock = "pending";

export function creditCalibrationBlock(
  meter: Pick<CreditMeterView, "pendingRequests"> | null | undefined,
): CreditCalibrationBlock | null {
  if (!meter) return null;
  if (meter.pendingRequests > 0) return "pending";
  return null;
}

export function presentedUsageOf(status: BillingStatus | null | undefined): ProviderUsageResponse | null {
  return status?.usage ? presentProviderUsage(status.usage) : null;
}

export function usageWindowFromProviderUsage(
  usage: ProviderUsage | null | undefined,
  accountId: string,
): UsageWindow {
  const blank: UsageWindow = {
    account_id: accountId,
    window_5h: 0,
    window_week: 0,
    window_month: 0,
    resets_in_5h: null,
    resets_in_week: null,
    resets_in_month: null,
  };
  if (!usage) return blank;
  const byKind = new Map(usage.quotaWindows.map((window) => [window.windowKind, window]));
  const five = byKind.get("five_hours");
  const week = byKind.get("week");
  const month = byKind.get("month") ?? byKind.get("monthly");
  return {
    account_id: usage.accountId || accountId,
    window_5h: five?.used ?? 0,
    window_week: week?.used ?? 0,
    window_month: month?.used ?? 0,
    resets_in_5h: five?.resetsAt ?? null,
    resets_in_week: week?.resetsAt ?? null,
    resets_in_month: month?.resetsAt ?? null,
  };
}

export function applyUsageCalibration(
  usage: ProviderUsage,
  key: UsageKey,
  window: UsageWindow,
  updatedAt: string,
): ProviderUsage {
  const kind = WINDOW_KIND[key];
  const resetsAt = key === "window_5h"
    ? window.resets_in_5h
    : key === "window_week"
      ? window.resets_in_week
      : window.resets_in_month;
  return {
    ...usage,
    quotaWindows: usage.quotaWindows.map((row) => (
      row.windowKind === kind
        ? { ...row, used: window[key], resetsAt, updatedAt }
        : row
    )),
  };
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
): CreditConfiguration {
  return {
    ...preset.configuration,
    monthly: monthlyWithReset(preset.configuration.monthly, nextResetAt, CHINA_OFFSET_MINUTES),
  };
}

export function initialMonthlyBucket(
  configuration: CreditConfiguration,
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
  creditsPerCurrency: number;
  rates: CreditRate[];
  remaining: number;
  monthlyEnabled: boolean;
  monthlyAmount: number | null;
  nextResetAt: string | null;
  timezoneOffsetMinutes: number;
  sourceUrl: string | null;
  startsAt: string;
}): { configuration: CreditConfiguration; initialBuckets: CreditBucket[] } | { issue: CreditSetupIssue } {
  if (input.monthlyEnabled) {
    if (input.monthlyAmount == null) return { issue: "missing" };
    if (!Number.isFinite(input.monthlyAmount) || input.monthlyAmount < 0) return { issue: "invalid" };
    if (!input.nextResetAt) return { issue: "date" };
    const configuration: CreditConfiguration = {
      name: input.name,
      currency: input.currency,
      creditsPerCurrency: input.creditsPerCurrency,
      rates: input.rates,
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
  const configuration: CreditConfiguration = {
    name: input.name,
    currency: input.currency,
    creditsPerCurrency: input.creditsPerCurrency,
    rates: input.rates,
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
  current: CreditConfiguration,
  input: {
    name: string;
    currency: string;
    creditsPerCurrency: number;
    rates: CreditRate[];
    monthlyEnabled: boolean;
    monthlyAmount: number | null;
    nextResetAt: string | null;
    timezoneOffsetMinutes: number;
  },
): CreditConfiguration | { issue: CreditSetupIssue } {
  if (!input.monthlyEnabled) {
    return {
      ...current,
      name: input.name,
      currency: input.currency,
      creditsPerCurrency: input.creditsPerCurrency,
      rates: input.rates,
      monthly: null,
    };
  }
  if (input.monthlyAmount == null) return { issue: "missing" };
  if (!Number.isFinite(input.monthlyAmount) || input.monthlyAmount < 0) return { issue: "invalid" };
  if (!input.nextResetAt) return { issue: "date" };
  return {
    ...current,
    name: input.name,
    currency: input.currency,
    creditsPerCurrency: input.creditsPerCurrency,
    rates: input.rates,
    monthly: {
      amount: input.monthlyAmount,
      nextResetAt: input.nextResetAt,
      timezoneOffsetMinutes: input.timezoneOffsetMinutes,
      renewalEndsAt: current.monthly?.renewalEndsAt ?? null,
    },
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
