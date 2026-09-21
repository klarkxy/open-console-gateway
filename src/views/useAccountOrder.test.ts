import assert from "node:assert/strict";
import test from "node:test";
import { computed, ref } from "vue";
import { dashboardApi, type Account } from "../api/dashboard.ts";
import type { DestinationGroup } from "../domain/destination-groups.ts";
import { useAccountOrder } from "./useAccountOrder.ts";

function fixture() {
  const accounts = ref<Account[]>([{ id: "a", name: "A" } as Account, { id: "b", name: "B" } as Account]);
  const previewOrder = ref<string[] | null>(null);
  let notifications = 0;
  let refreshes = 0;
  const groups = computed(() => (previewOrder.value ?? accounts.value.map((a) => a.id)).map((id) => ({
    id, destination: { name: "Supplier" }, credentials: [{ id, legacy_account_id: id, name: id }],
  } as DestinationGroup)));
  const notify = () => { notifications++; };
  const order = useAccountOrder({
    accounts, previewOrder, groups, busy: ref(false),
    message: { success: notify, error: notify, warning: notify } as unknown as Parameters<typeof useAccountOrder>[0]["message"],
    reloadAfterRevisionConflict: async () => {},
    refreshAfterDetachedSave: async () => { refreshes++; accounts.value = [{ id: "b", name: "new B" } as Account, { id: "a", name: "new A" } as Account]; },
  });
  return { accounts, previewOrder, order, notifications: () => notifications, refreshes: () => refreshes };
}

function deferred() {
  let resolve!: (value: Account[]) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<Account[]>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test("pending order is local and a rejected unmounted save cannot pollute the account store", async (t) => {
  const original = dashboardApi.reorderAccounts;
  t.after(() => { dashboardApi.reorderAccounts = original; });
  const request = deferred();
  dashboardApi.reorderAccounts = () => request.promise;
  const f = fixture();
  const saving = f.order.persistExplicitOrder(["b", "a"]);
  assert.deepEqual(f.previewOrder.value, ["b", "a"]);
  assert.deepEqual(f.accounts.value.map((a) => a.id), ["a", "b"]);
  f.order.revertActiveDrag();
  request.reject(new Error("save rejected"));
  await saving;
  assert.equal(f.previewOrder.value, null);
  assert.deepEqual(f.accounts.value.map((a) => a.id), ["a", "b"]);
  assert.equal(f.notifications(), 0);
});

test("a confirmed detached save revalidates instead of committing its old receipt", async (t) => {
  const original = dashboardApi.reorderAccounts;
  t.after(() => { dashboardApi.reorderAccounts = original; });
  const request = deferred();
  dashboardApi.reorderAccounts = () => request.promise;
  const f = fixture();
  const saving = f.order.persistExplicitOrder(["b", "a"]);
  f.order.revertActiveDrag();
  request.resolve([{ id: "a", name: "stale" } as Account]);
  await saving;
  assert.equal(f.refreshes(), 1);
  assert.deepEqual(f.accounts.value.map((a) => a.name), ["new B", "new A"]);
  assert.equal(f.notifications(), 0);
});

test("failed preview reveals newer account data without restoring an old snapshot", async (t) => {
  const original = dashboardApi.reorderAccounts;
  t.after(() => { dashboardApi.reorderAccounts = original; });
  const request = deferred();
  dashboardApi.reorderAccounts = () => request.promise;
  const f = fixture();
  const saving = f.order.persistExplicitOrder(["b", "a"]);
  f.accounts.value[0].name = "changed during save";
  request.reject(new Error("save rejected"));
  await saving;
  assert.equal(f.previewOrder.value, null);
  assert.equal(f.accounts.value[0].name, "changed during save");
});
