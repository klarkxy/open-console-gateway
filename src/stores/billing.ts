import { computed, ref, shallowRef } from "vue";
import type { ComputedRef, ShallowRef } from "vue";
import { defineStore } from "pinia";
import { dashboardApi, type PricingLimits, type UsageWindow } from "../api/dashboard.ts";
import { dashboardV3, isRevisionConflict, type WithoutExpectation } from "../api/dashboard-v3.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import {
  billingApi,
  type BillingStatus,
  type CreditBalanceCorrection,
  type CreditBucket,
  type CreditConfiguration,
  type CreditGrantRequest,
} from "../api/billing.ts";
import { officialApi } from "../api/official-api.ts";
import type { OfficialApiPrices } from "../api/generated/dashboard-v4.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import {
  applyUsageCalibration,
  cashRefreshKind,
  type BillingClientError,
} from "../domain/billing.ts";
import type { UsageKey } from "../domain/accounts-usage.ts";

export interface BillingSlot {
  status: BillingStatus | null;
  loaded: boolean;
  loading: boolean;
  mutating: boolean;
  error: BillingClientError | null;
  boundVersion: string;
  generation: number;
  resyncBeforeMutate: boolean;
}

export interface BillingPriceSlot {
  prices: OfficialApiPrices | null;
  loaded: boolean;
  loading: boolean;
  mutating: boolean;
  error: BillingClientError | null;
  boundVersion: string;
  generation: number;
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
  const pricingLimits = ref<PricingLimits | null>(null);
  const pricingLoading = ref(false);
  const pricingError = ref("");
  let pricingGeneration = 0;
  const priceSlots = new Map<string, ShallowRef<BillingPriceSlot>>();
  const priceSlotIndex = ref(0);

  function putSlot(accountId: string, slot: BillingSlot): void {
    const existing = slots.get(accountId);
    if (existing) {
      existing.value = slot;
      return;
    }
    slots.set(accountId, shallowRef(slot));
    slotIndex.value += 1;
  }

  function putPriceSlot(providerId: string, slot: BillingPriceSlot): void {
    const existing = priceSlots.get(providerId);
    if (existing) {
      existing.value = slot;
      return;
    }
    priceSlots.set(providerId, shallowRef(slot));
    priceSlotIndex.value += 1;
  }

  function begin(accountId: string, binding: string, flags: BeginFlags): RequestToken {
    const current = slots.get(accountId)?.value;
    const sameBinding = current !== undefined && current.boundVersion === binding;
    const generation = (current?.generation ?? 0) + 1;
    putSlot(accountId, {
      status: sameBinding ? current.status : null,
      loaded: sameBinding ? current.loaded : false,
      loading: flags.loading,
      mutating: flags.mutating,
      error: sameBinding && !flags.clearError ? current.error : null,
      boundVersion: binding,
      generation,
      resyncBeforeMutate: sameBinding ? current.resyncBeforeMutate : false,
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
      write(accountId, { status, loaded: true, resyncBeforeMutate: false });
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
    input: {
      configuration: CreditConfiguration;
      initialBuckets?: CreditBucket[] | null;
    },
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

  async function initializeCredits(accountId: string, binding: string, input: {
    configuration: CreditConfiguration; initialBuckets: CreditBucket[];
  }): Promise<BillingStatus> {
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
    usage: UsageWindow,
    updatedAt: string,
  ): void {
    const token = begin(accountId, binding, { loading: false, mutating: false, clearError: false });
    const current = slots.get(accountId)?.value.status;
    if (!owns(token) || !current?.usage) return;
    applyStatus(accountId, {
      ...current,
      usage: applyUsageCalibration(current.usage, key, usage, updatedAt),
    });
  }

  function beginPrices(providerId: string, flags: { loading: boolean; mutating: boolean }): {
    session: number;
    providerId: string;
    generation: number;
  } {
    const current = priceSlots.get(providerId)?.value;
    const generation = (current?.generation ?? 0) + 1;
    putPriceSlot(providerId, {
      prices: current?.prices ?? null,
      loaded: current?.loaded ?? false,
      loading: flags.loading,
      mutating: flags.mutating,
      error: current?.error ?? null,
      boundVersion: providerId,
      generation,
    });
    return { session: sessionEpoch.value, providerId, generation };
  }

  function ownsPrices(token: { session: number; providerId: string; generation: number }): boolean {
    if (token.session !== sessionEpoch.value) return false;
    const slot = priceSlots.get(token.providerId)?.value;
    if (!slot) return false;
    return slot.boundVersion === token.providerId && slot.generation === token.generation;
  }

  function writePrices(providerId: string, patch: Partial<BillingPriceSlot>): void {
    const current = priceSlots.get(providerId);
    if (!current) return;
    current.value = { ...current.value, ...patch };
  }

  async function loadPrices(providerId: string): Promise<void> {
    const token = beginPrices(providerId, { loading: true, mutating: false });
    try {
      const prices = await officialApi.prices(providerId);
      if (!ownsPrices(token)) return;
      writePrices(providerId, { prices, loaded: true, error: null });
    } catch (error) {
      if (!ownsPrices(token)) return;
      writePrices(providerId, { error: clientErrorFrom(error) });
    } finally {
      if (ownsPrices(token)) writePrices(providerId, { loading: false });
    }
  }

  async function refreshPrices(providerId: string): Promise<void> {
    const token = beginPrices(providerId, { loading: false, mutating: true });
    const snapshot = priceSlots.get(providerId)?.value.prices;
    try {
      const control = useControlPlaneStore();
      if (!control.hasTokens()) await control.refresh();
      const prices = await officialApi.refreshPrices(providerId, snapshot
        ? { expectedRevision: snapshot.revision, processGeneration: snapshot.processGeneration }
        : control.expectation());
      if (!ownsPrices(token)) return;
      writePrices(providerId, { prices, loaded: true, error: null });
    } catch (error) {
      if (!ownsPrices(token)) return;
      writePrices(providerId, { error: clientErrorFrom(error) });
      throw error;
    } finally {
      if (ownsPrices(token)) writePrices(providerId, { mutating: false, loading: false });
    }
  }

  async function loadPricing(): Promise<boolean> {
    const generation = ++pricingGeneration;
    const session = sessionEpoch.value;
    pricingLoading.value = true;
    pricingError.value = "";
    try {
      const pricing = await dashboardApi.getPricing();
      if (generation !== pricingGeneration || session !== sessionEpoch.value) return false;
      pricingLimits.value = pricing.limits;
      return true;
    } catch (error) {
      if (generation !== pricingGeneration || session !== sessionEpoch.value) return false;
      pricingLimits.value = null;
      pricingError.value = error instanceof Error ? error.message : String(error);
      return false;
    } finally {
      if (generation === pricingGeneration && session === sessionEpoch.value) {
        pricingLoading.value = false;
      }
    }
  }

  function remove(accountId: string): void {
    if (!slots.delete(accountId)) return;
    slotIndex.value += 1;
  }

  function clear(): void {
    sessionEpoch.value += 1;
    pricingGeneration += 1;
    slots.clear();
    slotIndex.value += 1;
    priceSlots.clear();
    priceSlotIndex.value += 1;
    pricingLimits.value = null;
    pricingLoading.value = false;
    pricingError.value = "";
  }

  function slotFor(accountId: string): ComputedRef<BillingSlot | undefined> {
    return computed(() => {
      slotIndex.value;
      return slots.get(accountId)?.value;
    });
  }

  function priceSlotFor(providerId: string): ComputedRef<BillingPriceSlot | undefined> {
    return computed(() => {
      priceSlotIndex.value;
      return priceSlots.get(providerId)?.value;
    });
  }

  return {
    byId: computed(() => {
      slotIndex.value;
      const out: Record<string, BillingSlot> = {};
      for (const [id, slot] of slots) out[id] = slot.value;
      return out;
    }),
    pricesById: computed(() => {
      priceSlotIndex.value;
      const out: Record<string, BillingPriceSlot> = {};
      for (const [id, slot] of priceSlots) out[id] = slot.value;
      return out;
    }),
    sessionEpoch: computed(() => sessionEpoch.value),
    pricingLimits: computed(() => pricingLimits.value),
    pricingLoading: computed(() => pricingLoading.value),
    pricingError: computed(() => pricingError.value),
    slotFor,
    priceSlotFor,
    load,
    refreshUsage,
    refreshCash,
    configureCredits,
    initializeCredits,
    calibrateCredits,
    grantCredits,
    disableCredits,
    applyCalibratedUsage,
    loadPricing,
    loadPrices,
    refreshPrices,
    remove,
    clear,
  };
});
