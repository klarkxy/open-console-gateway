import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useCpaStore } from "./cpa.ts";
import { useSessionStore } from "./session.ts";

interface DeferredCall {
  url: string;
  method: string;
  resolve: (body: object) => void;
  reject: (error: unknown) => void;
}

function installDeferredFetch(): DeferredCall[] {
  installWindowDashboard();
  const calls: DeferredCall[] = [];
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: (input: string, init: RequestInit = {}) => new Promise<Response>((resolvePromise, rejectPromise) => {
      calls.push({
        url: String(input),
        method: init.method ?? "GET",
        resolve: (body) => resolvePromise(new Response(
          JSON.stringify(body),
          { headers: { "Content-Type": "application/json" } },
        )),
        reject: (error) => rejectPromise(error),
      });
    }),
  });
  return calls;
}

async function waitForCalls(calls: DeferredCall[], count: number): Promise<void> {
  for (let i = 0; i < 200 && calls.length < count; i++) {
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.equal(calls.length, count, `expected ${count} fetch calls, saw ${calls.length}`);
}

function integrationBody(overrides: Record<string, unknown> = {}): object {
  return {
    accountId: "cpa",
    baseUrl: "http://127.0.0.1:8317",
    baseUrlReadOnly: true,
    configured: true,
    currentOperation: null,
    enabled: true,
    inferenceKeyConfigured: true,
    installedVersion: "1.0.0",
    latestVersion: null,
    managementKeyConfigured: true,
    modelCount: 0,
    modelsRefreshedAt: null,
    processGeneration: 1,
    revision: 1,
    runtimeOwned: true,
    runtimeRunning: true,
    runtimeSupported: true,
    runtimeUnavailableReason: null,
    updateAvailable: false,
    ...overrides,
  };
}

function runtimeBody(overrides: Record<string, unknown> = {}): object {
  return {
    assetSha256: null,
    baseUrl: "http://127.0.0.1:8317",
    currentOperation: null,
    currentVersion: "1.0.0",
    error: null,
    installed: true,
    latestVersion: null,
    owned: true,
    phase: "idle",
    port: 8317,
    previousVersion: null,
    processGeneration: 1,
    revision: 1,
    running: true,
    supported: true,
    unavailableReason: null,
    updateAvailable: false,
    ...overrides,
  };
}

function resolveCpa(calls: DeferredCall[], integration: object, runtime: object): void {
  for (const call of calls) {
    if (call.url.includes("/external-integrations/cpa/runtime")) call.resolve(runtime);
    else if (call.url.includes("/external-integrations/cpa")) call.resolve(integration);
  }
}

test("CPA store: a stale load cannot overwrite a newer snapshot or a cleared session", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useCpaStore();
  const first = installDeferredFetch();
  const pendingFirst = store.load();
  await waitForCalls(first, 2);

  const second = installDeferredFetch();
  const pendingSecond = store.load();
  await waitForCalls(second, 2);
  resolveCpa(
    second,
    integrationBody({ runtimeRunning: false, revision: 2 }),
    runtimeBody({ running: false }),
  );
  await pendingSecond;
  assert.equal(store.cardStatus, "stopped");
  assert.equal(store.loaded, true);

  resolveCpa(first, integrationBody({ runtimeRunning: true }), runtimeBody({ running: true }));
  await pendingFirst;
  assert.equal(store.cardStatus, "stopped");

  const third = installDeferredFetch();
  const pendingThird = store.load();
  await waitForCalls(third, 2);
  store.clear();
  resolveCpa(third, integrationBody({ runtimeRunning: true }), runtimeBody({ running: true }));
  await pendingThird;
  assert.equal(store.integration, null);
  assert.equal(store.runtime, null);
  assert.equal(store.cardStatus, null);
  assert.equal(store.loaded, false);
});

test("dropSession clears the CPA snapshot so a later load cannot write back", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useCpaStore();
  const calls = installDeferredFetch();
  const pending = store.load();
  await waitForCalls(calls, 2);
  useSessionStore().dropSession();
  assert.equal(store.integration, null);
  resolveCpa(calls, integrationBody(), runtimeBody());
  await pending;
  assert.equal(store.integration, null);
  assert.equal(store.cardStatus, null);
});
