import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { effectScope, nextTick, ref } from "vue";
import { dashboardApi, type Account, type UsageWindow } from "../api/dashboard.ts";
import { billingApi, type BillingStatus } from "../api/billing.ts";
import { useBillingStore } from "../stores/billing.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { createAccountsAutoRefresh } from "./accounts-auto-refresh.ts";
import { useAccountUsage } from "./useAccountUsage.ts";

function status(used: number): BillingStatus {
  return {
    accountId: "a", model: "quota", source: "local_estimate", unit: "USD",
    configurableCredits: false, manualCalibration: true, officialRefresh: true,
    cash: null, credits: null, presets: [], revision: 3, processGeneration: 1,
    usage: {
      accountId: "a", providerId: "opencode-go", availability: "available",
      creditBalances: [], experimental: false, freeCooldownUntil: null,
      pricingRevision: null, processGeneration: 1, revision: 3, syncState: null,
      quotaWindows: [{
        accountId: "a", windowKind: "five_hours", used, limitValue: 100,
        startedAt: null, resetsAt: null, calibrationOffset: 0, unit: "USD",
        source: "local_estimate", observedAt: null, updatedAt: "2026-09-21T00:00:00Z",
      }],
    },
  };
}

function deferred() {
  let resolve!: (value: UsageWindow) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<UsageWindow>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const savedUsage: UsageWindow = {
  account_id: "a", window_5h: 80, window_week: 0, window_month: 0,
  resets_in_5h: null, resets_in_week: null, resets_in_month: null,
};

async function fixture(
  t: TestContext,
  afterUsageRefresh?: (accountId: string, isCurrent: () => boolean) => Promise<void>,
) {
  setActivePinia(createPinia());
  const originalStatus = billingApi.status;
  const originalSave = dashboardApi.updateAccountUsage;
  const request = deferred();
  dashboardApi.updateAccountUsage = () => request.promise;
  let used = 10;
  billingApi.status = async () => status(used);
  const accounts = ref([{ id: "a", provider_id: "opencode-go", updated_at: "v1" } as Account]);
  let notifications = 0;
  const notify = () => { notifications++; return {} as never; };
  const scope = effectScope();
  const usage = scope.run(() => useAccountUsage(accounts, ref(Date.now()), ref(null), {
    message: { success: notify, warning: notify, error: notify },
    afterUsageRefresh,
  }))!;
  t.after(() => {
    scope.stop(); billingApi.status = originalStatus; dashboardApi.updateAccountUsage = originalSave;
  });
  await usage.loadAccountUsage("a");
  usage.updateUsageDraft("a", "window_5h", 80);
  return {
    accounts, usage, request, scope, store: useBillingStore(),
    notifications: () => notifications,
    reload: async (next: number) => { used = next; await usage.loadAccountUsage("a"); },
  };
}

test("manual calibration updates its current billing snapshot", async (t) => {
  const f = await fixture(t);
  const saving = f.usage.saveUsage("a", "window_5h");
  f.request.resolve(savedUsage);
  await saving;
  assert.equal(f.usage.getUsage("a").window_5h, 80);
  assert.equal(f.usage.usageEdits.value.a?.window_5h.saving, false);
});

test("late calibration cannot overwrite the same account in a new session", async (t) => {
  const f = await fixture(t);
  const saving = f.usage.saveUsage("a", "window_5h");
  f.store.clear();
  await nextTick();
  await f.reload(25);
  f.request.resolve(savedUsage);
  await saving;
  assert.equal(f.usage.getUsage("a").window_5h, 25);
  assert.equal(f.usage.usageEdits.value.a?.window_5h.saved, 25);
  assert.equal(f.notifications(), 0);
});

test("late calibration cannot overwrite a changed account binding", async (t) => {
  const f = await fixture(t);
  const saving = f.usage.saveUsage("a", "window_5h");
  f.accounts.value = [{ ...f.accounts.value[0]!, updated_at: "v2" }];
  await f.reload(30);
  f.request.resolve(savedUsage);
  await saving;
  assert.equal(f.usage.getUsage("a").window_5h, 30);
  assert.equal(f.usage.usageEdits.value.a?.window_5h.saving, false);
  assert.equal(f.notifications(), 0);
});

test("a disposed editor ignores a late calibration failure", async (t) => {
  const f = await fixture(t);
  const saving = f.usage.saveUsage("a", "window_5h");
  f.scope.stop();
  f.request.reject(new Error("old request failed"));
  await saving;
  assert.equal(f.notifications(), 0);
  assert.equal(f.usage.getUsage("a").window_5h, 10);
});

test("late refresh does not notify or start companion discovery after logout", async (t) => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; });
  f.store.refreshUsage = async () => { await f.request.promise; return status(80); };
  const refreshing = f.usage.refreshAccountUsage("a");
  f.store.clear();
  await nextTick();
  await f.reload(25);
  f.request.resolve(savedUsage);
  await refreshing;
  assert.equal(f.usage.getUsage("a").window_5h, 25);
  assert.equal(f.notifications(), 0);
  assert.equal(companions, 0);
});

test("companion discovery receives a guard that expires during its own await", async (t) => {
  const discovery = deferred();
  let started = false;
  let writes = 0;
  const f = await fixture(t, async (_id, isCurrent) => {
    started = true;
    await discovery.promise;
    if (isCurrent()) writes++;
  });
  f.store.refreshUsage = async () => status(10);
  const refreshing = f.usage.refreshAccountUsage("a");
  await nextTick();
  assert.equal(started, true);
  f.store.clear();
  f.scope.stop();
  discovery.resolve(savedUsage);
  await refreshing;
  assert.equal(writes, 0);
});

test("late usage load cannot recreate disposed editor drafts", async (t) => {
  const f = await fixture(t);
  let resolve!: (status: BillingStatus) => void;
  billingApi.status = () => new Promise((yes) => { resolve = yes; });
  const loading = f.usage.loadAccountUsage("a");
  f.scope.stop();
  f.usage.usageEdits.value = {};
  resolve(status(25));
  await loading;
  assert.deepEqual(f.usage.usageEdits.value, {});
});


test("automatic refresh uses the existing mutation silently without companion discovery", async (t) => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; });
  let refreshes = 0;
  f.store.refreshUsage = async () => { refreshes++; return status(10); };
  f.accounts.value[0]!.enabled = true;
  f.accounts.value[0]!.setup_step = "ready";
  // An unsaved calibration defers automatic work.
  assert.equal(f.usage.automaticRefreshTarget(f.accounts.value[0]!)?.busy, true);
  f.usage.updateUsageDraft("a", "window_5h", 10);
  const target = f.usage.automaticRefreshTarget(f.accounts.value[0]!)!;
  assert.equal(target.busy, false);
  await target.refresh(() => true);
  assert.equal(refreshes, 1);
  assert.equal(f.notifications(), 0);
  assert.equal(companions, 0);
  f.accounts.value[0]!.enabled = false;
  assert.equal(f.usage.automaticRefreshTarget(f.accounts.value[0]!), null);
});

test("automatic work rechecks visibility after its local read and keeps manual feedback", async (t) => {
  const f = await fixture(t);
  f.accounts.value[0]!.enabled = true;
  f.accounts.value[0]!.setup_step = "ready";
  f.usage.updateUsageDraft("a", "window_5h", 10);
  let refreshes = 0;
  f.store.refreshUsage = async () => { refreshes++; return status(10); };
  await f.usage.automaticRefreshTarget(f.accounts.value[0]!)!.refresh(() => false);
  assert.equal(refreshes, 0);
  await f.usage.refreshAccountUsage("a");
  assert.equal(refreshes, 1);
  assert.equal(f.notifications(), 1);
});

test("automatic refresh failure is quiet and cannot wipe the last good usage", async (t) => {
  const f = await fixture(t);
  f.store.refreshUsage = async () => { throw new Error("offline"); };
  await f.usage.refreshAccountUsage("a", true);
  assert.equal(f.notifications(), 0);
  assert.equal(f.usage.getUsage("a").window_5h, 10);
});


test("a serial automatic pass obtains fresh CAS tokens for each real billing mutation", async (t) => {
  const originalFetch = globalThis.fetch;
  t.after(() => { globalThis.fetch = originalFetch; });
  setupControlPlane(3, 1);
  let revision = 3;
  const posts: string[] = [];
  installFetchMock(req => {
    const id = req.url.includes("/accounts/a/") ? "a" : "b";
    const snapshot = status(10);
    snapshot.accountId = id;
    snapshot.revision = revision;
    snapshot.usage!.accountId = id;
    snapshot.usage!.revision = revision;
    if (req.method === "POST") {
      assert.equal(req.body?.expectedRevision, revision);
      posts.push(id);
      snapshot.usage!.revision = ++revision;
      return snapshot.usage!;
    }
    assert.ok(req.url.endsWith("/billing"));
    return snapshot;
  });
  const accounts = ref(["a", "b"].map(id => ({
    id, provider_id: "opencode-go", updated_at: "v1", enabled: true, setup_step: "ready",
  } as Account)));
  const scope = effectScope(); t.after(() => scope.stop());
  const unexpectedToast = () => { assert.fail("automatic pass must be quiet"); };
  const usage = scope.run(() => useAccountUsage(accounts, ref(Date.now()), ref(null), {
    message: { success: unexpectedToast, warning: unexpectedToast, error: unexpectedToast },
  }))!;
  await Promise.all(accounts.value.map(account => usage.loadAccountUsage(account.id)));
  const refresh = createAccountsAutoRefresh({
    allowed: () => true,
    targets: () => accounts.value.flatMap(account => {
      const target = usage.automaticRefreshTarget(account); return target ? [target] : [];
    }),
  });
  await refresh.run();
  assert.deepEqual(posts, ["a", "b"]);
  assert.equal(useBillingStore().byId.b?.status?.revision, 5);
});


test("declared support retries a missing billing snapshot before any upstream refresh", async (t) => {
  const f = await fixture(t);
  f.accounts.value[0] = { ...f.accounts.value[0]!, provider_id: "opencode", enabled: true, setup_step: "ready" };
  f.store.remove("a");
  delete f.usage.usageEdits.value.a;
  let refreshes = 0;
  f.store.refreshUsage = async () => { refreshes++; return status(10); };
  billingApi.status = async () => { throw new Error("temporary local read failure"); };
  await f.usage.automaticRefreshTarget(f.accounts.value[0]!)!.refresh(() => true);
  assert.equal(refreshes, 0);
  billingApi.status = async () => status(10);
  await f.usage.automaticRefreshTarget(f.accounts.value[0]!)!.refresh(() => true);
  assert.equal(refreshes, 1);
  assert.equal(f.notifications(), 0);
});
