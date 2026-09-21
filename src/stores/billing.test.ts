import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useBillingStore } from "./billing.ts";
import { billingApi, type BillingStatus, type CreditMeterView, type ProviderUsage } from "../api/billing.ts";
import type { OfficialApiPrices, OfficialApiStatus } from "../api/generated/dashboard-v4.ts";
import { billingBinding } from "../domain/billing.ts";

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

test("a stale load cannot overwrite a newer snapshot, mutation, or cleared session", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
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
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
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
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
    credits: credits({ remaining: 10 }),
    revision: 3,
  }));
  await pendingLoad;
  useControlPlaneStore().sync({ revision: 6, processGeneration: 99, pricingRevision: null });

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
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
    credits: credits({ remaining: 4 }),
    revision: 3,
  }));
  await pendingLoad;
  useControlPlaneStore().sync({ revision: 6, processGeneration: 99, pricingRevision: null });

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
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
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
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
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

test("logout epoch also drops pricing limits so a late GET cannot write back", async () => {
  setActivePinia(createPinia());
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pending = store.loadPricing();
  await waitForCalls(calls, 1);
  store.clear();
  calls[0]!.resolve({
    providerId: "opencode",
    availability: "available",
    snapshot: {
      pricingRevision: "p1",
      revision: 1,
      processGeneration: 99,
      activatedAt: "2026-09-01T00:00:00Z",
      documentUpdatedAt: "2026-09-01T00:00:00Z",
      sourceUrl: "https://example.test",
      contentHash: "h",
      adjustmentPolicyVersion: "1",
      limits: { window5h: 1, windowWeek: 2, windowMonth: 3 },
      models: [],
    },
    revision: 1,
    processGeneration: 99,
    pricingRevision: "p1",
    providerPricingRevision: "p1",
  });
  await pending;
  assert.equal(store.pricingLimits, null);
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
    balances: [{ currency: "CNY", granted: 0, observedAt: "2026-09-21T00:00:00Z", toppedUp: 0, total: 20 }],
    kind: "deepseek",
    lifetimeSpend: [],
    monthSpend: [],
    monthStartedAt: "2026-09-01T00:00:00Z",
    prices: {
      kind: "deepseek",
      observedAt: "2026-09-21T00:00:00Z",
      revision: "r1",
      rows: [],
      sourceUrl: "https://example.test",
      validUntil: "2026-10-21T00:00:00Z",
    },
    processGeneration: 99,
    providerId: "deepseek",
    revision: 5,
    unpricedRequests: 0,
    ...overrides,
  };
}

function officialPrices(overrides: Partial<OfficialApiPrices> = {}): OfficialApiPrices {
  return {
    providerId: "deepseek",
    processGeneration: 99,
    revision: 3,
    prices: {
      kind: "deepseek",
      observedAt: "2026-09-21T00:00:00Z",
      revision: "r1",
      rows: [{
        cacheReadPerMillion: 0.1,
        currency: "USD",
        inputPerMillion: 1,
        model: "m",
        outputPerMillion: 2,
        period: "all",
      }],
      sourceUrl: "https://example.test",
      validUntil: "2026-10-21T00:00:00Z",
    },
    ...overrides,
  };
}

test("cash with null OfficialApiStatus refreshes provider-usage and keeps last balances on failure", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
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
});

test("cash with OfficialApiStatus refreshes the official balance endpoint", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
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
  await pendingRefresh;
  assert.equal(store.byId["acc-1"]?.status?.cash?.revision, 6);
  assert.equal(store.byId["acc-1"]?.status?.cash?.balances[0]?.total, 18);
});

test("provider price revalidation keeps the table; errors keep the last good sheet", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
  const store = useBillingStore();
  const first = installDeferredFetch();
  const pendingFirst = store.loadPrices("deepseek");
  await waitForCalls(first, 1);
  assert.match(first[0]!.url, /\/providers\/deepseek\/official-api\/pricing$/);

  const second = installDeferredFetch();
  const pendingSecond = store.loadPrices("deepseek");
  await waitForCalls(second, 1);
  second[0]!.resolve(officialPrices({ revision: 4 }));
  await pendingSecond;
  assert.equal(store.pricesById.deepseek?.prices?.revision, 4);
  assert.equal(store.pricesById.deepseek?.prices?.prices.rows.length, 1);

  first[0]!.resolve(officialPrices({ revision: 3, prices: { ...officialPrices().prices, rows: [] } }));
  await pendingFirst;
  assert.equal(store.pricesById.deepseek?.prices?.revision, 4);
  assert.equal(store.pricesById.deepseek?.prices?.prices.rows.length, 1);

  const fail = installDeferredFetch();
  const pendingFail = store.loadPrices("deepseek");
  await waitForCalls(fail, 1);
  fail[0]!.reject(new TypeError("Failed to fetch"));
  await pendingFail;
  assert.equal(store.pricesById.deepseek?.prices?.revision, 4);
  assert.equal(store.pricesById.deepseek?.error, "load_failed");

  const late = installDeferredFetch();
  const pendingLate = store.loadPrices("deepseek");
  await waitForCalls(late, 1);
  store.clear();
  late[0]!.resolve(officialPrices({ revision: 9 }));
  await pendingLate;
  assert.equal(store.pricesById.deepseek, undefined);
});

test("generic credit configure uses the credits PUT path", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
  const store = useBillingStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.load("acc-1", "v1");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(billingStatus({
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
    model: "credits",
    credits: credits(),
    revision: 4,
  }));
  await pending;
  assert.equal(store.byId["acc-1"]?.status?.model, "credits");
});

test("a rejected credit calibration keeps the last good remaining and does not retry", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: null });
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
