import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { effectScope, ref } from "vue";
import type { Account } from "../api/dashboard.ts";
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import { billingApi, type BillingStatus } from "../api/billing.ts";
import { useBillingStore } from "../stores/billing.ts";
import { useAccountUsage } from "./useAccountUsage.ts";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(yes => { resolve = yes; });
  return { promise, resolve };
}
const flush = () => new Promise<void>(resolve => setImmediate(resolve));
function status(officialRefresh = true): BillingStatus {
  return {
    accountId: "a", model: "quota", source: "local_estimate", unit: "USD",
    configurableCredits: false, manualCalibration: true, officialRefresh,
    cash: null, credits: null, presets: [], revision: 3, processGeneration: 1,
    usage: {
      accountId: "a", providerId: "opencode-go", availability: "available",
      creditBalances: [], experimental: false, freeCooldownUntil: null,
      pricingRevision: null, processGeneration: 1, revision: 3, syncState: null,
      quotaWindows: [{ accountId: "a", windowKind: "five_hours", used: 10, limitValue: 100,
        startedAt: null, resetsAt: null, calibrationOffset: 0, unit: "USD", source: "local_estimate",
        observedAt: null, updatedAt: "2026-09-21T00:00:00Z" }],
    },
  };
}
async function fixture(t: TestContext, companion: (id: string, current: () => boolean) => Promise<void>, official = true) {
  setActivePinia(createPinia());
  t.mock.method(billingApi, "status", async () => status(official));
  const accounts = ref<Account[]>([{ id: "a", name: "a", provider_id: "opencode-go", updated_at: "v1",
    enabled: true, setup_step: "ready" } as Account]);
  const events: string[] = [];
  const notify = (kind: string) => () => { events.push(kind); return {} as never; };
  const scope = effectScope();
  const usage = scope.run(() => useAccountUsage(accounts, ref(Date.now()), ref(null), {
    message: { success: notify("success"), error: notify("error"), warning: notify("warning") },
    afterUsageRefresh: companion,
  }))!;
  t.after(() => scope.stop());
  await usage.loadAccountUsage("a");
  return { accounts, events, usage, billing: useBillingStore() };
}

test("model-only manual refresh reaches discovery without sending a quota mutation", async t => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; }, false);
  const quota = t.mock.method(f.billing, "refreshUsage", async () => status());
  await f.usage.refreshAccountUsage("a");
  assert.equal(companions, 1);
  assert.equal(quota.mock.callCount(), 0);
  await f.usage.refreshAccountUsage("a", true);
  assert.equal(companions, 1);
  assert.equal(quota.mock.callCount(), 0);
});

test("busy and duplicate suppression cover discovery, not just the quota request", async t => {
  const discovery = deferred();
  let companions = 0;
  const f = await fixture(t, async () => { companions++; await discovery.promise; });
  const quota = t.mock.method(f.billing, "refreshUsage", async () => status());
  const refreshing = f.usage.refreshAccountUsage("a");
  await flush();
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, true);
  assert.equal(f.usage.automaticRefreshTarget(f.accounts.value[0]!)?.busy, true);
  assert.deepEqual(f.events, []);
  await f.usage.refreshAccountUsage("a");
  assert.equal(quota.mock.callCount(), 1);
  assert.equal(companions, 1);
  discovery.resolve();
  await refreshing;
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, false);
  assert.deepEqual(f.events, ["success"]);
});

test("failed quota refresh keeps known usage and never starts model writes", async t => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; });
  t.mock.method(f.billing, "refreshUsage", async () => { throw new Error("offline"); });
  await f.usage.refreshAccountUsage("a");
  assert.equal(companions, 0);
  assert.equal(f.usage.getUsage("a").window_5h, 10);
  assert.deepEqual(f.events, ["error"]);
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, false);
});

test("429 records the next eligible time without starting companion discovery", async t => {
  let companions = 0;
  const f = await fixture(t, async () => { companions++; });
  const nextAllowed = "2099-01-01T00:00:00Z";
  t.mock.method(f.billing, "refreshUsage", async () => {
    throw new DashboardRequestError("throttled", 429, "throttled", 3, 1, 60, nextAllowed);
  });
  await f.usage.refreshAccountUsage("a");
  assert.equal(companions, 0);
  assert.equal(f.accounts.value[0]?.usage_sync_next_allowed_at, nextAllowed);
  assert.equal(f.usage.automaticRefreshTarget(f.accounts.value[0]!)?.nextAllowedAt, Date.parse(nextAllowed));
  assert.deepEqual(f.events, ["warning"]);
});

test("deleting during refresh prevents companion work and draft resurrection", async t => {
  const response = deferred();
  let companions = 0;
  const f = await fixture(t, async () => { companions++; });
  t.mock.method(f.billing, "refreshUsage", async () => { await response.promise; return status(); });
  const refreshing = f.usage.refreshAccountUsage("a");
  f.accounts.value = [];
  response.resolve();
  await refreshing;
  assert.equal(companions, 0);
  assert.deepEqual(f.events, []);
  assert.deepEqual(f.usage.usageEdits.value, {});
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, false);
});

test("an old finally cannot clear the new session's refresh operation", async t => {
  const old = deferred();
  const fresh = deferred();
  const f = await fixture(t, async () => {});
  let calls = 0;
  t.mock.method(f.billing, "refreshUsage", async () => {
    await (++calls === 1 ? old.promise : fresh.promise);
    return status();
  });
  const first = f.usage.refreshAccountUsage("a");
  f.billing.clear();
  await f.usage.loadAccountUsage("a");
  const second = f.usage.refreshAccountUsage("a");
  old.resolve();
  await first;
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, true);
  assert.deepEqual(f.events, []);
  fresh.resolve();
  await second;
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, false);
  assert.deepEqual(f.events, ["success"]);
});

test("a failed companion releases the operation without reporting overall success", async t => {
  const f = await fixture(t, async () => { throw new Error("discovery failed"); });
  t.mock.method(f.billing, "refreshUsage", async () => status());
  await f.usage.refreshAccountUsage("a");
  assert.equal(f.usage.usageRefreshLoadingFor("a").value, false);
  assert.deepEqual(f.events, []);
});
