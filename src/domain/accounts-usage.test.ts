import assert from "node:assert/strict";
import test from "node:test";
import { dashboardApi } from "../api/dashboard.ts";
import {
  installFetchMock,
  setupControlPlane,
} from "../test-helpers/dashboard-v3-fetch.ts";
import { mapWithConcurrency } from "../utils/async.ts";
import {
  isCooling,
  isFreeCooling,
  isUsageLimitReached,
  mergeCalibratedProviderUsage,
  mergeUsageEdit,
  normalizeUsagePercent,
  providerQuotaWindowLabel,
  resetTimeForWindow,
  resetsFieldsToMinutes,
  resetsFirstFieldMax,
  resetsFirstFieldValue,
  resetsInMinutesForSave,
  resetsSecondFieldMax,
  resetsSecondFieldValue,
  usagePercentFromCost,
  usageProgressPercentage,
  usageProgressStatus,
} from "./accounts-usage.ts";
import type { UsageEditState, UsageKey } from "./accounts-usage.ts";

function usageWindow() {
  return {
    accountId: "acc-1",
    pricingRevision: null,
    processGeneration: 99,
    resetsIn5h: null,
    resetsInMonth: null,
    resetsInWeek: null,
    revision: 7,
    window5h: 50,
    windowMonth: 10,
    windowWeek: 20,
  };
}

test("fills every active 5-hour, weekly, or monthly limit", () => {
  const cases: Array<[UsageKey, "cooldown_5h_until" | "cooldown_week_until" | "cooldown_month_until"]> = [
    ["window_5h", "cooldown_5h_until"],
    ["window_week", "cooldown_week_until"],
    ["window_month", "cooldown_month_until"],
  ];

  for (const [key, field] of cases) {
    assert.equal(
      isUsageLimitReached({
        cooldown_5h_until: field === "cooldown_5h_until" ? "2099-01-01T00:00:00Z" : null,
        cooldown_week_until: field === "cooldown_week_until" ? "2099-01-01T00:00:00Z" : null,
        cooldown_month_until: field === "cooldown_month_until" ? "2099-01-01T00:00:00Z" : null,
      }, key),
      true,
    );
  }
  assert.equal(
    isUsageLimitReached(
      {
        cooldown_5h_until: null,
        cooldown_week_until: "2099-01-01T00:00:00Z",
        cooldown_month_until: null,
      },
      "window_month",
    ),
    false,
  );
  assert.equal(
    isUsageLimitReached(
      {
        cooldown_5h_until: null,
        cooldown_week_until: "2000-01-01T00:00:00Z",
        cooldown_month_until: null,
      },
      "window_week",
    ),
    false,
  );
});


test("treats free promo cooldown as cooling without Go usage windows", () => {
  assert.equal(isFreeCooling({
    cooldown_free_until: "2099-01-01T00:00:00Z",
  }), true);
  assert.equal(isFreeCooling({
    cooldown_free_until: null,
  }), false);
  assert.equal(isCooling({
    cooldown_until: null,
    cooldown_5h_until: null,
    cooldown_week_until: null,
    cooldown_month_until: null,
    cooldown_free_until: "2099-01-01T00:00:00Z",
  }), true);
  assert.equal(isCooling({
    cooldown_until: null,
    cooldown_5h_until: null,
    cooldown_week_until: null,
    cooldown_month_until: null,
    cooldown_free_until: null,
  }), false);
});

test("keeps generic and overlapping window cooldowns visible", () => {
  assert.equal(isCooling({
    cooldown_until: "2099-01-01T00:00:00Z",
    cooldown_5h_until: null,
    cooldown_week_until: null,
    cooldown_month_until: null,
    cooldown_free_until: null,
  }), true);

  const overlapping = {
    cooldown_5h_until: "2099-01-01T00:00:00Z",
    cooldown_week_until: "2099-01-02T00:00:00Z",
    cooldown_month_until: null,
  };
  assert.equal(isUsageLimitReached(overlapping, "window_5h"), true);
  assert.equal(isUsageLimitReached(overlapping, "window_week"), true);
});

test("shows local estimated saturation as a warning, not a real breaker", () => {
  const available = {
    cooldown_5h_until: null,
    cooldown_week_until: null,
    cooldown_month_until: null,
  };
  const realWeeklyBreaker = {
    cooldown_5h_until: null,
    cooldown_week_until: "2099-01-01T00:00:00Z",
    cooldown_month_until: null,
  };

  assert.equal(
    usageProgressStatus(
      available,
      "window_week",
      100,
    ),
    "warning",
  );
  assert.equal(
    usageProgressStatus(
      realWeeklyBreaker,
      "window_week",
      0,
    ),
    "error",
  );
  assert.equal(usageProgressPercentage(available, "window_week", 100), 100);
  assert.equal(usageProgressPercentage(realWeeklyBreaker, "window_week", 0), 100);
});

test("maps each usage window to its cooldown reset deadline", () => {
  const account = {
    cooldown_5h_until: "2026-07-20T01:00:00Z",
    cooldown_week_until: "2026-07-21T01:00:00Z",
    cooldown_month_until: null,
  };
  assert.equal(resetTimeForWindow(account, "window_5h"), account.cooldown_5h_until);
  assert.equal(resetTimeForWindow(account, "window_week"), account.cooldown_week_until);
  assert.equal(resetTimeForWindow(account, "window_month"), null);
});

test("normalizes manually entered percentages to the supported range and precision", () => {
  assert.equal(normalizeUsagePercent(-1), 0);
  assert.equal(normalizeUsagePercent(42.56), 42.6);
  assert.equal(normalizeUsagePercent(101), 100);
  assert.equal(usagePercentFromCost(6, 12), 50);
  assert.equal(usagePercentFromCost(180, 100), 100);
});

test("manual calibration updates the visible provider window and preserves siblings", () => {
  const current = {
    account_id: "acc-1",
    provider_id: "command-code",
    availability: "local_state",
    quota_windows: [
      {
        account_id: "acc-1",
        window_kind: "five_hours",
        used: 10,
        limit_value: 14,
        started_at: null,
        resets_at: "2026-09-14T05:00:00Z",
        calibration_offset: 1,
        unit: "usd",
        source: "command-code-goat-local",
        observed_at: null,
        updated_at: "2026-09-14T00:00:00Z",
      },
      {
        account_id: "acc-1",
        window_kind: "week",
        used: 20,
        limit_value: 35,
        started_at: null,
        resets_at: null,
        calibration_offset: 2,
        unit: "usd",
        source: "command-code-goat-local",
        observed_at: null,
        updated_at: "2026-09-14T00:00:00Z",
      },
    ],
    credit_balances: [],
    sync_state: null,
  };
  const usage = {
    account_id: "acc-1",
    window_5h: 7,
    window_week: 21,
    window_month: 0,
    resets_in_5h: "2026-09-14T06:00:00Z",
    resets_in_week: null,
    resets_in_month: null,
    revision: 8,
    process_generation: 99,
    pricing_revision: null,
  };

  const merged = mergeCalibratedProviderUsage(
    current,
    "window_5h",
    usage,
    "2026-09-14T01:00:00Z",
  )!;
  assert.equal(merged.quota_windows[0]?.used, 7);
  assert.equal(merged.quota_windows[0]?.resets_at, "2026-09-14T06:00:00Z");
  assert.equal(merged.quota_windows[0]?.updated_at, "2026-09-14T01:00:00Z");
  assert.deepEqual(merged.quota_windows[1], current.quota_windows[1]);

  const weekUsage = {
    ...usage,
    window_week: 17.5,
    resets_in_week: "2026-09-20T00:00:00Z",
  };
  const afterConcurrentWindow = mergeCalibratedProviderUsage(
    merged,
    "window_week",
    weekUsage,
    "2026-09-14T01:00:01Z",
  )!;
  assert.equal(afterConcurrentWindow.quota_windows[0]?.used, 7);
  assert.equal(afterConcurrentWindow.quota_windows[1]?.used, 17.5);
  assert.equal(
    afterConcurrentWindow.quota_windows[1]?.resets_at,
    "2026-09-20T00:00:00Z",
  );
});

test("usage refresh preserves dirty drafts unless a real 429 reset that window", () => {
  const dirty: UsageEditState = {
    draft: 75,
    saved: 20,
    saving: false,
    error: "save failed",
    resets_in_minutes_draft: 240,
    resets_at_saved: "2099-01-01T00:00:00Z",
    resets_dirty: true,
  };

  assert.deepEqual(mergeUsageEdit(dirty, 35, false), {
    draft: 75,
    saved: 35,
    saving: false,
    error: "save failed",
    resets_in_minutes_draft: 240,
    resets_at_saved: "2099-01-01T00:00:00Z",
    resets_dirty: true,
  });
  assert.deepEqual(mergeUsageEdit(dirty, 0, true), {
    draft: 0,
    saved: 0,
    saving: false,
    error: null,
    resets_in_minutes_draft: 240,
    resets_at_saved: "2099-01-01T00:00:00Z",
    resets_dirty: true,
  });
  assert.deepEqual(mergeUsageEdit(undefined, 35, false), {
    draft: 35,
    saved: 35,
    saving: false,
    error: null,
    resets_in_minutes_draft: null,
    resets_at_saved: null,
    resets_dirty: false,
  });
});

test("percent-only usage saves keep counting down from the backend deadline", () => {
  const resetAt = "2026-07-19T12:05:30Z";
  const clean: UsageEditState = {
    draft: 50,
    saved: 40,
    saving: false,
    error: null,
    resets_in_minutes_draft: 6,
    resets_at_saved: resetAt,
    resets_dirty: false,
  };

  assert.equal(
    resetsInMinutesForSave(clean, "window_5h", Date.parse("2026-07-19T12:00:00Z")),
    5,
  );
  assert.equal(
    resetsInMinutesForSave(clean, "window_5h", Date.parse("2026-07-19T12:02:00Z")),
    3,
  );
  assert.equal(
    resetsInMinutesForSave({ ...clean, resets_in_minutes_draft: 240, resets_dirty: true }, "window_5h"),
    240,
  );
  assert.equal(
    resetsInMinutesForSave(clean, "window_5h", Date.parse("2026-07-19T12:05:00Z")),
    1,
  );
  assert.equal(
    resetsInMinutesForSave(clean, "window_5h", Date.parse("2026-07-19T12:06:00Z")),
    300,
  );
  assert.equal(
    resetsInMinutesForSave({ ...clean, resets_at_saved: "invalid" }, "window_5h"),
    300,
  );
  assert.equal(resetsInMinutesForSave(clean, "window_month"), null);
});

test("reset editor splits minutes into hour/minute or day/hour field pairs", () => {
  assert.equal(resetsFirstFieldMax("window_5h"), 5);
  assert.equal(resetsSecondFieldMax("window_5h"), 59);
  assert.equal(resetsFirstFieldMax("window_week"), 7);
  assert.equal(resetsSecondFieldMax("window_week"), 23);
  assert.equal(resetsFirstFieldMax("window_month"), 0);
  assert.equal(resetsSecondFieldMax("window_month"), 0);

  assert.equal(resetsFieldsToMinutes(1, 30, "window_5h"), 90);
  assert.equal(resetsFieldsToMinutes(1, 2, "window_week"), 1 * 24 * 60 + 2 * 60);
  assert.equal(resetsFieldsToMinutes(3, 4, "window_month"), 0);

  const dirty = { resets_in_minutes_draft: 90, resets_at_saved: null, resets_dirty: true };
  assert.equal(resetsFirstFieldValue(dirty, "window_5h"), 1);
  assert.equal(resetsSecondFieldValue(dirty, "window_5h"), 30);
  const weekly = { ...dirty, resets_in_minutes_draft: 1 * 24 * 60 + 2 * 60 };
  assert.equal(resetsFirstFieldValue(weekly, "window_week"), 1);
  assert.equal(resetsSecondFieldValue(weekly, "window_week"), 2);
  assert.equal(resetsFirstFieldValue(undefined, "window_5h"), 0);
  assert.equal(resetsSecondFieldValue(undefined, "window_week"), 0);
  assert.equal(resetsFirstFieldValue(dirty, "window_month"), 0);
});

test("bounded concurrency rejects invalid limits instead of dropping work", async () => {
  const worker = async (value: number) => value * 2;

  await assert.rejects(mapWithConcurrency([1], 0, worker), RangeError);
  await assert.rejects(mapWithConcurrency([1], -1, worker), RangeError);
  await assert.rejects(mapWithConcurrency([1], Number.NaN, worker), RangeError);
  await assert.rejects(mapWithConcurrency([1], 0.5, worker), RangeError);
});

test("usage API patches the selected window and percent, and refreshes with POST", async () => {
  setupControlPlane(7);
  const requests = installFetchMock(({ url, method }) => {
    if (method === "PATCH" && url.endsWith("/accounts/acc%201/usage")) {
      return { revision: 7, processGeneration: 99, usage: usageWindow() };
    }
    if (method === "POST" && url.endsWith("/accounts/acc%201/usage/refresh")) {
      return {
        lastSuccessAt: "2026-08-21T00:00:00Z",
        nextAllowedAt: "2026-08-21T00:01:00Z",
        processGeneration: 99,
        revision: 7,
        source: "official_go_usage",
        usage: usageWindow(),
      };
    }
    throw new Error(`unexpected ${method} ${url}`);
  });

  await dashboardApi.updateAccountUsage("acc 1", "window_week", 42, 15);
  await dashboardApi.refreshAccountUsage("acc 1");

  assert.equal(requests[0]?.url, "/dashboard/api/v3/accounts/acc%201/usage");
  assert.equal(requests[0]?.method, "PATCH");
  assert.deepEqual(requests[0]?.body, {
    window: "window_week",
    percent: 42,
    resetsInMinutes: 15,
    expectedRevision: 7,
    processGeneration: 99,
  });
  assert.equal(requests[1]?.url, "/dashboard/api/v3/accounts/acc%201/usage/refresh");
  assert.equal(requests[1]?.method, "POST");
  assert.deepEqual(requests[1]?.body, {
    expectedRevision: 7,
    processGeneration: 99,
  });
});

test("provider quota labels preserve known windows and humanize unknown scopes", () => {
  const labels = {
    fiveHours: "5 hours",
    week: "This week",
    month: "This month",
    hours: (count: number) => `${count} hours`,
  };
  assert.equal(providerQuotaWindowLabel({
    window_kind: "kimi_5h",
    started_at: null,
    resets_at: null,
  }, labels), "5 hours");
  assert.equal(providerQuotaWindowLabel({
    window_kind: "minimax_current:text_generation",
    started_at: "2026-09-14T00:00:00Z",
    resets_at: "2026-09-14T06:00:00Z",
  }, labels), "6 hours · Text generation");
  assert.equal(providerQuotaWindowLabel({
    window_kind: "future_burst_window",
    started_at: null,
    resets_at: null,
  }, labels), "Future burst window");
});
