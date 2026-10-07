import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { effectScope, watch } from "vue";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useBillingStore } from "./billing.ts";
import { billingApi, type BillingStatus, type CreditMeterView, type ProviderUsage } from "../api/billing.ts";
import type { OfficialApiStatus } from "../api/generated/dashboard-v4.ts";
import { billingBinding } from "../domain/billing.ts";
import type { ObservedUsageWindow } from "../domain/accounts-usage.ts";

interface DeferredCall {
  url: string;
  method: string;
  body: Record<string, unknown> | null;
  resolve: (body: object) => void;
  resolveHttp: (status: number, body: object) => void;
  reject: (error: unknown) => void;
}

function installDeferredFetch(): DeferredCall[] {
  installWindowDashboard();
  const calls: DeferredCall[] = [];
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: (input: string, init: RequestInit = {}) => new Promise<Response>((resolvePromise, rejectPromise) => {
      calls.push({
        url: String(input),
        method: init.method ?? "GET",
        body: init.body ? JSON.parse(String(init.body)) as Record<string, unknown> : null,
        resolve: (body) => resolvePromise(new Response(
          JSON.stringify(body),
          { headers: { "Content-Type": "application/json" } },
        )),
        resolveHttp: (status, body) => resolvePromise(new Response(
          JSON.stringify(body),
          { status, headers: { "Content-Type": "application/json" } },
        )),
        reject: (error) => rejectPromise(error),
      });
    }),
  });
  return calls;
}

async function waitForCalls(calls: DeferredCall[], count: number): Promise<void> {
  for (let i = 0; i < 200 && calls.length < count; i++) {
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.equal(calls.length, count, `expected ${count} fetch calls, saw ${calls.length}`);
}

function assertNoRetiredPriceRoute(calls: readonly DeferredCall[]): void {
  for (const call of calls) {
    assert.equal(
      /\/official-api\/pricing|\/pricing\/multipliers|\/providers\/[^/]+\/pricing(?:\?|$)/.test(call.url),
      false,
      call.url,
    );
  }
}

function credits(overrides: Partial<CreditMeterView> = {}): CreditMeterView {
  return {
    credentialId: "cred-1",
    meterId: "meter-1",
    configuration: {
      name: "Mini",
      currency: "CNY",
      creditsPerCurrency: 1_000_000,
      rates: [],
      monthly: {
        amount: 400_000_000,
        nextResetAt: "2026-09-30T16:00:00.000Z",
        timezoneOffsetMinutes: 480,
        renewalEndsAt: null,
      },
      sourceUrl: null,
    },
    expiredBuckets: [],
    scheduledBuckets: [],
    calibrationBlock: null,
    canCalibrate: false,
    buckets: [],
    remaining: 400_000_000,
    activeGranted: 400_000_000,
    spentSinceCalibration: 0,
    overdrawn: 0,
    unpricedRequests: 0,
    pendingRequests: 0,
    lastCalibrationAt: null,
    estimatedAt: "2026-09-21T00:00:00.000Z",
    nextResetAt: null,
    ...overrides,
  };
}

function billingStatus(overrides: Partial<BillingStatus> = {}): BillingStatus {
  return {
    accountId: "acc-1",
    surfaceKind: "credits_meter",
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

test("a per-account slot update does not invalidate another account's selector", async () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const original = billingApi.status;
  try {
    billingApi.status = async () => billingStatus();
    await store.load("acc-1", "v1");
    await store.load("acc-2", "v1");
  } finally {
    billingApi.status = original;
  }

  const scope = effectScope();
  let acc2Runs = 0;
  scope.run(() => {
    const slot = store.slotFor("acc-2");
    watch(slot, () => { acc2Runs += 1; }, { flush: "sync" });
  });
  const before = store.slotFor("acc-2").value;

  const calls = installDeferredFetch();
  const pending = store.load("acc-1", "v2");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({ revision: 5 }));
  await pending;

  assert.equal(acc2Runs, 0);
  assert.equal(store.slotFor("acc-2").value, before);
  assert.equal(store.slotFor("acc-1").value?.boundVersion, "v2");
  assert.equal(store.byId["acc-1"]?.status?.revision, 5);
  scope.stop();
});

test("slotFor tracks a slot added after the selector was created", async () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const late = store.slotFor("acc-late");
  const read = () => late.value;
  assert.equal(read(), undefined);
  const calls = installDeferredFetch();
  const pending = store.load("acc-late", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus());
  await pending;
  assert.equal(read()?.loaded, true);
});

test("a stale load cannot overwrite a newer snapshot, mutation, or cleared session", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const first = installDeferredFetch();
  const pendingFirst = store.load("acc-1", "v1");
  await waitForCalls(first, 1);

  const second = installDeferredFetch();
  const pendingSecond = store.load("acc-1", "v1");
  await waitForCalls(second, 1);
  second[0]!.resolve(billingStatus({
    credits: credits({ remaining: 200_000_000 }),
    revision: 4,
  }));
  await pendingSecond;
  assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 200_000_000);
  assert.equal(store.byId["acc-1"]?.status?.revision, 4);

  first[0]!.resolve(billingStatus({ credits: credits({ remaining: 400_000_000 }), revision: 3 }));
  await pendingFirst;
  assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 200_000_000);
  assert.equal(store.byId["acc-1"]?.status?.revision, 4);

  const third = installDeferredFetch();
  const pendingThird = store.load("acc-1", "v1");
  await waitForCalls(third, 1);
  store.clear();
  third[0]!.resolve(billingStatus({ revision: 5, credits: credits({ remaining: 1 }) }));
  await pendingThird;
  assert.equal(store.byId["acc-1"], undefined);
});

test("account version change and a later mutation reject earlier loads", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const first = installDeferredFetch();
  const pendingFirst = store.load("acc-1", "v1");
  await waitForCalls(first, 1);

  const second = installDeferredFetch();
  const pendingSecond = store.load("acc-1", "v2");
  await waitForCalls(second, 1);
  second[0]!.resolve(billingStatus({ revision: 8, credits: credits({ remaining: 10 }) }));
  await pendingSecond;
  assert.equal(store.byId["acc-1"]?.boundVersion, "v2");
  assert.equal(store.byId["acc-1"]?.status?.revision, 8);

  first[0]!.resolve(billingStatus({ revision: 3, credits: credits({ remaining: 400_000_000 }) }));
  await pendingFirst;
  assert.equal(store.byId["acc-1"]?.status?.revision, 8);
  assert.equal(store.byId["acc-1"]?.boundVersion, "v2");
});

test("same-binding revalidation keeps evidence; endpoint change is a new binding", async () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const original = billingApi.status;
  const plan = "https://api.stepfun.com/step_plan";
  const snapshot = () => billingStatus({ credits: credits({ remaining: 9 }) });
  try {
    billingApi.status = async () => snapshot();
    await store.load("acc-1", billingBinding("v1", plan));
    assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 9);

    let resolveSame: ((value: BillingStatus) => void) | undefined;
    billingApi.status = () => new Promise((resolve) => { resolveSame = resolve; });
    const same = store.load("acc-1", billingBinding("v1", plan));
    assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 9);
    resolveSame!(snapshot());
    await same;

    let resolveEndpoint: ((value: BillingStatus) => void) | undefined;
    billingApi.status = () => new Promise((resolve) => { resolveEndpoint = resolve; });
    const changed = store.load("acc-1", billingBinding("v1", `${plan}/v1`));
    assert.equal(store.byId["acc-1"]?.status, null);
    resolveEndpoint!({ ...snapshot(), credits: null });
    await changed;
  } finally {
    billingApi.status = original;
  }
});

test("unrelated revision advance 409s once, GETs billing, and the next explicit action uses the fresh revision", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
    credits: credits({ remaining: 10 }),
    revision: 3,
  }));
  await pendingLoad;
  useControlPlaneStore().sync({ revision: 6, processGeneration: 99 });

  const pendingCalibrate = store.calibrateCredits("acc-1", "v1", [
    { bucketId: "monthly", remaining: 8 },
  ]).then(() => "ok", (error: unknown) => error);
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "POST");
  assert.match(calls[1]!.url, /\/billing\/credits\/calibrate$/);
  assert.equal(calls[1]!.body?.expectedRevision, 3);
  calls[1]!.resolveHttp(409, {
    code: "revisionConflict",
    message: "conflict",
    currentRevision: 6,
    processGeneration: 99,
  });
  await waitForCalls(calls, 3);
  assert.match(calls[2]!.url, /\/contract$/);
  calls[2]!.resolve({ revision: 6, processGeneration: 99, pricingRevision: null });
  await waitForCalls(calls, 4);
  assert.equal(calls[3]!.method, "GET");
  assert.match(calls[3]!.url, /\/accounts\/acc-1\/billing$/);
  assert.equal(calls[3]!.url.includes("/calibrate"), false);
  calls[3]!.resolve(billingStatus({
    credits: credits({ remaining: 10 }),
    revision: 6,
  }));
  const first = await pendingCalibrate;
  assert.notEqual(first, "ok");
  assert.equal(calls.filter((call) => call.method === "POST").length, 1);
  assert.equal(store.byId["acc-1"]?.status?.revision, 6);
  assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 10);
  assert.equal(store.byId["acc-1"]?.error, "conflict");

  const pendingRetry = store.calibrateCredits("acc-1", "v1", [
    { bucketId: "monthly", remaining: 8 },
  ]);
  await waitForCalls(calls, 5);
  assert.equal(calls[4]!.method, "POST");
  assert.match(calls[4]!.url, /\/billing\/credits\/calibrate$/);
  assert.equal(calls[4]!.body?.expectedRevision, 6);
  calls[4]!.resolve(billingStatus({
    credits: credits({ remaining: 8 }),
    revision: 7,
  }));
  await pendingRetry;
  assert.equal(store.byId["acc-1"]?.status?.revision, 7);
  assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 8);
  assert.equal(store.byId["acc-1"]?.error, null);
  assert.equal(calls.filter((call) => call.method === "POST").length, 2);
});

test("failed conflict GET keeps the snapshot and the next action loads status before sending", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
    credits: credits({ remaining: 4 }),
    revision: 3,
  }));
  await pendingLoad;
  useControlPlaneStore().sync({ revision: 6, processGeneration: 99 });

  const pendingGrant = store.grantCredits("acc-1", "v1", {
    label: "topup",
    amount: 400_000_000,
    expiresAt: "2026-10-21T00:00:00.000Z",
  }).then(() => "ok", (error: unknown) => error);
  await waitForCalls(calls, 2);
  calls[1]!.resolveHttp(409, {
    code: "revisionConflict",
    message: "conflict",
    currentRevision: 6,
    processGeneration: 99,
  });
  await waitForCalls(calls, 3);
  calls[2]!.resolve({ revision: 6, processGeneration: 99, pricingRevision: null });
  await waitForCalls(calls, 4);
  calls[3]!.reject(new TypeError("Failed to fetch"));
  await pendingGrant;
  assert.equal(store.byId["acc-1"]?.status?.revision, 3);
  assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 4);
  assert.equal(store.byId["acc-1"]?.error, "conflict");
  assert.equal(calls.filter((call) => call.method === "POST").length, 1);

  const pendingRetry = store.grantCredits("acc-1", "v1", {
    label: "topup",
    amount: 400_000_000,
  });
  await waitForCalls(calls, 5);
  assert.equal(calls[4]!.method, "GET");
  assert.match(calls[4]!.url, /\/accounts\/acc-1\/billing$/);
  calls[4]!.resolve(billingStatus({
    credits: credits({ remaining: 4 }),
    revision: 6,
  }));
  await waitForCalls(calls, 6);
  assert.equal(calls[5]!.method, "POST");
  assert.equal(calls[5]!.body?.expectedRevision, 6);
  calls[5]!.resolve(billingStatus({ credits: credits({ remaining: 404 }), revision: 7 }));
  await pendingRetry;
  assert.equal(store.byId["acc-1"]?.status?.revision, 7);
  assert.equal(calls.filter((call) => call.method === "POST").length, 2);
});

test("configure commits the returned status; late GET cannot overwrite a saved calibration", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const original = billingApi.status;
  const originalConfigure = billingApi.configureCredits;
  try {
    billingApi.status = async () => billingStatus({ revision: 3 });
    await store.load("acc-1", "v1");
    let resolveLate: ((value: BillingStatus) => void) | undefined;
    billingApi.status = () => new Promise((resolve) => { resolveLate = resolve; });
    const late = store.load("acc-1", "v1");
    billingApi.configureCredits = async () => billingStatus({
      revision: 4,
      credits: credits({ remaining: 320_000_000 }),
    });
    await store.configureCredits("acc-1", "v1", {
      configuration: credits().configuration,
      initialBuckets: [],
    });
    assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 320_000_000);
    resolveLate!(billingStatus({ revision: 3, credits: credits({ remaining: 400_000_000 }) }));
    await late;
    assert.equal(store.byId["acc-1"]?.status?.revision, 4);
    assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 320_000_000);
  } finally {
    billingApi.status = original;
    billingApi.configureCredits = originalConfigure;
  }
});

test("clear is the dropSession epoch and rejects a late mutation", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pending = store.configureCredits("acc-1", "v1", {
    configuration: credits().configuration,
  });
  await waitForCalls(calls, 1);
  store.clear();
  calls[0]!.resolve(billingStatus({ revision: 12, credits: credits() }));
  await pending.catch(() => undefined);
  assert.equal(store.byId["acc-1"], undefined);
});

function providerUsage(overrides: Partial<ProviderUsage> = {}): ProviderUsage {
  return {
    accountId: "acc-1",
    availability: "available",
    creditBalances: [{
      accountId: "acc-1",
      amount: 12.5,
      balanceKind: "wallet",
      observedAt: "2026-09-21T00:00:00Z",
      source: "official",
      unit: "CNY",
      updatedAt: "2026-09-21T00:00:00Z",
    }],
    experimental: false,
    freeCooldownUntil: null,
    pricingRevision: null,
    processGeneration: 99,
    providerId: "stepfun",
    quotaWindows: [],
    revision: 4,
    syncState: null,
    ...overrides,
  };
}

function officialCash(overrides: Partial<OfficialApiStatus> = {}): OfficialApiStatus {
  return {
    accountId: "acc-1",
    balanceAvailable: true,
    meter: { remainingEmpty: "not_queried", remaining: [] },
    balances: [{ currency: "CNY", granted: 0, observedAt: "2026-09-21T00:00:00Z", toppedUp: 0, total: 20 }],
    kind: "deepseek",
    lifetimeSpend: [],
    monthSpend: [],
    monthStartedAt: "2026-09-01T00:00:00Z",
    processGeneration: 99,
    providerId: "deepseek",
    revision: 5,
    unpricedRequests: 0,
    ...overrides,
  };
}

test("cash with null OfficialApiStatus refreshes provider-usage and keeps last balances on failure", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
    surfaceKind: "cash_balances",
    quotaManualCalibration: false,
    providerWindows: true,
    quotaEditorLimits: [],
    model: "cash",
    officialRefresh: true,
    cash: null,
    usage: providerUsage({ revision: 3 }),
    revision: 3,
  }));
  await pendingLoad;
  assert.equal(store.byId["acc-1"]?.status?.usage?.creditBalances[0]?.amount, 12.5);

  const pendingRefresh = store.refreshCash("acc-1", "v1").then(() => "ok", (error: unknown) => error);
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "POST");
  assert.match(calls[1]!.url, /\/accounts\/acc-1\/provider-usage$/);
  assert.equal(calls[1]!.url.includes("official-api"), false);
  calls[1]!.resolveHttp(500, { message: "upstream" });
  const result = await pendingRefresh;
  assert.notEqual(result, "ok");
  assert.equal(store.byId["acc-1"]?.status?.usage?.creditBalances[0]?.amount, 12.5);
  assert.equal(store.byId["acc-1"]?.error, "load_failed");
  assertNoRetiredPriceRoute(calls);
});

test("cash with OfficialApiStatus refreshes the official balance endpoint", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
    surfaceKind: "cash",
    quotaManualCalibration: false,
    providerWindows: true,
    quotaEditorLimits: [],
    model: "cash",
    officialRefresh: true,
    cash: officialCash(),
    revision: 3,
  }));
  await pendingLoad;

  const pendingRefresh = store.refreshCash("acc-1", "v1");
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "POST");
  assert.match(calls[1]!.url, /\/accounts\/acc-1\/official-api\/balance$/);
  calls[1]!.resolve(officialCash({ revision: 6, balances: [{
    currency: "CNY",
    granted: 0,
    observedAt: "2026-09-21T01:00:00Z",
    toppedUp: 0,
    total: 18,
  }] }));
  await waitForCalls(calls, 3);
  assert.match(calls[2]!.url, /\/billing$/);
  calls[2]!.resolve(billingStatus({ surfaceKind: "cash", model: "cash", cash: officialCash({ revision: 6, balances: [{ currency: "CNY", granted: 0, observedAt: "2026-09-21T01:00:00Z", toppedUp: 0, total: 18 }] }), revision: 6 }));
  await pendingRefresh;
  assert.equal(store.byId["acc-1"]?.status?.surfaceKind, "cash");
  assert.equal(store.byId["acc-1"]?.status?.cash?.revision, 6);
  assert.equal(store.byId["acc-1"]?.status?.cash?.balances[0]?.total, 18);
  assertNoRetiredPriceRoute(calls);
});

test("the billing store does not expose active price reads or multiplier edits", () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  for (const key of [
    "loadPrices",
    "refreshPrices",
    "loadPricing",
    "pricesById",
    "priceSlotFor",
    "pricingLimits",
    "pricingLoading",
    "pricingError",
  ]) {
    assert.equal(key in store, false, key);
  }
});

test("a refresh replaces all billing facts and retains the prior snapshot when its canonical read fails", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const loaded = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  const original = billingStatus({ surfaceKind: "credits_usd_month", revision: 3 });
  calls[0]!.resolve(original);
  await loaded;
  const refreshed = store.refreshUsage("acc-1", "v1");
  await waitForCalls(calls, 2);
  calls[1]!.resolve(providerUsage({ revision: 4 }));
  await waitForCalls(calls, 3);
  assert.equal(store.byId["acc-1"]?.status?.surfaceKind, "credits_usd_month");
  const canonical = billingStatus({ surfaceKind: "quota", source: "official", quotaManualCalibration: true, revision: 4 });
  calls[2]!.resolve(canonical);
  await refreshed;
  assert.deepEqual(store.byId["acc-1"]?.status, canonical);
  const failed = store.refreshUsage("acc-1", "v1").catch(error => error);
  await waitForCalls(calls, 4);
  calls[3]!.resolve(providerUsage({ revision: 5 }));
  await waitForCalls(calls, 5);
  calls[4]!.reject(new Error("canonical read unavailable"));
  await failed;
  assert.deepEqual(store.byId["acc-1"]?.status, canonical);
  assert.equal(store.byId["acc-1"]?.resyncBeforeMutate, true);
  assert.equal(store.byId["acc-1"]?.error, "load_failed");
});

test("generic credit configure uses the credits PUT path", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
    surfaceKind: "cash_balances",
    quotaManualCalibration: false,
    providerWindows: true,
    quotaEditorLimits: [],
    model: "cash",
    configurableCredits: true,
    cash: null,
    revision: 3,
  }));
  await pendingLoad;
  const pending = store.configureCredits("acc-1", "v1", {
    configuration: credits().configuration,
    initialBuckets: [],
  });
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "PUT");
  assert.match(calls[1]!.url, /\/accounts\/acc-1\/billing\/credits$/);
  calls[1]!.resolve(billingStatus({
    surfaceKind: "credits_meter",
    quotaManualCalibration: false,
    providerWindows: true,
    quotaEditorLimits: [],
    model: "credits",
    credits: credits(),
    revision: 4,
  }));
  await pending;
  assert.equal(store.byId["acc-1"]?.status?.model, "credits");
});

test("a rejected credit calibration keeps the last good remaining and does not retry", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
    credits: credits({ remaining: 400_000_000, pendingRequests: 2 }),
    revision: 3,
  }));
  await pendingLoad;
  const pending = store.calibrateCredits("acc-1", "v1", [
    { bucketId: "monthly", remaining: 320_000_000 },
  ]).then(() => "ok", (error: unknown) => error);
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "POST");
  assert.match(calls[1]!.url, /\/billing\/credits\/calibrate$/);
  calls[1]!.resolveHttp(409, {
    code: "pendingRequests",
    message: "pending",
    currentRevision: 3,
    processGeneration: 99,
  });
  const result = await pending;
  assert.notEqual(result, "ok");
  assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 400_000_000);
  assert.equal(store.byId["acc-1"]?.status?.credits?.pendingRequests, 2);
  assert.equal(calls.filter((call) => call.url.includes("/calibrate")).length, 1);
});


test("initialization retries read back a committed ledger without replaying initial balances", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const input = { configuration: credits().configuration, initialBuckets: [] };
  const first = store.initializeCredits("acc-1", "v1", input);
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus());
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "PUT");
  assert.deepEqual(calls[1]!.body?.initialBuckets, []);
  calls[1]!.reject(new TypeError("response lost"));
  await assert.rejects(first);
  const retry = store.initializeCredits("acc-1", "v1", input);
  await waitForCalls(calls, 3);
  calls[2]!.resolve(billingStatus({ credits: credits({ remaining: 123 }), revision: 4 }));
  await retry;
  assert.equal(calls.length, 3);
  assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, 123);
});

test("initialization cannot start its write after logout or rebinding during its read", async () => {
  for (const transition of ["logout", "binding"] as const) {
    setActivePinia(createPinia());
    useControlPlaneStore().sync({ revision: 3, processGeneration: 99 });
    const store = useBillingStore();
    const calls = installDeferredFetch();
    const pending = store.initializeCredits("acc-1", "v1", { configuration: credits().configuration, initialBuckets: [] });
    await waitForCalls(calls, 1);
    let reload: Promise<void> | undefined;
    if (transition === "logout") store.clear();
    else { reload = store.load("acc-1", "v2"); await waitForCalls(calls, 2); }
    calls[0]!.resolve(billingStatus());
    await assert.rejects(pending);
    assert.ok(calls.every(call => call.method === "GET"));
    if (reload) { calls[1]!.resolve(billingStatus({ credits: credits({ remaining: 456 }) })); await reload; }
    assert.equal(store.byId["acc-1"]?.status?.credits?.remaining, transition === "logout" ? undefined : 456);
  }
});

const CALIBRATED_RESET = "2026-10-01T00:30:00.000Z";
const FRESH_RESET = "2026-10-01T01:00:00.000Z";

function calibratedWindow(accountId: string, percent: number): ObservedUsageWindow {
  return {
    account_id: accountId,
    window_5h: percent,
    window_week: null,
    window_month: null,
    resets_in_5h: CALIBRATED_RESET,
    resets_in_week: null,
    resets_in_month: null,
  };
}

function quotaBilling(
  accountId: string,
  windows: ReadonlyArray<{ kind: string; used: number; resetsAt?: string | null }>,
  overrides: Partial<BillingStatus> = {},
): BillingStatus {
  return billingStatus({
    accountId,
    surfaceKind: "quota",
    quotaManualCalibration: true,
    providerWindows: true,
    quotaEditorLimits: [],
    model: "quota",
    source: windows.length === 0 ? "unavailable" : "official",
    unit: "percent",
    configurableCredits: false,
    manualCalibration: true,
    officialRefresh: true,
    cash: null,
    credits: null,
    presets: [],
    revision: 3,
    processGeneration: 99,
    usage: {
      accountId,
      providerId: "command-code",
      availability: "available",
      creditBalances: [],
      experimental: false,
      freeCooldownUntil: null,
      pricingRevision: null,
      processGeneration: 99,
      revision: 3,
      syncState: null,
      quotaWindows: windows.map((window) => ({
        accountId,
        calibrationOffset: 0,
        limitValue: 100,
        observedAt: "2026-10-01T00:00:00.000Z",
        resetsAt: window.resetsAt ?? null,
        source: "manual",
        startedAt: null,
        unit: "percent",
        updatedAt: "2026-10-01T00:00:00.000Z",
        used: window.used,
        windowKind: window.kind,
      })),
    },
    ...overrides,
  });
}

function moneyPoison(accountId: string, windows: ReadonlyArray<{ kind: string; used: number }>): BillingStatus {
  return quotaBilling(accountId, windows, {
    surfaceKind: "credits_meter",
    quotaManualCalibration: false,
    providerWindows: true,
    quotaEditorLimits: [],
    model: "credits",
    unit: "credits",
    configurableCredits: true,
    revision: 9,
    cash: officialCash({
      accountId,
      balances: [{
        currency: "CNY",
        granted: 0,
        observedAt: "2026-10-01T00:00:00.000Z",
        toppedUp: 0,
        total: 0,
      }],
    }),
    credits: credits({ remaining: 0, activeGranted: 0 }),
    usage: {
      ...quotaBilling(accountId, windows).usage!,
      creditBalances: [{
        accountId,
        amount: 0,
        balanceKind: "wallet",
        observedAt: "2026-10-01T00:00:00.000Z",
        source: "official",
        unit: "CNY",
        updatedAt: "2026-10-01T00:00:00.000Z",
      }],
      revision: 9,
    },
  });
}

function calibrationView(store: ReturnType<typeof useBillingStore>, accountId: string) {
  const slot = store.slotFor(accountId).value;
  const status = slot?.status ?? null;
  const rows = slot?.manualReceipt?.windows ?? status?.usage?.quotaWindows ?? [];
  return {
    loading: slot?.loading ?? null,
    surfaceKind: status?.surfaceKind ?? null,
    quotaManualCalibration: status?.quotaManualCalibration ?? false,
    providerWindows: status?.providerWindows ?? false,
    quotaEditorLimits: status?.quotaEditorLimits ?? [],
    model: status?.model ?? null,
    cash: status?.cash ?? null,
    credits: status?.credits ?? null,
    presets: status?.presets.length ?? 0,
    balances: status?.usage?.creditBalances.length ?? 0,
    windows: rows.map((row) => ({ kind: row.windowKind, used: row.used, resetsAt: row.resetsAt })),
    zeroUsed: rows.filter((row) => row.used === 0).length,
  };
}

function manualReceiptView() {
  return {
    loading: false,
    surfaceKind: "quota",
    quotaManualCalibration: true,
    providerWindows: true,
    quotaEditorLimits: [],
    model: "quota",
    cash: null,
    credits: null,
    presets: 0,
    balances: 0,
    windows: [{ kind: "five_hours", used: 42.5, resetsAt: CALIBRATED_RESET }],
    zeroUsed: 0,
  };
}

async function deferQuotaReread(accountId: string, used: number) {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const first = installDeferredFetch();
  const pendingFirst = store.load(accountId, "v1");
  await waitForCalls(first, 1);
  first[0]!.resolve(quotaBilling(accountId, [{ kind: "five_hours", used }]));
  await pendingFirst;
  const second = installDeferredFetch();
  const pendingSecond = store.load(accountId, "v1");
  await waitForCalls(second, 1);
  return { store, second, pendingSecond };
}

test("a calibration receipt stays visible when an older billing read returns no windows", { timeout: 5_000 }, async () => {
  const { store, second, pendingSecond } = await deferQuotaReread("acc-1", 10);
  assert.equal(store.slotFor("acc-1").value?.loading, true);
  store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 42.5), "2026-10-01T00:00:01.000Z");
  assert.deepEqual(calibrationView(store, "acc-1"), manualReceiptView());
  assert.equal(second.length, 2);
  second[0]!.resolve(moneyPoison("acc-1", []));
  await pendingSecond;
  assert.deepEqual(calibrationView(store, "acc-1"), manualReceiptView());
  assert.equal(second.length, 2);
});

test("a calibration receipt stays visible when an older billing read returns a lower percent", { timeout: 5_000 }, async () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const other = installDeferredFetch();
  const pendingOther = store.load("acc-2", "v2");
  await waitForCalls(other, 1);
  other[0]!.resolve(quotaBilling("acc-2", [{ kind: "five_hours", used: 10 }]));
  await pendingOther;
  const first = installDeferredFetch();
  const pendingFirst = store.load("acc-1", "v1");
  await waitForCalls(first, 1);
  first[0]!.resolve(quotaBilling("acc-1", [{ kind: "five_hours", used: 10 }]));
  await pendingFirst;
  const second = installDeferredFetch();
  const pendingSecond = store.load("acc-1", "v1");
  await waitForCalls(second, 1);
  const scope = effectScope();
  let acc2Runs = 0;
  try {
    scope.run(() => {
      watch(store.slotFor("acc-2"), () => { acc2Runs += 1; }, { flush: "sync" });
    });
    const before = store.slotFor("acc-2").value;
    store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 42.5), "2026-10-01T00:00:01.000Z");
    assert.equal(acc2Runs, 0);
    assert.equal(store.slotFor("acc-2").value, before);
    assert.deepEqual(calibrationView(store, "acc-1"), manualReceiptView());
    assert.equal(calibrationView(store, "acc-2").windows[0]?.used, 10);
    second[0]!.resolve(moneyPoison("acc-1", [
      { kind: "five_hours", used: 1 },
      { kind: "week", used: 0 },
    ]));
    await pendingSecond;
    assert.deepEqual(calibrationView(store, "acc-1"), manualReceiptView());
    assert.equal(acc2Runs, 0);
    assert.equal(store.slotFor("acc-2").value, before);
    assert.equal(second.length, 2);
  } finally {
    scope.stop();
  }
});

const ACK_AT = "2026-10-01T00:00:01.000Z";
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

function coldFragmentView(store: ReturnType<typeof useBillingStore>, accountId: string) {
  const slot = store.slotFor(accountId).value;
  const status = slot?.status ?? null;
  const receipt = slot?.manualReceipt ?? null;
  return {
    loading: slot?.loading ?? null,
    loaded: slot?.loaded ?? null,
    status,
    error: slot?.error ?? null,
    revision: status?.revision ?? null,
    processGeneration: status?.processGeneration ?? null,
    providerId: status?.usage?.providerId ?? null,
    cash: status?.cash ?? null,
    credits: status?.credits ?? null,
    receiptKeys: receipt ? Object.keys(receipt).sort() : [],
    windowKeys: (receipt?.windows ?? []).map((row) => Object.keys(row).sort()),
    windows: (receipt?.windows ?? []).map((row) => ({
      kind: row.windowKind,
      used: row.used,
      resetsAt: row.resetsAt,
      unit: row.unit,
      source: row.source,
      limit: row.limitValue,
      observedAt: row.observedAt,
      updatedAt: row.updatedAt,
    })),
  };
}

function coldQuotaFragment(used: number, resetsAt = CALIBRATED_RESET) {
  return {
    loading: false,
    loaded: false,
    status: null,
    error: null,
    revision: null,
    processGeneration: null,
    providerId: null,
    cash: null,
    credits: null,
    receiptKeys: ["windows"],
    windowKeys: [QUOTA_WINDOW_KEYS],
    windows: [{
      kind: "five_hours",
      used,
      resetsAt,
      unit: "percent" as const,
      source: "manual" as const,
      limit: 100 as const,
      observedAt: ACK_AT,
      updatedAt: ACK_AT,
    }],
  };
}

function observedWindow(accountId: string, window: Partial<ObservedUsageWindow>): ObservedUsageWindow {
  return {
    account_id: accountId,
    window_5h: null,
    window_week: null,
    window_month: null,
    resets_in_5h: null,
    resets_in_week: null,
    resets_in_month: null,
    ...window,
  };
}

async function startPendingRead(accountId = "acc-1", binding = "v1") {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pending = store.load(accountId, binding);
  await waitForCalls(calls, 1);
  assert.equal(store.slotFor(accountId).value?.status, null);
  assert.equal(store.slotFor(accountId).value?.loaded, false);
  assert.equal(store.slotFor(accountId).value?.loading, true);
  assert.equal(calls[0]?.method, "GET");
  return { store, calls, pending };
}

test("a calibration receipt is presented while the first billing read is still pending", { timeout: 5_000 }, async () => {
  const { store, calls, pending } = await startPendingRead();
  store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 42.5), ACK_AT);
  assert.equal(calls.length, 2);
  assert.deepEqual(coldFragmentView(store, "acc-1"), coldQuotaFragment(42.5));
  calls[0]!.resolve(moneyPoison("acc-1", [
    { kind: "five_hours", used: 1 },
    { kind: "week", used: 0 },
  ]));
  await pending;
  assert.equal(calls.length, 2);
  assert.deepEqual(coldFragmentView(store, "acc-1"), coldQuotaFragment(42.5));
  const fresh = installDeferredFetch();
  const pendingFresh = store.load("acc-1", "v1");
  await waitForCalls(fresh, 1);
  fresh[0]!.resolve(quotaBilling("acc-1", [
    { kind: "five_hours", used: 55, resetsAt: FRESH_RESET },
    { kind: "week", used: 12 },
  ]));
  await pendingFresh;
  assert.equal(store.slotFor("acc-1").value?.manualReceipt, null);
  assert.equal(store.slotFor("acc-1").value?.loaded, true);
  assert.deepEqual(calibrationView(store, "acc-1"), {
    loading: false,
    surfaceKind: "quota",
    quotaManualCalibration: true,
    providerWindows: true,
    quotaEditorLimits: [],
    model: "quota",
    cash: null,
    credits: null,
    presets: 0,
    balances: 0,
    windows: [
      { kind: "five_hours", used: 55, resetsAt: FRESH_RESET },
      { kind: "week", used: 12, resetsAt: null },
    ],
    zeroUsed: 0,
  });
  assert.equal(fresh.length, 1);
  assert.equal(fresh[0]?.method, "GET");
});

test("an older billing read error and its finally leave the quota fragment in place", { timeout: 5_000 }, async () => {
  const { store, calls, pending } = await startPendingRead();
  store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 42.5), ACK_AT);
  calls[0]!.reject(new Error("offline"));
  await pending;
  assert.equal(calls.length, 2);
  assert.deepEqual(coldFragmentView(store, "acc-1"), coldQuotaFragment(42.5));
});

test("a later failed billing read keeps the known quota fragment", { timeout: 5_000 }, async () => {
  const { store, calls, pending } = await startPendingRead();
  store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 42.5), ACK_AT);
  calls[0]!.resolve(moneyPoison("acc-1", []));
  await pending;
  const fresh = installDeferredFetch();
  const pendingFresh = store.load("acc-1", "v1");
  await waitForCalls(fresh, 1);
  fresh[0]!.reject(new Error("offline"));
  await pendingFresh;
  assert.equal(calls.length, 2);
  assert.equal(fresh.length, 1);
  assert.deepEqual(coldFragmentView(store, "acc-1"), {
    ...coldQuotaFragment(42.5),
    error: "load_failed",
  });
});

test("two acknowledged windows coexist while the first billing read is pending", { timeout: 5_000 }, async () => {
  const { store, calls, pending } = await startPendingRead();
  store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 42.5), ACK_AT);
  store.applyCalibratedUsage("acc-1", "v1", "window_week", observedWindow("acc-1", {
    window_week: 18,
    resets_in_week: "2026-10-08T00:00:00.000Z",
  }), ACK_AT);
  assert.equal(calls.length, 3);
  assert.equal(store.slotFor("acc-1").value?.status, null);
  assert.equal(store.slotFor("acc-1").value?.loaded, false);
  const fragment = coldFragmentView(store, "acc-1");
  assert.deepEqual(fragment.windows.map((row) => `${row.kind}:${row.used}`), ["five_hours:42.5", "week:18"]);
  assert.equal(fragment.windows.some((row) => row.used === 0), false);
  assert.deepEqual(fragment.windowKeys, [QUOTA_WINDOW_KEYS, QUOTA_WINDOW_KEYS]);
  assert.deepEqual(fragment.receiptKeys, ["windows"]);
  assert.equal(fragment.revision, null);
  assert.equal(fragment.processGeneration, null);
  assert.equal(fragment.providerId, null);
  assert.equal(fragment.cash, null);
  assert.equal(fragment.credits, null);
  calls[0]!.resolve(moneyPoison("acc-1", [
    { kind: "five_hours", used: 1 },
    { kind: "week", used: 0 },
  ]));
  await pending;
  assert.equal(calls.length, 3);
  assert.deepEqual(
    coldFragmentView(store, "acc-1").windows.map((row) => `${row.kind}:${row.used}`),
    ["five_hours:42.5", "week:18"],
  );
  assert.equal(store.slotFor("acc-1").value?.status, null);
  assert.equal(store.slotFor("acc-1").value?.loaded, false);
});

test("an explicit zero on a cold slot stays zero without inventing sibling windows", { timeout: 5_000 }, async () => {
  const { store, calls, pending } = await startPendingRead();
  store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 0), ACK_AT);
  assert.deepEqual(coldFragmentView(store, "acc-1"), coldQuotaFragment(0));
  calls[0]!.resolve(moneyPoison("acc-1", [
    { kind: "five_hours", used: 1 },
    { kind: "week", used: 0 },
  ]));
  await pending;
  assert.equal(calls.length, 2);
  assert.deepEqual(coldFragmentView(store, "acc-1"), coldQuotaFragment(0));
});

test("logout, remove, and a binding change drop a cold quota fragment", { timeout: 5_000 }, async () => {
  const loggedOut = await startPendingRead();
  loggedOut.store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 42.5), ACK_AT);
  loggedOut.store.clear();
  loggedOut.calls[0]!.resolve(moneyPoison("acc-1", [{ kind: "five_hours", used: 42.5 }]));
  await loggedOut.pending;
  assert.equal(loggedOut.store.slotFor("acc-1").value, undefined);
  assert.equal(loggedOut.calls.length, 2);

  const removed = await startPendingRead();
  removed.store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 42.5), ACK_AT);
  removed.store.remove("acc-1");
  removed.calls[0]!.resolve(moneyPoison("acc-1", [{ kind: "five_hours", used: 42.5 }]));
  await removed.pending;
  assert.equal(removed.store.slotFor("acc-1").value, undefined);
  assert.equal(removed.calls.length, 2);

  const rebound = await startPendingRead();
  rebound.store.applyCalibratedUsage("acc-1", "v1", "window_5h", calibratedWindow("acc-1", 42.5), ACK_AT);
  const next = installDeferredFetch();
  const pendingNext = rebound.store.load("acc-1", "v2");
  await waitForCalls(next, 1);
  assert.equal(rebound.store.slotFor("acc-1").value?.boundVersion, "v2");
  assert.equal(rebound.store.slotFor("acc-1").value?.manualReceipt, null);
  assert.equal(rebound.store.slotFor("acc-1").value?.status, null);
  assert.equal(rebound.store.slotFor("acc-1").value?.loaded, false);
  rebound.calls[0]!.resolve(moneyPoison("acc-1", [{ kind: "five_hours", used: 42.5 }]));
  await rebound.pending;
  assert.equal(rebound.calls.length, 2);
  assert.equal(next.length, 1);
  assert.equal(rebound.store.slotFor("acc-1").value?.manualReceipt, null);
  assert.equal(rebound.store.slotFor("acc-1").value?.status, null);
  next[0]!.resolve(quotaBilling("acc-1", []));
  await pendingNext;
  assert.equal(rebound.store.slotFor("acc-1").value?.manualReceipt, null);
  assert.equal(rebound.store.slotFor("acc-1").value?.loaded, true);
  assert.deepEqual(calibrationView(rebound.store, "acc-1").windows, []);
  assert.equal(next.length, 1);
});

test("batch reads retain good data on per-account errors and ignore logged-out replies", async () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const original = billingApi.snapshots;
  try {
    billingApi.snapshots = async ids => ({
      statuses: ids.map(accountId => billingStatus({ accountId })), errors: {}, revision: 3, processGeneration: 99,
    });
    await store.loadMany([{ accountId: "a", binding: "v1" }, { accountId: "b", binding: "v1" }]);
    const previous = store.slotFor("b").value?.status;
    billingApi.snapshots = async () => ({
      statuses: [billingStatus({ accountId: "a", revision: 4 })],
      errors: { b: { code: "internal_error", message: "failed", currentRevision: null, processGeneration: null } },
      revision: 4, processGeneration: 99,
    });
    await store.loadMany([{ accountId: "a", binding: "v1" }, { accountId: "b", binding: "v1" }]);
    assert.equal(store.slotFor("a").value?.status?.revision, 4);
    assert.equal(store.slotFor("b").value?.status, previous);
    assert.equal(store.slotFor("b").value?.error, "load_failed");
    let resolve!: (value: Awaited<ReturnType<typeof billingApi.snapshots>>) => void;
    billingApi.snapshots = () => new Promise(done => { resolve = done; });
    const pending = store.loadMany([{ accountId: "a", binding: "v2" }]);
    store.clear();
    resolve({ statuses: [billingStatus({ accountId: "a" })], errors: {}, revision: 3, processGeneration: 99 });
    await pending;
    assert.equal(store.slotFor("a").value, undefined);
  } finally { billingApi.snapshots = original; }
});


test("batch loading bounds requests and never overwrites a newer binding", async () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const oldSnapshots = billingApi.snapshots;
  const oldStatus = billingApi.status;
  try {
    const requests: string[][] = [];
    billingApi.snapshots = async ids => {
      requests.push(ids);
      return { statuses: ids.map(accountId => billingStatus({ accountId })), errors: {}, revision: 3, processGeneration: 99 };
    };
    await store.loadMany(Array.from({ length: 65 }, (_, i) => ({ accountId: `a${i}`, binding: "v1" })));
    assert.deepEqual(requests.map(ids => ids.length), [32, 32, 1]);
    let resolve!: (value: Awaited<ReturnType<typeof billingApi.snapshots>>) => void;
    billingApi.snapshots = () => new Promise(done => { resolve = done; });
    const oldRead = store.loadMany([{ accountId: "a0", binding: "v1" }]);
    billingApi.status = async accountId => billingStatus({ accountId, revision: 7 });
    await store.load("a0", "v2");
    resolve({ statuses: [billingStatus({ accountId: "a0", revision: 3 })], errors: {}, revision: 3, processGeneration: 99 });
    await oldRead;
    assert.equal(store.slotFor("a0").value?.boundVersion, "v2");
    assert.equal(store.slotFor("a0").value?.status?.revision, 7);
  } finally {
    billingApi.snapshots = oldSnapshots;
    billingApi.status = oldStatus;
  }
});
