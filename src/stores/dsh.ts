import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardV4, type DshApplicationView } from "../api/dashboard-v4.ts";
import type { MutationExpectation } from "../api/generated/dashboard-v3.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

export interface DshLoadInput {
  profilePath?: string;
  runtimeUrl?: string;
  retain?: boolean;
}

export interface DshMutationInput {
  profilePath?: string;
  runtimeUrl?: string;
  expectedFingerprint: string;
}

export interface DshInstallInput extends DshMutationInput {
  keyId: string;
}

/**
 * Single owner of DSH application server state. Views keep only drafts and
 * modal flags; data writes are generation-guarded so a stale response cannot
 * replace a newer snapshot or a cleared session. Load and mutation busy flags
 * are counted separately so overlapping operations still clear.
 */
export const useDshStore = defineStore("dsh", () => {
  const application = ref<DshApplicationView | null>(null);
  const loaded = ref(false);
  const loading = ref(false);
  const mutating = ref(false);
  const error = ref("");

  let dataGeneration = 0;
  let sessionGeneration = 0;
  let loadInFlight = 0;
  let mutationInFlight = 0;

  function beginLoad(): number {
    loadInFlight += 1;
    loading.value = true;
    return ++dataGeneration;
  }

  function endLoad(generation: number): void {
    if (generation < sessionGeneration) return;
    loadInFlight = Math.max(0, loadInFlight - 1);
    if (loadInFlight === 0) loading.value = false;
  }

  function beginMutation(): number {
    mutationInFlight += 1;
    mutating.value = true;
    return ++dataGeneration;
  }

  function endMutation(generation: number): void {
    if (generation < sessionGeneration) return;
    mutationInFlight = Math.max(0, mutationInFlight - 1);
    if (mutationInFlight === 0) mutating.value = false;
  }

  function commit(generation: number, result: DshApplicationView): boolean {
    if (generation !== dataGeneration) return false;
    application.value = result;
    error.value = "";
    loaded.value = true;
    return true;
  }

  async function load(input: DshLoadInput = {}): Promise<void> {
    const generation = beginLoad();
    if (!input.retain) error.value = "";
    try {
      const result = await dashboardV4.getDshApplication(input.profilePath, input.runtimeUrl);
      if (generation !== dataGeneration) return;
      application.value = result;
      error.value = "";
      loaded.value = true;
    } catch (reason) {
      if (generation !== dataGeneration) return;
      error.value = dashboardErrorDetail(reason);
    } finally {
      endLoad(generation);
    }
  }

  async function install(
    input: DshInstallInput,
    expectation: MutationExpectation,
  ): Promise<DshApplicationView> {
    const generation = beginMutation();
    try {
      const result = await dashboardV4.installDshApplication(
        {
          keyId: input.keyId,
          profilePath: input.profilePath,
          runtimeUrl: input.runtimeUrl,
          expectedFingerprint: input.expectedFingerprint,
        },
        expectation,
      );
      commit(generation, result);
      return result;
    } catch (reason) {
      if (generation === dataGeneration) {
        error.value = dashboardErrorDetail(reason);
      }
      throw reason;
    } finally {
      endMutation(generation);
    }
  }

  async function uninstall(
    input: DshMutationInput,
    expectation: MutationExpectation,
  ): Promise<DshApplicationView> {
    const generation = beginMutation();
    try {
      const result = await dashboardV4.uninstallDshApplication(
        {
          profilePath: input.profilePath,
          runtimeUrl: input.runtimeUrl,
          expectedFingerprint: input.expectedFingerprint,
        },
        expectation,
      );
      commit(generation, result);
      return result;
    } catch (reason) {
      if (generation === dataGeneration) {
        error.value = dashboardErrorDetail(reason);
      }
      throw reason;
    } finally {
      endMutation(generation);
    }
  }

  function clear(): void {
    sessionGeneration = ++dataGeneration;
    loadInFlight = 0;
    mutationInFlight = 0;
    application.value = null;
    loaded.value = false;
    loading.value = false;
    mutating.value = false;
    error.value = "";
  }

  return {
    application: computed(() => application.value),
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    mutating: computed(() => mutating.value),
    error: computed(() => error.value),
    load,
    install,
    uninstall,
    clear,
  };
});
