import { computed, ref, shallowRef } from "vue";
import type { ComputedRef, ShallowRef } from "vue";
import { defineStore } from "pinia";
import { dashboardV3, isRevisionConflict, type WithoutExpectation } from "../api/dashboard-v3.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import {
  billingApi,
  type BillingStatus,
  type CreditBalanceCorrection,
  type CreditConfigureWrite,
  type CreditGrantRequest,
} from "../api/billing.ts";
import { officialApi } from "../api/official-api.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import {
  applyUsageCalibration,
  cashRefreshKind,
  mergeManualQuotaReceipt,
  type BillingClientError,
  type ManualQuotaReceipt,
} from "../domain/billing.ts";
import type { ObservedUsageWindow, UsageKey } from "../domain/accounts-usage.ts";

export interface BillingSlot {
  status: BillingStatus | null;
  loaded: boolean;
  loading: boolean;
  mutating: boolean;
  error: BillingClientError | null;
  boundVersion: string;
  generation: number;
  resyncBeforeMutate: boolean;
  /**
   * Quota-only manual receipt for this binding when `status.usage` is missing.
   * `loaded` stays true only after a full BillingStatus. A later successful
   * status read clears this; a failed read does not.
   */
  manualReceipt: ManualQuotaReceipt | null;
}

interface RequestToken {
  session: number;
  accountId: string;
  accountVersion: string;
  generation: number;
}

interface BeginFlags {
  loading: boolean;
  mutating: boolean;
  clearError: boolean;
}

function clientErrorFrom(error: unknown): BillingClientError {
  if (isRevisionConflict(error)) return "conflict";
  return "load_failed";
}

/**
 * Sole owner of per-credential billing snapshots. Binding is account version
 * plus endpoint. Evidence is kept only for the same binding.
 */
export const useBillingStore = defineStore("billing", () => {
  // Each account/provider gets its own shallow ref so a begin/write on one
  // key only invalidates subscribers of that key. Slots are always replaced
  // wholesale, never mutated in place, so shallow refs are sufficient.
  const slots = new Map<string, ShallowRef<BillingSlot>>();
  // Membership version: byId and the per-key selectors track it so a key
  // add/remove invalidates them; value updates flow through each slot ref.
  const slotIndex = ref(0);
  const sessionEpoch = ref(0);
  // Slot eviction must not reuse a pending request's identity (A -> B -> A).
  let requestSequence = 0;

  function putSlot(accountId: string, slot: BillingSlot): void {
    const existing = slots.get(accountId);
    if (existing) {
      existing.value = slot;
      return;
    }
    slots.set(accountId, shallowRef(slot));
    slotIndex.value += 1;
  }

  function begin(accountId: string, binding: string, flags: BeginFlags): RequestToken {
    const current = slots.get(accountId)?.value;
    const sameBinding = current !== undefined && current.boundVersion === binding;
    const generation = ++requestSequence;
    putSlot(accountId, {
      status: sameBinding ? current.status : null,
      loaded: sameBinding ? current.loaded : false,
      loading: flags.loading,
      mutating: flags.mutating,
      error: sameBinding && !flags.clearError ? current.error : null,
      boundVersion: binding,
      generation,
      resyncBeforeMutate: sameBinding ? current.resyncBeforeMutate : false,
      manualReceipt: sameBinding ? (current.manualReceipt ?? null) : null,
    });
    return {
      session: sessionEpoch.value,
      accountId,
      accountVersion: binding,
      generation,
    };
  }

  function owns(token: RequestToken): boolean {
    if (token.session !== sessionEpoch.value) return false;
    const slot = slots.get(token.accountId)?.value;
    if (!slot) return false;
    if (slot.boundVersion !== token.accountVersion) return false;
    return slot.generation === token.generation;
  }

  function write(accountId: string, patch: Partial<BillingSlot>): void {
    const current = slots.get(accountId);
    if (!current) return;
    current.value = { ...current.value, ...patch };
  }

  function applyStatus(accountId: string, status: BillingStatus): void {
    write(accountId, {
      status,
      loaded: true,
      error: null,
      resyncBeforeMutate: false,
      manualReceipt: null,
    });
  }

  function expectationFor(accountId: string): MutationExpectation | undefined {
    const status = slots.get(accountId)?.value.status;
    if (!status) return undefined;
    return {
      expectedRevision: status.revision,
      processGeneration: status.processGeneration,
    };
  }

  async function recoverConflict(token: RequestToken, accountId: string): Promise<void> {
    if (!owns(token)) return;
    try {
      const status = await billingApi.status(accountId);
      if (!owns(token)) return;
      write(accountId, {
        status,
        loaded: true,
        error: "conflict",
        resyncBeforeMutate: false,
        manualReceipt: null,
      });
    } catch {
      if (!owns(token)) return;
      write(accountId, { error: "conflict", resyncBeforeMutate: true });
    }
  }

  async function resyncIfNeeded(token: RequestToken, accountId: string): Promise<boolean> {
    if (!owns(token)) return false;
    if (!slots.get(accountId)?.value.resyncBeforeMutate) return true;
    try {
      const status = await billingApi.status(accountId);
      if (!owns(token)) return false;
      write(accountId, { status, loaded: true, resyncBeforeMutate: false, manualReceipt: null });
      return true;
    } catch (error) {
      if (owns(token)) {
        write(accountId, { error: clientErrorFrom(error), resyncBeforeMutate: true });
      }
      return false;
    }
  }

  async function sendMutation(
    token: RequestToken,
    accountId: string,
    mutate: (expectation: MutationExpectation | undefined) => Promise<BillingStatus>,
  ): Promise<BillingStatus> {
    if (!await resyncIfNeeded(token, accountId)) {
      throw new Error("billing is not current");
    }
    try {
      const status = await mutate(expectationFor(accountId));
      if (!owns(token)) return status;
      applyStatus(accountId, status);
      return status;
    } catch (error) {
      if (owns(token) && isRevisionConflict(error)) {
        await recoverConflict(token, accountId);
      } else if (owns(token)) {
        write(accountId, { error: clientErrorFrom(error) });
      }
      throw error;
    }
  }

  async function load(accountId: string, binding: string): Promise<void> {
    const token = begin(accountId, binding, { loading: true, mutating: false, clearError: false });
    try {
      const status = await billingApi.status(accountId);
      if (!owns(token)) return;
      applyStatus(accountId, status);
    } catch (error) {
      if (!owns(token)) return;
      write(accountId, { error: clientErrorFrom(error) });
    } finally {
      if (owns(token)) write(accountId, { loading: false });
    }
  }

  async function refreshUsage(accountId: string, binding: string): Promise<BillingStatus | null> {
    const token = begin(accountId, binding, { loading: false, mutating: true, clearError: true });
    try {
      if (!await resyncIfNeeded(token, accountId)) {
        throw new Error("billing is not current");
      }
      const control = useControlPlaneStore();
      if (!control.hasTokens()) await control.refresh();
      const usage = await control.runMutation(
        (expectation) => dashboardV3.refreshProviderUsage(accountId, expectation),
        expectationFor(accountId),
      );
      if (!owns(token)) return slots.get(accountId)?.value.status ?? null;
      const current = slots.get(accountId)?.value.status;
      if (current) {
        const status: BillingStatus = {
          ...current,
          usage,
          revision: usage.revision,
          processGeneration: usage.processGeneration,
        };
        applyStatus(accountId, status);
        return status;
      }
      const status = await billingApi.status(accountId);
      if (!owns(token)) return status;
      applyStatus(accountId, status);
      return status;
    } catch (error) {
      if (owns(token) && isRevisionConflict(error)) {
        await recoverConflict(token, accountId);
      } else if (owns(token)) {
        write(accountId, { error: clientErrorFrom(error) });
      }
      throw error;
    } finally {
      if (owns(token)) write(accountId, { mutating: false, loading: false });
    }
  }

  async function refreshCash(accountId: string, binding: string): Promise<BillingStatus | null> {
    const token = begin(accountId, binding, { loading: false, mutating: true, clearError: true });
    try {
      if (!await resyncIfNeeded(token, accountId)) {
        throw new Error("billing is not current");
      }
      const current = slots.get(accountId)?.value.status;
      const control = useControlPlaneStore();
      if (!control.hasTokens()) await control.refresh();
      if (current && cashRefreshKind(current) === "official_balance") {
        const cash = await officialApi.refreshBalance(
          accountId,
          expectationFor(accountId) ?? control.expectation(),
        );
        if (!owns(token)) return slots.get(accountId)?.value.status ?? null;
        const latest = slots.get(accountId)?.value.status;
        if (!latest) {
          const status = await billingApi.status(accountId);
          if (!owns(token)) return status;
          applyStatus(accountId, status);
          return status;
        }
        const status: BillingStatus = {
          ...latest,
          cash,
          revision: cash.revision,
          processGeneration: cash.processGeneration,
        };
        applyStatus(accountId, status);
        return status;
      }
      const usage = await control.runMutation(
        (expectation) => dashboardV3.refreshProviderUsage(accountId, expectation),
        expectationFor(accountId),
      );
      if (!owns(token)) return slots.get(accountId)?.value.status ?? null;
      const latest = slots.get(accountId)?.value.status;
      if (latest) {
        const status: BillingStatus = {
          ...latest,
          usage,
          revision: usage.revision,
          processGeneration: usage.processGeneration,
        };
        applyStatus(accountId, status);
        return status;
      }
      const status = await billingApi.status(accountId);
      if (!owns(token)) return status;
      applyStatus(accountId, status);
      return status;
    } catch (error) {
      if (owns(token) && isRevisionConflict(error)) {
        await recoverConflict(token, accountId);
      } else if (owns(token)) {
        write(accountId, { error: clientErrorFrom(error) });
      }
      throw error;
    } finally {
      if (owns(token)) write(accountId, { mutating: false, loading: false });
    }
  }

  async function configureCredits(
    accountId: string,
    binding: string,
    input: CreditConfigureWrite,
  ): Promise<BillingStatus> {
    const token = begin(accountId, binding, { loading: false, mutating: true, clearError: true });
    try {
      return await sendMutation(
        token,
        accountId,
        (expected) => billingApi.configureCredits(accountId, input, expected),
      );
    } finally {
      if (owns(token)) write(accountId, { mutating: false, loading: false });
    }
  }

  async function initializeCredits(
    accountId: string,
    binding: string,
    input: CreditConfigureWrite,
  ): Promise<BillingStatus> {
    const token = begin(accountId, binding, { loading: false, mutating: true, clearError: true });
    try {
      const current = await billingApi.status(accountId);
      if (!owns(token)) throw new Error("billing is not current");
      applyStatus(accountId, current);
      // A previous save may have succeeded despite a lost response. Never
      // replay initial balances or replace an already configured ledger.
      if (current.credits) return current;
      return await sendMutation(token, accountId, expected => billingApi.configureCredits(accountId, input, expected));
    } catch (error) {
      if (owns(token)) write(accountId, { error: clientErrorFrom(error) });
      throw error;
    } finally {
      if (owns(token)) write(accountId, { mutating: false, loading: false });
    }
  }

  async function calibrateCredits(
    accountId: string,
    binding: string,
    balances: CreditBalanceCorrection[],
  ): Promise<BillingStatus> {
    const token = begin(accountId, binding, { loading: false, mutating: true, clearError: true });
    try {
      return await sendMutation(
        token,
        accountId,
        (expected) => billingApi.calibrateCredits(accountId, { balances }, expected),
      );
    } finally {
      if (owns(token)) write(accountId, { mutating: false, loading: false });
    }
  }

  async function grantCredits(
    accountId: string,
    binding: string,
    input: WithoutExpectation<CreditGrantRequest>,
  ): Promise<BillingStatus> {
    const token = begin(accountId, binding, { loading: false, mutating: true, clearError: true });
    try {
      return await sendMutation(
        token,
        accountId,
        (expected) => billingApi.grantCredits(accountId, input, expected),
      );
    } finally {
      if (owns(token)) write(accountId, { mutating: false, loading: false });
    }
  }

  async function disableCredits(accountId: string, binding: string): Promise<BillingStatus> {
    const token = begin(accountId, binding, { loading: false, mutating: true, clearError: true });
    try {
      return await sendMutation(
        token,
        accountId,
        (expected) => billingApi.disableCredits(accountId, expected),
      );
    } finally {
      if (owns(token)) write(accountId, { mutating: false, loading: false });
    }
  }

  function applyCalibratedUsage(
    accountId: string,
    binding: string,
    key: UsageKey,
    usage: ObservedUsageWindow,
    updatedAt: string,
  ): void {
    // Bump generation before adopting the ack so a billing read that started
    // earlier cannot commit its success, error, or finally over this receipt.
    const token = begin(accountId, binding, { loading: false, mutating: false, clearError: false });
    if (!owns(token)) return;
    const current = slots.get(accountId)?.value;
    if (!current) return;
    if (current.status?.usage) {
      applyStatus(accountId, {
        ...current.status,
        usage: applyUsageCalibration(current.status.usage, key, usage, updatedAt),
      });
      return;
    }
    write(accountId, {
      manualReceipt: mergeManualQuotaReceipt(current.manualReceipt, key, usage, updatedAt),
    });
  }

  function remove(accountId: string): void {
    if (!slots.delete(accountId)) return;
    slotIndex.value += 1;
  }

  function clear(): void {
    sessionEpoch.value += 1;
    slots.clear();
    slotIndex.value += 1;
  }

  function slotFor(accountId: string): ComputedRef<BillingSlot | undefined> {
    return computed(() => {
      slotIndex.value;
      return slots.get(accountId)?.value;
    });
  }

  return {
    byId: computed(() => {
      slotIndex.value;
      const out: Record<string, BillingSlot> = {};
      for (const [id, slot] of slots) out[id] = slot.value;
      return out;
    }),
    sessionEpoch: computed(() => sessionEpoch.value),
    slotFor,
    load,
    refreshUsage,
    refreshCash,
    configureCredits,
    initializeCredits,
    calibrateCredits,
    grantCredits,
    disableCredits,
    applyCalibratedUsage,
    remove,
    clear,
  };
});
