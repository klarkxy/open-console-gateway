import assert from "node:assert/strict";
import test from "node:test";
import type { BillingStatus, CreditBucket, CreditMeterView, CreditPreset, ProviderUsage } from "../api/billing.ts";
import type { OfficialApiStatus } from "../api/generated/dashboard-v4.ts";
import { presentProviderUsage } from "../api/providers.ts";
import {
  applyUsageCalibration,
  billingBinding,
  billingManualCalibration,
  billingPanelMode,
  billingPanelOverlayError,
  billingSurfaceKind,
  calibrationBalances,
  cashRefreshKind,
  creditCalibrationBlock,
  CHINA_OFFSET_MINUTES,
  buildCreditSettingsConfiguration,
  buildInitialCreditConfigure,
  configurationFromPreset,
  CREDIT_DISPLAY_SCALE,
  creditDisplayFactor,
  creditsToScaled,
  formatOffsetDateTime,
  formatScaledCredits,
  fromDatetimeLocalValue,
  initialMonthlyBucket,
  meterNextResetAt,
  meterOffsetMinutes,
  monthlyWithReset,
  manualReceiptQuotaView,
  nextCalendarMonthStart,
  parseCreditAmount,
  scaledToCredits,
  toDatetimeLocalValue,
  usageWindowFromProviderUsage,
} from "./billing.ts";

function usage(overrides: Partial<ProviderUsage> = {}): ProviderUsage {
  return {
    accountId: "acc-1",
    availability: "available",
    creditBalances: [],
    experimental: false,
    freeCooldownUntil: null,
    pricingRevision: null,
    processGeneration: 99,
    providerId: "stepfun",
    quotaWindows: [],
    revision: 3,
    syncState: null,
    ...overrides,
  };
}

function bucket(overrides: Partial<CreditBucket> = {}): CreditBucket {
  return {
    id: "monthly",
    kind: "monthly",
    label: "Mini",
    granted: 400_000_000,
    remaining: 250_000_000,
    startsAt: "2026-09-01T16:00:00.000Z",
    expiresAt: null,
    ...overrides,
  };
}

function meter(overrides: Partial<CreditMeterView> = {}): CreditMeterView {
  return {
    credentialId: "cred-1",
    meterId: "meter-1",
    configuration: {
      name: "Mini",
      currency: "CNY",
      creditsPerCurrency: CREDIT_DISPLAY_SCALE,
      rates: [{
        model: "step-3.5-flash",
        inputPerMillion: 1,
        outputPerMillion: 4,
        cacheReadPerMillion: null,
        cacheWritePerMillion: null,
      }],
      monthly: {
        amount: 400_000_000,
        nextResetAt: "2026-09-30T16:00:00.000Z",
        timezoneOffsetMinutes: CHINA_OFFSET_MINUTES,
        renewalEndsAt: null,
      },
      sourceUrl: "https://platform.stepfun.com/docs/zh/step-plan/overview",
    },
    buckets: [bucket()],
    expiredBuckets: [],
    scheduledBuckets: [],
    calibrationBlock: null,
    canCalibrate: true,
    remaining: 250_000_000,
    activeGranted: 400_000_000,
    spentSinceCalibration: 10_000_000,
    overdrawn: 0,
    unpricedRequests: 0,
    pendingRequests: 0,
    lastCalibrationAt: "2026-09-20T00:00:00.000Z",
    estimatedAt: "2026-09-21T00:00:00.000Z",
    nextResetAt: null,
    ...overrides,
  };
}

function status(overrides: Partial<BillingStatus> = {}): BillingStatus {
  return {
    accountId: "acc-1",
    surfaceKind: "credits_setup",
    quotaManualCalibration: false,
    providerWindows: true,
    quotaEditorLimits: [],
    model: "credits",
    source: "local_estimate",
    unit: "credits",
    configurableCredits: true,
    manualCalibration: true,
    officialRefresh: false,
    usage: null,
    cash: null,
    credits: null,
    presets: [],
    revision: 3,
    processGeneration: 99,
    ...overrides,
  };
}

test("credit display scale round-trips integer millions including 40B", () => {
  assert.equal(creditsToScaled(400_000_000), 400);
  assert.equal(scaledToCredits(400), 400_000_000);
  assert.equal(scaledToCredits(creditsToScaled(40_000_000_000) ?? Number.NaN), 40_000_000_000);
  assert.equal(creditsToScaled(1_600_000_000), 1600);
  assert.equal(scaledToCredits(1.5, 1_000), 1500);
});

test("generic credit display keeps small remaining amounts instead of rounding them to zero", () => {
  assert.equal(creditDisplayFactor(0), 1);
  assert.equal(creditDisplayFactor(4), CREDIT_DISPLAY_SCALE);
  assert.equal(formatScaledCredits(15, "en-US", 1), "15");
  assert.equal(creditsToScaled(15, 1), 15);
  assert.equal(scaledToCredits(15, 1), 15);
  assert.deepEqual(parseCreditAmount(15, 1), { amount: 15 });
});

test("invalid or missing credit drafts stay invalid instead of becoming zero", () => {
  assert.equal(creditsToScaled(Number.NaN), null);
  assert.equal(creditsToScaled(1, 0), null);
  assert.equal(creditsToScaled(1, Number.NEGATIVE_INFINITY), null);
  assert.equal(scaledToCredits(Number.NaN), null);
  assert.equal(scaledToCredits(-1), null);
  assert.equal(scaledToCredits(1, 0), null);
  assert.deepEqual(parseCreditAmount(null), { issue: "missing" });
  assert.deepEqual(parseCreditAmount(undefined), { issue: "missing" });
  assert.deepEqual(parseCreditAmount(Number.NaN), { issue: "invalid" });
  assert.deepEqual(parseCreditAmount(0), { amount: 0 });
  assert.equal(calibrationBalances([{ bucketId: "monthly", remainingScaled: null }], CREDIT_DISPLAY_SCALE), null);
});

test("China month boundary uses a fixed UTC offset rather than UTC midnight", () => {
  const now = Date.parse("2026-09-21T07:00:00.000Z");
  const next = nextCalendarMonthStart(now, CHINA_OFFSET_MINUTES);
  assert.equal(next.toISOString(), "2026-09-30T16:00:00.000Z");
  assert.equal(
    formatOffsetDateTime(next.toISOString(), CHINA_OFFSET_MINUTES),
    "2026-10-01 00:00 UTC+08",
  );
  const local = toDatetimeLocalValue(next.toISOString(), CHINA_OFFSET_MINUTES);
  assert.equal(local, "2026-10-01T00:00");
  assert.equal(fromDatetimeLocalValue(local, CHINA_OFFSET_MINUTES), next.toISOString());
});

test("datetime-local round-trips through the configured offset", () => {
  const iso = fromDatetimeLocalValue("2026-11-01T00:00", CHINA_OFFSET_MINUTES);
  assert.equal(iso, "2026-10-31T16:00:00.000Z");
  assert.equal(toDatetimeLocalValue(iso!, CHINA_OFFSET_MINUTES), "2026-11-01T00:00");
});

test("datetime-local rejects impossible calendar dates instead of rolling over", () => {
  assert.equal(fromDatetimeLocalValue("2026-02-31T00:00", CHINA_OFFSET_MINUTES), null);
  assert.equal(fromDatetimeLocalValue("2026-02-30T00:00", 0), null);
  assert.equal(fromDatetimeLocalValue("2026-13-01T00:00", 0), null);
  assert.equal(fromDatetimeLocalValue("2026-04-31T12:00", 0), null);
  assert.equal(fromDatetimeLocalValue("2026-11-01T24:00", 0), null);
  assert.equal(fromDatetimeLocalValue("not-a-date", 0), null);
  assert.equal(fromDatetimeLocalValue("2026-11-01T00:00", 0), "2026-11-01T00:00:00.000Z");
});

test("timezone offset 0 is UTC and is not replaced with China", () => {
  const monthly = monthlyWithReset({
    amount: 1,
    nextResetAt: "2026-11-01T00:00:00.000Z",
    timezoneOffsetMinutes: 0,
    renewalEndsAt: null,
  }, "2026-12-01T00:00:00.000Z");
  assert.equal(monthly?.timezoneOffsetMinutes, 0);
  assert.equal(meterOffsetMinutes({
    ...meter(),
    configuration: {
      ...meter().configuration,
      monthly: {
        amount: 1,
        nextResetAt: "2026-11-01T00:00:00.000Z",
        timezoneOffsetMinutes: 0,
        renewalEndsAt: null,
      },
    },
  }), 0);
  assert.equal(
    formatOffsetDateTime("2026-11-01T00:00:00.000Z", 0),
    "2026-11-01 00:00 UTC+00",
  );
});

test("calibration drafts convert through the display multiplier", () => {
  assert.deepEqual(
    calibrationBalances([
      { bucketId: "monthly", remainingScaled: 320 },
      { bucketId: "topup-2", remainingScaled: 50 },
    ], CREDIT_DISPLAY_SCALE),
    [
      { bucketId: "monthly", remaining: 320_000_000 },
      { bucketId: "topup-2", remaining: 50_000_000 },
    ],
  );
});

function officialCash(): OfficialApiStatus {
  return {
    accountId: "acc-1",
    balanceAvailable: true,
    meter: { remainingEmpty: "not_queried", remaining: [] },
    balances: [],
    kind: "deepseek",
    lifetimeSpend: [],
    monthSpend: [],
    monthStartedAt: "2026-09-01T00:00:00Z",
    processGeneration: 99,
    providerId: "deepseek",
    revision: 3,
    unpricedRequests: 0,
  };
}

test("billing surface consumes the canonical server choice even when raw fields differ", () => {
  assert.equal(billingSurfaceKind(status({ surfaceKind: "credits_usd_month", usage: null })), "credits_usd_month");
  assert.equal(billingSurfaceKind(status({ surfaceKind: "empty", credits: meter() })), "empty");
  assert.equal(cashRefreshKind(status({ surfaceKind: "cash", cash: officialCash() })), "official_balance");
  assert.equal(cashRefreshKind(status({ surfaceKind: "cash_balances", cash: null })), "provider_usage");
});

test("generic setup can omit monthly grant and keep remaining on a manual bucket", () => {
  const built = buildInitialCreditConfigure({
    name: "lab",
    currency: "USD",
    remaining: 15,
    monthlyEnabled: false,
    monthlyAmount: 100,
    nextResetAt: "2026-10-01T00:00:00.000Z",
    timezoneOffsetMinutes: 0,
    sourceUrl: null,
    startsAt: "2026-09-21T00:00:00.000Z",
  });
  assert.ok(!("issue" in built));
  if ("issue" in built) return;
  assert.equal(built.configuration.monthly, null);
  assert.equal(built.initialBuckets[0]?.kind, "manual");
  assert.equal(built.initialBuckets[0]?.expiresAt, null);
  assert.equal(built.initialBuckets[0]?.remaining, 15);
  assert.equal(built.initialBuckets[0]?.granted, 15);
});

test("generic monthly setup keeps grant independent from current remaining", () => {
  const built = buildInitialCreditConfigure({
    name: "lab",
    currency: "USD",
    remaining: 15,
    monthlyEnabled: true,
    monthlyAmount: 100,
    nextResetAt: "2026-10-01T00:00:00.000Z",
    timezoneOffsetMinutes: 0,
    sourceUrl: null,
    startsAt: "2026-09-21T00:00:00.000Z",
  });
  assert.ok(!("issue" in built));
  if ("issue" in built) return;
  assert.equal(built.configuration.monthly?.amount, 100);
  assert.equal(built.initialBuckets[0]?.kind, "monthly");
  assert.equal(built.initialBuckets[0]?.granted, 100);
  assert.equal(built.initialBuckets[0]?.remaining, 15);
  assert.equal(built.initialBuckets[0]?.expiresAt, "2026-10-01T00:00:00.000Z");
});

test("editing monthly on or off does not refill remaining", () => {
  const current = meter().configuration;
  const off = buildCreditSettingsConfiguration(current, {
    name: current.name,
    currency: current.currency,
    monthlyEnabled: false,
    monthlyAmount: current.monthly?.amount ?? null,
    nextResetAt: current.monthly?.nextResetAt ?? null,
    timezoneOffsetMinutes: CHINA_OFFSET_MINUTES,
  });
  assert.ok(!("issue" in off));
  if ("issue" in off) return;
  assert.equal(off.monthly, null);
  assert.equal("rates" in off, false);
  const on = buildCreditSettingsConfiguration(off, {
    name: off.name,
    currency: off.currency,
    monthlyEnabled: true,
    monthlyAmount: 50,
    nextResetAt: "2026-11-01T00:00:00.000Z",
    timezoneOffsetMinutes: 0,
  });
  assert.ok(!("issue" in on));
  if ("issue" in on) return;
  assert.equal(on.monthly?.amount, 50);
  assert.equal(on.monthly?.nextResetAt, "2026-11-01T00:00:00.000Z");
  assert.equal("rates" in on, false);
  assert.equal(JSON.stringify(on).includes("PerMillion"), false);
});

test("live nextResetAt is the caption boundary and can be absent", () => {
  assert.equal(meterNextResetAt(meter()), null);
  assert.equal(
    meterNextResetAt({ ...meter(), nextResetAt: "2026-10-01T16:00:00.000Z" }),
    "2026-10-01T16:00:00.000Z",
  );
  assert.notEqual(meter().configuration.monthly?.nextResetAt, meterNextResetAt(meter()));
});

test("preset setup keeps the reviewable grant, monthly renewal, and expiry", () => {
  const preset: CreditPreset = {
    id: "mini",
    initialGrant: 400_000_000,
    configuration: meter().configuration,
  };
  const configuration = configurationFromPreset(preset, "2026-09-30T16:00:00.000Z");
  assert.equal(configuration.monthly?.amount, 400_000_000);
  assert.equal(configuration.monthly?.timezoneOffsetMinutes, CHINA_OFFSET_MINUTES);
  assert.equal(configuration.monthly?.renewalEndsAt, null);
  const initial = initialMonthlyBucket(configuration, 100_000_000, "2026-09-21T00:00:00.000Z");
  assert.equal(initial.remaining, 100_000_000);
  assert.equal(initial.granted, 400_000_000);
  assert.equal(initial.expiresAt, "2026-09-30T16:00:00.000Z");
  assert.equal(initial.kind, "monthly");
});

test("preset activation does not copy token rates into the ledger", () => {
  const preset: CreditPreset = {
    id: "mini",
    initialGrant: 400_000_000,
    configuration: meter().configuration,
  };
  const configuration = configurationFromPreset(preset, "2026-09-30T16:00:00.000Z");
  assert.equal(configuration.monthly?.amount, 400_000_000);
  assert.equal(configuration.name, "Mini");
  assert.equal("rates" in configuration, false);
  assert.equal("creditsPerCurrency" in configuration, false);
  assert.equal(JSON.stringify(configuration).includes("PerMillion"), false);
});

test("usage windows project from camelCase BillingStatus.usage through one presenter", () => {
  const raw = usage({
    quotaWindows: [{
      accountId: "acc-1",
      calibrationOffset: 0,
      limitValue: 100,
      observedAt: "2026-09-21T00:00:00.000Z",
      resetsAt: "2026-09-21T05:00:00.000Z",
      source: "local",
      startedAt: "2026-09-21T00:00:00.000Z",
      unit: "usd",
      updatedAt: "2026-09-21T01:00:00.000Z",
      used: 7,
      windowKind: "five_hours",
    }, {
      accountId: "acc-1",
      calibrationOffset: 0,
      limitValue: 200,
      observedAt: null,
      resetsAt: "2026-09-28T00:00:00.000Z",
      source: "local",
      startedAt: null,
      unit: "usd",
      updatedAt: "2026-09-21T01:00:00.000Z",
      used: 20,
      windowKind: "week",
    }],
  });
  const presented = presentProviderUsage(raw);
  assert.equal(presented.quota_windows[0]?.window_kind, "five_hours");
  assert.equal(presented.quota_windows[0]?.used, 7);
  const window = usageWindowFromProviderUsage(raw, "acc-1");
  assert.equal(window.window_5h, 7);
  assert.equal(window.window_week, 20);
  assert.equal(window.resets_in_5h, "2026-09-21T05:00:00.000Z");
  const calibrated = applyUsageCalibration(raw, "window_5h", {
    ...window,
    window_5h: 9,
    resets_in_5h: "2026-09-21T06:00:00.000Z",
  }, "2026-09-21T02:00:00.000Z");
  assert.equal(calibrated.quotaWindows[0]?.used, 9);
  assert.equal(calibrated.quotaWindows[0]?.resetsAt, "2026-09-21T06:00:00.000Z");
  assert.equal(calibrated.quotaWindows[1]?.used, 20);
});

test("an official percent window stays percent over 100 and an empty observation list stays empty", () => {
  const raw = usage({
    quotaWindows: [{
      accountId: "acc-1",
      calibrationOffset: 0,
      limitValue: 100,
      observedAt: "2026-09-21T00:00:00.000Z",
      resetsAt: "2026-09-22T00:00:00.000Z",
      source: "official",
      startedAt: "2026-09-21T00:00:00.000Z",
      unit: "percent",
      updatedAt: "2026-09-21T00:00:00.000Z",
      used: 37,
      windowKind: "five_hours",
    }],
  });
  const presented = presentProviderUsage(raw);
  assert.equal(presented.quota_windows.length, 1);
  assert.equal(presented.quota_windows[0]?.unit, "percent");
  assert.equal(presented.quota_windows[0]?.used, 37);
  assert.equal(presented.quota_windows[0]?.limit_value, 100);
  assert.equal(presentProviderUsage(usage()).quota_windows.length, 0);
});

test("a missing usage window is unavailable instead of a zero observation", () => {
  const raw = usage({
    quotaWindows: [{
      accountId: "acc-1",
      calibrationOffset: 0,
      limitValue: 100,
      observedAt: null,
      resetsAt: "2026-09-28T00:00:00.000Z",
      source: "manual",
      startedAt: null,
      unit: "percent",
      updatedAt: "2026-09-21T01:00:00.000Z",
      used: 20,
      windowKind: "week",
    }],
  });
  const window = usageWindowFromProviderUsage(raw, "acc-1");
  assert.equal(window.window_week, 20);
  assert.equal(window.window_5h, null);
  assert.equal(window.window_month, null);
  const blank = usageWindowFromProviderUsage(undefined, "acc-missing");
  assert.equal(blank.account_id, "acc-missing");
  assert.equal(blank.window_5h, null);
  assert.equal(blank.window_week, null);
  assert.equal(blank.window_month, null);
});

test("revalidation errors overlay a last snapshot; first failure replaces content", () => {
  assert.equal(billingPanelMode({
    status: null,
    loaded: false,
    loading: true,
    error: null,
  }), "initial_loading");
  assert.equal(billingPanelMode({
    status: null,
    loaded: false,
    loading: false,
    error: "load_failed",
  }), "initial_error");
  assert.equal(billingPanelMode({
    status: status({ model: "cash" }),
    loaded: true,
    loading: false,
    error: "conflict",
  }), "ready");
  assert.equal(billingPanelOverlayError({
    status: status({ model: "cash" }),
    error: "conflict",
  }), "conflict");
  assert.equal(billingPanelOverlayError({
    status: null,
    error: "load_failed",
  }), null);
});

test("calibration consumes the server block rather than reinterpreting request counters", () => {
  assert.equal(creditCalibrationBlock({ calibrationBlock: null }), null);
  assert.equal(creditCalibrationBlock({ calibrationBlock: "pending" }), "pending");
  assert.equal(creditCalibrationBlock(meter({ pendingRequests: 2, calibrationBlock: null })), null);
});

test("quota calibration consumes canonical eligibility", () => {
  assert.equal(billingManualCalibration(status({ quotaManualCalibration: true, manualCalibration: false })), true);
  assert.equal(billingManualCalibration(status({ quotaManualCalibration: false, manualCalibration: true })), false);
});

test("binding identity includes account version and endpoint", () => {
  assert.equal(
    billingBinding("v1", "https://api.stepfun.com/step_plan"),
    billingBinding("v1", "https://api.stepfun.com/step_plan"),
  );
  assert.notEqual(
    billingBinding("v1", "https://api.stepfun.com/step_plan"),
    billingBinding("v2", "https://api.stepfun.com/step_plan"),
  );
  assert.notEqual(
    billingBinding("v1", "https://api.stepfun.com/step_plan"),
    billingBinding("v1", "https://api.stepfun.com/v1"),
  );
});

test("a manual receipt replaces its acknowledged kind and preserves known sibling rows", () => {
  const canonical = presentProviderUsage(usage({ quotaWindows: [
    { accountId: "acc-1", windowKind: "monthly", used: 12, limitValue: 60, unit: "usd_credits", source: "local", startedAt: null, resetsAt: null, calibrationOffset: 0, observedAt: null, updatedAt: "2026-10-07T00:00:00Z" },
    { accountId: "acc-1", windowKind: "week", used: 22, limitValue: 100, unit: "percent", source: "official", startedAt: null, resetsAt: null, calibrationOffset: 0, observedAt: "2026-10-07T00:00:00Z", updatedAt: "2026-10-07T00:00:00Z" },
  ] }));
  const receipt = { windows: [{ windowKind: "month", used: 0, limitValue: 100 as const, unit: "percent" as const, source: "manual" as const, observedAt: "2026-10-07T01:00:00Z", resetsAt: null, updatedAt: "2026-10-07T01:00:00Z" }] };
  const rows = manualReceiptQuotaView(receipt, "acc-1", canonical)!.quota_windows;
  assert.equal(rows.length, 2);
  assert.equal(rows[0]?.used, 0);
  assert.equal(rows[0]?.unit, "percent");
  assert.equal(rows[1], canonical.quota_windows[1]);
  assert.deepEqual(manualReceiptQuotaView(receipt, "acc-1")!.quota_windows.map(row => row.window_kind), ["month"]);
});
