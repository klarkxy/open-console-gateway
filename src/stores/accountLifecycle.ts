import { computed, ref, watch } from "vue";
import { defineStore } from "pinia";
import { dashboardApi, isRevisionConflict } from "../api/dashboard.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useAccountsStore } from "./accounts.ts";
import { useBillingStore } from "./billing.ts";
import { useDestinationsStore } from "./destinations.ts";
import { useIdentitiesStore } from "./identities.ts";
import { usePlatformAccountsStore } from "./platformAccounts.ts";
import { useProvidersStore } from "./providers.ts";

export type AccountRemovalOutcome =
  | { kind: "conflict" | "cancelled" }
  | { kind: "deleted"; revalidation: Promise<AccountRemovalRevalidation> };

export type AccountRemovalRevalidation =
  | { kind: "refreshed" | "cancelled" }
  | { kind: "failed"; detail: string };

/** One local DELETE, followed only by non-destructive projection reads. */
export const useAccountLifecycleStore = defineStore("accountLifecycle", () => {
  const accounts = useAccountsStore();
  const billing = useBillingStore();
  const platforms = usePlatformAccountsStore();
  const deleting = ref<Record<string, boolean>>({});
  const requests = new Map<string, Promise<AccountRemovalOutcome>>();

  watch(() => billing.sessionEpoch, () => {
    requests.clear();
    deleting.value = {};
  }, { flush: "sync" });

  function remove(accountId: string): Promise<AccountRemovalOutcome> {
    const pending = requests.get(accountId);
    if (pending) return pending;
    if (!accounts.byId.has(accountId) || platforms.mutating) return Promise.resolve({ kind: "cancelled" });
    const session = billing.sessionEpoch;
    deleting.value[accountId] = true;
    const current = () => session === billing.sessionEpoch && requests.get(accountId) === request;
    const request: Promise<AccountRemovalOutcome> = Promise.resolve().then(async (): Promise<AccountRemovalOutcome> => {
      try {
        if (!current()) return { kind: "cancelled" };
        await dashboardApi.deleteAccount(accountId);
        if (!current()) return { kind: "cancelled" };
        accounts.removeAccount(accountId);
        billing.remove(accountId);
        platforms.forgetAccount(accountId);
        const revalidation = Promise.allSettled([
          useDestinationsStore().refreshAfterMutation(),
          platforms.load(),
          useIdentitiesStore().loadPresented(),
          useProvidersStore().loadConnections(),
        ]).then((results): AccountRemovalRevalidation => {
          if (session !== billing.sessionEpoch) return { kind: "cancelled" };
          const failed = results.find(result => result.status === "rejected");
          return failed?.status === "rejected"
            ? { kind: "failed", detail: dashboardErrorDetail(failed.reason) }
            : { kind: "refreshed" };
        });
        // The DELETE receipt is the completion boundary. Slow projection reads
        // must not retain the confirmation dialog or the account's delete lock.
        return { kind: "deleted", revalidation };
      } catch (error) {
        if (!current()) return { kind: "cancelled" };
        if (!isRevisionConflict(error)) throw error;
        // A conflict is not permission to retry a destructive request.
        await Promise.allSettled([
          accounts.loadPresented(),
          platforms.load(),
          useDestinationsStore().load(),
        ]);
        return { kind: current() ? "conflict" : "cancelled" };
      } finally {
        if (current()) {
          requests.delete(accountId);
          delete deleting.value[accountId];
        }
      }
    });
    requests.set(accountId, request);
    return request;
  }

  return { deleting: computed(() => deleting.value), remove };
});
