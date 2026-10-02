import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { watch } from "vue";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useSessionStore } from "./session.ts";
import { useSettingsStore } from "./settings.ts";

interface DeferredCall {
  url: string;
  method: string;
  body: Record<string, unknown> | null;
  resolve: (body: object) => void;
  respond: (status: number, body: object) => void;
  reject: (error: unknown) => void;
}

let openCalls: DeferredCall[] = [];

function installDeferredFetch(): DeferredCall[] {
  installWindowDashboard();
  const calls: DeferredCall[] = [];
  openCalls = calls;
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: (input: string, init: RequestInit = {}) => {
      const promise = new Promise<Response>((resolvePromise, rejectPromise) => {
        const respond = (status: number, body: object) => resolvePromise(new Response(
          JSON.stringify(body),
          {
            status,
            statusText: status === 200 ? "OK" : status === 409 ? "Conflict" : "Error",
            headers: { "Content-Type": "application/json" },
          },
        ));
        calls.push({
          url: String(input),
          method: init.method ?? "GET",
          body: init.body ? JSON.parse(String(init.body)) as Record<string, unknown> : null,
          resolve: (body) => respond(200, body),
          respond,
          reject: (error) => rejectPromise(error),
        });
      });
      void promise.catch(() => undefined);
      return promise;
    },
  });
  return calls;
}

afterEach(() => {
  for (const call of openCalls) call.reject(new Error("unsettled fetch"));
  openCalls = [];
});

async function waitForCalls(calls: DeferredCall[], count: number): Promise<void> {
  for (let i = 0; i < 200 && calls.length < count; i++) {
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.equal(calls.length, count, `expected ${count} fetch calls, saw ${calls.length}`);
}

async function waitForMethod(calls: DeferredCall[], method: string, fromIndex = 0): Promise<DeferredCall> {
  for (let i = 0; i < 200; i++) {
    const found = calls.slice(fromIndex).find((call) => call.method === method);
    if (found) return found;
    await new Promise((resolve) => setImmediate(resolve));
  }
  assert.fail(`expected a ${method} request from ${fromIndex}`);
}

async function flush(ticks = 24): Promise<void> {
  for (let i = 0; i < ticks; i++) {
    await new Promise((resolve) => setImmediate(resolve));
  }
}

function trackPromise<T>(promise: Promise<T>): { status: () => "pending" | "fulfilled" | "rejected" } {
  let status: "pending" | "fulfilled" | "rejected" = "pending";
  void promise.then(
    () => { status = "fulfilled"; },
    () => { status = "rejected"; },
  );
  return { status: () => status };
}

function freshPinia(): void {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
}

function settingsBody(revision: number, extra: Record<string, unknown> = {}): object {
  return {
    revision,
    processGeneration: 99,
    gatewayPort: 9042,
    gatewayPortFromEnv: false,
    proxyMode: "manual",
    proxyUrl: "http://last-good.example:8080",
    proxyListDirection: "whitelist",
    proxyListModels: ["canonical-model"],
    proxySupportedModels: [],
    opencodeInviteUrl: "https://example.test/invite",
    clientRootUrl: "http://canonical.example",
    clientRootUrlFromEnv: false,
    autoStart: false,
    autoStartSupported: true,
    showDockIcon: true,
    dockVisibilitySupported: false,
    connectTimeoutSecs: 30,
    nonStreamTimeoutSecs: 900,
    streamIdleTimeoutSecs: 300,
    routingMode: "strict-priority",
    conversationSticky: true,
    ...extra,
  };
}

function draftFromStore(store: ReturnType<typeof useSettingsStore>) {
  const current = store.settings;
  assert.ok(current);
  return {
    ...current,
    proxy_url: "http://draft-proxy.example:8080",
    proxy_list_models: ["user-draft-model"],
    client_root_url: "http://draft.example/v1",
  };
}

function settingsGets(calls: DeferredCall[]): DeferredCall[] {
  return calls.filter((call) => call.method === "GET" && call.url.includes("/settings"));
}

async function seedSettings(
  store: ReturnType<typeof useSettingsStore>,
  calls: DeferredCall[],
): Promise<void> {
  const pending = store.loadPresented();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(settingsBody(7));
  await pending;
}

test("settings PUT ack completes save without a duplicate GET", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  const started = calls.length;
  const pending = store.putPresented(draftFromStore(store));
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "PUT", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();

  assert.equal(tracked.status(), "fulfilled", "save completes at the PUT ack");
  assert.ok(
    settingsGets(calls.slice(started)).length <= 1,
    "save must not depend on a second settings GET",
  );
});

test("a submitted settings draft is not a canonical fetched AppConfig", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  const started = calls.length;
  const draft = draftFromStore(store);
  const pending = store.putPresented(draft);
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "PUT", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();
  assert.equal(tracked.status(), "fulfilled");

  const followup = settingsGets(calls.slice(started)).at(-1);
  assert.ok(followup, "canonical fields come from a single revalidation GET");
  followup.resolve(settingsBody(8, {
    proxyUrl: "http://canonical-proxy.example:8080",
    proxyListModels: ["server-canonical-model"],
    clientRootUrl: "http://canonical.example",
  }));
  await flush();

  assert.equal(tracked.status(), "fulfilled");
  assert.equal(store.settings?.revision, 8);
  assert.equal(store.settings?.proxy_url, "http://canonical-proxy.example:8080");
  assert.deepEqual(store.settings?.proxy_list_models, ["server-canonical-model"]);
  assert.equal(store.settings?.client_root_url, "http://canonical.example");
  assert.notEqual(store.settings?.proxy_url, draft.proxy_url);
  assert.notDeepEqual(store.settings?.proxy_list_models, draft.proxy_list_models);
});

test("failed settings revalidation keeps last-good fields and the ack revision", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  const started = calls.length;
  const draft = draftFromStore(store);
  const pending = store.putPresented(draft);
  const tracked = trackPromise(pending);
  const write = await waitForMethod(calls, "PUT", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await flush();
  assert.equal(tracked.status(), "fulfilled");
  for (const call of settingsGets(calls.slice(started))) {
    call.reject(new Error("old port closed"));
  }
  await flush();

  assert.equal(tracked.status(), "fulfilled", "GET failure is not a save failure");
  assert.ok(store.settings);
  assert.equal(store.settings?.revision, 8);
  assert.equal(store.settings?.proxy_url, "http://last-good.example:8080");
  assert.deepEqual(store.settings?.proxy_list_models, ["canonical-model"]);
  assert.notEqual(store.settings?.proxy_url, draft.proxy_url);
  assert.notDeepEqual(store.settings?.proxy_list_models, draft.proxy_list_models);
});

test("dropSession clears cached settings", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  assert.equal(store.settings?.proxy_url, "http://last-good.example:8080");
  useSessionStore().dropSession();
  assert.equal(store.settings, null);
  assert.equal(store.loading, false);
});

test("a settings GET dispatched before reset cannot restore the resource", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  const pending = store.loadPresented();
  await waitForCalls(calls, 1);
  useSessionStore().dropSession();
  calls[0]!.resolve(settingsBody(9, { proxyUrl: "http://late.example:8080" }));
  await pending.then(() => undefined, () => undefined);
  assert.equal(store.settings, null);
});

test("a settings PUT dispatched before reset cannot restore the resource", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  const started = calls.length;
  const pending = store.putPresented(draftFromStore(store));
  const write = await waitForMethod(calls, "PUT", started);
  useSessionStore().dropSession();
  write.resolve({ revision: 8, processGeneration: 99 });
  await pending.then(() => undefined, () => undefined);
  await flush();
  assert.equal(store.settings, null);
  assert.equal(settingsGets(calls.slice(started)).length, 0, "reset must not start a settings GET");
});

test("a settings save loaded at revision 7 uses that captured pair after an unrelated CAS advance", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  useControlPlaneStore().sync({ revision: 9, processGeneration: 99 });
  const started = calls.length;
  const pending = store.putPresented(draftFromStore(store));
  const write = await waitForMethod(calls, "PUT", started);
  try {
    assert.equal(write.body?.expectedRevision, 7, "editor baseline revision is captured");
    assert.equal(write.body?.processGeneration, 99, "editor baseline process identity is captured");
  } finally {
    write.reject(new Error("conflict"));
    await pending.then(() => undefined, () => undefined);
  }
});

test("a settings save loaded at process 99 keeps that process identity after a new backend generation", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  useControlPlaneStore().sync({ revision: 2, processGeneration: 100 });
  const started = calls.length;
  const pending = store.putPresented(draftFromStore(store));
  const write = await waitForMethod(calls, "PUT", started);
  try {
    assert.equal(write.body?.expectedRevision, 7);
    assert.equal(write.body?.processGeneration, 99);
  } finally {
    write.reject(new Error("conflict"));
    await pending.then(() => undefined, () => undefined);
  }
});

test("a later full settings write cannot confirm last-good fields under a post-PUT CAS pair", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  const firstStarted = calls.length;
  const first = store.putPresented(draftFromStore(store));
  const firstWrite = await waitForMethod(calls, "PUT", firstStarted);
  firstWrite.resolve({ revision: 8, processGeneration: 99 });
  await first;
  await flush();
  for (const call of settingsGets(calls.slice(firstStarted))) {
    call.reject(new Error("canonical read stalled"));
  }
  await flush();
  assert.equal(store.settings?.revision, 8);
  assert.equal(store.settings?.proxy_url, "http://last-good.example:8080");

  const secondStarted = calls.length;
  const baseline = store.settings;
  assert.ok(baseline);
  const second = store.putPresented({ ...baseline, conversation_sticky: !baseline.conversation_sticky });
  const secondWrite = await waitForMethod(calls, "PUT", secondStarted);
  try {
    const confirmedLastGoodUnderNewCas = secondWrite.body?.expectedRevision === 8
      && secondWrite.body?.proxyUrl === "http://last-good.example:8080";
    assert.equal(
      confirmedLastGoodUnderNewCas,
      false,
      "a full write must not stamp pre-PUT last-good fields with the committed revision",
    );
  } finally {
    secondWrite.reject(new Error("stop"));
    await second.then(() => undefined, () => undefined);
  }
});

test("canonical confirmation is readonly and changes only when a detached GET commits", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  const observed: boolean[] = [];
  const stop = watch(() => store.canonicalConfirmed, (value) => {
    observed.push(value);
  }, { flush: "sync" });
  try {
    assert.equal(store.canonicalConfirmed, false);
    await seedSettings(store, calls);
    const started = calls.length;
    const pending = store.putPresented(draftFromStore(store));
    const write = await waitForMethod(calls, "PUT", started);
    write.resolve({ revision: 8, processGeneration: 99 });
    await pending;
    await flush();
    const followup = settingsGets(calls.slice(started)).at(-1);
    assert.ok(followup);
    assert.equal(store.canonicalConfirmed, false);
    followup.resolve(settingsBody(8, { processGeneration: 100, streamIdleTimeoutSecs: 120 }));
    await flush();

    const changes = observed.filter((value, index) => index === 0 || value !== observed[index - 1]);
    const before = store.canonicalConfirmed;
    let held = true;
    try {
      (store as { canonicalConfirmed: boolean }).canonicalConfirmed = false;
      held = store.canonicalConfirmed === before;
    } catch {
      held = store.canonicalConfirmed === before;
    }
    assert.deepEqual({
      confirmed: store.canonicalConfirmed,
      process: store.settings?.process_generation,
      stream: store.settings?.stream_idle_timeout_secs,
      changes,
      held,
    }, {
      confirmed: true,
      process: 100,
      stream: 120,
      changes: [true, false, true],
      held: true,
    });
  } finally {
    stop();
  }
});

test("a successful canonical GET is the pair and the fields used by the next full save", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  const firstStarted = calls.length;
  const first = store.putPresented(draftFromStore(store));
  const firstWrite = await waitForMethod(calls, "PUT", firstStarted);
  firstWrite.resolve({ revision: 8, processGeneration: 99 });
  await first;
  const followup = settingsGets(calls.slice(firstStarted)).at(-1);
  assert.ok(followup);
  followup.resolve(settingsBody(8, {
    processGeneration: 100,
    proxyUrl: "http://canonical-proxy.example:8080",
    streamIdleTimeoutSecs: 120,
    clientRootUrl: "http://normalized.example",
  }));
  await flush();
  assert.equal(settingsGets(calls.slice(firstStarted)).length, 1);

  const secondStarted = calls.length;
  const canonical = store.settings;
  assert.ok(canonical);
  const second = store.putPresented({ ...canonical, conversation_sticky: !canonical.conversation_sticky });
  const secondWrite = await waitForMethod(calls, "PUT", secondStarted);
  try {
    assert.equal(settingsGets(calls.slice(secondStarted)).length, 0);
    assert.deepEqual({
      expectedRevision: secondWrite.body?.expectedRevision,
      processGeneration: secondWrite.body?.processGeneration,
      proxyUrl: secondWrite.body?.proxyUrl,
      streamIdleTimeoutSecs: secondWrite.body?.streamIdleTimeoutSecs,
      clientRootUrl: secondWrite.body?.clientRootUrl,
    }, {
      expectedRevision: 8,
      processGeneration: 100,
      proxyUrl: "http://canonical-proxy.example:8080",
      streamIdleTimeoutSecs: 120,
      clientRootUrl: "http://normalized.example",
    });
  } finally {
    secondWrite.resolve({ revision: 9, processGeneration: 100 });
    await second;
  }
  const secondRead = settingsGets(calls.slice(secondStarted));
  assert.equal(secondRead.length, 1);
  secondRead[0]?.resolve(settingsBody(9, { processGeneration: 100 }));
  await flush();
});

test("a failed canonical read keeps confirmation unset and does not replay the write", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  const started = calls.length;
  const pending = store.putPresented(draftFromStore(store));
  const write = await waitForMethod(calls, "PUT", started);
  write.resolve({ revision: 8, processGeneration: 99 });
  await pending;
  await flush();
  for (const call of settingsGets(calls.slice(started))) call.reject(new Error("canonical read failed"));
  await flush();
  assert.deepEqual({
    confirmed: store.canonicalConfirmed,
    refreshError: store.refreshError,
    puts: calls.slice(started).filter((call) => call.method === "PUT").length,
    gets: settingsGets(calls.slice(started)).length,
    revision: store.settings?.revision,
    proxy: store.settings?.proxy_url,
  }, {
    confirmed: false,
    refreshError: "canonical read failed",
    puts: 1,
    gets: 1,
    revision: 8,
    proxy: "http://last-good.example:8080",
  });
});

test("a settings conflict recovery reloads the canonical resource once", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();
  await seedSettings(store, calls);
  const started = calls.length;
  const pending = store.putPresented(draftFromStore(store));
  const write = await waitForMethod(calls, "PUT", started);
  write.respond(409, {
    code: "revisionConflict",
    message: "settings changed since they were loaded; reload and try again",
    currentRevision: 11,
    processGeneration: 99,
  });
  const recovery = await waitForMethod(calls, "GET", started);
  await flush();
  assert.equal(settingsGets(calls.slice(started)).length, 1);
  recovery.resolve(settingsBody(11, { streamIdleTimeoutSecs: 120 }));
  await pending.then(() => undefined, () => undefined);
  await flush();
  assert.deepEqual({
    gets: settingsGets(calls.slice(started)).length,
    confirmed: store.canonicalConfirmed,
    revision: store.settings?.revision,
    stream: store.settings?.stream_idle_timeout_secs,
  }, {
    gets: 1,
    confirmed: true,
    revision: 11,
    stream: 120,
  });
});
