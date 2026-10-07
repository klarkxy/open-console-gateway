import assert from "node:assert/strict";
import test from "node:test";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useDashboardPageStore } from "./dashboardPage.ts";
import { useSessionStore } from "./session.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
function page(name = "current", processGeneration = 99, revision = 7, validUntil = new Date(Date.now() + 15_000).toISOString()) {
  return {
    revision: { revision, processGeneration }, readVersion: `${processGeneration}:${revision}`,
    asOf: new Date().toISOString(), validUntil,
    summary: { totalAccounts: 71, availableAccounts: 4, gatewayRunning: true, todayCost: null, weekCost: null, monthCost: null },
    attentionItems: [{ accountId: "attention", accountName: name, reason: "expired", expiredDays: 3 }],
    attentionTotal: 70, attentionLimit: 50, chartSeries: [], modelTotals: [{ model: "backend-order", tokens: 17 }],
    totalTokens: 17, dailyAverageTokens: 1, chartDays: 30, errors: [],
  };
}

test("dashboard coalesces page reads, forwards calendar offset, and reuses fresh snapshots", async () => {
  setupControlPlane(); const gate = deferred<object>(); const requests = installFetchMock(() => gate.promise);
  const store = useDashboardPageStore(); const first = store.load({ utcOffsetMinutes: 480 }); const second = store.load({ utcOffsetMinutes: 480 });
  assert.equal(requests.length, 1); assert.match(requests[0]!.url, /\/pages\/dashboard\?utcOffsetMinutes=480$/);
  gate.resolve(page()); await Promise.all([first, second]);
  assert.equal(store.page?.attentionTotal, 70); assert.equal(store.page?.attentionItems.length, 1);
  await store.load({ utcOffsetMinutes: 480 }, { maxAgeMs: 15_000 }); assert.equal(requests.length, 1);
  await store.load({ utcOffsetMinutes: 480 }); assert.equal(requests.length, 2);
});

test("dashboard invalidation and timezone changes prevent an old response replacing current content", async () => {
  setupControlPlane(); const old = deferred<object>();
  installFetchMock(request => request.url.includes("=0") ? old.promise : page("new-calendar"));
  const store = useDashboardPageStore(); const pending = store.load({ utcOffsetMinutes: 0 });
  await store.load({ utcOffsetMinutes: 480 }); old.resolve(page("old-calendar")); await pending;
  assert.equal(store.page?.attentionItems[0]?.accountName, "new-calendar");
  const late = deferred<object>(); installFetchMock(() => late.promise); const abandoned = store.load({ utcOffsetMinutes: 480 });
  store.invalidate(); installFetchMock(() => page("confirmed")); await store.load({ utcOffsetMinutes: 480 });
  late.resolve(page("old-invalidation")); await abandoned;
  assert.equal(store.page?.attentionItems[0]?.accountName, "confirmed");
});

test("failed dashboard refresh retains content and retries even within the old freshness window", async () => {
  setupControlPlane(); installFetchMock(() => page()); const store = useDashboardPageStore(); await store.load();
  const fail = deferred<object>(); installFetchMock(() => fail.promise); const revalidation = store.load();
  assert.equal(store.loading, true); assert.equal(store.page?.totalTokens, 17);
  fail.reject(new Error("offline")); await assert.rejects(revalidation, /offline/);
  assert.equal(store.page?.totalTokens, 17); assert.match(store.error, /offline/); assert.equal(store.loading, false);
  const requests = installFetchMock(() => page("recovered")); await store.load({}, { maxAgeMs: 15_000 });
  assert.equal(requests.length, 1); assert.equal(store.error, "");
});

test("dashboard honours server deadline and cross-page revision fences", async () => {
  setupControlPlane(); const requests = installFetchMock(() => page("expired", 99, 7, new Date(Date.now() - 1).toISOString()));
  const store = useDashboardPageStore(); await store.load(); await store.load({}, { maxAgeMs: 15_000 }); assert.equal(requests.length, 2);
  const late = deferred<object>(); installFetchMock(() => late.promise); const pending = store.load();
  useControlPlaneStore().sync({ processGeneration: 99, revision: 9 }); late.resolve(page("behind", 99, 8)); await pending;
  assert.equal(store.page?.attentionItems[0]?.accountName, "expired");
});

test("logout clears the canonical dashboard and prevents a late read restoring it", async () => {
  setupControlPlane(); const session = useSessionStore(); session.applyStatus({ authenticated: true, initialized: true, local: true, processGeneration: 99, revision: 7 });
  installFetchMock(() => page()); const store = useDashboardPageStore(); await store.load();
  const late = deferred<object>(); installFetchMock(() => late.promise); const pending = store.load();
  session.dropSession(); assert.equal(store.page, null); late.resolve(page("abandoned")); await pending;
  assert.equal(store.page, null); assert.equal(store.error, ""); assert.equal(store.loading, false);
});

test("a dashboard response can announce a new backend while old-process reads stay abandoned", async () => {
  setupControlPlane(); const late = deferred<object>(); let calls = 0;
  installFetchMock(() => ++calls === 1 ? late.promise : page("new-process", 100));
  const store = useDashboardPageStore(); const pending = store.load({ utcOffsetMinutes: 0 }); await store.load({ utcOffsetMinutes: 480 });
  assert.equal(store.page?.revision.processGeneration, 100); late.resolve(page("old-process", 99)); await pending;
  assert.equal(store.page?.attentionItems[0]?.accountName, "new-process");
});
