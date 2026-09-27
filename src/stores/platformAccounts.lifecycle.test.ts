import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { platformAccountsApi, type PlatformAccountsView } from "../api/platform-accounts.ts";
import { usePlatformAccountsStore } from "./platformAccounts.ts";
import { useDestinationsStore } from "./destinations.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const flush = () => new Promise<void>(resolve => setImmediate(resolve));
function view(revision = 1): PlatformAccountsView {
  return {
    revision, processGeneration: 1,
    accounts: ["parent", "other"].map(id => ({ id, kind: "new_api", name: id,
      baseUrl: "https://example.test", hasUserCredential: true, version: 1, snapshot: null })),
    links: ["a", "b"].map(accountId => ({ accountId, platformAccountId: "parent",
      group: { id: null, platform: null, subscriptionType: null, autoGroups: [], verified: false }, snapshot: null })),
  };
}
function fixture() {
  setActivePinia(createPinia());
  const store = usePlatformAccountsStore();
  store.acceptView(view());
  return store;
}
function currentRevision(store: ReturnType<typeof usePlatformAccountsStore>): number | undefined {
  return store.view?.revision;
}

test("identical platform refreshes share one upstream request and retain busy until settlement", async t => {
  const store = fixture();
  const pending = deferred<PlatformAccountsView>();
  const refresh = t.mock.method(platformAccountsApi, "refresh", () => pending.promise);
  const first = store.refreshParent("parent");
  const second = store.refreshParent("parent");
  await flush();
  assert.equal(refresh.mock.callCount(), 1);
  assert.equal(store.refreshing.parent, true);
  assert.equal(await store.refreshChild("parent", "a"), "error");
  assert.equal(refresh.mock.callCount(), 1);
  pending.resolve(view(2));
  assert.deepEqual(await Promise.all([first, second]), ["ok", "ok"]);
  assert.deepEqual(store.refreshing, {});
});

test("a late parent response cannot repopulate a cleared store or unlock a newer refresh", async t => {
  const store = fixture();
  const old = deferred<PlatformAccountsView>();
  const fresh = deferred<PlatformAccountsView>();
  let calls = 0;
  t.mock.method(platformAccountsApi, "refresh", () => (++calls === 1 ? old.promise : fresh.promise));
  const first = store.refreshParent("parent");
  await flush();
  store.clear();
  const second = store.refreshParent("parent");
  await flush();
  old.resolve(view(99));
  assert.equal(await first, "error");
  assert.equal(store.view, null);
  assert.equal(store.refreshing.parent, true);
  fresh.resolve(view(2));
  assert.equal(await second, "ok");
  assert.equal(currentRevision(store), 2);
  assert.deepEqual(store.refreshing, {});
});

test("failed refresh preserves the last accepted view and releases its lock", async t => {
  const store = fixture();
  t.mock.method(platformAccountsApi, "refresh", async () => { throw new Error("offline"); });
  await assert.rejects(store.refreshParent("parent"), /offline/);
  assert.equal(store.view?.revision, 1);
  assert.equal(store.links.length, 2);
  assert.deepEqual(store.refreshing, {});
});

test("a create-and-link observation cannot return old session data", async t => {
  const store = fixture();
  const pending = deferred<PlatformAccountsView>();
  t.mock.method(platformAccountsApi, "refresh", () => pending.promise);
  const refreshing = store.commitRefresh("parent", "a");
  store.clear();
  pending.resolve(view(99));
  await refreshing;
  assert.equal(store.view, null);
});

test("forgetting a deleted Key removes only its link and fences pending observations", async t => {
  const store = fixture();
  const pending = deferred<PlatformAccountsView>();
  t.mock.method(platformAccountsApi, "refresh", () => pending.promise);
  const refreshing = store.refreshChild("parent", "a");
  await flush();
  store.setPendingLink({ parentId: "parent", accountId: "a" });
  store.forgetAccount("a");
  assert.deepEqual(store.links.map(link => link.accountId), ["b"]);
  assert.equal(store.parents.length, 2);
  assert.equal(store.pendingLink, null);
  pending.resolve(view(99));
  assert.equal(await refreshing, "error");
  assert.deepEqual(store.links.map(link => link.accountId), ["b"]);
});

test("confirmed platform deletion survives failed revalidations and late refresh", async t => {
  const store = fixture();
  const before = deferred<PlatformAccountsView>();
  t.mock.method(platformAccountsApi, "refresh", () => before.promise);
  const refreshing = store.refreshParent("parent");
  await flush();
  const remove = t.mock.method(platformAccountsApi, "remove", async () => ({ revision: 2, processGeneration: 1 }));
  t.mock.method(platformAccountsApi, "list", async () => { throw new Error("list offline"); });
  t.mock.method(useDestinationsStore(), "refreshAfterMutation", async () => { throw new Error("projection offline"); });
  assert.equal(await store.remove("parent"), "ok");
  assert.equal(remove.mock.callCount(), 1);
  assert.deepEqual(store.parents.map(parent => parent.id), ["other"]);
  assert.deepEqual(store.links, []);
  assert.ok(store.error);
  assert.ok(store.destinationRefreshError);
  before.resolve(view(99));
  assert.equal(await refreshing, "error");
  assert.deepEqual(store.parents.map(parent => parent.id), ["other"]);
});

test("late deletion after logout performs no reload and does not release a new mutation", async t => {
  const store = fixture();
  const pending = deferred<{ revision: number; processGeneration: number }>();
  t.mock.method(platformAccountsApi, "remove", () => pending.promise);
  const list = t.mock.method(platformAccountsApi, "list", async () => view());
  const remove = store.remove("parent");
  store.clear();
  assert.equal(store.beginMutation(), true);
  pending.resolve({ revision: 2, processGeneration: 1 });
  assert.equal(await remove, "error");
  assert.equal(list.mock.callCount(), 0);
  assert.equal(store.view, null);
  assert.equal(store.mutating, true);
  store.endMutation();
});
