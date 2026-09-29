import { computed, ref } from "vue";
import { defineStore } from "pinia";
import { dashboardApi } from "../api/dashboard.ts";
import type {
  ForwardLog,
  ForwardLogClientKey,
  ForwardLogQuery,
  ForwardLogSummary,
  GatewayLog,
} from "../api/dashboard.ts";
import type { GatewayLogQuery } from "../api/generated/dashboard-v3.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

function emptySummary(): ForwardLogSummary {
  return { total_requests: 0, prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cost: 0 };
}

/** Read-only log snapshots. Each resource has its own generation so refreshes cannot overwrite newer filters. */
export const useObservabilityStore = defineStore("observability", () => {
  const gatewayLogs = ref<GatewayLog[]>([]);
  const gatewayLoaded = ref(false);
  const gatewayLoading = ref(false);
  const gatewayError = ref("");
  const gatewayLoadedAt = ref(0);
  const forwardLogs = ref<ForwardLog[]>([]);
  const forwardTotals = ref<ForwardLogSummary>(emptySummary());
  const forwardLoaded = ref(false);
  const forwardLoading = ref(false);
  const forwardError = ref("");
  const forwardLoadedAt = ref(0);
  const models = ref<string[]>([]);
  const clientKeys = ref<ForwardLogClientKey[]>([]);
  let gatewayGeneration = 0;
  let gatewayAbort: AbortController | null = null;
  let forwardGeneration = 0;
  let forwardAbort: AbortController | null = null;
  let modelsGeneration = 0;
  let keysGeneration = 0;

  async function loadGateway(query: GatewayLogQuery): Promise<string | null | undefined> {
    const generation = ++gatewayGeneration;
    gatewayAbort?.abort();
    const abort = new AbortController();
    gatewayAbort = abort;
    gatewayLoading.value = true;
    gatewayError.value = "";
    try {
      const result = await dashboardApi.getGatewayLogs(query, abort.signal);
      if (generation !== gatewayGeneration) return;
      gatewayLogs.value = result;
      gatewayLoaded.value = true;
      gatewayLoadedAt.value = Date.now();
      return null;
    } catch (error) {
      if (generation !== gatewayGeneration || abort.signal.aborted) return;
      gatewayError.value = dashboardErrorDetail(error);
      return gatewayError.value;
    } finally {
      if (generation === gatewayGeneration) gatewayLoading.value = false;
    }
  }

  async function loadForward(query: ForwardLogQuery): Promise<string | null | undefined> {
    const generation = ++forwardGeneration;
    forwardAbort?.abort();
    const abort = new AbortController();
    forwardAbort = abort;
    forwardLoading.value = true;
    forwardError.value = "";
    try {
      const result = await dashboardApi.getForwardLogs(query, abort.signal);
      if (generation !== forwardGeneration) return;
      forwardLogs.value = result.items;
      forwardTotals.value = result.summary;
      forwardLoaded.value = true;
      forwardLoadedAt.value = Date.now();
      return null;
    } catch (error) {
      if (generation !== forwardGeneration || abort.signal.aborted) return;
      forwardError.value = dashboardErrorDetail(error);
      return forwardError.value;
    } finally {
      if (generation === forwardGeneration) forwardLoading.value = false;
    }
  }

  async function loadModels(): Promise<string | null | undefined> {
    const generation = ++modelsGeneration;
    try {
      const result = await dashboardApi.getForwardLogModels();
      if (generation !== modelsGeneration) return;
      models.value = result;
      return null;
    } catch (error) {
      if (generation === modelsGeneration) return dashboardErrorDetail(error);
    }
  }

  async function loadKeys(): Promise<string | null | undefined> {
    const generation = ++keysGeneration;
    try {
      const result = await dashboardApi.getForwardLogKeys();
      if (generation !== keysGeneration) return;
      clientKeys.value = result;
      return null;
    } catch (error) {
      if (generation === keysGeneration) return dashboardErrorDetail(error);
    }
  }

  function clear(): void {
    gatewayGeneration++;
    gatewayAbort?.abort();
    gatewayAbort = null;
    forwardGeneration++;
    forwardAbort?.abort();
    forwardAbort = null;
    modelsGeneration++;
    keysGeneration++;
    gatewayLogs.value = [];
    gatewayLoaded.value = false;
    gatewayLoading.value = false;
    gatewayError.value = "";
    gatewayLoadedAt.value = 0;
    forwardLogs.value = [];
    forwardTotals.value = emptySummary();
    forwardLoaded.value = false;
    forwardLoading.value = false;
    forwardError.value = "";
    forwardLoadedAt.value = 0;
    models.value = [];
    clientKeys.value = [];
  }

  return {
    gatewayLogs: computed(() => gatewayLogs.value), gatewayLoaded: computed(() => gatewayLoaded.value),
    gatewayLoading: computed(() => gatewayLoading.value), gatewayError: computed(() => gatewayError.value),
    gatewayLoadedAt: computed(() => gatewayLoadedAt.value),
    forwardLogs: computed(() => forwardLogs.value), forwardTotals: computed(() => forwardTotals.value),
    forwardLoaded: computed(() => forwardLoaded.value), forwardLoading: computed(() => forwardLoading.value),
    forwardError: computed(() => forwardError.value), forwardLoadedAt: computed(() => forwardLoadedAt.value),
    models: computed(() => models.value), clientKeys: computed(() => clientKeys.value),
    loadGateway, loadForward, loadModels, loadKeys, clear,
  };
});
