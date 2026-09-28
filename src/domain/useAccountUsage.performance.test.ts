import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { effectScope, nextTick, ref, watch } from "vue";
import type { Account } from "../api/dashboard.ts";
import { billingApi, type BillingStatus } from "../api/billing.ts";
import { useBillingStore } from "../stores/billing.ts";
import { useAccountUsage } from "./useAccountUsage.ts";

function status(accountId: string, used: number): BillingStatus {
  return {
    accountId, model: "quota", source: "local_estimate", unit: "USD",
    configurableCredits: false, manualCalibration: true, officialRefresh: true,
    cash: null, credits: null, presets: [], revision: 3, processGeneration: 1,
    usage: {
      accountId, providerId: "opencode-go", availability: "available",
      creditBalances: [], experimental: false, freeCooldownUntil: null,
      pricingRevision: null, processGeneration: 1, revision: 3, syncState: null,
      quotaWindows: [{
        accountId, windowKind: "five_hours", used, limitValue: 100,
        startedAt: null, resetsAt: null, calibrationOffset: 0, unit: "USD",
        source: "local_estimate", observedAt: null, updatedAt: "2026-09-21T00:00:00Z",
      }],
    },
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function account(id: string, updatedAt = "v1"): Account {
  return { id, provider_id: "opencode-go", updated_at: updatedAt } as Account;
}

async function fixture(t: TestContext) {
  setActivePinia(createPinia());
  const originalStatus = billingApi.status;
  t.after(() => { billingApi.status = originalStatus; });
  const snapshots = new Map([
    ["a", status("a", 10)],
    ["b", status("b", 20)],
  ]);
  const gates = new Map<string, ReturnType<typeof deferred<BillingStatus>>>();
  const failures = new Set<string>();
  const loads: string[] = [];
  billingApi.status = async (accountId: string) => {
    loads.push(accountId);
    const gate = gates.get(accountId);
    if (gate) {
      gates.delete(accountId);
      return gate.promise;
    }
    if (failures.has(accountId)) {
      failures.delete(accountId);
      throw new Error("offline");
    }
    const snapshot = snapshots.get(accountId);
    if (!snapshot) throw new Error(`unknown account ${accountId}`);
    return snapshot;
  };
  const accounts = ref([account("a"), account("b")]);
  const notify = () => ({} as never);
  const scope = effectScope();
  const usage = scope.run(() => useAccountUsage(accounts, ref(Date.now()), ref(null), {
    message: { success: notify, warning: notify, error: notify },
  }))!;
  t.after(() => scope.stop());
  return {
    accounts,
    loads,
    usage,
    store: useBillingStore(),
    gate: (id: string) => {
      const gate = deferred<BillingStatus>();
      gates.set(id, gate);
      return gate;
    },
    failNext: (id: string) => failures.add(id),
    setUsed: (id: string, used: number) => snapshots.set(id, status(id, used)),
  };
}

test("account A load start, update, and completion never touch account B projections", async (t) => {
  const f = await fixture(t);
  await f.usage.loadAccountUsage("a");
  await f.usage.loadAccountUsage("b");

  const aEntry = f.usage.usageMap.value.a;
  const bEntry = f.usage.usageMap.value.b;
  const bPresented = f.usage.providerUsageMap.value.b;
  const bUsageSelector = f.usage.usageFor("b");
  const bPresentedSelector = f.usage.providerUsageFor("b");
  const bUsageValue = bUsageSelector.value;
  const bPresentedValue = bPresentedSelector.value;
  assert.ok(aEntry && bEntry && bPresented && bPresentedValue);

  let bMapTriggers = 0;
  let bSelectorTriggers = 0;
  watch(() => f.usage.usageMap.value.b, () => { bMapTriggers++; }, { flush: "sync" });
  watch(bUsageSelector, () => { bSelectorTriggers++; }, { flush: "sync" });

  // A's load begins: only A's slot flags change.
  const gate = f.gate("a");
  const loading = f.usage.loadAccountUsage("a");
  await nextTick();
  assert.equal(f.usage.usageLoading.value.a, true);
  assert.equal(f.usage.usageMap.value.b, bEntry);
  assert.equal(f.usage.providerUsageMap.value.b, bPresented);
  assert.equal(bUsageSelector.value, bUsageValue);
  assert.equal(bPresentedSelector.value, bPresentedValue);
  assert.equal(bMapTriggers, 0);
  assert.equal(bSelectorTriggers, 0);

  // A's load completes with new usage: A gets a fresh projection, B stays put.
  gate.resolve(status("a", 42));
  await loading;
  assert.equal(f.usage.usageLoading.value.a, false);
  assert.notEqual(f.usage.usageMap.value.a, aEntry);
  assert.equal(f.usage.usageMap.value.a?.window_5h, 42);
  assert.equal(f.usage.usageFor("a").value.window_5h, 42);
  assert.equal(f.usage.usageFor("a").value, f.usage.usageMap.value.a);
  assert.equal(f.usage.providerUsageFor("a").value, f.usage.providerUsageMap.value.a);
  assert.equal(f.usage.usageMap.value.b, bEntry);
  assert.equal(f.usage.providerUsageMap.value.b, bPresented);
  assert.equal(bUsageSelector.value, bUsageValue);
  assert.equal(bPresentedSelector.value, bPresentedValue);
  assert.equal(bMapTriggers, 0);
  assert.equal(bSelectorTriggers, 0);
});

test("a reload with an unchanged snapshot reuses the projection and stays silent", async (t) => {
  const f = await fixture(t);
  await f.usage.loadAccountUsage("a");
  const entry = f.usage.usageMap.value.a;
  const presented = f.usage.providerUsageMap.value.a;
  const selector = f.usage.usageFor("a");
  let triggers = 0;
  watch(selector, () => { triggers++; }, { flush: "sync" });

  // The mock returns the same BillingStatus object: a full load begin/end
  // cycle with unchanged data must not churn any projection reference.
  await f.usage.loadAccountUsage("a");
  assert.equal(f.usage.usageMap.value.a, entry);
  assert.equal(f.usage.providerUsageMap.value.a, presented);
  assert.equal(selector.value, entry);
  assert.equal(triggers, 0);

  // A real update returns the new value exactly once.
  f.setUsed("a", 55);
  await f.usage.loadAccountUsage("a");
  assert.notEqual(f.usage.usageMap.value.a, entry);
  assert.equal(f.usage.usageMap.value.a?.window_5h, 55);
  assert.equal(triggers, 1);
});

test("a failed load only sets that account's error and changes no projection", async (t) => {
  const f = await fixture(t);
  await f.usage.loadAccountUsage("a");
  await f.usage.loadAccountUsage("b");
  const aEntry = f.usage.usageMap.value.a;
  const bEntry = f.usage.usageMap.value.b;
  const bSelectorValue = f.usage.usageFor("b").value;
  let bTriggers = 0;
  watch(f.usage.usageFor("b"), () => { bTriggers++; }, { flush: "sync" });

  f.failNext("a");
  await f.usage.loadAccountUsage("a");
  assert.ok(f.usage.usageLoadErrorFor("a").value);
  assert.equal(f.usage.usageMap.value.a, aEntry);
  assert.equal(f.usage.usageMap.value.b, bEntry);
  assert.equal(f.usage.usageFor("b").value, bSelectorValue);
  assert.equal(bTriggers, 0);
});

test("per-account selectors are shared instances and survive unrelated updates", async (t) => {
  const f = await fixture(t);
  await f.usage.loadAccountUsage("a");
  await f.usage.loadAccountUsage("b");
  assert.equal(f.usage.usageFor("b"), f.usage.usageFor("b"));
  assert.equal(f.usage.providerUsageFor("b"), f.usage.providerUsageFor("b"));
  assert.equal(f.usage.usageLoadingFor("b"), f.usage.usageLoadingFor("b"));
  assert.equal(f.usage.usageLoadErrorFor("b"), f.usage.usageLoadErrorFor("b"));
  assert.equal(f.usage.usageRefreshLoadingFor("b"), f.usage.usageRefreshLoadingFor("b"));

  f.setUsed("a", 77);
  await f.usage.loadAccountUsage("a");
  assert.equal(f.usage.usageFor("b").value.window_5h, 20);
});

test("deleting an account evicts its projections and a re-added account loads fresh", async (t) => {
  const f = await fixture(t);
  // Read-only usage has no edit draft or refresh operation to trigger eviction.
  await f.store.load("a", "v1");
  await f.usage.loadAccountUsage("b");
  const stale = f.usage.usageMap.value.a;
  const staleSelector = f.usage.usageFor("a");
  assert.ok(stale);

  f.accounts.value = f.accounts.value.filter((item) => item.id !== "a");
  const afterDelete = f.usage.usageMap.value;
  const presentedAfterDelete = f.usage.providerUsageMap.value;
  assert.equal(afterDelete.a, undefined);
  assert.equal(presentedAfterDelete.a, undefined);
  assert.ok(afterDelete.b);
  assert.notEqual(f.usage.usageFor("a"), staleSelector);

  f.accounts.value = [...f.accounts.value, account("a", "v2")];
  f.setUsed("a", 33);
  await f.usage.loadAccountUsage("a");
  assert.equal(f.usage.usageMap.value.a?.window_5h, 33);
  assert.notEqual(f.usage.usageMap.value.a, stale);
});

test("logout clears cached projections and a new session never sees the old ones", async (t) => {
  const f = await fixture(t);
  await f.usage.loadAccountUsage("a");
  const before = f.usage.usageMap.value.a;
  assert.equal(before?.window_5h, 10);

  f.store.clear();
  assert.deepEqual(f.usage.usageMap.value, {});
  assert.deepEqual(f.usage.providerUsageMap.value, {});

  f.setUsed("a", 25);
  await f.usage.loadAccountUsage("a");
  const after = f.usage.usageMap.value.a;
  assert.equal(after?.window_5h, 25);
  assert.notEqual(after, before);
});

test("matching overlapping usage loads share completion and one request", async (t) => {
  const f = await fixture(t);
  const gate = f.gate("a");
  const first = f.usage.loadAccountUsage("a");
  const second = f.usage.loadAccountUsage("a");
  assert.equal(first, second);
  assert.deepEqual(f.loads, ["a"]);
  gate.resolve(status("a", 42));
  await Promise.all([first, second]);
  assert.equal(f.usage.usageFor("a").value.window_5h, 42);
});

test("a binding change hides old usage immediately and only reloads that account", async (t) => {
  const f = await fixture(t);
  await f.usage.loadAccountUsage("a");
  await f.usage.loadAccountUsage("b");
  const other = f.usage.usageMap.value.b;
  const gate = f.gate("a");
  f.accounts.value = [account("a", "v2"), account("b")];
  assert.equal(f.usage.providerUsageFor("a").value, null);
  assert.equal(f.usage.usageMap.value.b, other);
  const pending = f.usage.loadAccountUsage("a");
  assert.equal(f.usage.usageLoadingFor("a").value, true);
  assert.deepEqual(f.loads, ["a", "b", "a"]);
  gate.resolve(status("a", 60));
  await pending;
  assert.equal(f.usage.usageFor("a").value.window_5h, 60);
  assert.equal(f.usage.usageMap.value.b, other);
});

test("automatic binding reads batch to the latest version and stop at logout", async (t) => {
  const f = await fixture(t);
  await f.usage.loadAccountUsage("a");
  f.setUsed("a", 45);
  f.accounts.value = [account("a", "v2"), account("b")];
  f.accounts.value = [account("a", "v3"), account("b")];
  await nextTick();
  await f.usage.loadAccountUsage("a");
  assert.deepEqual(f.loads, ["a", "a"]);
  assert.equal(f.usage.usageFor("a").value.window_5h, 45);
  f.accounts.value = [account("a", "v4"), account("b")];
  f.store.clear();
  await nextTick();
  assert.deepEqual(f.loads, ["a", "a"]);
});

test("returning to an earlier binding cannot revive its pending usage request", async (t) => {
  const f = await fixture(t);
  const oldGate = f.gate("a");
  const oldRequest = f.usage.loadAccountUsage("a");

  f.accounts.value = [account("a", "v2"), account("b")];
  await f.usage.loadAccountUsage("a");
  f.accounts.value = [account("a", "v1"), account("b")];
  f.setUsed("a", 75);
  await f.usage.loadAccountUsage("a");
  assert.equal(f.usage.usageFor("a").value.window_5h, 75);

  oldGate.resolve(status("a", 5));
  await oldRequest;
  assert.equal(f.usage.usageFor("a").value.window_5h, 75);
  assert.equal(f.store.slotFor("a").value?.loading, false);
  assert.deepEqual(f.loads, ["a", "a", "a"]);
});

test("account-list replacement with unchanged bindings creates no automatic reads", async (t) => {
  const f = await fixture(t);
  assert.deepEqual(f.loads, []);
  await f.usage.loadAccountUsage("a");
  f.accounts.value = [account("a"), account("b")];
  await nextTick();
  assert.deepEqual(f.loads, ["a"]);
});

test("warm return fills missing or failed usage without reloading current accounts", async (t) => {
  const f = await fixture(t);
  await f.usage.loadAccountUsage("a");
  await Promise.all([f.usage.ensureAccountUsage("a"), f.usage.ensureAccountUsage("b")]);
  assert.deepEqual(f.loads, ["a", "b"]);
  f.failNext("b");
  await f.usage.loadAccountUsage("b");
  await f.usage.ensureAccountUsage("b");
  assert.deepEqual(f.loads, ["a", "b", "b", "b"]);
  assert.equal(f.usage.usageLoadErrorFor("b").value, null);
});
