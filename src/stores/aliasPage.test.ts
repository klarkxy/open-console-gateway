import assert from "node:assert/strict";
import test from "node:test";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";
import { useAliasPageStore } from "./aliasPage.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

function deferred<T>() { let resolve!: (value: T) => void; let reject!: (cause: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; }
function page(name = "one", processGeneration = 99, revision = 7, validUntil = new Date(Date.now() + 15_000).toISOString()) {
  return { revision: { revision, processGeneration }, readVersion: `${processGeneration}:${revision}`, asOf: new Date().toISOString(), validUntil,
    totalGroups: 2, totalRows: 2, filteredGroups: 2, filteredRows: 2, offset: 0, limit: 50, hasMore: false, errors: [],
    groups: [name, "two"].map(publicModel => ({ publicModel, publicationKey: publicModel, published: true, totalRows: 1, matchingRows: 1, hasOverlap: false, continued: false, rows: [] })) };
}

test("alias page coalesces reads, reuses fresh data, and forces manual revalidation", async () => {
  setupControlPlane(); const gate = deferred<object>(); const requests = installFetchMock(() => gate.promise);
  const store = useAliasPageStore(); const first = store.load({ limit: 50 }); const second = store.load({ limit: 50 });
  assert.equal(requests.length, 1); gate.resolve(page()); await Promise.all([first, second]);
  await store.load({ limit: 50 }, { maxAgeMs: 15_000 }); assert.equal(requests.length, 1);
  await store.load({ limit: 50 }); assert.equal(requests.length, 2);
});

test("alias query and session changes fence late responses; a read error retains content", async () => {
  setupControlPlane(); const old = deferred<object>();
  installFetchMock(request => request.url.includes("search=old") ? old.promise : page("current"));
  const store = useAliasPageStore(); const pending = store.load({ search: "old" }); await store.load({ search: "new" });
  old.resolve(page("old")); await pending; assert.equal(store.groups[0]?.publicModel, "current");
  installFetchMock(() => { throw new Error("offline"); }); await assert.rejects(store.load({ search: "new" }), /offline/);
  assert.equal(store.groups[0]?.publicModel, "current"); assert.match(store.error, /offline/);
  const late = deferred<object>(); installFetchMock(() => late.promise); const abandoned = store.load({});
  store.clear(); late.resolve(page("abandoned")); await abandoned; assert.equal(store.page, null);
});

test("server expiry bounds fresh alias reuse", async () => {
  setupControlPlane(); const requests = installFetchMock(() => page("expired", 99, 7, new Date(Date.now() - 1).toISOString()));
  const store = useAliasPageStore(); await store.load({}); await store.load({}, { maxAgeMs: 15_000 }); assert.equal(requests.length, 2);
});

test("the response announcing a new backend commits; a late old-process result cannot restore it", async () => {
  setupControlPlane(); const late = deferred<object>(); let count = 0;
  installFetchMock(() => ++count === 1 ? late.promise : page("new", 100));
  const store = useAliasPageStore(); const old = store.load({ search: "old" }); await store.load({ search: "new" });
  assert.equal(store.page?.revision.processGeneration, 100); late.resolve(page("old", 99)); await old;
  assert.equal(store.groups[0]?.publicModel, "new");
});

test("publication commits per name and one failed toggle preserves an unrelated receipt", async () => {
  setupControlPlane(); const first = deferred<object>(); const second = deferred<object>();
  const requests = installFetchMock(request => request.method === "GET" ? page()
    : request.body?.publicModel === "one" ? first.promise : second.promise);
  const store = useAliasPageStore(); await store.load({});
  const one = store.setPublished("one", false); const two = store.setPublished("two", false);
  assert.ok(store.groups.every(group => !group.published));
  first.resolve({ revision: { revision: 8, processGeneration: 99 }, unpublished: ["one"] }); await one;
  second.reject(new Error("second failed")); await two;
  assert.equal(store.groups[0]?.published, false); assert.equal(store.groups[1]?.published, true);
  assert.match(store.publicationErrors.two!, /second failed/); assert.equal(requests.filter(request => request.method === "PATCH").length, 2);
});
test("a cross-page revision announcement fences an older same-process alias response", async () => {
  setupControlPlane(); installFetchMock(() => page()); const store = useAliasPageStore(); await store.load({});
  const late = deferred<object>(); installFetchMock(() => late.promise); const pending = store.load({});
  useControlPlaneStore().sync({ revision: 9, processGeneration: 99 });
  late.resolve(page("old-cross-page", 99, 8)); await pending;
  assert.equal(store.groups[0]?.publicModel, "one"); assert.equal(store.page?.revision.revision, 7);
});
