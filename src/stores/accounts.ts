import { computed, ref, shallowRef, watch } from "vue";
import { defineStore } from "pinia";
import { dashboardApi } from "../api/dashboard.ts";
import type { Account } from "../api/dashboard.ts";
import { dropSnapshot } from "./persistence.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { createReadLifecycle, readSnapshotIsCurrent, type ReadOptions } from "./readLifecycle.ts";

const SNAPSHOT_KEY = "accounts";

/**
 * Single owner of the account list. Views issue API mutations through
 * `dashboardApi`, then commit the results here via `upsertAccount` /
 * `removeAccount` / `setAccounts`; a pending load can never clobber state
 * committed by a newer load or an in-place mutation. Commits always replace
 * the list wholesale, so the snapshot is a shallow ref.
 *
 * Business state stays in memory. Rust owns persisted inventory; full list
 * reads establish completeness, while lazy details only populate their rows.
 */
export const useAccountsStore = defineStore("accounts", () => {
  dropSnapshot(SNAPSHOT_KEY);
  const accounts = shallowRef<Account[]>([]);
  const loaded = ref(false);
  const loading = ref(false);
  const error = ref("");
  // A delayed model/usage mutation may return an account after its DELETE.
  // Only a newer authoritative list can confirm an intentional restoration.
  const removedIds = shallowRef<ReadonlySet<string>>(new Set());

  const byId = computed(() => {
    const map = new Map<string, Account>();
    for (const account of accounts.value) map.set(account.id, account);
    return map;
  });

  // Identical reads share a flight. After invalidation, only the latest
  // generation commits; detached callers still receive their own result.
  let loadGeneration = 0;
  const reads = createReadLifecycle();
  const controlPlane = useControlPlaneStore();
  watch(() => controlPlane.processGeneration, (_next, previous) => {
    if (previous === null) return;
    reads.invalidate();
    loading.value = false;
  }, { flush: "sync" });

  function loadPresented(options?: ReadOptions): Promise<Account[]> {
    return reads.run("accounts", options, () => accounts.value, readPresented);
  }

  async function readPresented(): Promise<Account[]> {
    const generation = ++loadGeneration;
    const origin = controlPlane.processGeneration;
    loading.value = true;
    try {
      const snapshot = await dashboardApi.getAccountsSnapshot();
      const list = snapshot.accounts;
      if (generation !== loadGeneration
        || !readSnapshotIsCurrent(snapshot.expectation.processGeneration, snapshot.expectation.expectedRevision, controlPlane, origin)) return list;
      const listedIds = new Set(list.map(account => account.id));
      removedIds.value = new Set([...removedIds.value].filter(id => !listedIds.has(id)));
      accounts.value = list;
      loaded.value = true;
      error.value = "";
      reads.markSuccessful("accounts");
      return list;
    } catch (e) {
      if (generation === loadGeneration && origin === controlPlane.processGeneration) {
        error.value = e instanceof Error ? e.message : String(e);
      }
      throw e;
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  // Committing mutation results invalidates in-flight loads so a stale
  // response cannot clobber the newer state; the superseded load's caller
  // still receives its own payload.
  function setAccounts(list: Account[]): void {
    reads.invalidate();
    loadGeneration++;
    loading.value = false;
    accounts.value = list.filter(account => !removedIds.value.has(account.id));
    loaded.value = true;
    error.value = "";
  }

  function upsertAccount(account: Account): void {
    upsertDetailAccount(account);
  }

  /** A lazy detail is complete for one account, not for the inventory. */
  function upsertDetailAccount(account: Account): void {
    if (removedIds.value.has(account.id)) return;
    reads.invalidate();
    loadGeneration++;
    loading.value = false;
    const exists = accounts.value.some(item => item.id === account.id);
    accounts.value = exists
      ? accounts.value.map(item => item.id === account.id ? account : item)
      : [...accounts.value, account];
    error.value = "";
  }

  function removeAccount(id: string): void {
    const complete = loaded.value;
    removedIds.value = new Set([...removedIds.value, id]);
    setAccounts(accounts.value.filter((item) => item.id !== id));
    loaded.value = complete;
  }

  /** Drop the cached list on 401 / logout so the next session reloads fresh. */
  function clearAccounts(): void {
    reads.invalidate();
    loadGeneration++;
    removedIds.value = new Set();
    accounts.value = [];
    loaded.value = false;
    loading.value = false;
    error.value = "";
    dropSnapshot(SNAPSHOT_KEY);
  }

  /** A complete authoritative list read by another dashboard read model. */
  function commitPresented(list: Account[]): void {
    setAccounts(list);
    const listedIds = new Set(list.map(account => account.id));
    removedIds.value = new Set([...removedIds.value].filter(id => !listedIds.has(id)));
    accounts.value = list;
    reads.markSuccessful("accounts");
  }

  return {
    accounts: computed(() => accounts.value),
    removedAccountIds: computed(() => removedIds.value),
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    byId,
    loadPresented,
    commitPresented,
    setAccounts,
    upsertAccount,
    upsertDetailAccount,
    removeAccount,
    clearAccounts,
  };
});
