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
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import { billingApi, type BillingStatus } from "../api/billing.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
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
    accountId: "a", model: "quota", surfaceKind: "quota", providerWindows: true, quotaManualCalibration: true,
    quotaEditorLimits: [{ windowKind: "five_hours", limit: 100, editable: true, editableAt: null }], source: "local_estimate", unit: "USD",
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

test("an unsupported provider with omitted windows never opens calibration", async t => {
  setActivePinia(createPinia());
  const snapshot: BillingStatus = {
    accountId: "mini",
    model: "quota",
    surfaceKind: "quota",
    providerWindows: true,
    quotaManualCalibration: false,
    quotaEditorLimits: [],
    source: "unavailable",
    unit: "percent",
    configurableCredits: false,
    manualCalibration: false,
    officialRefresh: true,
    cash: null,
    credits: null,
    presets: [],
    revision: 3,
    processGeneration: 1,
    usage: {
      accountId: "mini",
      providerId: "minimax",
      availability: "available",
      creditBalances: [],
      experimental: false,
      freeCooldownUntil: null,
      pricingRevision: null,
      processGeneration: 1,
      revision: 3,
      syncState: null,
      quotaWindows: [],
    },
  };
  t.mock.method(billingApi, "status", async () => snapshot);
  let posts = 0;
  t.mock.method(dashboardApi, "updateAccountUsage", async () => {
    posts += 1;
    throw new Error("unsupported provider must not post usage");
  });
  const accounts = ref<Account[]>([{
    id: "mini",
    provider_id: "minimax",
    updated_at: "v1",
    enabled: true,
    setup_step: "ready",
  } as Account]);
  const catalog = ref([{
    provider_id: "minimax",
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "plan",
    display_name: "minimax",
    display_family: "minimax",
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
    manual_usage_calibration: false,
    quota_unit: "request",
    model_source: "builtin",
    key_prefix: null,
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
  } as ProviderCatalogEntry]);
  const scope = effectScope();
  const usage = scope.run(() => useAccountUsage(accounts, ref(Date.now()), catalog, {
    message: { success: () => ({} as never), warning: () => ({} as never), error: () => ({} as never) },
  }))!;
  t.after(() => scope.stop());
  await usage.loadAccountUsage("mini");
  assert.equal(usage.hasAvailableUsageEditor(accounts.value[0]!), false);
  assert.deepEqual(usage.usageEdits.value, {});
  assert.equal(usage.getUsage("mini").window_5h, null);
  assert.equal(usage.getUsage("mini").window_week, null);
  assert.equal(usage.getUsage("mini").window_month, null);
  assert.equal(usage.providerUsageFor("mini").value?.quota_windows.length, 0);
  await usage.saveUsage("mini", "window_5h");
  assert.equal(posts, 0);
});
