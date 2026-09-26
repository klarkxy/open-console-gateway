import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import { isRevisionConflict } from "../api/dashboard-v3.ts";
import type {
  MutationExpectation,
} from "../api/generated/dashboard-v3.ts";
import type {
  TemporaryPolicyConfiguration,
  TemporaryPolicyRestrictions,
} from "../api/generated/dashboard-v4.ts";
import {
  type PolicyClientError,
  type PolicyRule,
} from "../domain/temporary-policy.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

/**
 * Single owner of temporary-unavailability configuration and live restriction
 * diagnostics. Views keep drafts/filters only. Loads are generation-guarded;
 * successful mutations commit in place; revalidation keeps the last snapshot.
 */
export const useTemporaryPolicyStore = defineStore("temporaryPolicy", () => {
  const controlPlane = useControlPlaneStore();

  const configuration = ref<TemporaryPolicyConfiguration | null>(null);
  const restrictions = ref<TemporaryPolicyRestrictions | null>(null);
  const restrictionsObservedAt = ref<number | null>(null);
  const loaded = ref(false);
  const restrictionsLoaded = ref(false);
  const loading = ref(false);
  const restrictionsLoading = ref(false);
  const mutating = ref(false);
  const clearing = ref(false);
  const error = ref<PolicyClientError | null>(null);
  const errorDetail = ref("");
  const restrictionsError = ref<PolicyClientError | null>(null);
  const restrictionsErrorDetail = ref("");

  let configGeneration = 0;
  let restrictionGeneration = 0;
  let sessionGeneration = 0;
  let configLoadInFlight = 0;
  let restrictionLoadInFlight = 0;
  let mutationInFlight = 0;
  let clearInFlight = 0;

  function beginConfigLoad(): number {
    configLoadInFlight += 1;
    loading.value = true;
    return ++configGeneration;
  }

  function endConfigLoad(generation: number): void {
    if (generation < sessionGeneration) return;
    configLoadInFlight = Math.max(0, configLoadInFlight - 1);
    if (configLoadInFlight === 0) loading.value = false;
  }

  function beginRestrictionLoad(): number {
    restrictionLoadInFlight += 1;
    restrictionsLoading.value = true;
    return ++restrictionGeneration;
  }

  function endRestrictionLoad(generation: number): void {
    if (generation < sessionGeneration) return;
    restrictionLoadInFlight = Math.max(0, restrictionLoadInFlight - 1);
    if (restrictionLoadInFlight === 0) restrictionsLoading.value = false;
  }

  function beginMutation(): number {
    mutationInFlight += 1;
    mutating.value = true;
    return ++configGeneration;
  }

  function endMutation(generation: number): void {
    if (generation < sessionGeneration) return;
    mutationInFlight = Math.max(0, mutationInFlight - 1);
    if (mutationInFlight === 0) mutating.value = false;
  }

  function beginClear(): number {
    clearInFlight += 1;
    clearing.value = true;
    return ++restrictionGeneration;
  }

  function endClear(generation: number): void {
    if (generation < sessionGeneration) return;
    clearInFlight = Math.max(0, clearInFlight - 1);
    if (clearInFlight === 0) clearing.value = false;
  }

  function commitConfiguration(generation: number, result: TemporaryPolicyConfiguration): boolean {
    if (generation !== configGeneration) return false;
    configuration.value = result;
    loaded.value = true;
    error.value = null;
    errorDetail.value = "";
    return true;
  }

  function commitRestrictions(generation: number, result: TemporaryPolicyRestrictions, now = Date.now()): boolean {
    if (generation !== restrictionGeneration) return false;
    restrictions.value = result;
    restrictionsObservedAt.value = now;
    restrictionsLoaded.value = true;
    restrictionsError.value = null;
    restrictionsErrorDetail.value = "";
    return true;
  }

  async function loadConfiguration(retain = false): Promise<void> {
    const generation = beginConfigLoad();
    if (!retain) {
      error.value = null;
      errorDetail.value = "";
    }
    try {
      const result = await dashboardV4.getTemporaryUnavailability();
      commitConfiguration(generation, result);
    } catch (reason) {
      if (generation !== configGeneration) return;
      error.value = "load_failed";
      errorDetail.value = dashboardErrorDetail(reason);
    } finally {
      endConfigLoad(generation);
    }
  }

  async function loadRestrictions(retain = false): Promise<void> {
    const generation = beginRestrictionLoad();
    if (!retain) {
      restrictionsError.value = null;
      restrictionsErrorDetail.value = "";
    }
    try {
      const result = await dashboardV4.getTemporaryUnavailabilityRestrictions();
      commitRestrictions(generation, result);
    } catch (reason) {
      if (generation !== restrictionGeneration) return;
      restrictionsError.value = "load_failed";
      restrictionsErrorDetail.value = dashboardErrorDetail(reason);
    } finally {
      endRestrictionLoad(generation);
    }
  }

  async function load(retain = false): Promise<void> {
    await Promise.all([loadConfiguration(retain), loadRestrictions(retain)]);
  }

  async function recoverConfiguration(generation: number): Promise<void> {
    if (generation !== configGeneration) return;
    try {
      const result = await dashboardV4.getTemporaryUnavailability();
      if (generation !== configGeneration) return;
      configuration.value = result;
      loaded.value = true;
    } catch {
      // Keep the last successful snapshot so the operator can retry.
    }
  }

  async function saveRules(rules: readonly PolicyRule[], captured?: MutationExpectation): Promise<void> {
    const generation = beginMutation();
    try {
      const snapshot = configuration.value?.revision;
      const result = await controlPlane.runMutation((expectation) =>
        dashboardV4.putTemporaryUnavailability(
          { rules: [...rules] },
          expectation,
        ),
        captured ?? (snapshot ? { expectedRevision: snapshot.revision, processGeneration: snapshot.processGeneration } : undefined),
      );
      commitConfiguration(generation, result);
    } catch (reason) {
      if (generation === configGeneration) {
        if (isRevisionConflict(reason)) {
          error.value = "conflict";
          errorDetail.value = "";
          await recoverConfiguration(generation);
        } else {
          error.value = "save_failed";
          errorDetail.value = dashboardErrorDetail(reason);
        }
      }
      throw reason;
    } finally {
      endMutation(generation);
    }
  }

  async function clearRestriction(id: string): Promise<void> {
    const generation = beginClear();
    try {
      const snapshot = restrictions.value?.revision;
      const result = await controlPlane.runMutation((expectation) =>
        dashboardV4.clearTemporaryUnavailabilityRestriction(id, {}, expectation),
        snapshot ? { expectedRevision: snapshot.revision, processGeneration: snapshot.processGeneration } : undefined,
      );
      commitRestrictions(generation, result);
    } catch (reason) {
      if (generation === restrictionGeneration) {
        if (isRevisionConflict(reason)) {
          restrictionsError.value = "conflict";
          restrictionsErrorDetail.value = "";
          try {
            const refreshed = await dashboardV4.getTemporaryUnavailabilityRestrictions();
            if (generation === restrictionGeneration) {
              restrictions.value = refreshed;
              restrictionsObservedAt.value = Date.now();
              restrictionsLoaded.value = true;
            }
          } catch {
            // Keep the last successful snapshot.
          }
        } else {
          restrictionsError.value = "clear_failed";
          restrictionsErrorDetail.value = dashboardErrorDetail(reason);
        }
      }
      throw reason;
    } finally {
      endClear(generation);
    }
  }

  function clear(): void {
    sessionGeneration = Math.max(configGeneration, restrictionGeneration) + 1;
    configGeneration = sessionGeneration;
    restrictionGeneration = sessionGeneration;
    configLoadInFlight = 0;
    restrictionLoadInFlight = 0;
    mutationInFlight = 0;
    clearInFlight = 0;
    configuration.value = null;
    restrictions.value = null;
    restrictionsObservedAt.value = null;
    loaded.value = false;
    restrictionsLoaded.value = false;
    loading.value = false;
    restrictionsLoading.value = false;
    mutating.value = false;
    clearing.value = false;
    error.value = null;
    errorDetail.value = "";
    restrictionsError.value = null;
    restrictionsErrorDetail.value = "";
  }

  return {
    configuration: computed(() => configuration.value),
    restrictions: computed(() => restrictions.value),
    restrictionsObservedAt: computed(() => restrictionsObservedAt.value),
    loaded: computed(() => loaded.value),
    restrictionsLoaded: computed(() => restrictionsLoaded.value),
    loading: computed(() => loading.value),
    restrictionsLoading: computed(() => restrictionsLoading.value),
    mutating: computed(() => mutating.value),
    clearing: computed(() => clearing.value),
    error: computed(() => error.value),
    errorDetail: computed(() => errorDetail.value),
    restrictionsError: computed(() => restrictionsError.value),
    restrictionsErrorDetail: computed(() => restrictionsErrorDetail.value),
    load,
    loadConfiguration,
    loadRestrictions,
    saveRules,
    clearRestriction,
    clear,
  };
});
