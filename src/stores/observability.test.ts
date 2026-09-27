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

test("session teardown clears both log snapshots and rejects late forward responses", async () => {
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
