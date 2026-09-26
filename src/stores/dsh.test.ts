import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useDshStore } from "./dsh.ts";
import { useSessionStore } from "./session.ts";
import type { DshApplicationView } from "../api/dashboard-v4.ts";

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

function appBody(overrides: Record<string, unknown> = {}): DshApplicationView {
  return {
    selectedProfilePath: "C:\\Users\\author\\.dsh\\profiles\\web",
    status: "ready",
    detected: true,
    installed: false,
    installSupported: true,
    activationRequired: false,
    version: null,
    detail: null,
    targetPaths: [],
    discoveredProfiles: [],
    fingerprint: "fp-1",
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
    runtimeUrl: "http://127.0.0.1:3080",
    uninstallSupported: false,
    enabled: false,
    application: null,
    ...overrides,
  };
}

test("DSH store: a stale load cannot overwrite a newer snapshot or a cleared session", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useDshStore();
  const first = installDeferredFetch();
  const pendingFirst = store.load();
  await waitForCalls(first, 1);

  const second = installDeferredFetch();
  const pendingSecond = store.load({ runtimeUrl: "http://127.0.0.1:19387" });
  await waitForCalls(second, 1);
  second[0]!.resolve(appBody({ runtimeUrl: "http://127.0.0.1:19387", revision: { revision: 2, processGeneration: 1, pricingRevision: "p" } }));
  await pendingSecond;
  assert.equal(store.application?.runtimeUrl, "http://127.0.0.1:19387");
  assert.equal(store.loaded, true);

  first[0]!.resolve(appBody({ runtimeUrl: "http://127.0.0.1:3080" }));
  await pendingFirst;
  assert.equal(store.application?.runtimeUrl, "http://127.0.0.1:19387");

  const third = installDeferredFetch();
  const pendingThird = store.load();
  await waitForCalls(third, 1);
  store.clear();
  third[0]!.resolve(appBody());
  await pendingThird;
  assert.equal(store.application, null);
  assert.equal(store.loaded, false);
});

test("DSH store: a stale mutation cannot write back after a later load", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useDshStore();
  const mutation = installDeferredFetch();
  const pendingInstall = store.install(
    { keyId: "primary", expectedFingerprint: "fp-1", runtimeUrl: "http://127.0.0.1:3080" },
    { expectedRevision: 1, processGeneration: 1 },
  );
  await waitForCalls(mutation, 1);

  const load = installDeferredFetch();
  const pendingLoad = store.load({ runtimeUrl: "http://127.0.0.1:19387" });
  await waitForCalls(load, 1);
  load[0]!.resolve(appBody({ runtimeUrl: "http://127.0.0.1:19387", installed: false }));
  await pendingLoad;
  assert.equal(store.application?.runtimeUrl, "http://127.0.0.1:19387");

  mutation[0]!.resolve(appBody({ status: "installed", installed: true, runtimeUrl: "http://127.0.0.1:3080" }));
  await pendingInstall;
  assert.equal(store.application?.runtimeUrl, "http://127.0.0.1:19387");
  assert.equal(store.application?.installed, false);
});

test("dropSession clears the DSH snapshot so a later load cannot write back", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useDshStore();
  const calls = installDeferredFetch();
  const pending = store.load();
  await waitForCalls(calls, 1);
  useSessionStore().dropSession();
  assert.equal(store.application, null);
  calls[0]!.resolve(appBody());
  await pending;
  assert.equal(store.application, null);
  assert.equal(store.loaded, false);
  assert.equal(store.loading, false);
  assert.equal(store.mutating, false);
});

test("DSH store: a load started during install still clears mutating", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useDshStore();
  const mutation = installDeferredFetch();
  const pendingInstall = store.install(
    { keyId: "primary", expectedFingerprint: "fp-1", runtimeUrl: "http://127.0.0.1:3080" },
    { expectedRevision: 1, processGeneration: 1 },
  );
  await waitForCalls(mutation, 1);
  assert.equal(store.mutating, true);

  const load = installDeferredFetch();
  const pendingLoad = store.load({ runtimeUrl: "http://127.0.0.1:19387" });
  await waitForCalls(load, 1);
  assert.equal(store.loading, true);
  assert.equal(store.mutating, true);

  mutation[0]!.resolve(appBody({ status: "installed", installed: true }));
  await pendingInstall;
  assert.equal(store.mutating, false);
  assert.equal(store.loading, true);

  load[0]!.resolve(appBody({ runtimeUrl: "http://127.0.0.1:19387" }));
  await pendingLoad;
  assert.equal(store.loading, false);
  assert.equal(store.application?.runtimeUrl, "http://127.0.0.1:19387");
});

test("DSH store: an install started during load still clears loading", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useDshStore();
  const load = installDeferredFetch();
  const pendingLoad = store.load();
  await waitForCalls(load, 1);
  assert.equal(store.loading, true);

  const mutation = installDeferredFetch();
  const pendingInstall = store.install(
    { keyId: "primary", expectedFingerprint: "fp-1", runtimeUrl: "http://127.0.0.1:3080" },
    { expectedRevision: 1, processGeneration: 1 },
  );
  await waitForCalls(mutation, 1);
  assert.equal(store.loading, true);
  assert.equal(store.mutating, true);

  load[0]!.resolve(appBody({ runtimeUrl: "http://127.0.0.1:19387" }));
  await pendingLoad;
  assert.equal(store.loading, false);
  assert.equal(store.mutating, true);
  assert.equal(store.application, null);

  mutation[0]!.resolve(appBody({ status: "installed", installed: true, runtimeUrl: "http://127.0.0.1:3080" }));
  await pendingInstall;
  assert.equal(store.mutating, false);
  const installed = store.application as DshApplicationView | null;
  assert.equal(installed?.runtimeUrl, "http://127.0.0.1:3080");
  assert.equal(installed?.installed, true);
});

test("dropSession during a deferred install clears busy flags and ignores the receipt", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useDshStore();
  const mutation = installDeferredFetch();
  const pending = store.install(
    { keyId: "primary", expectedFingerprint: "fp-1" },
    { expectedRevision: 1, processGeneration: 1 },
  );
  await waitForCalls(mutation, 1);
  useSessionStore().dropSession();
  assert.equal(store.mutating, false);
  mutation[0]!.resolve(appBody({ installed: true }));
  await pending;
  assert.equal(store.application, null);
  assert.equal(store.mutating, false);
});

test("a previous session request cannot clear the new session loading flag", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useDshStore();
  const calls = installDeferredFetch();
  const oldLoad = store.load();
  await waitForCalls(calls, 1);
  store.clear();
  const newLoad = store.load();
  await waitForCalls(calls, 2);
  calls[0]!.resolve(appBody());
  await oldLoad;
  assert.equal(store.loading, true);
  assert.equal(store.application, null);
  calls[1]!.resolve(appBody());
  await newLoad;
  assert.equal(store.loading, false);
  assert.equal(store.loaded, true);
});
