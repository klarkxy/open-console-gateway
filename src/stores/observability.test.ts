import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useObservabilityStore } from "./observability.ts";
import { useSessionStore } from "./session.ts";

type DeferredCall = { resolve: (body: object) => void; reject: (error: Error) => void };

function deferredFetch(): DeferredCall[] {
  installWindowDashboard();
  const calls: DeferredCall[] = [];
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: () => new Promise<Response>((resolve, reject) => {
      calls.push({
        resolve: (body) => resolve(new Response(JSON.stringify(body), {
          headers: { "Content-Type": "application/json" },
        })),
        reject,
      });
    }),
  });
  return calls;
}

function gatewayBody(id: number) {
  return { items: [{
    id, level: "debug", category: "gateway", message: "request captured",
    createdAt: "2026-09-27T00:00:00Z", requestId: `request-${id}`,
    attempt: null, errorSource: null, errorStage: null, durationMs: null, diagnostic: null,
  }] };
}

function forwardBody(id: number) {
  return {
    items: [{ id }],
    summary: { totalRequests: id, promptTokens: 2, completionTokens: 3, cachedTokens: 0, cost: 0 },
  };
}

test("gateway filter reload keeps the previous snapshot and discards an older response", async () => {
  const pinia = createPinia();
  setActivePinia(pinia);
  const calls = deferredFetch();
  const store = useObservabilityStore(pinia);
  const initial = store.loadGateway({ limit: 200, level: "INFO" });
  calls[0]!.resolve(gatewayBody(1));
  await initial;
  assert.equal(store.gatewayLogs[0]?.id, 1);
  assert.equal(store.gatewayLoaded, true);

  const oldFilter = store.loadGateway({ limit: 200, level: "WARN" });
  const newFilter = store.loadGateway({ limit: 200, level: "DEBUG", category: "gateway" });
  assert.equal(store.gatewayLogs[0]?.id, 1);
  assert.equal(store.gatewayLoading, true);
  calls[2]!.resolve(gatewayBody(3));
  await newFilter;
  calls[1]!.resolve(gatewayBody(2));
  await oldFilter;
  assert.equal(store.gatewayLogs[0]?.id, 3);
  assert.equal(store.gatewayLoading, false);

  const failed = store.loadGateway({ limit: 200, level: "ERROR" });
  calls[3]!.reject(new Error("network unavailable"));
  assert.equal(await failed, "network unavailable");
  assert.equal(store.gatewayLogs[0]?.id, 3);
  assert.equal(store.gatewayLoaded, true);
});

test("a newer log filter aborts the request it replaces", async () => {
  const pinia = createPinia();
  setActivePinia(pinia);
  installWindowDashboard();
  const signals: AbortSignal[] = [];
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: (_input: string, init: RequestInit = {}) => new Promise<Response>((_resolve, reject) => {
      signals.push(init.signal!);
      init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
    }),
  });
  const store = useObservabilityStore(pinia);
  const first = store.loadGateway({ limit: 200, level: "INFO" });
  const second = store.loadGateway({ limit: 200, level: "DEBUG" });
  assert.equal(signals[0]?.aborted, true);
  assert.equal(signals[1]?.aborted, false);
  assert.equal(await first, undefined);
  store.clear();
  assert.equal(signals[1]?.aborted, true);
  assert.equal(await second, undefined);
  assert.equal(store.gatewayError, "");
});

function requestPage(id: string, offset: number) {
  return {
    items: [{
      requestKey: `request:${id}`,
      requestId: id,
      timestamp: "2026-10-02T00:00:00.000Z",
      status: "success",
      httpStatus: 200,
      model: "public",
      requestedModel: "public",
      resolvedAlias: "alias",
      upstreamModel: "upstream-exact",
      attemptCount: 0,
      recordedRowCount: 1,
      promptTokens: 1,
      completionTokens: 1,
      cachedTokens: 1,
      durationMs: null,
      isLegacy: false,
      clientKeyId: "key-1",
      clientKeyName: "deleted key",
      providerId: "provider",
      accountId: "route-account",
      accountName: "Route",
      routeAccountId: "route-account",
      credentialAccountId: "credential-account",
      route: "proxy",
    }],
    total: 50,
    limit: 20,
    offset,
    summary: {
      totalRequests: 4,
      totalAttempts: 7,
      promptTokens: 10,
      completionTokens: 3,
      cachedTokens: 2,
    },
  };
}

test("request reload keeps the previous page and discards an older offset", async () => {
  const pinia = createPinia();
  setActivePinia(pinia);
  const calls = deferredFetch();
  const store = useObservabilityStore(pinia);
  const first = store.loadRequests({ limit: 20, offset: 0 });
  calls[0]!.resolve(requestPage("one", 0));
  await first;
  assert.equal(store.requestLogs[0]?.requestKey, "request:one");
  assert.equal(store.requestLoaded, true);
  assert.equal(store.requestSummary.totalRequests, 4);
  assert.equal(store.requestSummary.totalAttempts, 7);
  assert.equal(store.requestTotal, 50);
  assert.equal(store.requestLogs[0]?.attemptCount, 0);

  const stale = store.loadRequests({ limit: 20, offset: 20 });
  const current = store.loadRequests({ limit: 20, offset: 40, status: "error" });
  assert.equal(store.requestLogs[0]?.requestKey, "request:one");
  assert.equal(store.requestLoading, true);
  assert.equal(store.requestLoaded, true);
  calls[2]!.resolve(requestPage("three", 40));
  await current;
  calls[1]!.resolve(requestPage("two", 20));
  await stale;
  assert.equal(store.requestLogs[0]?.requestKey, "request:three");
  assert.equal(store.requestOffset, 40);
  assert.equal(store.requestSummary.totalAttempts, 7);
  assert.equal(store.requestLoading, false);
});

test("a changed request query drops attempt details and ignores the stale detail response", async () => {
  const pinia = createPinia();
  setActivePinia(pinia);
  installWindowDashboard();
  const requests: { url: string; signal: AbortSignal }[] = [];
  const pending: Array<{ url: string; resolve: (body: object) => void }> = [];
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: (input: string, init: RequestInit = {}) => new Promise<Response>((resolve) => {
      requests.push({ url: input, signal: init.signal! });
      pending.push({
        url: input,
        resolve: (body) => resolve(new Response(JSON.stringify(body), {
          headers: { "Content-Type": "application/json" },
        })),
      });
    }),
  });
  const store = useObservabilityStore(pinia);
  const page = store.loadRequests({ limit: 20, offset: 0, requestId: "one" });
  pending[0]!.resolve(requestPage("one", 0));
  await page;
  const detail = store.loadRequestAttempts("request:one");
  assert.equal(requests[1]?.url.includes("/logs/requests/request%3Aone/attempts"), true);
  assert.equal(store.requestDetails["request:one"]?.loading, true);

  const filtered = store.loadRequests({ limit: 20, offset: 0, requestId: "two" });
  assert.equal(requests[1]?.signal.aborted, true);
  assert.equal(store.requestDetails["request:one"]?.loading, false);
  pending[1]!.resolve({ items: [{ id: 9, requestId: "one", status: "error" }] });
  await detail;
  assert.equal(store.requestDetails["request:one"]?.loaded, false);

  pending[2]!.resolve(requestPage("two", 0));
  await filtered;
  assert.equal(store.requestLogs[0]?.requestId, "two");
  assert.deepEqual(store.requestDetails, {});
  assert.equal(store.requestLoaded, true);
});

test("a same-page refresh omits a departed request and ignores its late attempt error", async () => {
  const pinia = createPinia();
  setActivePinia(pinia);
  const calls = deferredFetch();
  const store = useObservabilityStore(pinia);
  const page = store.loadRequests({ limit: 20, offset: 0 });
  calls[0]!.resolve(requestPage("one", 0));
  await page;
  const detail = store.loadRequestAttempts("request:one");
  assert.equal(store.requestDetails["request:one"]?.loading, true);

  const refresh = store.loadRequests({ limit: 20, offset: 0 });
  calls[2]!.resolve(requestPage("other", 0));
  await refresh;
  assert.equal(store.requestLogs[0]?.requestKey, "request:other");
  assert.equal(store.requestDetails["request:one"], undefined);

  calls[1]!.reject(new Error("attempt failed"));
  assert.equal(await detail, undefined);
  assert.equal(store.requestDetails["request:one"], undefined);
  assert.deepEqual(store.requestDetails, {});
});

test("session teardown clears operation, request, summary, and attempt caches", async () => {
  const pinia = createPinia();
  setActivePinia(pinia);
  const calls = deferredFetch();
  const store = useObservabilityStore(pinia);
  const session = useSessionStore(pinia);
  const gateway = store.loadGateway({ limit: 200 });
  calls[0]!.resolve(gatewayBody(4));
  await gateway;
  const forward = store.loadForward({ limit: 20 });
  calls[1]!.resolve(forwardBody(5));
  await forward;
  assert.equal(store.forwardTotals.total_requests, 5);

  const revalidation = store.loadForward({ limit: 20, status: "error" });
  assert.equal(store.forwardLogs[0]?.id, 5);
  assert.equal(store.forwardTotals.total_requests, 5);
  session.dropSession();
  assert.deepEqual(store.gatewayLogs, []);
  assert.deepEqual(store.forwardLogs, []);
  assert.equal(store.forwardLoaded, false);
  calls[2]!.resolve(forwardBody(6));
  await revalidation;
  assert.deepEqual(store.forwardLogs, []);
  assert.equal(store.forwardTotals.total_requests, 0);
  assert.equal(store.forwardLoading, false);
});

test("logout clears a loaded operation page, request summary, and late attempt rows", async () => {
  const pinia = createPinia();
  setActivePinia(pinia);
  const calls = deferredFetch();
  const store = useObservabilityStore(pinia);
  const session = useSessionStore(pinia);
  const operations = store.loadOperations({ limit: 20, offset: 0, outcome: "pending" });
  calls[0]!.resolve({
    items: [{
      operationId: "op-1",
      startedAt: "2026-10-02T00:00:00.000Z",
      completedAt: null,
      action: "account.create",
      source: "cli",
      actorId: null,
      subjectType: "account",
      subjectId: "acc-1",
      outcome: "pending",
      reasonCode: null,
      metadata: {
        changedFields: [],
        requestedCount: null,
        completedCount: null,
        failedCount: null,
        revision: null,
        compensated: null,
        relatedIds: [],
      },
    }],
    total: 3,
    limit: 20,
    offset: 0,
  });
  await operations;
  const requests = store.loadRequests({ limit: 20, offset: 0 });
  calls[1]!.resolve(requestPage("kept", 0));
  await requests;
  const attempts = store.loadRequestAttempts("request:kept");
  assert.equal(store.operationLogs[0]?.actorId, null);
  assert.equal(store.operationTotal, 3);
  assert.equal(store.requestSummary.totalRequests, 4);
  session.dropSession();
  assert.deepEqual(store.operationLogs, []);
  assert.equal(store.operationTotal, 0);
  assert.equal(store.operationLoaded, false);
  assert.deepEqual(store.requestLogs, []);
  assert.equal(store.requestSummary.totalRequests, 0);
  assert.equal(store.requestSummary.totalAttempts, 0);
  assert.equal(store.requestLoaded, false);
  calls[2]!.resolve({ items: [{ id: 1, requestId: "kept" }] });
  await attempts;
  assert.deepEqual(store.requestDetails, {});
  assert.equal(store.requestLoading, false);
  assert.equal(store.operationLoading, false);
});

test("refreshing an expanded request preserves its rendered attempts until a fresh receipt arrives", async () => {
  const pinia = createPinia();
  const calls = deferredFetch();
  const store = useObservabilityStore(pinia);
  const initial = store.loadRequests({ limit: 20, offset: 0 });
  calls[0]!.resolve(requestPage("kept", 0));
  await initial;
  const firstDetail = store.loadRequestAttempts("request:kept");
  calls[1]!.resolve({ items: [{ id: 1, status: "streaming", requestId: "kept" }] });
  await firstDetail;
  const refresh = store.loadRequestAttempts("request:kept", true);
  assert.equal(store.requestDetails["request:kept"]?.loaded, true);
  assert.equal(store.requestDetails["request:kept"]?.loading, true);
  assert.equal(store.requestDetails["request:kept"]?.items[0]?.status, "streaming");
  calls[2]!.resolve({ items: [{ id: 1, status: "success", requestId: "kept" }] });
  await refresh;
  assert.equal(store.requestDetails["request:kept"]?.loading, false);
  assert.equal(store.requestDetails["request:kept"]?.items[0]?.status, "success");
});

test("a rolling request-window reload can refill the same expanded request", async () => {
  const pinia = createPinia();
  const calls = deferredFetch();
  const store = useObservabilityStore(pinia);
  const initial = store.loadRequests({ limit: 20, startTime: "2026-10-01T00:00:00Z" });
  calls[0]!.resolve(requestPage("kept", 0));
  await initial;
  const firstDetail = store.loadRequestAttempts("request:kept");
  calls[1]!.resolve({ items: [{ id: 1, status: "streaming", requestId: "kept" }] });
  await firstDetail;
  const page = store.loadRequests({ limit: 20, startTime: "2026-10-01T00:01:00Z" });
  calls[2]!.resolve(requestPage("kept", 0));
  await page;
  const detail = store.loadRequestAttempts("request:kept", true);
  calls[3]!.resolve({ items: [{ id: 1, status: "success", requestId: "kept" }] });
  await detail;
  assert.equal(store.requestDetails["request:kept"]?.loaded, true);
  assert.equal(store.requestDetails["request:kept"]?.items[0]?.status, "success");
});
