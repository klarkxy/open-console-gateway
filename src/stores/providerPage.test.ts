import type { Destination } from "../api/destinations.ts";
import type { ProviderCatalogPresentation } from "../api/generated/dashboard-v3.ts";
import type { ProviderContractsResponse } from "../api/providers.ts";
import assert from "node:assert/strict";
import test from "node:test";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { useProviderPageStore } from "./providerPage.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(yes => { resolve = yes; }); return { promise, resolve }; }
function rail(name = "current", processGeneration = 99) {
  return { revision: { revision: 7, processGeneration }, readVersion: `${processGeneration}:7`, asOf: new Date().toISOString(),
    validUntil: new Date(Date.now() + 15_000).toISOString(), total: 1000, filteredTotal: 1000, offset: 0, limit: 50, hasMore: true, errors: [],
    items: [{ railKey: `d:${name}`, name }] };
}
test("provider rail coalesces duplicates and keeps only the latest query or session", async () => {
  setupControlPlane(); const gate = deferred<object>(); const requests = installFetchMock(request => request.url.includes("search=old") ? gate.promise : rail());
  const store = useProviderPageStore(); const old = store.loadRail({ search: "old" }); const duplicate = store.loadRail({ search: "old" });
  assert.equal(requests.length, 1); await store.loadRail({ search: "new" }); gate.resolve(rail("old")); await Promise.all([old, duplicate]);
  assert.equal(store.rail?.items[0]?.name, "current"); await store.loadRail({ search: "new" }, { maxAgeMs: 15_000 }); assert.equal(requests.length, 2);
  const late = deferred<object>(); installFetchMock(() => late.promise); const pending = store.loadRail({}); store.clear(); late.resolve(rail("abandoned")); await pending;
  assert.equal(store.rail, null);
});
test("provider failures preserve the last page and expose the failed resource", async () => {
  setupControlPlane(); installFetchMock(() => rail()); const store = useProviderPageStore(); await store.loadRail({});
  installFetchMock(() => { throw new Error("offline"); }); await assert.rejects(store.loadRail({}), /offline/);
  assert.equal(store.rail?.items[0]?.name, "current"); assert.match(store.errors.rail!, /offline/);
});
test("new backend discovery commits while old-process responses remain fenced", async () => {
  setupControlPlane(); const late = deferred<object>(); let count = 0;
  installFetchMock(() => ++count === 1 ? late.promise : rail("new", 100));
  const store = useProviderPageStore(); const old = store.loadRail({ search: "old" }); await store.loadRail({ search: "new" });
  assert.equal(store.rail?.revision.processGeneration, 100); late.resolve(rail("old", 99)); await old;
  assert.equal(store.rail?.items[0]?.name, "new");
});
test("a cross-page revision announcement fences an older same-process rail response", async () => {
  setupControlPlane(); installFetchMock(() => rail()); const store = useProviderPageStore(); await store.loadRail({});
  const late = deferred<object>(); installFetchMock(() => late.promise); const pending = store.loadRail({});
  useControlPlaneStore().sync({ revision: 9, processGeneration: 99 });
  late.resolve({ ...rail("old-cross-page"), revision: { revision: 8, processGeneration: 99 } }); await pending;
  assert.equal(store.rail?.items[0]?.name, "current"); assert.equal(store.rail?.revision.revision, 7);
});
test("provider and connection bookmarks reuse the canonical selected header while fresh", async () => {
  setupControlPlane();
  const requests = installFetchMock(() => ({ revision: { revision: 7, processGeneration: 99, pricingRevision: null }, readVersion: "current",
    item: { railKey: "d:destination", destinationId: "destination", connectionId: "connection", providerId: "provider" } }));
  const store = useProviderPageStore(); await store.loadDetail("p:provider");
  await store.loadDetail("d:destination", { maxAgeMs: 15_000 });
  await store.loadDetail("c:connection", { maxAgeMs: 15_000 });
  assert.equal(requests.length, 1); assert.equal(store.detail?.item.railKey, "d:destination");
});

function receiptPresentation(): ProviderCatalogPresentation {
  return { total: 2, allDisabled: true, models: ["Model 2", "Model 10"].map(id => ({
    publicModel: id, upstreamModel: id, upstreamOverride: null,
    contract: { alias: id, modelId: id, preferredProtocol: "messages", protocols: {
      chat_completions: null, responses: null, messages: null }, routable: false, disabledReasons: [] },
    targetProtocol: "messages", testProtocol: null, writableProtocols: ["responses"], effectiveOn: false,
    actions: [{ key: "toggle", allowed: false, reason: "canonical_denial" }],
  })) };
}
async function selectedPage() {
  setupControlPlane();
  const revision = { revision: 7, processGeneration: 99, pricingRevision: "p1" };
  const presentation = receiptPresentation();
  installFetchMock(request => request.url.endsWith("/models") ? {
    revision, readVersion: "page7", total: 1, filteredTotal: 1, allDisabled: false,
    offset: 0, limit: 50, hasMore: false, models: [{ ...presentation.models[0], effectiveOn: true,
      metadata: { marker: "retained" }, metadataSource: "saved" }],
  } : { revision, readVersion: "page7", item: { railKey: "d:destination", providerId: "provider", catalogCount: 1 },
    destination: { id: "destination" }, scope: { scopeKind: "provider", scopeId: "provider", revision: 7,
      allDisabled: false, catalog: { modelCount: 1 } } });
  const store = useProviderPageStore();
  await store.loadDetail("d:destination"); await store.loadModels("d:destination", {});
  return store;
}
function canonicalReceipt(revision = 8, process_generation = 99, scope_id = "provider"): ProviderContractsResponse {
  return { revision, process_generation, pricing_revision: "p1", custom_endpoints: [], providers: [{ scope_id,
    presentation: receiptPresentation() }] } as unknown as ProviderContractsResponse;
}
test("canonical receipt facts and metadata survive a failed page revalidation", async () => {
  const store = await selectedPage();
  useControlPlaneStore().sync({ revision: 8, processGeneration: 99 });
  store.commitContracts(canonicalReceipt());
  assert.equal(store.models?.total, 2); assert.equal(store.models?.allDisabled, true);
  assert.equal(store.models?.models[0]?.targetProtocol, "messages");
  assert.deepEqual(store.models?.models[0]?.writableProtocols, ["responses"]);
  assert.equal(store.models?.models[0]?.actions[0]?.allowed, false);
  assert.equal(store.models?.models[0]?.metadataSource, "saved");
  assert.equal(store.detail?.scope?.catalog.modelCount, 2);
  installFetchMock(() => { throw new Error("offline after acknowledgement"); });
  await assert.rejects(store.loadModels("d:destination", {}), /offline/);
  assert.equal(store.models?.revision.revision, 8); assert.equal(store.models?.models.length, 2);
});

test("a refreshed header preserves a pending edit read for the same selection", async () => {
  setupControlPlane();
  const store = useProviderPageStore();
  const body = (revision: number, railKey = "d:destination") => ({
    revision: { revision, processGeneration: 99 }, readVersion: `page${revision}`, item: { railKey },
  });
  installFetchMock(() => body(7));
  await store.loadDetail("d:destination");
  const gate = deferred<object>();
  let editSignal: AbortSignal | null = null;
  Object.defineProperty(globalThis, "fetch", { configurable: true, value: async (url: string, init: RequestInit) => {
    const result = url.endsWith("/edit-detail")
      ? (editSignal = init.signal!, await gate.promise) : body(8);
    return new Response(JSON.stringify(result), { headers: { "Content-Type": "application/json" } });
  } });
  const editing = store.loadEditDetail("d:destination");
  await store.loadDetail("d:destination");
  assert.equal((editSignal as AbortSignal | null)?.aborted, false);
  gate.resolve(body(8));
  await editing;
  assert.equal(store.editDetail?.readVersion, "page8");
});

test("switching provider selections still cancels and fences a pending editor", async () => {
  setupControlPlane();
  const store = useProviderPageStore();
  const body = (railKey: string) => ({
    revision: { revision: 7, processGeneration: 99 }, readVersion: "page7", item: { railKey },
  });
  installFetchMock(() => body("d:destination"));
  await store.loadDetail("d:destination");
  const gate = deferred<object>();
  let editSignal: AbortSignal | null = null;
  Object.defineProperty(globalThis, "fetch", { configurable: true, value: async (url: string, init: RequestInit) => {
    const result = url.endsWith("/edit-detail")
      ? (editSignal = init.signal!, await gate.promise) : body("d:other");
    return new Response(JSON.stringify(result), { headers: { "Content-Type": "application/json" } });
  } });
  const editing = store.loadEditDetail("d:destination");
  await store.loadDetail("d:other");
  assert.equal((editSignal as AbortSignal | null)?.aborted, true);
  gate.resolve(body("d:destination"));
  await editing;
  assert.equal(store.editDetail, null);
  assert.equal(store.detail?.item.railKey, "d:other");
});

test("a fresh installation without a pricing snapshot still commits catalog receipts", async () => {
  const store = await selectedPage();
  useControlPlaneStore().sync({ revision: 8, processGeneration: 99 });
  store.commitContracts({ ...canonicalReceipt(), pricing_revision: "" });
  assert.equal(store.models?.revision.revision, 8);
  assert.equal(store.models?.revision.pricingRevision, "");
  assert.equal(store.models?.total, 2);
  assert.equal(store.models?.models[0]?.actions[0]?.allowed, false);
});
test("canonical receipts cannot replace another selection, a newer revision, or a cleared session", async () => {
  const store = await selectedPage();
  useControlPlaneStore().sync({ revision: 9, processGeneration: 99 });
  store.commitContracts(canonicalReceipt(8));
  store.commitContracts(canonicalReceipt(9, 98));
  store.commitContracts(canonicalReceipt(9, 99, "another"));
  assert.equal(store.models?.revision.revision, 7);
  const gate = deferred<object>(); installFetchMock(() => gate.promise);
  const switched = store.loadDetail("d:another");
  store.commitContracts(canonicalReceipt(9));
  assert.equal(store.models?.revision.revision, 7);
  store.clear(); store.commitContracts(canonicalReceipt(9));
  gate.resolve({ revision: { revision: 9, processGeneration: 99 }, item: { railKey: "d:another" } });
  await switched; assert.equal(store.models, null); assert.equal(store.detail, null);
});

test("destination receipts carry their revision and remapped models drop unrelated metadata", async () => {
  const store = await selectedPage();
  const presentation = receiptPresentation();
  presentation.models[0]!.upstreamModel = "replacement";
  useControlPlaneStore().sync({ revision: 8, processGeneration: 99 });
  const receipt = { id: "destination", presentation,
    presentation_revision: { revision: 7, processGeneration: 99, pricingRevision: "p1" } } as unknown as Destination;
  store.commitDestination(receipt);
  assert.equal(store.models?.revision.revision, 7);
  store.commitDestination({ ...receipt, presentation_revision: { revision: 8, processGeneration: 98, pricingRevision: "p1" } });
  assert.equal(store.models?.revision.revision, 7);
  store.commitDestination({ ...receipt, presentation_revision: { revision: 8, processGeneration: 99, pricingRevision: "p1" } });
  assert.equal(store.models?.revision.revision, 8);
  assert.equal(store.models?.models[0]?.metadata, null);
  assert.equal(store.models?.models[0]?.metadataSource, null);
});

test("switching providers drops old model rows even when the new read fails at the same revision", async () => {
  const store = await selectedPage();
  const previous = store.models!;
  const oldRead = deferred<object>();
  installFetchMock(() => oldRead.promise);
  const pending = store.loadModels("d:destination", {});
  installFetchMock(() => ({ revision: previous.revision, readVersion: previous.readVersion,
    item: { railKey: "d:other", providerId: "other" } }));
  await store.loadDetail("d:other");
  assert.equal(store.models, null);
  installFetchMock(() => { throw new Error("models unavailable"); });
  await assert.rejects(store.loadModels("d:other", {}), /models unavailable/);
  oldRead.resolve(previous);
  await pending;
  assert.equal(store.detail?.item.railKey, "d:other");
  assert.equal(store.models, null);
  assert.equal(store.loading.models, false);
  assert.match(store.errors.models!, /models unavailable/);
});

test("same-provider failed model refresh preserves its confirmed rows", async () => {
  const store = await selectedPage();
  const previous = store.models;
  installFetchMock(() => ({ ...store.detail!, readVersion: "page8" }));
  await store.loadDetail("d:destination");
  installFetchMock(() => { throw new Error("models unavailable"); });
  await assert.rejects(store.loadModels("d:destination", {}), /models unavailable/);
  assert.equal(store.models, previous);
  assert.equal(store.detail?.item.railKey, "d:destination");
});
