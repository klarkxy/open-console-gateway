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
import type {
  OperationLog,
  OperationLogQuery,
  RequestLog,
  RequestLogQuery,
  RequestLogSummary,
} from "../api/log-ledger-types.ts";
import { requestLogQueryKey } from "../domain/log-ledger.ts";
import { dashboardErrorDetail } from "../utils/errors.ts";

function emptySummary(): ForwardLogSummary {
  return { total_requests: 0, prompt_tokens: 0, completion_tokens: 0, cached_tokens: 0, cost: 0 };
}

function emptyRequestSummary(): RequestLogSummary {
  return {
    totalRequests: 0,
    totalAttempts: 0,
    promptTokens: 0,
    completionTokens: 0,
    cachedTokens: 0,
  };
}

export interface RequestAttemptDetail {
  items: ForwardLog[];
  loaded: boolean;
  loading: boolean;
  error: string;
}

/**
 * Read-only log snapshots. Each resource has its own generation so a slower
 * filter cannot overwrite a newer one. Request attempt details use a separate
 * generation and are dropped when the request page identity changes.
 */
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
  const operationLogs = ref<OperationLog[]>([]);
  const operationTotal = ref(0);
  const operationLimit = ref(0);
  const operationOffset = ref(0);
  const operationLoaded = ref(false);
  const operationLoading = ref(false);
  const operationError = ref("");
  const operationLoadedAt = ref(0);
  const requestLogs = ref<RequestLog[]>([]);
  const requestSummary = ref<RequestLogSummary>(emptyRequestSummary());
  const requestTotal = ref(0);
  const requestLimit = ref(0);
  const requestOffset = ref(0);
  const requestLoaded = ref(false);
  const requestLoading = ref(false);
  const requestError = ref("");
  const requestLoadedAt = ref(0);
  const requestDetails = ref<Record<string, RequestAttemptDetail>>({});
  const models = ref<string[]>([]);
  const clientKeys = ref<ForwardLogClientKey[]>([]);
  let gatewayGeneration = 0;
  let gatewayAbort: AbortController | null = null;
  let forwardGeneration = 0;
  let forwardAbort: AbortController | null = null;
  let operationGeneration = 0;
  let operationAbort: AbortController | null = null;
  let requestGeneration = 0;
  let requestAbort: AbortController | null = null;
  let requestPageKey = "";
  let detailSession = 0;
  const detailGenerations = new Map<string, number>();
  const detailAborts = new Map<string, AbortController>();
  let modelsGeneration = 0;
  let keysGeneration = 0;

  function dropRequestDetails(): void {
    detailSession += 1;
    for (const abort of detailAborts.values()) abort.abort();
    detailAborts.clear();
    detailGenerations.clear();
    requestDetails.value = {};
  }

  function invalidateRequestDetailReads(): void {
    detailSession += 1;
    for (const abort of detailAborts.values()) abort.abort();
    detailAborts.clear();
    detailGenerations.clear();
    requestDetails.value = Object.fromEntries(Object.entries(requestDetails.value)
      .map(([key, detail]) => [key, { ...detail, loading: false }]));
  }

  function pruneRequestDetails(items: readonly RequestLog[]): void {
    const allowed = new Set(items.map((item) => item.requestKey));
    const next: Record<string, RequestAttemptDetail> = {};
    for (const [key, detail] of Object.entries(requestDetails.value)) {
      if (allowed.has(key)) next[key] = detail;
    }
    requestDetails.value = next;
  }

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

  async function loadOperations(query: OperationLogQuery): Promise<string | null | undefined> {
    const generation = ++operationGeneration;
    operationAbort?.abort();
    const abort = new AbortController();
    operationAbort = abort;
    operationLoading.value = true;
    operationError.value = "";
    try {
      const result = await dashboardApi.getOperationLogs(query, abort.signal);
      if (generation !== operationGeneration) return;
      operationLogs.value = result.items;
      operationTotal.value = result.total;
      operationLimit.value = result.limit;
      operationOffset.value = result.offset;
      operationLoaded.value = true;
      operationLoadedAt.value = Date.now();
      return null;
    } catch (error) {
      if (generation !== operationGeneration || abort.signal.aborted) return;
      operationError.value = dashboardErrorDetail(error);
      return operationError.value;
    } finally {
      if (generation === operationGeneration) operationLoading.value = false;
    }
  }

  async function loadRequests(query: RequestLogQuery): Promise<string | null | undefined> {
    const nextPageKey = requestLogQueryKey(query);
    const pageChanged = nextPageKey !== requestPageKey;
    requestPageKey = nextPageKey;
    // The previous page is still rendered until this load succeeds. Keep its
    // detail content while invalidating every obsolete asynchronous read.
    if (pageChanged) invalidateRequestDetailReads();
    const generation = ++requestGeneration;
    requestAbort?.abort();
    const abort = new AbortController();
    requestAbort = abort;
    requestLoading.value = true;
    requestError.value = "";
    try {
      const result = await dashboardApi.getRequestLogs(query, abort.signal);
      if (generation !== requestGeneration) return;
      requestLogs.value = result.items;
      requestSummary.value = result.summary;
      requestTotal.value = result.total;
      requestLimit.value = result.limit;
      requestOffset.value = result.offset;
      requestLoaded.value = true;
      requestLoadedAt.value = Date.now();
      pruneRequestDetails(result.items);
      return null;
    } catch (error) {
      if (generation !== requestGeneration || abort.signal.aborted) return;
      requestError.value = dashboardErrorDetail(error);
      return requestError.value;
    } finally {
      if (generation === requestGeneration) requestLoading.value = false;
    }
  }

  async function loadRequestAttempts(
    requestKey: string,
    force = false,
  ): Promise<string | null | undefined> {
    const session = detailSession;
    if (!requestLogs.value.some((row) => row.requestKey === requestKey)) return;
    const current = requestDetails.value[requestKey];
    if (!force && (current?.loaded || current?.loading)) return null;
    const generation = (detailGenerations.get(requestKey) ?? 0) + 1;
    detailGenerations.set(requestKey, generation);
    detailAborts.get(requestKey)?.abort();
    const abort = new AbortController();
    detailAborts.set(requestKey, abort);
    requestDetails.value = {
      ...requestDetails.value,
      [requestKey]: {
        items: current?.items ?? [],
        loaded: current?.loaded ?? false,
        loading: true,
        error: "",
      },
    };
    try {
      const result = await dashboardApi.getRequestLogAttempts(requestKey, abort.signal);
      if (
        session !== detailSession
        || generation !== detailGenerations.get(requestKey)
        || !requestLogs.value.some((row) => row.requestKey === requestKey)
      ) return;
      requestDetails.value = {
        ...requestDetails.value,
        [requestKey]: { items: result.items, loaded: true, loading: false, error: "" },
      };
      return null;
    } catch (error) {
      if (
        session !== detailSession
        || generation !== detailGenerations.get(requestKey)
        || abort.signal.aborted
        || !requestLogs.value.some((row) => row.requestKey === requestKey)
      ) return;
      const message = dashboardErrorDetail(error);
      const previous = requestDetails.value[requestKey];
      requestDetails.value = {
        ...requestDetails.value,
        [requestKey]: {
          items: previous?.items ?? [],
          loaded: previous?.loaded ?? false,
          loading: false,
          error: message,
        },
      };
      return message;
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
    operationGeneration++;
    operationAbort?.abort();
    operationAbort = null;
    requestGeneration++;
    requestAbort?.abort();
    requestAbort = null;
    requestPageKey = "";
    dropRequestDetails();
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
    operationLogs.value = [];
    operationTotal.value = 0;
    operationLimit.value = 0;
    operationOffset.value = 0;
    operationLoaded.value = false;
    operationLoading.value = false;
    operationError.value = "";
    operationLoadedAt.value = 0;
    requestLogs.value = [];
    requestSummary.value = emptyRequestSummary();
    requestTotal.value = 0;
    requestLimit.value = 0;
    requestOffset.value = 0;
    requestLoaded.value = false;
    requestLoading.value = false;
    requestError.value = "";
    requestLoadedAt.value = 0;
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
    operationLogs: computed(() => operationLogs.value), operationTotal: computed(() => operationTotal.value),
    operationLimit: computed(() => operationLimit.value), operationOffset: computed(() => operationOffset.value),
    operationLoaded: computed(() => operationLoaded.value), operationLoading: computed(() => operationLoading.value),
    operationError: computed(() => operationError.value), operationLoadedAt: computed(() => operationLoadedAt.value),
    requestLogs: computed(() => requestLogs.value), requestSummary: computed(() => requestSummary.value),
    requestTotal: computed(() => requestTotal.value), requestLimit: computed(() => requestLimit.value),
    requestOffset: computed(() => requestOffset.value),
    requestLoaded: computed(() => requestLoaded.value), requestLoading: computed(() => requestLoading.value),
    requestError: computed(() => requestError.value), requestLoadedAt: computed(() => requestLoadedAt.value),
    requestDetails: computed(() => requestDetails.value),
    models: computed(() => models.value), clientKeys: computed(() => clientKeys.value),
    loadGateway, loadForward, loadOperations, loadRequests, loadRequestAttempts, loadModels, loadKeys, clear,
  };
});
