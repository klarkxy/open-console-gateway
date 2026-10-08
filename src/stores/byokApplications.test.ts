import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { computed } from "vue";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useByokApplicationsStore } from "./byokApplications.ts";
import { useSessionStore } from "./session.ts";
import type { ByokApplicationView } from "../api/byok-applications.ts";

interface DeferredCall {
  url: string;
  method: string;
  body: Record<string, unknown> | null;
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
        body: init.body ? JSON.parse(String(init.body)) as Record<string, unknown> : null,
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

function viewBody(overrides: Record<string, unknown> = {}): ByokApplicationView {
  return {
    client: "codex",
    adopted: false,
    copilotTokenBudget: null,
    status: "ready",
    detected: true,
    configPath: "C:\\Users\\author\\.codex\\config.toml",
    discoverySource: "default",
    targetPaths: ["C:\\Users\\author\\.codex\\config.toml"],
    configureSupported: true,
    removeSupported: false,
    recoverySupported: false,
    requiresClosedClient: true,
    activationRequired: false,
    fingerprint: "fp-1",
    configuredModelIds: [],
    defaultModelId: null,
    backupPath: null,
    detail: null,
    gatewayV1Url: "http://127.0.0.1:8317/v1",
    revision: { revision: 1, processGeneration: 1, pricingRevision: "p" },
    ...overrides,
  } as ByokApplicationView;
}

const EXPECTATION = { expectedRevision: 1, processGeneration: 1 };

test("BYOK store: a stale inspect cannot overwrite a later mutation or a cleared session", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const inspectCalls = installDeferredFetch();
  const pendingInspect = store.inspect("codex");
  await waitForCalls(inspectCalls, 1);

  const mutation = installDeferredFetch();
  const pendingConfigure = store.configure("codex", {
    targetPath: null,
    expectedFingerprint: "fp-1",
    clientClosed: true,
  }, EXPECTATION);
  await waitForCalls(mutation, 1);
  mutation[0]!.resolve(viewBody({ fingerprint: "fp-mutation" }));
  await pendingConfigure;
  assert.equal(store.peek("codex")?.view?.fingerprint, "fp-mutation");

  inspectCalls[0]!.resolve(viewBody({ fingerprint: "fp-stale" }));
  await pendingInspect;
  assert.equal(store.peek("codex")?.view?.fingerprint, "fp-mutation");

  const later = installDeferredFetch();
  const pendingLater = store.inspect("codex");
  await waitForCalls(later, 1);
  store.clear();
  later[0]!.resolve(viewBody({ fingerprint: "fp-3" }));
  await pendingLater;
  assert.equal(store.peek("codex"), null);
});

test("BYOK store: keys for different clients are independent", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const calls = installDeferredFetch();
  const codexLoad = store.inspect("codex");
  const kimiLoad = store.inspect("kimi");
  await waitForCalls(calls, 2);
  assert.ok(calls[0]!.url.includes("/applications/byok/codex"));
  assert.ok(calls[1]!.url.includes("/applications/byok/kimi"));
  calls[1]!.resolve(viewBody({ client: "kimi", configPath: "C:\\Users\\author\\.kimi-code\\config.toml" }));
  await kimiLoad;
  assert.equal(store.peek("kimi")?.loaded, true);
  assert.equal(store.peek("codex")?.loaded, false);
  calls[0]!.resolve(viewBody());
  await codexLoad;
  assert.equal(store.peek("codex")?.loaded, true);
});

test("BYOK store: inspect passes the exact target path as a query parameter", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const calls = installDeferredFetch();
  const pending = store.inspect("zcode", "D:\\zcode profiles\\a\\provider_config.json");
  await waitForCalls(calls, 1);
  assert.match(calls[0]!.url, /\/applications\/byok\/zcode\?targetPath=/);
  assert.ok(calls[0]!.url.includes(encodeURIComponent("D:\\zcode profiles\\a\\provider_config.json")));
  calls[0]!.resolve(viewBody({ client: "zcode" }));
  await pending;
});

test("BYOK store: a stale mutation cannot write back after a later load", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const mutation = installDeferredFetch();
  const pendingConfigure = store.configure("codex", {
    targetPath: null,
    expectedFingerprint: "fp-1",
    clientClosed: true,
  }, EXPECTATION);
  await waitForCalls(mutation, 1);
  assert.equal(mutation[0]!.method, "POST");
  assert.equal(Object.prototype.hasOwnProperty.call(mutation[0]!.body ?? {}, "keyId"), false);
  assert.equal(Object.prototype.hasOwnProperty.call(mutation[0]!.body ?? {}, "models"), false);
  assert.equal(Object.prototype.hasOwnProperty.call(mutation[0]!.body ?? {}, "defaultModelId"), false);
  assert.equal(mutation[0]!.body?.clientClosed, true);
  assert.equal(mutation[0]!.body?.expectedFingerprint, "fp-1");

  const load = installDeferredFetch();
  const pendingLoad = store.inspect("codex");
  await waitForCalls(load, 1);
  load[0]!.resolve(viewBody({ fingerprint: "fp-2" }));
  await pendingLoad;

  mutation[0]!.resolve(viewBody({ configuredModelIds: ["ocg/model-a"], fingerprint: "fp-1" }));
  await pendingConfigure;
  assert.equal(store.peek("codex")?.view?.fingerprint, "fp-2");
  assert.deepEqual(store.peek("codex")?.view?.configuredModelIds, []);
  assert.equal(store.peek("codex")?.mutating, false);
  assert.equal(store.peek("codex")?.loading, false);
});

test("dropSession clears BYOK snapshots so a later response cannot write back", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const calls = installDeferredFetch();
  const pending = store.inspect("codex");
  await waitForCalls(calls, 1);
  useSessionStore().dropSession();
  assert.equal(store.peek("codex"), null);
  calls[0]!.resolve(viewBody());
  await pending;
  assert.equal(store.peek("codex"), null);
});

test("dropSession during a deferred configure clears busy flags and ignores the receipt", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const mutation = installDeferredFetch();
  const pending = store.configure("kimi", {
    targetPath: null,
    expectedFingerprint: "fp-1",
    clientClosed: true,
  }, EXPECTATION);
  await waitForCalls(mutation, 1);
  useSessionStore().dropSession();
  mutation[0]!.resolve(viewBody({ client: "kimi", configuredModelIds: ["ocg/model-a"] }));
  await pending;
  assert.equal(store.peek("kimi"), null);
});

test("BYOK store: remove and recover use their dedicated methods and paths", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const calls = installDeferredFetch();
  const input = { targetPath: null, expectedFingerprint: "fp-1", clientClosed: true };
  const pendingRemove = store.remove("codex", input, EXPECTATION);
  await waitForCalls(calls, 1);
  assert.equal(calls[0]!.method, "DELETE");
  assert.ok(calls[0]!.url.endsWith("/applications/byok/codex"));
  calls[0]!.resolve(viewBody({ configuredModelIds: [], fingerprint: "fp-2" }));
  await pendingRemove;
  assert.equal(store.peek("codex")?.view?.fingerprint, "fp-2");

  const pendingRecover = store.recover("codex", { ...input, expectedFingerprint: "fp-2" }, EXPECTATION);
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "POST");
  assert.ok(calls[1]!.url.endsWith("/applications/byok/codex/recover"));
  calls[1]!.resolve(viewBody({ fingerprint: "fp-3" }));
  await pendingRecover;
  assert.equal(store.peek("codex")?.view?.fingerprint, "fp-3");
});

test("BYOK store: mutation failure surfaces an error on the entry and rethrows", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const calls = installDeferredFetch();
  const pending = store.remove("codex", { targetPath: null, expectedFingerprint: "fp-1", clientClosed: true }, EXPECTATION);
  await waitForCalls(calls, 1);
  calls[0]!.reject(new Error("boom"));
  await assert.rejects(pending, /boom/);
  assert.equal(store.peek("codex")?.error, "boom");
  assert.equal(store.peek("codex")?.mutating, false);
});

test("BYOK store: a computed consumer observes the first load through the reactive Map", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const observed = computed(() => store.peek("codex")?.view ?? null);
  const busy = computed(() => store.peek("codex")?.loading ?? false);
  // Boolean comparisons: assert.equal's `asserts actual is expected` would
  // narrow the property chain to null and poison later reads.
  assert.equal(observed.value === null, true);

  const calls = installDeferredFetch();
  const pending = store.inspect("codex");
  await waitForCalls(calls, 1);
  assert.equal(busy.value, true);
  assert.equal(observed.value === null, true);

  calls[0]!.resolve(viewBody());
  await pending;
  assert.equal(observed.value?.fingerprint, "fp-1");
  assert.equal(busy.value, false);
});

test("BYOK store: same-target overlapping inspects share one request", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const calls = installDeferredFetch();
  const first = store.inspect("codex");
  const second = store.inspect("codex");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(viewBody({ fingerprint: "fp-shared" }));
  await Promise.all([first, second]);
  assert.equal(store.peek("codex")?.loading, false);
  assert.equal(store.peek("codex")?.view?.fingerprint, "fp-shared");
  assert.equal(calls.length, 1);
});

test("BYOK store: overlapping inspects of different targets stay independent", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const calls = installDeferredFetch();
  const defaultLoad = store.inspect("codex");
  const customLoad = store.inspect("codex", "D:\\profiles\\a\\config.toml");
  await waitForCalls(calls, 2);
  calls[1]!.resolve(viewBody({ fingerprint: "fp-custom", configPath: "D:\\profiles\\a\\config.toml" }));
  await customLoad;
  assert.equal(store.peek("codex", "D:\\profiles\\a\\config.toml")?.view?.fingerprint, "fp-custom");
  assert.equal(store.peek("codex")?.view, null);
  calls[0]!.resolve(viewBody({ fingerprint: "fp-default" }));
  await defaultLoad;
  assert.equal(store.peek("codex")?.view?.fingerprint, "fp-default");
  assert.equal(store.peek("codex", "D:\\profiles\\a\\config.toml")?.view?.fingerprint, "fp-custom");
});

test("BYOK store: a load started during a mutation still clears both busy flags", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const mutation = installDeferredFetch();
  const pendingConfigure = store.configure("codex", {
    targetPath: null,
    expectedFingerprint: "fp-1",
    clientClosed: true,
  }, EXPECTATION);
  await waitForCalls(mutation, 1);
  assert.equal(store.peek("codex")?.mutating, true);

  const load = installDeferredFetch();
  const pendingLoad = store.inspect("codex");
  await waitForCalls(load, 1);
  assert.equal(store.peek("codex")?.loading, true);
  assert.equal(store.peek("codex")?.mutating, true);

  mutation[0]!.resolve(viewBody({ configuredModelIds: ["ocg/model-a"] }));
  await pendingConfigure;
  assert.equal(store.peek("codex")?.mutating, false);
  assert.equal(store.peek("codex")?.loading, true);

  load[0]!.resolve(viewBody({ configuredModelIds: ["ocg/model-a"], fingerprint: "fp-2" }));
  await pendingLoad;
  assert.equal(store.peek("codex")?.loading, false);
  assert.equal(store.peek("codex")?.view?.fingerprint, "fp-2");
});


test("BYOK preview is read-only, keeps content visible, and latest budget response wins", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = useByokApplicationsStore();
  const calls = installDeferredFetch();
  const load = store.inspect("copilot");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(viewBody({ client: "copilot", fingerprint: "saved" }));
  await load;
  const first = store.preview("copilot", { targetPath: null });
  const latest = store.preview("copilot", { copilotTokenBudget: { maxInputTokens: 6000, maxOutputTokens: 2000 } });
  await waitForCalls(calls, 3);
  assert.ok(calls[1]!.url.endsWith("/copilot/preview"));
  assert.equal(calls[1]!.method, "POST");
  assert.deepEqual(calls[1]!.body, { targetPath: null });
  assert.equal(store.peek("copilot")?.view?.fingerprint, "saved");
  assert.equal(store.peek("copilot")?.loading, true);
  calls[2]!.resolve(viewBody({ fingerprint: "new-budget", preview: { planFingerprint: "new-plan" } }));
  assert.equal(await latest, true);
  calls[1]!.resolve(viewBody({ fingerprint: "old-budget", preview: { planFingerprint: "old-plan" } }));
  assert.equal(await first, false);
  assert.equal(store.peek("copilot")?.view?.preview?.planFingerprint, "new-plan");
  assert.equal(store.peek("copilot")?.loading, false);
});

test("discard or session teardown prevents a pending preview from committing", async () => {
  for (const cancel of ["discard", "logout"] as const) {
    setActivePinia(createPinia()); useControlPlaneStore();
    const store = useByokApplicationsStore();
    const calls = installDeferredFetch();
    const pending = store.preview("codex", { targetPath: null });
    await waitForCalls(calls, 1);
    if (cancel === "discard") store.discardPreview("codex");
    else useSessionStore().dropSession();
    calls[0]!.resolve(viewBody({ preview: { planFingerprint: "cancelled" } }));
    assert.equal(await pending, false);
    assert.equal(store.peek("codex")?.view?.preview, undefined);
  }
});
