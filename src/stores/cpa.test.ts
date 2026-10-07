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
    actions: { install: false, start: true, stop: false, checkUpdate: true, update: false, rollback: false, remove: true },
    clientKeysAvailable: true,
    codexDeviceLoginAvailable: overrides.running === true,
    startupRestorePending: overrides.desiredRunning === true,
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

function observedNull(value: unknown): boolean {
  return value === null;
}

function resolveCpa(calls: DeferredCall[], integration: object, runtime: object): void {
  for (const call of calls) {
    if (call.url.includes("/external-integrations/cpa/runtime")) call.resolve(runtime);
    else if (call.url.includes("/external-integrations/cpa")) call.resolve(integration);
  }
}

test("a runtime receipt commits server eligibility and fences an older runtime read", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useCpaStore();
  const calls = installDeferredFetch();
  const load = store.load();
  await waitForCalls(calls, 2);
  resolveCpa(calls, integrationBody(), runtimeBody());
  await load;
  assert.ok(store.runtime);
  const oldRead = store.refreshRuntime();
  await waitForCalls(calls, 3);
  const denied = { install: false, start: false, stop: false, checkUpdate: false, update: false, rollback: false, remove: false };
  store.commitRuntimeSnapshot({
    ...store.runtime,
    actions: denied,
    clientKeysAvailable: false,
    codexDeviceLoginAvailable: false,
    startupRestorePending: false,
  });
  calls[2]!.resolve(runtimeBody({ clientKeysAvailable: true }));
  assert.equal(await oldRead, null);
  assert.deepEqual(store.runtime?.actions, denied);
  assert.equal(store.runtime?.clientKeysAvailable, false);
  assert.equal(store.runtime?.codexDeviceLoginAvailable, false);
});

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

test("a snapshot from before logout cannot overwrite the next CPA session", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useCpaStore();
  const first = installDeferredFetch();
  const pendingFirst = store.load();
  await waitForCalls(first, 2);
  useSessionStore().dropSession();
  assert.equal(observedNull(store.integration), true);
  assert.equal(observedNull(store.runtime), true);

  const second = installDeferredFetch();
  const pendingSecond = store.load();
  await waitForCalls(second, 2);
  resolveCpa(
    second,
    integrationBody({ revision: 4, runtimeRunning: false }),
    runtimeBody({ currentVersion: "runtime-next", running: false }),
  );
  await pendingSecond;
  assert.equal(store.cardStatus, "stopped");
  assert.equal(store.integration?.revision, 4);
  assert.equal(store.runtime?.currentVersion, "runtime-next");

  resolveCpa(
    first,
    integrationBody({ revision: 2, runtimeRunning: true }),
    runtimeBody({ currentVersion: "runtime-old", running: true }),
  );
  await pendingFirst;
  assert.equal(store.cardStatus, "stopped");
  assert.equal(store.integration?.revision, 4);
  assert.equal(store.runtime?.currentVersion, "runtime-next");
  assert.equal(store.loaded, true);
});

test("refreshIntegration refuses a replaced session before the integration GET", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useCpaStore();
  const loaded = installDeferredFetch();
  const pendingLoad = store.load();
  await waitForCalls(loaded, 2);
  resolveCpa(loaded, integrationBody({ revision: 3 }), runtimeBody());
  await pendingLoad;
  assert.equal(store.integration?.revision, 3);

  const expired = store.currentSession();
  store.clear();
  assert.equal(observedNull(store.integration), true);
  const calls = installDeferredFetch();
  const refreshIntegration: (
    expectedSession?: number,
  ) => Promise<{ revision: number } | null> = store.refreshIntegration;
  const pending = refreshIntegration(expired);
  void pending.then(() => undefined, () => undefined);
  for (let attempt = 0; attempt < 30 && calls.length === 0; attempt += 1) {
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.equal(calls.length, 0, "a replaced session must not start the integration GET");
  assert.equal(observedNull(store.integration), true);
  assert.equal(store.loaded, false);
});
