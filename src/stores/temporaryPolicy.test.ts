import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { BUILTIN_GOAT_ID } from "../domain/temporary-policy.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useSessionStore } from "./session.ts";
import { useTemporaryPolicyStore } from "./temporaryPolicy.ts";
import type { PolicyRule } from "../domain/temporary-policy.ts";

interface DeferredCall {
  url: string;
  method: string;
  body: Record<string, unknown> | null;
  resolve: (body: object, status?: number) => void;
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
        body: init.body ? JSON.parse(String(init.body)) as Record<string, unknown> : null,
        resolve: (body, status = 200) => resolvePromise(new Response(
          JSON.stringify(body),
          { status, headers: { "Content-Type": "application/json" } },
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

function configBody(overrides: Record<string, unknown> = {}) {
  return {
    revision: { revision: 3, processGeneration: 99, pricingRevision: "p" },
    rules: [] as PolicyRule[],
    builtins: [{
      id: BUILTIN_GOAT_ID,
      scope: "credential_model",
      backoff: { initialSeconds: 30, maxSeconds: 300 },
    }],
    ...overrides,
  };
}

function restrictionsBody(overrides: Record<string, unknown> = {}) {
  return {
    revision: { revision: 3, processGeneration: 99, pricingRevision: "p" },
    restrictions: [],
    ...overrides,
  };
}

const sampleRule: PolicyRule = {
  kind: "custom",
  id: "custom.lab",
  destinationId: null,
  enabled: true,
  scope: "credential_model",
  match: { statusCodes: [429] },
  backoff: { initialSeconds: 30, maxSeconds: 300 },
};

test("policy writes retain snapshot tokens after unrelated responses advance the control plane", async () => {
  setActivePinia(createPinia());
  const control = useControlPlaneStore();
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const load = store.loadConfiguration();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(configBody());
  await load;
  const diagnostics = store.loadRestrictions();
  await waitForCalls(calls, 2);
  calls[1]!.resolve(restrictionsBody({ revision: { revision: 4, processGeneration: 99 } }));
  await diagnostics;
  assert.equal(control.revision, 4);
  const save = store.saveRules([sampleRule]);
  await waitForCalls(calls, 3);
  assert.equal(calls[2]!.body?.expectedRevision, 3);
  assert.equal(calls[2]!.body?.processGeneration, 99);
  calls[2]!.resolve(configBody());
  await save;

  const capturedSave = store.saveRules([sampleRule], { expectedRevision: 2, processGeneration: 98 });
  await waitForCalls(calls, 4);
  assert.equal(calls[3]!.body?.expectedRevision, 2);
  assert.equal(calls[3]!.body?.processGeneration, 98);
  calls[3]!.resolve(configBody());
  await capturedSave;

  control.sync({ revision: 1, processGeneration: 100 });
  const clear = store.clearRestriction("tp-1");
  await waitForCalls(calls, 5);
  assert.equal(calls[4]!.body?.expectedRevision, 4);
  assert.equal(calls[4]!.body?.processGeneration, 99);
  calls[4]!.resolve(restrictionsBody());
  await clear;
});

test("a stale configuration load cannot overwrite a newer snapshot or a cleared session", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useTemporaryPolicyStore();
  const first = installDeferredFetch();
  const pendingFirst = store.loadConfiguration();
  await waitForCalls(first, 1);

  const second = installDeferredFetch();
  const pendingSecond = store.loadConfiguration();
  await waitForCalls(second, 1);
  second[0]!.resolve(configBody({
    revision: { revision: 4, processGeneration: 99, pricingRevision: "p" },
    rules: [sampleRule],
  }));
  await pendingSecond;
  assert.equal(store.configuration?.revision.revision, 4);
  assert.equal(store.configuration?.rules.length, 1);

  first[0]!.resolve(configBody({ rules: [] }));
  await pendingFirst;
  assert.equal(store.configuration?.revision.revision, 4);
  assert.equal(store.configuration?.rules.length, 1);

  const third = installDeferredFetch();
  const pendingThird = store.loadConfiguration();
  await waitForCalls(third, 1);
  store.clear();
  third[0]!.resolve(configBody({ rules: [sampleRule] }));
  await pendingThird;
  assert.equal(store.configuration, null);
  assert.equal(store.loaded, false);
});

test("a stale mutation cannot write back after a later load", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: "p" });
  const store = useTemporaryPolicyStore();
  const mutation = installDeferredFetch();
  const pendingSave = store.saveRules([sampleRule]).then(() => "ok", (error: unknown) => error);
  await waitForCalls(mutation, 1);

  const load = installDeferredFetch();
  const pendingLoad = store.loadConfiguration();
  await waitForCalls(load, 1);
  load[0]!.resolve(configBody({
    revision: { revision: 8, processGeneration: 99, pricingRevision: "p" },
    rules: [],
  }));
  await pendingLoad;
  assert.equal(store.configuration?.rules.length, 0);

  mutation[0]!.resolve(configBody({
    revision: { revision: 9, processGeneration: 99, pricingRevision: "p" },
    rules: [sampleRule],
  }));
  await pendingSave;
  assert.equal(store.configuration?.revision.revision, 8);
  assert.equal(store.configuration?.rules.length, 0);
});

test("dropSession clears snapshots so a later load cannot write back", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const pending = store.loadConfiguration();
  await waitForCalls(calls, 1);
  useSessionStore().dropSession();
  assert.equal(store.configuration, null);
  calls[0]!.resolve(configBody({ rules: [sampleRule] }));
  await pending;
  assert.equal(store.configuration, null);
  assert.equal(store.loaded, false);
  assert.equal(store.loading, false);
  assert.equal(store.mutating, false);
});

test("dropSession during a deferred save clears busy flags and ignores the receipt", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: "p" });
  const store = useTemporaryPolicyStore();
  const mutation = installDeferredFetch();
  const pending = store.saveRules([sampleRule]).then(() => "ok", (error: unknown) => error);
  await waitForCalls(mutation, 1);
  useSessionStore().dropSession();
  assert.equal(store.mutating, false);
  mutation[0]!.resolve(configBody({ rules: [sampleRule] }));
  await pending;
  assert.equal(store.configuration, null);
  assert.equal(store.mutating, false);
});

test("a previous session request cannot clear the new session loading flag", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const oldLoad = store.loadConfiguration();
  await waitForCalls(calls, 1);
  store.clear();
  const newLoad = store.loadConfiguration();
  await waitForCalls(calls, 2);
  calls[0]!.resolve(configBody());
  await oldLoad;
  assert.equal(store.loading, true);
  assert.equal(store.configuration === null, true);
  calls[1]!.resolve(configBody({ rules: [sampleRule] }));
  await newLoad;
  assert.equal(store.loading, false);
  assert.equal(store.loaded, true);
  assert.equal(store.configuration?.rules.length, 1);
});

test("CAS conflict reloads configuration, keeps the error, and does not replay the PUT", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: "p" });
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.loadConfiguration();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(configBody({ rules: [] }));
  await pendingLoad;

  const pendingSave = store.saveRules([sampleRule]).then(() => "ok", (error: unknown) => error);
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "PUT");
  assert.match(calls[1]!.url, /\/routing\/temporary-unavailability$/);
  assert.equal(calls[1]!.body?.expectedRevision, 3);
  calls[1]!.resolve({
    code: "revisionConflict",
    message: "conflict",
    currentRevision: 8,
    processGeneration: 99,
  }, 409);
  await waitForCalls(calls, 3);
  assert.match(calls[2]!.url, /\/contract$/);
  calls[2]!.resolve({ revision: 8, processGeneration: 99, pricingRevision: "p" });
  await waitForCalls(calls, 4);
  assert.equal(calls[3]!.method, "GET");
  assert.match(calls[3]!.url, /\/routing\/temporary-unavailability$/);
  assert.equal(calls[3]!.url.includes("/restrictions"), false);
  calls[3]!.resolve(configBody({
    revision: { revision: 8, processGeneration: 99, pricingRevision: "p" },
    rules: [],
  }));
  const result = await pendingSave;
  assert.notEqual(result, "ok");
  assert.equal(calls.filter((call) => call.method === "PUT").length, 1);
  assert.equal(store.configuration?.revision.revision, 8);
  assert.equal(store.configuration?.rules.length, 0);
  assert.equal(store.error, "conflict");
});

test("a failed save keeps the last successful snapshot for retry", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: "p" });
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.loadConfiguration();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(configBody({ rules: [] }));
  await pendingLoad;

  const pendingSave = store.saveRules([sampleRule]).then(() => "ok", (error: unknown) => error);
  await waitForCalls(calls, 2);
  calls[1]!.resolve({ message: "backend rejected" }, 400);
  const result = await pendingSave;
  assert.notEqual(result, "ok");
  assert.equal(store.configuration?.rules.length, 0);
  assert.equal(store.error, "save_failed");
  assert.equal(store.loaded, true);
});

test("revalidation keeps the current configuration while a later load is in flight", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const first = store.loadConfiguration();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(configBody({ rules: [sampleRule] }));
  await first;
  assert.equal(store.loaded, true);
  assert.equal(store.configuration?.rules.length, 1);

  const second = store.loadConfiguration(true);
  await waitForCalls(calls, 2);
  assert.equal(store.loading, true);
  assert.equal(store.loaded, true);
  assert.equal(store.configuration?.rules.length, 1);
  calls[1]!.resolve(configBody({
    revision: { revision: 4, processGeneration: 99, pricingRevision: "p" },
    rules: [sampleRule],
  }));
  await second;
  assert.equal(store.loading, false);
  assert.equal(store.configuration?.revision.revision, 4);
});

test("a failed revalidation keeps the last successful snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const first = store.loadConfiguration();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(configBody({ rules: [sampleRule] }));
  await first;

  const second = store.loadConfiguration(true);
  await waitForCalls(calls, 2);
  calls[1]!.resolve({ message: "unavailable" }, 500);
  await second;
  assert.equal(store.loaded, true);
  assert.equal(store.configuration?.rules.length, 1);
  assert.equal(store.error, "load_failed");
});

test("restriction diagnostics are GET-only and never send a probe", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const pending = store.loadRestrictions();
  await waitForCalls(calls, 1);
  assert.equal(calls[0]!.method, "GET");
  assert.match(calls[0]!.url, /\/routing\/temporary-unavailability\/restrictions$/);
  assert.equal(calls[0]!.url.includes("probe"), false);
  calls[0]!.resolve(restrictionsBody({
    restrictions: [{
      id: "wait-1",
      ruleId: BUILTIN_GOAT_ID,
      ruleGeneration: 1,
      source: "builtin",
      credentialId: "cred-1",
      destinationId: "dest-1",
      scope: "credential_model",
      upstreamModel: "minimax-m3",
      state: "waiting",
      nextProbeInSeconds: 9,
      probeInFlight: false,
    }],
  }));
  await pending;
  assert.equal(store.restrictions?.restrictions.length, 1);
  assert.equal(calls.some((call) => call.method !== "GET"), false);
});

test("clear restriction posts the id, commits the refreshed list, and does not replay on 409", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 3, processGeneration: 99, pricingRevision: "p" });
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const pendingLoad = store.loadRestrictions();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(restrictionsBody({
    restrictions: [{
      id: "wait-1",
      ruleId: "custom.lab",
      ruleGeneration: 2,
      source: "global",
      credentialId: "cred-1",
      destinationId: "dest-1",
      scope: "credential",
      upstreamModel: null,
      state: "waiting",
      nextProbeInSeconds: 4,
      probeInFlight: false,
    }],
  }));
  await pendingLoad;

  const pendingClear = store.clearRestriction("wait-1").then(() => "ok", (error: unknown) => error);
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "POST");
  assert.match(calls[1]!.url, /\/restrictions\/wait-1\/clear$/);
  assert.equal(calls[1]!.body?.expectedRevision, 3);
  calls[1]!.resolve({
    code: "revisionConflict",
    message: "conflict",
    currentRevision: 6,
    processGeneration: 99,
  }, 409);
  await waitForCalls(calls, 3);
  assert.match(calls[2]!.url, /\/contract$/);
  calls[2]!.resolve({ revision: 6, processGeneration: 99, pricingRevision: "p" });
  await waitForCalls(calls, 4);
  assert.equal(calls[3]!.method, "GET");
  assert.match(calls[3]!.url, /\/temporary-unavailability\/restrictions$/);
  calls[3]!.resolve(restrictionsBody({
    revision: { revision: 6, processGeneration: 99, pricingRevision: "p" },
    restrictions: [],
  }));
  const result = await pendingClear;
  assert.notEqual(result, "ok");
  assert.equal(calls.filter((call) => call.method === "POST").length, 1);
  assert.equal(store.restrictions?.restrictions.length, 0);
  assert.equal(store.restrictionsError, "conflict");
});

test("a failed first restrictions GET does not invent an empty snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const pending = store.loadRestrictions();
  await waitForCalls(calls, 1);
  calls[0]!.resolve({ message: "not found" }, 404);
  await pending;
  assert.equal(store.restrictions, null);
  assert.equal(store.restrictionsLoaded, false);
  assert.equal(store.restrictionsError, "load_failed");
});

test("a failed restrictions revalidation keeps the last successful snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useTemporaryPolicyStore();
  const calls = installDeferredFetch();
  const first = store.loadRestrictions();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(restrictionsBody({
    restrictions: [{
      id: "wait-1",
      ruleId: sampleRule.id,
      ruleGeneration: 1,
      source: "global",
      credentialId: "cred-1",
      destinationId: "dest-1",
      scope: "credential_model",
      upstreamModel: "minimax-m3",
      state: "waiting",
      nextProbeInSeconds: 9,
      probeInFlight: false,
    }],
  }));
  await first;
  assert.equal(store.restrictionsLoaded, true);
  assert.equal(store.restrictions?.restrictions.length, 1);

  const second = store.loadRestrictions(true);
  await waitForCalls(calls, 2);
  calls[1]!.resolve({ message: "unavailable" }, 500);
  await second;
  assert.equal(store.restrictionsLoaded, true);
  assert.equal(store.restrictions?.restrictions.length, 1);
  assert.equal(store.restrictionsError, "load_failed");
});
