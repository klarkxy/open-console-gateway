import assert from "node:assert/strict";
import nodeTest, { type TestContext } from "node:test";

// Shared billing client: one test body at a time so session guards stay isolated.
let lane: Promise<void> = Promise.resolve();
function test(
  name: string,
  body: (t: TestContext) => Promise<void> | void,
  timeout?: number,
): Promise<void> {
  const execute = (t: TestContext) => {
    const run = lane.then(() => body(t));
    lane = run.then(() => undefined, () => undefined);
    return run;
  };
  if (timeout === undefined) return nodeTest(name, execute);
  return nodeTest(name, { timeout }, execute);
}
import { createPinia, setActivePinia } from "pinia";
import { effectScope, ref } from "vue";
import { dashboardApi, type Account } from "../api/dashboard.ts";
type UsageWindow = Awaited<ReturnType<typeof dashboardApi.updateAccountUsage>>;
import { billingApi, type BillingStatus, type CreditMeterView } from "../api/billing.ts";
import type { OfficialApiStatus } from "../api/generated/dashboard-v4.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { useBillingStore } from "../stores/billing.ts";
import { useAccountUsage } from "./useAccountUsage.ts";

const flush = () => new Promise<void>(resolve => setImmediate(resolve));

const RESET_AT = "2026-10-01T00:30:00.000Z";
const FRESH_RESET_AT = "2026-10-01T01:00:00.000Z";
const CALIBRATION_NOW = Date.parse("2026-10-01T00:00:00.000Z");
const WEEK_FULL_MINUTES = 7 * 24 * 60;
const WEEK_RESET = new Date(CALIBRATION_NOW + WEEK_FULL_MINUTES * 60_000).toISOString();
const QUOTA_WINDOW_KEYS = [
  "limitValue",
  "observedAt",
  "resetsAt",
  "source",
  "unit",
  "updatedAt",
  "used",
  "windowKind",
];

function commandCatalog(): ProviderCatalogEntry {
  return {
    provider_id: "command-code",
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: "command-code",
    display_family: "command-code",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    creation_unavailable_reason: null,
    verification_policy: "not_required",
    verification_runtime_availability: "not_applicable",
    routable: true,
    managed_registration: false,
    usage_availability: "available",
    manual_usage_calibration: true,
    quota_unit: "percent",
    model_source: "builtin",
    key_prefix: null,
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
  } as ProviderCatalogEntry;
}

function commandAccount(updatedAt = "v1"): Account {
  return {
    id: "cc",
    name: "cc",
    provider_id: "command-code",
    updated_at: updatedAt,
    enabled: true,
    setup_step: "ready",
  } as Account;
}

function percentWindow(accountId: string, windowKind: string, used: number, resetsAt: string | null = null) {
  return {
    accountId,
    windowKind,
    used,
    limitValue: 100,
    startedAt: null,
    resetsAt,
    calibrationOffset: 0,
    unit: "percent",
    source: "manual",
    observedAt: "2026-10-01T00:00:00.000Z",
    updatedAt: "2026-10-01T00:00:00.000Z",
  };
}

function zeroCash(accountId: string): OfficialApiStatus {
  return {
    accountId,
    balanceAvailable: true,
    meter: { remainingEmpty: null, remaining: [{ currency: "CNY", total: 0, gift: null, observedAt: "2026-10-01T00:00:00.000Z" }] },
    balances: [{
      currency: "CNY",
      granted: 0,
      observedAt: "2026-10-01T00:00:00.000Z",
      toppedUp: 0,
      total: 0,
    }],
    kind: "deepseek",
    lifetimeSpend: [],
    monthSpend: [],
    monthStartedAt: "2026-09-01T00:00:00.000Z",
    processGeneration: 1,
    providerId: "command-code",
    revision: 9,
    unpricedRequests: 0,
  };
}

function zeroCredits(): CreditMeterView {
  return {
    credentialId: "cred-cc",
    meterId: "meter-cc",
    configuration: {
      name: "Zero",
      currency: "CNY",
      creditsPerCurrency: 1,
      rates: [],
      monthly: {
        amount: 0,
        nextResetAt: "2026-10-31T16:00:00.000Z",
        timezoneOffsetMinutes: 480,
        renewalEndsAt: null,
      },
      sourceUrl: null,
    },
    buckets: [],
    expiredBuckets: [],
    scheduledBuckets: [],
    canCalibrate: false,
    calibrationBlock: null,
    remaining: 0,
    activeGranted: 0,
    spentSinceCalibration: 0,
    overdrawn: 0,
    unpricedRequests: 0,
    pendingRequests: 0,
    lastCalibrationAt: null,
    estimatedAt: "2026-10-01T00:00:00.000Z",
    nextResetAt: null,
  };
}

function commandStatus(
  accountId: string,
  windows: ReturnType<typeof percentWindow>[],
  money = false,
): BillingStatus {
  return {
    accountId,
    model: money ? "credits" : "quota",
    surfaceKind: money ? "credits_meter" : "quota",
    providerWindows: true,
    quotaManualCalibration: !money,
    quotaEditorLimits: ["five_hours" as const, "week" as const, "month" as const].map(windowKind => ({ windowKind, limit: 100, editable: true, editableAt: null })),
    source: windows.length === 0 ? "unavailable" : "official",
    unit: money ? "credits" : "percent",
    configurableCredits: money,
    manualCalibration: true,
    officialRefresh: true,
    cash: money ? zeroCash(accountId) : null,
    credits: money ? zeroCredits() : null,
    presets: [],
    revision: money ? 9 : 3,
    processGeneration: 1,
    usage: {
      accountId,
      providerId: "command-code",
      availability: "available",
      creditBalances: money ? [{
        accountId,
        amount: 0,
        balanceKind: "wallet",
        observedAt: "2026-10-01T00:00:00.000Z",
        source: "official",
        unit: "CNY",
        updatedAt: "2026-10-01T00:00:00.000Z",
      }] : [],
      experimental: false,
      freeCooldownUntil: null,
      pricingRevision: null,
      processGeneration: 1,
      revision: money ? 9 : 3,
      syncState: null,
      quotaWindows: windows,
    },
  };
}

function calibrationAck(
  id: string,
  window: string,
  percent: number,
  resets?: number | null,
): UsageWindow {
  const resetAt = typeof resets === "number" && Number.isFinite(resets)
    ? new Date(CALIBRATION_NOW + resets * 60_000).toISOString()
    : null;
  return {
    observed_at: "2026-10-01T00:00:00.000Z",
    account_id: id,
    window_5h: window === "window_5h" ? percent : null,
    window_week: window === "window_week" ? percent : null,
    window_month: window === "window_month" ? percent : null,
    resets_in_5h: window === "window_5h" ? resetAt : null,
    resets_in_week: window === "window_week" ? resetAt : null,
    resets_in_month: window === "window_month" ? resetAt : null,
  };
}

type StatusRelease = (status: BillingStatus) => void;

function watchSettle(promise: Promise<void>) {
  let settled = false;
  let error: unknown;
  const done = promise.then(
    () => { settled = true; },
    (reason: unknown) => { settled = true; error = reason; },
  );
  return { done, settled: () => settled, error: () => error };
}

function presentedCalibration(
  usage: ReturnType<typeof useAccountUsage>,
  billing: ReturnType<typeof useBillingStore>,
  id: string,
) {
  const observed = usage.getUsage(id);
  const status = billing.slotFor(id).value?.status ?? null;
  const rows = billing.slotFor(id).value?.manualReceipt?.windows ?? status?.usage?.quotaWindows ?? [];
  return {
    percent: observed.window_5h,
    week: observed.window_week,
    month: observed.window_month,
    reset: observed.resets_in_5h,
    weekReset: observed.resets_in_week,
    monthReset: observed.resets_in_month,
    model: status?.model ?? null,
    cash: status?.cash ?? null,
    credits: status?.credits ?? null,
    presets: status?.presets.length ?? 0,
    balances: status?.usage?.creditBalances.length ?? 0,
    kinds: rows.map((row) => `${row.windowKind}:${row.used}`),
  };
}

async function mountCommandCode(t: TestContext, initial: BillingStatus | null) {
  setActivePinia(createPinia());
  const account = commandAccount();
  const accounts = ref<Account[]>([account]);
  const posts: Array<{ id: string; window: string; percent: number; resets: number | null | undefined }> = [];
  const reads: string[] = [];
  const pendingReads: StatusRelease[] = [];
  const pendingFailures: Array<(error: unknown) => void> = [];
  let servedInitial = false;
  t.mock.method(billingApi, "status", (id: string) => {
    reads.push(id);
    if (initial && !servedInitial && id === account.id) {
      servedInitial = true;
      return Promise.resolve(initial);
    }
    return new Promise<BillingStatus>((resolve, reject) => {
      pendingReads.push(resolve);
      pendingFailures.push(reject);
    });
  });
  t.mock.method(dashboardApi, "updateAccountUsage", async (id: string, window: string, percent: number, resets?: number | null) => {
    posts.push({ id, window, percent, resets });
    return calibrationAck(id, window, percent, resets);
  });
  const scope = effectScope();
  const usage = scope.run(() => useAccountUsage(
    accounts,
    ref(CALIBRATION_NOW),
    ref([commandCatalog()]),
    {
      message: { success: () => ({} as never), warning: () => ({} as never), error: () => ({} as never) },
      calibrationPlanFor: () => ({
        manual_calibration: true,
        windows: [{ kind: "five_hours" }, { kind: "week" }, { kind: "month" }],
      }),
    },
  ))!;
  t.after(() => scope.stop());
  return { account, accounts, posts, reads, pendingReads, pendingFailures, usage, billing: useBillingStore() };
}

function visibleReceipt(reads: number) {
  return {
    settled: true,
    saving: false,
    error: null,
    posts: 1,
    reads,
    percent: 42.5,
    week: null,
    month: null,
    reset: RESET_AT,
    weekReset: null,
    monthReset: null,
    model: "quota",
    cash: null,
    credits: null,
    presets: 0,
    balances: 0,
    kinds: ["five_hours:42.5"],
  };
}

async function beginWarmCalibration(t: TestContext) {
  const mounted = await mountCommandCode(t, commandStatus("cc", [percentWindow("cc", "five_hours", 10)]));
  await mounted.usage.loadAccountUsage("cc");
  assert.equal(mounted.usage.hasAvailableUsageEditor(mounted.account), true);
  assert.equal(mounted.usage.getUsage("cc").window_5h, 10);
  assert.equal(mounted.usage.getUsage("cc").window_week, null);
  assert.equal(mounted.usage.getUsage("cc").window_month, null);
  const read = mounted.usage.loadAccountUsage("cc");
  await flush();
  assert.equal(mounted.reads.length, 2);
  assert.equal(mounted.pendingReads.length, 1);
  assert.equal(mounted.usage.usageLoadingFor("cc").value, true);
  const edit = mounted.usage.usageEdits.value.cc?.window_5h;
  assert.ok(edit);
  mounted.usage.updateUsageDraft("cc", "window_5h", 42.5);
  mounted.usage.updateResetsFirstField("cc", "window_5h", 0);
  mounted.usage.updateResetsSecondField("cc", "window_5h", 30);
  assert.equal(edit.draft, 42.5);
  const watched = watchSettle(mounted.usage.saveUsage("cc", "window_5h"));
  assert.deepEqual(mounted.posts, [{ id: "cc", window: "window_5h", percent: 42.5, resets: 30 }]);
  await flush();
  await flush();
  assert.equal(mounted.pendingReads.length, 2);
  assert.equal(mounted.reads.length, 3);
  return { ...mounted, read, edit, ...watched };
}

function calibrationSnapshot(
  mounted: Awaited<ReturnType<typeof beginWarmCalibration>>,
) {
  return {
    settled: mounted.settled(),
    saving: mounted.edit.saving,
    error: mounted.error() ?? null,
    posts: mounted.posts.length,
    reads: mounted.reads.length,
    ...presentedCalibration(mounted.usage, mounted.billing, "cc"),
  };
}

function receiptWindows(billing: ReturnType<typeof useBillingStore>, id: string) {
  const receipt = billing.slotFor(id).value?.manualReceipt;
  return (receipt?.windows ?? []).map((row) => ({
    kind: row.windowKind,
    used: row.used,
    resetsAt: row.resetsAt,
    unit: row.unit,
    source: row.source,
    limit: row.limitValue,
  }));
}

function coldFragment(
  usage: ReturnType<typeof useAccountUsage>,
  billing: ReturnType<typeof useBillingStore>,
  id: string,
) {
  const observed = usage.getUsage(id);
  const slot = billing.slotFor(id).value;
  const receipt = slot?.manualReceipt ?? null;
  return {
    percent: observed.window_5h,
    week: observed.window_week,
    month: observed.window_month,
    reset: observed.resets_in_5h,
    weekReset: observed.resets_in_week,
    monthReset: observed.resets_in_month,
    presented: usage.providerUsageFor(id).value,
    status: slot?.status ?? null,
    loaded: slot?.loaded ?? null,
    slotError: slot?.error ?? null,
    receiptKeys: receipt ? Object.keys(receipt).sort() : [],
    windowKeys: (receipt?.windows ?? []).map((row) => Object.keys(row).sort()),
    windows: receiptWindows(billing, id),
  };
}

async function beginColdCalibration(t: TestContext, percent = 42.5) {
  // Capabilities are canonical, while every observation is still missing.
  const mounted = await mountCommandCode(t, capabilityStatus());
  await mounted.usage.loadAccountUsage("cc");
  const read = mounted.usage.loadAccountUsage("cc");
  await flush();
  assert.equal(mounted.usage.usageLoadingFor("cc").value, true);
  assert.equal(mounted.usage.hasAvailableUsageEditor(mounted.account), true);
  assert.equal(mounted.reads.length, 2);
  assert.equal(mounted.pendingReads.length, 1);
  mounted.usage.updateUsageDraft("cc", "window_5h", percent);
  mounted.usage.updateResetsFirstField("cc", "window_5h", 0);
  mounted.usage.updateResetsSecondField("cc", "window_5h", 30);
  const edit = mounted.usage.usageEdits.value.cc?.window_5h;
  assert.ok(edit);
  assert.equal(edit.draft, percent);
  const watched = watchSettle(mounted.usage.saveUsage("cc", "window_5h"));
  assert.deepEqual(mounted.posts, [{ id: "cc", window: "window_5h", percent, resets: 30 }]);
  await flush();
  await flush();
  assert.equal(mounted.reads.length, 3);
  assert.equal(mounted.pendingReads.length, 2);
  return { ...mounted, read, edit, ...watched };
}

test("missing canonical capabilities deny calibration despite a supporting catalog and plan", async (t) => {
  const mounted = await mountCommandCode(t, null);
  const read = mounted.usage.loadAccountUsage("cc");
  await flush();
  assert.equal(mounted.usage.hasAvailableUsageEditor(mounted.account), false);
  assert.deepEqual(mounted.usage.usageLimitsFor(mounted.account), []);
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  mounted.usage.updateUsageDraft("cc", "window_5h", 42.5);
  await mounted.usage.saveUsage("cc", "window_5h");
  assert.deepEqual(mounted.posts, []);
  assert.deepEqual(mounted.usage.usageEdits.value, {});
  mounted.pendingReads[0]!(commandStatus("cc", []));
  await read;
  assert.equal(mounted.usage.hasAvailableUsageEditor(mounted.account), true);
  assert.equal(mounted.usage.usageEdits.value.cc?.window_5h?.draft, null);
});

function capabilityStatus(): BillingStatus {
  return { ...commandStatus("cc", []), usage: null };
}

function acknowledgedFive(used: number) {
  return {
    percent: used,
    week: null,
    month: null,
    reset: RESET_AT,
    weekReset: null,
    monthReset: null,
    presented: null,
    status: capabilityStatus(),
    loaded: true,
    slotError: null,
    receiptKeys: ["windows"],
    windowKeys: [QUOTA_WINDOW_KEYS],
    windows: [{
      kind: "five_hours",
      used,
      resetsAt: RESET_AT,
      unit: "percent",
      source: "manual",
      limit: 100,
    }],
  };
}

test("a confirmed calibration stays visible when an older billing read returns no windows", async (t) => {
  const mounted = await beginWarmCalibration(t);
  assert.deepEqual(calibrationSnapshot(mounted), visibleReceipt(3));
  mounted.pendingReads[0]!(commandStatus("cc", [], true));
  await mounted.read;
  await mounted.done;
  await flush();
  assert.deepEqual(calibrationSnapshot(mounted), visibleReceipt(3));
  assert.equal(mounted.edit.saving, false);
}, 5_000);

test("a confirmed calibration stays visible when an older billing read returns a lower percent", async (t) => {
  const mounted = await beginWarmCalibration(t);
  assert.deepEqual(calibrationSnapshot(mounted), visibleReceipt(3));
  mounted.pendingReads[0]!(commandStatus("cc", [
    percentWindow("cc", "five_hours", 1),
    percentWindow("cc", "week", 0),
  ], true));
  await mounted.read;
  await mounted.done;
  await flush();
  assert.deepEqual(calibrationSnapshot(mounted), visibleReceipt(3));
  assert.equal(mounted.posts.length, 1);
}, 5_000);

test("the first explicit percent is visible before its observation revalidation returns", async (t) => {
  const mounted = await beginColdCalibration(t);
  assert.equal(mounted.settled(), true);
  assert.equal(mounted.edit.saving, false);
  assert.equal(mounted.billing.slotFor("cc").value?.loading, false);
  assert.deepEqual(coldFragment(mounted.usage, mounted.billing, "cc"), acknowledgedFive(42.5));
  mounted.pendingReads[0]!(commandStatus("cc", [
    percentWindow("cc", "five_hours", 1),
    percentWindow("cc", "week", 0),
  ], true));
  await mounted.read;
  await mounted.done;
  await flush();
  assert.equal(mounted.reads.length, 3);
  assert.equal(mounted.posts.length, 1);
  assert.deepEqual(coldFragment(mounted.usage, mounted.billing, "cc"), acknowledgedFive(42.5));
  const fresh = mounted.usage.loadAccountUsage("cc");
  await flush();
  assert.equal(mounted.reads.length, 4);
  mounted.pendingReads[2]!(commandStatus("cc", [
    percentWindow("cc", "five_hours", 55, FRESH_RESET_AT),
    percentWindow("cc", "week", 12),
  ]));
  await fresh;
  const observed = mounted.usage.getUsage("cc");
  assert.equal(observed.window_5h, 55);
  assert.equal(observed.window_week, 12);
  assert.equal(observed.window_month, null);
  assert.equal(observed.resets_in_5h, FRESH_RESET_AT);
  assert.equal(observed.resets_in_week, null);
  assert.equal(mounted.billing.slotFor("cc").value?.manualReceipt, null);
  assert.equal(mounted.billing.slotFor("cc").value?.loaded, true);
  assert.equal(mounted.billing.slotFor("cc").value?.status?.cash ?? null, null);
  assert.equal(mounted.billing.slotFor("cc").value?.status?.credits ?? null, null);
  assert.deepEqual(
    (mounted.billing.slotFor("cc").value?.status?.usage?.quotaWindows ?? []).map((row) => `${row.windowKind}:${row.used}`),
    ["five_hours:55", "week:12"],
  );
  assert.equal(mounted.posts.length, 1);
  assert.equal(mounted.reads.length, 4);
}, 5_000);

test("an older billing read error and its finally leave the quota fragment in place", async (t) => {
  const mounted = await beginColdCalibration(t);
  mounted.pendingFailures[0]!(new Error("offline"));
  await mounted.read;
  await mounted.done;
  await flush();
  assert.equal(mounted.settled(), true);
  assert.equal(mounted.edit.saving, false);
  assert.equal(mounted.reads.length, 3);
  assert.equal(mounted.posts.length, 1);
  assert.deepEqual(coldFragment(mounted.usage, mounted.billing, "cc"), acknowledgedFive(42.5));
  assert.equal(mounted.billing.slotFor("cc").value?.loading, false);
}, 5_000);

test("a later failed billing read keeps the known quota fragment", async (t) => {
  const mounted = await beginColdCalibration(t);
  mounted.pendingReads[0]!(commandStatus("cc", [], true));
  await mounted.read;
  const failed = mounted.usage.loadAccountUsage("cc");
  await flush();
  assert.equal(mounted.reads.length, 4);
  mounted.pendingFailures[2]!(new Error("offline"));
  await failed;
  await flush();
  assert.equal(mounted.posts.length, 1);
  assert.equal(mounted.reads.length, 4);
  assert.deepEqual(coldFragment(mounted.usage, mounted.billing, "cc"), {
    ...acknowledgedFive(42.5),
    slotError: "load_failed",
  });
}, 5_000);

test("two acknowledged windows coexist while the first billing read is pending", async (t) => {
  const mounted = await beginColdCalibration(t);
  mounted.usage.updateUsageDraft("cc", "window_week", 18);
  const week = watchSettle(mounted.usage.saveUsage("cc", "window_week"));
  await flush();
  await flush();
  await week.done;
  assert.deepEqual(mounted.posts, [
    { id: "cc", window: "window_5h", percent: 42.5, resets: 30 },
    { id: "cc", window: "window_week", percent: 18, resets: WEEK_FULL_MINUTES },
  ]);
  const observed = mounted.usage.getUsage("cc");
  assert.equal(observed.window_5h, 42.5);
  assert.equal(observed.window_week, 18);
  assert.equal(observed.window_month, null);
  assert.equal(observed.resets_in_5h, RESET_AT);
  assert.equal(observed.resets_in_week, WEEK_RESET);
  assert.equal(observed.resets_in_month, null);
  assert.equal(mounted.usage.providerUsageFor("cc").value, null);
  assert.equal(mounted.billing.slotFor("cc").value?.status?.usage, null);
  assert.equal(mounted.billing.slotFor("cc").value?.loaded, true);
  assert.deepEqual(receiptWindows(mounted.billing, "cc"), [
    {
      kind: "five_hours",
      used: 42.5,
      resetsAt: RESET_AT,
      unit: "percent",
      source: "manual",
      limit: 100,
    },
    {
      kind: "week",
      used: 18,
      resetsAt: WEEK_RESET,
      unit: "percent",
      source: "manual",
      limit: 100,
    },
  ]);
  assert.deepEqual(
    (mounted.billing.slotFor("cc").value?.manualReceipt?.windows ?? []).map((row) => Object.keys(row).sort()),
    [QUOTA_WINDOW_KEYS, QUOTA_WINDOW_KEYS],
  );
  assert.equal(receiptWindows(mounted.billing, "cc").some((row) => row.used === 0), false);
  assert.equal(mounted.reads.length, 4);
  mounted.pendingReads[0]!(commandStatus("cc", [percentWindow("cc", "five_hours", 1)], true));
  await mounted.read;
  await flush();
  assert.equal(mounted.usage.getUsage("cc").window_5h, 42.5);
  assert.equal(mounted.usage.getUsage("cc").window_week, 18);
  assert.equal(mounted.usage.getUsage("cc").window_month, null);
  assert.equal(mounted.usage.getUsage("cc").resets_in_5h, RESET_AT);
  assert.equal(mounted.usage.getUsage("cc").resets_in_week, WEEK_RESET);
  assert.equal(mounted.billing.slotFor("cc").value?.status?.usage, null);
  assert.equal(mounted.billing.slotFor("cc").value?.loaded, true);
  assert.equal(mounted.reads.length, 4);
  assert.equal(mounted.posts.length, 2);
}, 5_000);

test("an explicit zero on a cold slot stays zero and omitted windows stay unknown", async (t) => {
  const mounted = await beginColdCalibration(t, 0);
  assert.equal(mounted.settled(), true);
  assert.equal(mounted.edit.saving, false);
  assert.deepEqual(coldFragment(mounted.usage, mounted.billing, "cc"), acknowledgedFive(0));
  mounted.pendingReads[0]!(commandStatus("cc", [
    percentWindow("cc", "five_hours", 1),
    percentWindow("cc", "week", 0),
  ], true));
  await mounted.read;
  await mounted.done;
  await flush();
  assert.equal(mounted.reads.length, 3);
  assert.equal(mounted.posts.length, 1);
  assert.deepEqual(coldFragment(mounted.usage, mounted.billing, "cc"), acknowledgedFive(0));
}, 5_000);

test("logout between a cold calibration ack and the billing read drops the fragment", async (t) => {
  const mounted = await beginColdCalibration(t);
  mounted.billing.clear();
  mounted.pendingReads[0]!(commandStatus("cc", [percentWindow("cc", "five_hours", 42.5, RESET_AT)], true));
  await mounted.read;
  await mounted.done;
  await flush();
  await flush();
  assert.equal(mounted.reads.length, 3);
  assert.equal(mounted.posts.length, 1);
  assert.equal(mounted.billing.slotFor("cc").value, undefined);
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  assert.equal(mounted.usage.getUsage("cc").window_week, null);
  assert.equal(mounted.usage.usageEdits.value.cc, undefined);
  await flush();
  assert.equal(mounted.reads.length, 3);
}, 5_000);

test("removing the account between a cold calibration ack and the billing read drops the fragment", async (t) => {
  const mounted = await beginColdCalibration(t);
  mounted.accounts.value = [];
  mounted.pendingReads[0]!(commandStatus("cc", [percentWindow("cc", "five_hours", 42.5, RESET_AT)], true));
  await mounted.read;
  await mounted.done;
  await flush();
  await flush();
  assert.equal(mounted.reads.length, 3);
  assert.equal(mounted.posts.length, 1);
  assert.equal(mounted.billing.slotFor("cc").value, undefined);
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  await flush();
  assert.equal(mounted.reads.length, 3);
}, 5_000);

test("a binding change between a cold calibration ack and the billing read drops the fragment", async (t) => {
  const mounted = await beginColdCalibration(t);
  assert.deepEqual(coldFragment(mounted.usage, mounted.billing, "cc"), acknowledgedFive(42.5));
  mounted.accounts.value = [commandAccount("v2")];
  await flush();
  assert.equal(mounted.reads.length, 4);
  assert.equal(mounted.posts.length, 1);
  const oldRead = mounted.pendingReads[0];
  const newRead = mounted.pendingReads[2];
  assert.ok(oldRead);
  assert.ok(newRead);
  oldRead(commandStatus("cc", [percentWindow("cc", "five_hours", 42.5, RESET_AT)], true));
  await mounted.read;
  await mounted.done;
  await flush();
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  assert.equal(mounted.billing.slotFor("cc").value?.manualReceipt ?? null, null);
  assert.equal(mounted.billing.slotFor("cc").value?.status ?? null, null);
  assert.equal(mounted.billing.slotFor("cc").value?.loaded, false);
  assert.equal(mounted.reads.length, 4);
  newRead(commandStatus("cc", []));
  await flush();
  await flush();
  assert.equal(mounted.reads.length, 4);
  assert.equal(mounted.posts.length, 1);
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  assert.equal(mounted.usage.getUsage("cc").window_week, null);
  assert.equal(mounted.usage.usageEdits.value.cc?.window_5h?.saving, false);
  assert.notEqual(mounted.usage.usageEdits.value.cc?.window_5h?.saved, 42.5);
}, 5_000);

test("an explicit zero stays visible and omitted windows stay unknown", async (t) => {
  const mounted = await mountCommandCode(t, commandStatus("cc", []));
  await mounted.usage.loadAccountUsage("cc");
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  assert.equal(mounted.usage.usageEdits.value.cc?.window_5h?.draft, null);
  mounted.usage.updateUsageDraft("cc", "window_5h", 0);
  mounted.usage.updateResetsFirstField("cc", "window_5h", 0);
  mounted.usage.updateResetsSecondField("cc", "window_5h", 30);
  await mounted.usage.saveUsage("cc", "window_5h");
  assert.deepEqual(mounted.posts, [{ id: "cc", window: "window_5h", percent: 0, resets: 30 }]);
  assert.equal(mounted.reads.length, 2);
  const observed = mounted.usage.getUsage("cc");
  assert.equal(observed.window_5h, 0);
  assert.equal(observed.window_week, null);
  assert.equal(observed.window_month, null);
  assert.equal(observed.resets_in_5h, RESET_AT);
  assert.notEqual(observed.window_week, 0);
  const rows = mounted.billing.slotFor("cc").value?.manualReceipt?.windows ?? mounted.billing.slotFor("cc").value?.status?.usage?.quotaWindows ?? [];
  assert.deepEqual(rows.map((row) => `${row.windowKind}:${row.used}`), ["five_hours:0"]);
  assert.equal(mounted.billing.slotFor("cc").value?.status?.cash, null);
  assert.equal(mounted.billing.slotFor("cc").value?.status?.credits, null);
}, 5_000);

test("an untouched blank draft and a reset-only draft do not post", async (t) => {
  const mounted = await mountCommandCode(t, commandStatus("cc", []));
  await mounted.usage.loadAccountUsage("cc");
  const edits = mounted.usage.usageEdits.value.cc;
  assert.equal(edits?.window_5h?.draft, null);
  assert.equal(edits?.window_week?.draft, null);
  assert.equal(edits?.window_month?.draft, null);
  await mounted.usage.saveUsage("cc", "window_5h");
  assert.equal(mounted.posts.length, 0);
  mounted.usage.updateResetsFirstField("cc", "window_5h", 0);
  mounted.usage.updateResetsSecondField("cc", "window_5h", 30);
  assert.equal(edits?.window_5h?.draft, null);
  assert.equal(edits?.window_5h?.resets_dirty, true);
  await mounted.usage.saveUsage("cc", "window_5h");
  assert.equal(mounted.posts.length, 0);
  assert.equal(mounted.reads.length, 1);
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  assert.equal(mounted.usage.getUsage("cc").window_week, null);
  assert.equal(mounted.usage.getUsage("cc").window_month, null);
}, 5_000);

test("logout between the calibration ack and the billing read drops the fragment", async (t) => {
  const mounted = await beginWarmCalibration(t);
  mounted.billing.clear();
  mounted.pendingReads[0]!(commandStatus("cc", [percentWindow("cc", "five_hours", 42.5, RESET_AT)], true));
  await mounted.read;
  await mounted.done;
  await flush();
  await flush();
  assert.equal(mounted.reads.length, 3);
  assert.equal(mounted.posts.length, 1);
  assert.equal(mounted.billing.slotFor("cc").value, undefined);
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  assert.equal(mounted.usage.usageEdits.value.cc, undefined);
  await flush();
  assert.equal(mounted.reads.length, 3);
}, 5_000);

test("removing the account between the calibration ack and the billing read drops the fragment", async (t) => {
  const mounted = await beginWarmCalibration(t);
  mounted.accounts.value = [];
  mounted.pendingReads[0]!(commandStatus("cc", [percentWindow("cc", "five_hours", 42.5, RESET_AT)], true));
  await mounted.read;
  await mounted.done;
  await flush();
  await flush();
  assert.equal(mounted.reads.length, 3);
  assert.equal(mounted.posts.length, 1);
  assert.equal(mounted.billing.slotFor("cc").value, undefined);
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  await flush();
  assert.equal(mounted.reads.length, 3);
}, 5_000);

test("a binding change between the calibration ack and the billing read drops the fragment", async (t) => {
  const mounted = await beginWarmCalibration(t);
  mounted.accounts.value = [commandAccount("v2")];
  await flush();
  assert.equal(mounted.reads.length, 4);
  assert.equal(mounted.posts.length, 1);
  const oldRead = mounted.pendingReads[0];
  const newRead = mounted.pendingReads[2];
  assert.ok(oldRead);
  assert.ok(newRead);
  oldRead(commandStatus("cc", [percentWindow("cc", "five_hours", 42.5, RESET_AT)], true));
  await mounted.read;
  await mounted.done;
  await flush();
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  assert.equal(mounted.billing.slotFor("cc").value?.status?.cash ?? null, null);
  assert.equal(mounted.billing.slotFor("cc").value?.status?.credits ?? null, null);
  assert.equal(mounted.reads.length, 4);
  newRead(commandStatus("cc", []));
  await flush();
  await flush();
  assert.equal(mounted.reads.length, 4);
  assert.equal(mounted.posts.length, 1);
  assert.equal(mounted.usage.getUsage("cc").window_5h, null);
  assert.equal(mounted.usage.getUsage("cc").window_week, null);
  assert.equal(mounted.usage.usageEdits.value.cc?.window_5h?.saving, false);
  assert.notEqual(mounted.usage.usageEdits.value.cc?.window_5h?.saved, 42.5);
}, 5_000);
