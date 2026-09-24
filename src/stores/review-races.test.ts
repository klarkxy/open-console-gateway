import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { toRaw } from "vue";
import { installWindowDashboard, v3AccountDto } from "../test-helpers/dashboard-v3-fetch.ts";
import { DashboardConflictError, type Account } from "../api/dashboard.ts";
import { useAccountsStore } from "./accounts.ts";
import { useConnectionStore } from "./connection.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useProvidersStore } from "./providers.ts";
import { useSessionStore } from "./session.ts";
import { useSettingsStore } from "./settings.ts";

/**
 * Regression tests for stale-load races: a slower, older request must never
 * clobber state committed by a newer request or mutation, and logout must
 * invalidate pending connection loads. Each test installs a deferred fetch
 * mock so response ordering is controlled explicitly.
 */

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

function freshPinia(): void {
  setActivePinia(createPinia());
  // Register the revision sink against this pinia so publishTokens during the
  // test cannot write into a store from an earlier test.
  useControlPlaneStore();
}

function connectionBody(primaryKey: string, revision: number): object {
  return {
    gatewayPort: 9042,
    clientRootUrl: "",
    primaryKey,
    subKeys: [],
    revision,
    processGeneration: 99,
  };
}

function accountsBody(ids: string[], revision: number): object {
  return {
    accounts: ids.map((id) => v3AccountDto(id)),
    revision,
    processGeneration: 99,
  };
}

function settingsBody(revision: number): object {
  return {
    revision,
    processGeneration: 99,
    gatewayPort: 9042,
    gatewayPortFromEnv: false,
    proxyMode: "auto",
    proxyUrl: "",
    proxyListDirection: "whitelist",
    proxyListModels: [],
    proxySupportedModels: [],
    opencodeInviteUrl: "",
    clientRootUrl: "",
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
  };
}

function contractsBody(revision: number, processGeneration = 99): object {
  return { revision, processGeneration, providers: [], customEndpoints: [] };
}

function definitionBody(name: string, revision: number): object {
  return {
    id: "dynamic-one",
    name,
    origin: "custom",
    offering: "api",
    editable: true,
    deletable: true,
    endpointUrl: "https://dynamic.example/v1",
    upstreamProtocol: "chat_completions",
    authKind: "bearer",
    models: [],
    presetId: null,
    createdAt: "2026-09-20T00:00:00Z",
    updatedAt: "2026-09-20T00:00:00Z",
    revision,
    processGeneration: 99,
  };
}

test("accounts store: an older load resolving last does not clobber newer state", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useAccountsStore();

  const first = store.loadPresented();
  const second = store.loadPresented();
  await waitForCalls(calls, 2);

  calls[1]!.resolve(accountsBody(["b1", "b2"], 8));
  const fresh = await second;
  assert.deepEqual(store.accounts.map(({ id }) => id), ["b1", "b2"]);
  assert.equal(store.loading, false, "the loading flag tracks the latest request");

  calls[0]!.resolve(accountsBody(["a1"], 7));
  const stale = await first;
  assert.deepEqual(stale.map(({ id }) => id), ["a1"], "stale caller still gets its own payload");
  assert.deepEqual(fresh.map(({ id }) => id), ["b1", "b2"]);
  assert.deepEqual(store.accounts.map(({ id }) => id), ["b1", "b2"]);
  assert.equal(store.loading, false);
  assert.equal(store.error, "");
});

test("accounts store: a stale load failure does not overwrite a fresh success", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useAccountsStore();

  const first = store.loadPresented();
  const second = store.loadPresented();
  await waitForCalls(calls, 2);

  calls[1]!.resolve(accountsBody(["b1"], 8));
  await second;
  calls[0]!.reject(new Error("stale failure"));
  await assert.rejects(first, /stale failure/);

  assert.equal(store.error, "");
  assert.deepEqual(store.accounts.map(({ id }) => id), ["b1"]);
  assert.equal(store.loading, false);
});

test("accounts store: a pending load cannot clobber an in-place mutation", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useAccountsStore();

  const pendingLoad = store.loadPresented();
  await waitForCalls(calls, 1);

  store.upsertAccount(v3AccountDto("m1") as unknown as Account);
  assert.deepEqual(store.accounts.map(({ id }) => id), ["m1"]);
  assert.equal(store.loading, false, "mutation releases the superseded load flag");

  calls[0]!.resolve(accountsBody(["a1", "a2"], 8));
  const stale = await pendingLoad;
  assert.deepEqual(stale.map(({ id }) => id), ["a1", "a2"], "stale caller still gets its own payload");
  assert.deepEqual(store.accounts.map(({ id }) => id), ["m1"], "stale load must not clobber the mutation");

  store.removeAccount("m1");
  assert.deepEqual(store.accounts, []);
  assert.equal(store.loaded, true);
});

test("connection store: a pending load cannot clobber post-mutation state", async () => {
  freshPinia();
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useConnectionStore();

  const pendingLoad = store.load();
  await waitForCalls(calls, 1);
  const mutation = store.updateKey("k1", { enabled: false });
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "PATCH");
  calls[1]!.resolve({ revision: 8, processGeneration: 99 });
  await waitForCalls(calls, 3);
  calls[2]!.resolve(connectionBody("new-primary", 9));
  await mutation;
  assert.equal(store.info?.primary_key, "new-primary");

  calls[0]!.resolve(connectionBody("old-primary", 7));
  const stale = await pendingLoad;
  assert.equal(stale.primary_key, "old-primary", "stale caller still gets its own payload");
  assert.equal(store.info?.primary_key, "new-primary");
  assert.equal(store.loading, false);
  assert.equal(store.error, "");
});

test("connection store: regeneratePrimaryKey commits the rotated key despite a pending old load", async () => {
  freshPinia();
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useConnectionStore();

  const pendingLoad = store.load();
  await waitForCalls(calls, 1);
  const rotation = store.regeneratePrimaryKey();
  await waitForCalls(calls, 2);
  assert.equal(calls[1]!.method, "POST");
  calls[1]!.resolve({ revision: 8, processGeneration: 99 });
  // The API helper reads the connection once for its return value…
  await waitForCalls(calls, 3);
  calls[2]!.resolve(connectionBody("new-primary", 8));
  // …and the store refreshes its cached resource through the guarded reload.
  await waitForCalls(calls, 4);
  calls[3]!.resolve(connectionBody("new-primary", 8));
  const rotated = await rotation;

  assert.equal(rotated, "new-primary");
  assert.equal(store.info?.primary_key, "new-primary");

  calls[0]!.resolve(connectionBody("old-primary", 7));
  const stale = await pendingLoad;
  assert.equal(stale.primary_key, "old-primary", "stale caller still gets its own payload");
  assert.equal(store.info?.primary_key, "new-primary", "pending old load must not overwrite the rotated key");
  assert.equal(store.loading, false);
  assert.equal(store.error, "");
});

test("connection store: clearSecrets invalidates a load that resolves after logout", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useConnectionStore();

  const pendingLoad = store.load();
  await waitForCalls(calls, 1);
  store.clearSecrets();
  calls[0]!.resolve(connectionBody("post-logout-primary", 8));
  await pendingLoad;

  assert.equal(store.info, null);
  assert.equal(store.loading, false);
  assert.equal(store.error, "");
});

test("connection store: a rejected mutation reload still releases the superseded load", async () => {
  freshPinia();
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useConnectionStore();

  const pendingLoad = store.load();
  await waitForCalls(calls, 1);
  const mutation = store.updateKey("k1", { enabled: false });
  await waitForCalls(calls, 2);
  calls[1]!.resolve({ revision: 8, processGeneration: 99 });
  await waitForCalls(calls, 3);
  calls[2]!.reject(new Error("reload failed"));
  await assert.rejects(mutation, /reload failed/);

  calls[0]!.resolve(connectionBody("old-primary", 7));
  await pendingLoad;

  assert.equal(store.loading, false, "rejected latest reload must release the flag");
  assert.equal(store.error, "reload failed");
  assert.equal(store.info, null, "stale load must not overwrite state after the failure");
});

test("settings store: a stale load failure does not overwrite a fresh success", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useSettingsStore();

  const first = store.loadPresented();
  const second = store.loadPresented();
  await waitForCalls(calls, 2);

  calls[1]!.resolve(settingsBody(8));
  await second;
  calls[0]!.reject(new Error("stale failure"));
  await assert.rejects(first, /stale failure/);

  assert.equal(store.error, "");
  assert.equal(store.settings?.revision, 8);
  assert.equal(store.loading, false);
});

test("providers store: a stale contracts load cannot clobber a mutation result", async () => {
  freshPinia();
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useProvidersStore();

  const pendingLoad = store.loadContracts();
  await waitForCalls(calls, 1);
  const mutation = store.putModelProtocolOverrides("provider", "opencode", []);
  await waitForCalls(calls, 2);
  calls[1]!.resolve(contractsBody(9));
  await mutation;
  assert.equal(store.contracts?.revision, 9);

  calls[0]!.resolve(contractsBody(8));
  await pendingLoad;
  assert.equal(store.contracts?.revision, 9, "stale load must not clobber the mutation result");
});

test("providers store: an older catalog load resolving last does not clobber newer state", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useProvidersStore();

  const first = store.loadCatalog();
  const second = store.loadCatalog();
  await waitForCalls(calls, 2);

  calls[1]!.resolve({ entries: [], revision: 8, processGeneration: 99 });
  const fresh = await second;
  assert.deepEqual(store.catalog, []);

  calls[0]!.reject(new Error("stale failure"));
  await assert.rejects(first, /stale failure/);
  assert.equal(toRaw(store.catalog), fresh, "stale resolution must not replace the fresh catalog");
});

test("providers store: clear() invalidates in-flight catalog, contracts, and connections", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useProvidersStore();

  const catalogLoad = store.loadCatalog();
  const contractsLoad = store.loadContracts();
  const connectionsLoad = store.loadConnections();
  await waitForCalls(calls, 3);
  store.clear();
  assert.equal(store.catalog, null);
  assert.equal(store.contracts, null);
  assert.equal(store.connections, null);
  assert.equal(store.loading, false);
  assert.equal(store.error, "");

  for (const call of calls) {
    if (call.url.includes("provider-contracts")) {
      call.resolve(contractsBody(8));
    } else if (call.url.endsWith("/providers")) {
      call.resolve({ entries: [], revision: 8, processGeneration: 99 });
    } else if (call.url.endsWith("/connections")) {
      call.resolve({
        connections: [],
        revision: { revision: 8, processGeneration: 99 },
      });
    } else {
      call.resolve({});
    }
  }
  await Promise.allSettled([catalogLoad, contractsLoad, connectionsLoad]);
  assert.equal(store.catalog, null);
  assert.equal(store.contracts, null);
  assert.equal(store.connections, null);
  assert.equal(store.loading, false);
  assert.equal(store.error, "");
});

test("providers store owns definition invalidation, refresh, and session guards", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const store = useProvidersStore();

  const first = store.loadDefinition("dynamic-one");
  await waitForCalls(calls, 1);
  calls[0]!.resolve(definitionBody("Before", 4));
  await first;
  assert.equal(store.definitions.get("dynamic-one")?.name, "Before");

  store.invalidateDefinition("dynamic-one");
  assert.equal(store.definitions.has("dynamic-one"), false);
  const refreshed = store.loadDefinition("dynamic-one", true);
  await waitForCalls(calls, 2);
  calls[1]!.resolve(definitionBody("After", 5));
  await refreshed;
  assert.equal(store.definitions.get("dynamic-one")?.name, "After");

  const stale = store.loadDefinition("dynamic-one", true);
  await waitForCalls(calls, 3);
  store.clear();
  calls[2]!.resolve(definitionBody("Stale", 6));
  await stale;
  assert.equal(store.definitions.size, 0);
});

test("providers store: a contract refresh resolving after clear returns to its caller without restoring cache", async () => {
  freshPinia();
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useProvidersStore();

  const pending = store.refreshContractCatalog("provider", "opencode");
  await waitForCalls(calls, 1);
  store.clear();

  calls[0]!.resolve(contractsBody(9));
  const result = await pending;
  assert.equal(result.revision, 9, "the original caller still receives the mutation result");
  assert.equal(store.contracts, null, "a response from the old session must not restore cache");
  assert.equal(store.loading, false);
  assert.equal(store.error, "");
});

test("providers store: a successful mutation wins over a load started after it", async () => {
  freshPinia();
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useProvidersStore();

  const mutation = store.refreshContractCatalog("provider", "opencode");
  await waitForCalls(calls, 1);
  const load = store.loadContracts();
  await waitForCalls(calls, 2);

  calls[1]!.resolve(contractsBody(7));
  await load;
  assert.equal(store.contracts?.revision, 7);

  calls[0]!.resolve(contractsBody(8));
  await mutation;
  assert.equal(store.contracts?.revision, 8, "the mutation receipt is authoritative");
  assert.equal(store.loading, false);
});

test("providers store: a failed mutation releases the load it invalidated", async () => {
  freshPinia();
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useProvidersStore();

  const staleLoad = store.loadContracts();
  await waitForCalls(calls, 1);
  const mutation = store.refreshContractCatalog("provider", "opencode");
  await waitForCalls(calls, 2);
  calls[1]!.reject(new Error("refresh failed"));
  await assert.rejects(mutation, /refresh failed/);

  calls[0]!.resolve(contractsBody(7));
  await staleLoad;
  assert.equal(store.contracts, null, "the invalidated load must not commit");
  assert.equal(store.loading, false, "no request owns the loading flag after failure");
});

test("providers store: a later failed mutation does not discard an earlier successful receipt", async () => {
  freshPinia();
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useProvidersStore();

  const initial = store.loadContracts();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(contractsBody(7));
  await initial;

  const first = store.refreshContractCatalog("provider", "opencode");
  const later = store.refreshContractCatalog("provider", "minimax");
  await waitForCalls(calls, 3);
  calls[2]!.reject(new Error("later mutation failed"));
  await assert.rejects(later, /later mutation failed/);
  calls[1]!.resolve(contractsBody(8));
  await first;

  assert.equal(store.contracts?.revision, 8, "a valid successful receipt must still commit");
  assert.equal(store.loading, false);
});

test("providers store: a new backend generation accepts its lower mutation revision", async () => {
  freshPinia();
  const control = useControlPlaneStore();
  control.sync({ revision: 900, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useProvidersStore();

  const initial = store.loadContracts();
  await waitForCalls(calls, 1);
  calls[0]!.resolve(contractsBody(900, 99));
  await initial;
  assert.equal(store.contracts?.revision, 900);
  assert.equal(store.contracts?.process_generation, 99);

  control.sync({ revision: 10, processGeneration: 100, pricingRevision: null });
  const mutation = store.refreshContractCatalog("provider", "opencode");
  await waitForCalls(calls, 2);
  calls[1]!.resolve(contractsBody(11, 100));
  await mutation;

  assert.equal(store.contracts?.revision, 11, "new-process revisions are not ordered against the old process");
  assert.equal(store.contracts?.process_generation, 100);
});

test("providers store: stale mutation conflicts after clear do not trigger a contracts reload", async () => {
  const mutations: Array<{
    name: string;
    run: (store: ReturnType<typeof useProvidersStore>) => Promise<unknown>;
  }> = [
    {
      name: "remove catalog models",
      run: (store) => store.removeContractCatalogModels("provider", "opencode", ["model-a"]),
    },
    {
      name: "put protocol overrides",
      run: (store) => store.putModelProtocolOverrides("provider", "opencode", []),
    },
  ];

  for (const mutation of mutations) {
    freshPinia();
    useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
    const calls = installDeferredFetch();
    const store = useProvidersStore();

    const pending = mutation.run(store);
    await waitForCalls(calls, 1);
    store.clear();
    calls[0]!.reject(new DashboardConflictError("stale conflict", 8, 99));
    // The control-plane helper refreshes `/contract`, then the API helper
    // observes provider contracts before rethrowing. The stale store
    // generation must not add a third recovery GET of its own.
    await waitForCalls(calls, 2);
    calls[1]!.resolve({ revision: 8, processGeneration: 99, pricingRevision: "p2" });
    await waitForCalls(calls, 3);
    calls[2]!.resolve(contractsBody(8));
    await assert.rejects(pending, DashboardConflictError, mutation.name);
    await new Promise((resolve) => setImmediate(resolve));

    assert.equal(calls.length, 3, `${mutation.name}: stale store must not add another reload`);
    assert.equal(store.contracts, null, `${mutation.name}: cache stays cleared`);
  }
});

test("dropSession clears providers and a deferred catalog fetch cannot write back", async () => {
  freshPinia();
  const calls = installDeferredFetch();
  const providers = useProvidersStore();
  const catalogLoad = providers.loadCatalog();
  const contractsLoad = providers.loadContracts();
  await waitForCalls(calls, 2);

  useSessionStore().dropSession();
  assert.equal(providers.catalog, null);
  assert.equal(providers.contracts, null);
  assert.equal(providers.connections, null);
  assert.equal(providers.loading, false);
  assert.equal(providers.error, "");

  for (const call of calls) {
    if (call.url.includes("provider-contracts")) {
      call.resolve(contractsBody(9));
    } else {
      call.resolve({ entries: [], revision: 9, processGeneration: 99 });
    }
  }
  await Promise.allSettled([catalogLoad, contractsLoad]);
  assert.equal(providers.catalog, null);
  assert.equal(providers.contracts, null);
  assert.equal(providers.error, "");
});

test("control plane sync never regresses the revision within one process generation", () => {
  freshPinia();
  const control = useControlPlaneStore();

  control.sync({ revision: 5, processGeneration: 99, pricingRevision: "p1" });
  control.sync({ revision: 3, processGeneration: 99, pricingRevision: "p2" });
  assert.equal(control.revision, 5, "delayed older GET must be ignored");
  assert.equal(control.pricingRevision, "p1");

  control.sync({ revision: 6, processGeneration: 99, pricingRevision: null });
  assert.equal(control.revision, 6);
  assert.equal(control.pricingRevision, "p1", "absent pricing revision keeps the previous value");

  control.sync({ revision: 1, processGeneration: 100, pricingRevision: "p3" });
  assert.equal(control.revision, 1, "a new generation is adopted without ordering assumptions");
  assert.equal(control.processGeneration, 100);
  assert.equal(control.pricingRevision, "p3");
});
