import assert from "node:assert/strict";
import test from "node:test";
import { providerApi, type ProviderDefinitionView, type ProviderContractsResponse } from "../api/providers.ts";
import { connectionsApi } from "../api/connections.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { dashboardV4 } from "../api/dashboard-v4.ts";
import { useProvidersStore } from "./providers.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function definition(id: string, revision = 7): ProviderDefinitionView {
  return {
    id, name: id, origin: "custom", offering: "api", editable: true, deletable: true,
    endpoint_url: "https://example.com/v1", upstream_protocol: "chat_completions", auth_kind: "bearer",
    models: [], preset_id: null, created_at: "", updated_at: "", revision, process_generation: 99,
  };
}

test("provider resources singleflight independently and reuse only successful explicit freshness", async t => {
  setupControlPlane();
  const store = useProvidersStore();
  const catalogGate = deferred<Awaited<ReturnType<typeof providerApi.getProviderCatalogSnapshot>>>();
  const connectionGate = deferred<Awaited<ReturnType<typeof connectionsApi.listSnapshot>>>();
  const contractGate = deferred<ProviderContractsResponse>();
  const catalog = t.mock.method(providerApi, "getProviderCatalogSnapshot", () => catalogGate.promise);
  const connections = t.mock.method(connectionsApi, "listSnapshot", () => connectionGate.promise);
  const contracts = t.mock.method(providerApi, "getProviderContracts", () => contractGate.promise);
  const first = [store.loadCatalog(), store.loadConnections(), store.loadContracts()];
  const second = [store.loadCatalog(), store.loadConnections(), store.loadContracts()];
  assert.deepEqual([catalog.mock.callCount(), connections.mock.callCount(), contracts.mock.callCount()], [1, 1, 1]);
  catalogGate.resolve({ catalog: [], expectation: { expectedRevision: 7, processGeneration: 99 } });
  connectionGate.resolve({ connections: [], expectation: { expectedRevision: 7, processGeneration: 99 } });
  contractGate.resolve({ providers: [], custom_endpoints: [], revision: 7, process_generation: 99 });
  await Promise.all([...first, ...second]);
  await Promise.all([store.loadCatalog({ maxAgeMs: 15_000 }), store.loadConnections({ maxAgeMs: 15_000 }), store.loadContracts({ maxAgeMs: 15_000 })]);
  assert.deepEqual([catalog.mock.callCount(), connections.mock.callCount(), contracts.mock.callCount()], [1, 1, 1]);
  await Promise.all([store.loadCatalog(), store.loadConnections(), store.loadContracts()]);
  assert.deepEqual([catalog.mock.callCount(), connections.mock.callCount(), contracts.mock.callCount()], [2, 2, 2]);
  store.clear();
});

test("definition requests are per provider and force bypasses completed cache without joining an invalidated flight", async t => {
  setupControlPlane();
  const gates = [deferred<ProviderDefinitionView>(), deferred<ProviderDefinitionView>(), deferred<ProviderDefinitionView>()];
  let count = 0;
  t.mock.method(providerApi, "getProviderDefinition", () => gates[count++]!.promise);
  const store = useProvidersStore();
  const oldA = store.loadDefinition("a");
  const aJoined = store.loadDefinition("a", true);
  const b = store.loadDefinition("b");
  assert.equal(count, 2);
  store.invalidateDefinition("a");
  const newA = store.loadDefinition("a", true);
  assert.equal(count, 3);
  gates[2]!.resolve(definition("a", 8));
  await newA;
  gates[1]!.resolve(definition("b"));
  await b;
  gates[0]!.resolve(definition("a"));
  await Promise.all([oldA, aJoined]);
  assert.equal(store.definitions.get("a")?.revision, 8);
  assert.equal(store.definitions.get("b")?.id, "b");
  await store.loadDefinition("a");
  assert.equal(count, 3);
  store.clear();
});

test("definition failures and backend identity changes cannot reuse previous freshness", async t => {
  setupControlPlane();
  let fail = false;
  const read = t.mock.method(providerApi, "getProviderDefinition", async () => {
    if (fail) throw new Error("offline");
    return { ...definition("a"), process_generation: useControlPlaneStore().processGeneration! };
  });
  const store = useProvidersStore();
  await store.loadDefinition("a");
  fail = true;
  await assert.rejects(store.loadDefinition("a", true));
  fail = false;
  await store.loadDefinition("a");
  assert.equal(read.mock.callCount(), 3);
  useControlPlaneStore().sync({ revision: 1, processGeneration: 100 });
  await store.loadDefinition("a");
  assert.equal(read.mock.callCount(), 4);
  assert.equal(store.definitions.get("a")?.process_generation, 100);
  store.clear();
});

test("complete provider read receipts detach older resource reads and can be reused immediately", async t => {
  setupControlPlane();
  const gate = deferred<ProviderContractsResponse>();
  const read = t.mock.method(providerApi, "getProviderContracts", () => gate.promise);
  const store = useProvidersStore();
  const old = store.loadContracts();
  store.commitReadProjection({ contracts: { providers: [], custom_endpoints: [], revision: 9, process_generation: 99 }, definitions: [definition("a")] });
  gate.resolve({ providers: [], custom_endpoints: [], revision: 7, process_generation: 99 });
  await old;
  await store.loadContracts({ maxAgeMs: 15_000 });
  assert.equal(read.mock.callCount(), 1);
  assert.equal(store.contracts?.revision, 9);
  assert.equal(store.definitions.get("a")?.id, "a");
  store.clear();
});

const restartReads = [
  {
    name: "catalog",
    load: (store: ReturnType<typeof useProvidersStore>, maxAgeMs = 0) => store.loadCatalog({ maxAgeMs }),
    body: (processGeneration: number, revision = 7) => ({ entries: [], revision, processGeneration, pricingRevision: "p1" }),
    committed: (store: ReturnType<typeof useProvidersStore>) => store.catalog !== null,
  },
  {
    name: "connections",
    load: (store: ReturnType<typeof useProvidersStore>, maxAgeMs = 0) => store.loadConnections({ maxAgeMs }),
    body: (processGeneration: number, revision = 7) => ({ connections: [], revision: { revision, processGeneration } }),
    committed: (store: ReturnType<typeof useProvidersStore>) => store.connections !== null,
  },
  {
    name: "alias publication",
    load: (store: ReturnType<typeof useProvidersStore>, maxAgeMs = 0) => store.loadAliasPublication({ maxAgeMs }),
    body: (processGeneration: number, revision = 7) => ({ unpublished: ["hidden-model"], revision: { revision, processGeneration } }),
    committed: (store: ReturnType<typeof useProvidersStore>) => store.aliasPublicationReady,
  },
  {
    name: "CPA catalog",
    load: (store: ReturnType<typeof useProvidersStore>, maxAgeMs = 0) => store.loadCpaModels({ maxAgeMs }),
    body: (processGeneration: number, revision = 7) => ({ models: [{ id: "model", enabled: true, ownedBy: null }], revision: { revision, processGeneration } }),
    committed: (store: ReturnType<typeof useProvidersStore>) => store.cpaModels !== null,
  },
  {
    name: "contracts",
    load: (store: ReturnType<typeof useProvidersStore>, maxAgeMs = 0) => store.loadContracts({ maxAgeMs }),
    body: (processGeneration: number, revision = 7) => ({ providers: [], customEndpoints: [], revision, processGeneration, pricingRevision: "p1" }),
    committed: (store: ReturnType<typeof useProvidersStore>) => store.contracts !== null,
  },
];

test("provider full reads commit their own valid backend restart discovery payload", async t => {
  for (const resource of restartReads) {
    await t.test(resource.name, async () => {
      setupControlPlane();
      const requests = installFetchMock(() => resource.body(100));
      const store = useProvidersStore();
      await resource.load(store);
      assert.equal(resource.committed(store), true);
      assert.equal(useControlPlaneStore().processGeneration, 100);
      await resource.load(store, 15_000);
      assert.equal(requests.length, 1);
      store.clear();
    });
  }
});

test("provider full reads reject old payloads after an independent backend discovery without resyncing old tokens", async t => {
  for (const resource of restartReads) {
    await t.test(resource.name, async () => {
      setupControlPlane();
      const old = deferred<object>();
      const requests = installFetchMock(() => {
        if (requests.length === 1) return old.promise;
        if (requests.length === 2) return { unpublished: [], revision: { revision: 7, processGeneration: 100 } };
        return resource.body(100);
      });
      const store = useProvidersStore();
      const pending = resource.load(store);
      await dashboardV4.getAliasPublication();
      old.resolve(resource.body(99));
      await pending;
      assert.equal(resource.committed(store), false);
      assert.equal(useControlPlaneStore().processGeneration, 100);
      await resource.load(store, 15_000);
      assert.equal(resource.committed(store), true);
      assert.equal(requests.length, 3);
      store.clear();
    });
  }
});

test("a publication mutation from an old backend cannot overwrite new publication state", async () => {
  setupControlPlane();
  const patch = deferred<object>();
  let restarted = false;
  const requests = installFetchMock(request => request.method === "PATCH" ? patch.promise : {
    unpublished: restarted ? ["current-hidden"] : [],
    revision: { revision: 7, processGeneration: restarted ? 100 : 99 },
  });
  const store = useProvidersStore();
  await store.loadAliasPublication();
  const pending = store.setAliasPublished("old-hidden", false);
  for (let i = 0; i < 10 && !requests.some(request => request.method === "PATCH"); i++) await Promise.resolve();
  assert.ok(requests.some(request => request.method === "PATCH"));
  restarted = true;
  await store.loadAliasPublication();
  patch.resolve({ unpublished: ["old-hidden"], revision: { revision: 8, processGeneration: 99 } });
  await pending;
  assert.deepEqual(store.aliasUnpublished, ["current-hidden"]);
  assert.equal(useControlPlaneStore().processGeneration, 100);
  assert.deepEqual(store.aliasPublicationPending, []);
  store.clear();
});

test("provider full reads reject lower revisions after an independent confirmed operation", async t => {
  for (const resource of restartReads) {
    await t.test(resource.name, async () => {
      setupControlPlane();
      const pending = deferred<object>();
      const requests = installFetchMock(() => {
        if (requests.length === 1) return pending.promise;
        if (requests.length === 2) return { unpublished: [], revision: { revision: 9, processGeneration: 99 } };
        return resource.body(99, 9);
      });
      const store = useProvidersStore();
      const read = resource.load(store);
      await dashboardV4.getAliasPublication();
      pending.resolve(resource.body(99, 8));
      await read;
      assert.equal(resource.committed(store), false);
      assert.equal(useControlPlaneStore().revision, 9);
      await resource.load(store, 15_000);
      assert.equal(resource.committed(store), true);
      assert.equal(requests.length, 3);
      store.clear();
    });
  }
});
