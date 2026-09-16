import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardApi } from "../api/dashboard.ts";
import type { Account } from "../api/dashboard.ts";

/**
 * Single owner of the account list. Views issue API mutations through
 * `dashboardApi`, then commit the results here via `upsertAccount` /
 * `removeAccount` / `setAccounts`; a pending load can never clobber state
 * committed by a newer load or an in-place mutation.
 */
export const useAccountsStore = defineStore("accounts", () => {
  const accounts = ref<Account[]>([]);
  const loaded = ref(false);
  const loading = ref(false);
  const error = ref("");

  const byId = computed(() => {
    const map = new Map<string, Account>();
    for (const account of accounts.value) map.set(account.id, account);
    return map;
  });

  // Overlapping loads resolve out of order; only the latest request commits
  // state. Stale calls still return/throw to their own caller unchanged.
  let loadGeneration = 0;

  async function loadPresented(): Promise<Account[]> {
    const generation = ++loadGeneration;
    loading.value = true;
    try {
      const list = await dashboardApi.getAccounts();
      if (generation !== loadGeneration) return list;
      accounts.value = list;
      loaded.value = true;
      error.value = "";
      return list;
    } catch (e) {
      if (generation === loadGeneration) {
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
    loadGeneration++;
    loading.value = false;
    accounts.value = list;
    loaded.value = true;
    error.value = "";
  }

  function upsertAccount(account: Account): void {
    const exists = accounts.value.some((item) => item.id === account.id);
    setAccounts(exists
      ? accounts.value.map((item) => (item.id === account.id ? account : item))
      : [...accounts.value, account]);
  }

  function removeAccount(id: string): void {
    setAccounts(accounts.value.filter((item) => item.id !== id));
  }

  /** Drop the cached list on 401 / logout so the next session reloads fresh. */
  function clearAccounts(): void {
    loadGeneration++;
    accounts.value = [];
    loaded.value = false;
    loading.value = false;
    error.value = "";
  }

  return {
    accounts: computed(() => accounts.value),
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    byId,
    loadPresented,
    setAccounts,
    upsertAccount,
    removeAccount,
    clearAccounts,
  };
});
