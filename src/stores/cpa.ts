import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardV3 } from "../api/dashboard-v3.ts";
import type { CpaIntegration, CpaRuntime } from "../api/generated/dashboard-v3.ts";
import { cpaCardStatus, type CpaCardStatus } from "../domain/cpa-runtime.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

/**
 * Secret-free CPA integration + runtime snapshot for Accounts (and any other
 * view that needs the pool's operational status). Mutations stay on the CPA
 * page; this store is a generation-guarded read.
 */
export const useCpaStore = defineStore("cpa", () => {
  const integration = ref<CpaIntegration | null>(null);
  const runtime = ref<CpaRuntime | null>(null);
  const loaded = ref(false);
  const loading = ref(false);
  const error = ref("");

  let loadGeneration = 0;

  const cardStatus = computed((): CpaCardStatus | null => (
    cpaCardStatus(integration.value, runtime.value)
  ));

  async function load(): Promise<void> {
    const generation = ++loadGeneration;
    loading.value = true;
    try {
      const [integrationResult, runtimeResult] = await Promise.allSettled([
        dashboardV3.getCpaIntegration(),
        dashboardV3.getCpaRuntime(),
      ]);
      if (generation !== loadGeneration) return;
      if (integrationResult.status === "rejected") {
        error.value = dashboardErrorDetail(integrationResult.reason);
        return;
      }
      integration.value = integrationResult.value;
      runtime.value = runtimeResult.status === "fulfilled" ? runtimeResult.value : null;
      error.value = "";
      loaded.value = true;
    } finally {
      if (generation === loadGeneration) loading.value = false;
    }
  }

  function clear(): void {
    loadGeneration += 1;
    integration.value = null;
    runtime.value = null;
    loaded.value = false;
    loading.value = false;
    error.value = "";
  }

  return {
    integration: computed(() => integration.value),
    runtime: computed(() => runtime.value),
    cardStatus,
    loaded: computed(() => loaded.value),
    loading: computed(() => loading.value),
    error: computed(() => error.value),
    load,
    clear,
  };
});
