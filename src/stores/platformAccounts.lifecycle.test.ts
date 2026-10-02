import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { platformAccountsApi, type PlatformAccountsView, type PlatformKeyImportResult, type PlatformSnapshot } from "../api/platform-accounts.ts";
import { routingCardsApi } from "../api/destinations.ts";
import { usePlatformAccountsStore } from "./platformAccounts.ts";
import { useDestinationsStore } from "./destinations.ts";
import { useSessionStore } from "./session.ts";

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
  // The warning is recorded when the rejected read settles. A receipt that
  // returns before that read still has to surface it on a later turn.
  await flush();
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

function labeled(revision: number, processGeneration: number, id: string): PlatformAccountsView {
  return {
    revision,
    processGeneration,
    accounts: [{
      id,
      kind: "new_api",
      name: id,
      baseUrl: "https://example.test",
      hasUserCredential: true,
      version: 1,
      snapshot: null,
    }],
    links: [],
  };
}

const PARENT = "parent";
const LINKED_KEY = "a";
const PRIOR_KEY = "b";

function observation(observedAt: number): PlatformSnapshot {
  return {
    billingPreference: null,
    errors: [],
    groups: [],
    models: [],
    observedAt,
    prices: [],
    quotas: [],
    stale: false,
    walletOverflow: null,
  };
}

function catalog(
  revision: number,
  processGeneration: number,
  accountIds: string[],
  parentSnapshot: PlatformSnapshot | null = null,
): PlatformAccountsView {
  return {
    revision,
    processGeneration,
    accounts: [{
      id: PARENT,
      kind: "new_api",
      name: "Site",
      baseUrl: "https://example.test",
      hasUserCredential: true,
      version: 1,
      snapshot: parentSnapshot,
    }],
    links: accountIds.map((accountId) => ({
      accountId,
      platformAccountId: PARENT,
      group: { id: null, platform: null, subscriptionType: null, autoGroups: [], verified: false },
      snapshot: null,
    })),
  };
}

function linkedKeys(store: ReturnType<typeof usePlatformAccountsStore>): string[] {
  return store.links.map((link) => `${link.accountId}->${link.platformAccountId}`).sort();
}

function raceStore(processGeneration: number) {
  setActivePinia(createPinia());
  const store = usePlatformAccountsStore();
  store.acceptView(catalog(2, processGeneration, [PRIOR_KEY]));
  return store;
}

function importResult(nextPage: number | null): PlatformKeyImportResult {
  return { nextPage, imported: 1, skippedExisting: 0, skippedDisabled: 0, failed: [] };
}

async function settled(promise: Promise<unknown>, turns = 8): Promise<boolean> {
  let done = false;
  void promise.then(() => { done = true; }, () => { done = true; });
  for (let turn = 0; turn < turns; turn += 1) await flush();
  return done;
}

test("confirmed platform deletion resolves and unlocks before deferred revalidation", async (t) => {
  const store = fixture();
  const listGate = deferred<PlatformAccountsView>();
  const projectionGate = deferred<void>();
  let listSettled = false;
  let projectionSettled = false;
  const remove = t.mock.method(platformAccountsApi, "remove", async () => ({ revision: 4, processGeneration: 1 }));
  t.mock.method(platformAccountsApi, "list", () => {
    void listGate.promise.then(() => { listSettled = true; }, () => { listSettled = true; });
    return listGate.promise;
  });
  t.mock.method(useDestinationsStore(), "refreshAfterMutation", () => {
    void projectionGate.promise.then(() => { projectionSettled = true; }, () => { projectionSettled = true; });
    return projectionGate.promise;
  });
  const pending = store.remove("parent");
  try {
    assert.equal(await settled(pending), true, "DELETE receipt should settle while follow-up reads are pending");
    assert.equal(await pending, "ok");
    assert.equal(store.mutating, false);
    assert.equal(remove.mock.callCount(), 1);
    assert.equal(listSettled, false);
    assert.equal(projectionSettled, false);
    assert.deepEqual(store.parents.map((parent) => parent.id), ["other"]);
    assert.deepEqual(store.links, []);
    listGate.reject(new Error("list offline"));
    projectionGate.reject(new Error("projection offline"));
    await flush();
    assert.equal(remove.mock.callCount(), 1);
    assert.deepEqual(store.parents.map((parent) => parent.id), ["other"]);
    assert.ok(store.error || store.destinationRefreshError);
  } finally {
    listGate.reject(new Error("list closed"));
    projectionGate.reject(new Error("projection closed"));
    await pending.catch(() => undefined);
  }
});

test("platform import receipt and page continuation unlock before deferred list revalidation", async (t) => {
  const store = fixture();
  const listGate = deferred<PlatformAccountsView>();
  const pages: Array<number | undefined> = [];
  t.mock.method(platformAccountsApi, "importKeys", async (_id: string, page?: number) => {
    pages.push(page);
    return importResult(pages.length === 1 ? 2 : null);
  });
  t.mock.method(platformAccountsApi, "list", () => listGate.promise);
  const first = store.importKeys("parent");
  let second: Promise<unknown> | undefined;
  try {
    assert.equal(await settled(first), true, "import receipt should settle while the list read is pending");
    assert.equal(store.mutating, false);
    assert.equal(Boolean(store.importing.parent), false);
    assert.deepEqual(pages, [undefined]);
    second = store.importKeys("parent");
    await flush();
    assert.deepEqual(pages, [undefined, 2]);
    listGate.resolve(labeled(3, 1, "imported"));
    const [firstResult, secondResult] = await Promise.all([first, second]);
    assert.notEqual(firstResult, "error");
    assert.notEqual(secondResult, "error");
    assert.equal(pages.filter((page) => page === undefined || page === 1).length, 1);
  } finally {
    listGate.resolve(labeled(3, 1, "imported"));
    await first.catch(() => undefined);
    if (second) await second.catch(() => undefined);
  }
});

test("a refresh response from the process that started it cannot replace a link committed under another process", async (t) => {
  // 8 and 3 are opaque identities, not a rank. Timeline:
  // 1. Parent refresh starts while the live view is process 8, revision 2, Key b only.
  // 2. Link reply arrives first under process 3: parent "parent", Keys b and a, revision 3.
  // 3. The original refresh body, still process 8, arrives afterward (revision 99, Key a absent).
  const started = 8;
  const restarted = 3;
  const store = raceStore(started);
  const refreshGate = deferred<PlatformAccountsView>();
  const linkGate = deferred<PlatformAccountsView>();
  t.mock.method(platformAccountsApi, "refresh", () => refreshGate.promise);
  t.mock.method(platformAccountsApi, "link", () => linkGate.promise);
  const refreshing = store.refreshParent(PARENT);
  const linking = store.link(LINKED_KEY, PARENT, { id: null, platform: null });
  const linked = catalog(3, restarted, [PRIOR_KEY, LINKED_KEY]);
  const staleRefresh = catalog(99, started, [PRIOR_KEY]);
  try {
    linkGate.resolve(linked);
    assert.equal(await linking, "ok");
    assert.equal(store.view?.processGeneration, restarted);
    assert.deepEqual(linkedKeys(store), ["a->parent", "b->parent"]);
    refreshGate.resolve(staleRefresh);
    await refreshing.catch(() => undefined);
    assert.equal(store.view?.processGeneration, restarted);
    assert.equal(store.view?.revision, 3);
    assert.equal(store.parents[0]?.id, PARENT);
    assert.deepEqual(linkedKeys(store), ["a->parent", "b->parent"]);
  } finally {
    refreshGate.resolve(staleRefresh);
    linkGate.resolve(linked);
    await Promise.allSettled([refreshing, linking]);
  }
});

test("a same-process refresh captured before a link cannot roll that link back", async (t) => {
  // Same process throughout. Timeline:
  // 1. Refresh renders revision 2 before Key a is linked, then the response waits.
  // 2. Link reply arrives first: revision 3, parent "parent", Keys b and a.
  // 3. The delayed revision 2 body must not remove Key a.
  const process = 4;
  const store = raceStore(process);
  const refreshGate = deferred<PlatformAccountsView>();
  const linkGate = deferred<PlatformAccountsView>();
  t.mock.method(platformAccountsApi, "refresh", () => refreshGate.promise);
  t.mock.method(platformAccountsApi, "link", () => linkGate.promise);
  const refreshing = store.refreshParent(PARENT);
  const linking = store.link(LINKED_KEY, PARENT, { id: null, platform: null });
  const linked = catalog(3, process, [PRIOR_KEY, LINKED_KEY]);
  const capturedBeforeLink = catalog(2, process, [PRIOR_KEY]);
  try {
    linkGate.resolve(linked);
    assert.equal(await linking, "ok");
    refreshGate.resolve(capturedBeforeLink);
    await refreshing.catch(() => undefined);
    assert.equal(store.view?.processGeneration, process);
    assert.equal(store.view?.revision, 3);
    assert.equal(store.parents[0]?.id, PARENT);
    assert.deepEqual(linkedKeys(store), ["a->parent", "b->parent"]);
  } finally {
    refreshGate.resolve(capturedBeforeLink);
    linkGate.resolve(linked);
    await Promise.allSettled([refreshing, linking]);
  }
});

test("a same-process refresh newer than a link keeps that link and its new observation", async (t) => {
  // Same process throughout. Timeline:
  // 1. Refresh is still awaiting upstream when the link commits.
  // 2. Link reply: revision 3, parent "parent", Keys b and a.
  // 3. Refresh then reads the current database: revision 4, both Keys, parent observation observedAt 40.
  //    That complete view must replace the link receipt.
  const process = 4;
  const store = raceStore(process);
  const refreshGate = deferred<PlatformAccountsView>();
  const linkGate = deferred<PlatformAccountsView>();
  t.mock.method(platformAccountsApi, "refresh", () => refreshGate.promise);
  t.mock.method(platformAccountsApi, "link", () => linkGate.promise);
  const refreshing = store.refreshParent(PARENT);
  const linking = store.link(LINKED_KEY, PARENT, { id: null, platform: null });
  const linked = catalog(3, process, [PRIOR_KEY, LINKED_KEY]);
  const current = catalog(4, process, [PRIOR_KEY, LINKED_KEY], observation(40));
  try {
    linkGate.resolve(linked);
    assert.equal(await linking, "ok");
    refreshGate.resolve(current);
    assert.equal(await refreshing, "ok");
    assert.equal(store.view?.processGeneration, process);
    assert.equal(store.view?.revision, 4);
    assert.equal(store.parents[0]?.id, PARENT);
    assert.equal(store.parents[0]?.snapshot?.observedAt, 40);
    assert.deepEqual(linkedKeys(store), ["a->parent", "b->parent"]);
  } finally {
    refreshGate.resolve(current);
    linkGate.resolve(linked);
    await Promise.allSettled([refreshing, linking]);
  }
});

test("an import list from the process that started it cannot replace a link committed under another process", async (t) => {
  // 8 and 3 are opaque identities. Timeline:
  // 1. Import's list read starts under process 8.
  // 2. Link reply arrives under process 3 with Key a on parent "parent".
  // 3. The original list body (process 8, revision 99, Key a absent) settles and must not replace the link.
  const started = 8;
  const restarted = 3;
  const store = raceStore(started);
  const listGate = deferred<PlatformAccountsView>();
  const linkGate = deferred<PlatformAccountsView>();
  t.mock.method(platformAccountsApi, "importKeys", async () => importResult(null));
  t.mock.method(platformAccountsApi, "list", () => listGate.promise);
  t.mock.method(platformAccountsApi, "link", () => linkGate.promise);
  const importing = store.importKeys(PARENT);
  await flush();
  const linking = store.link(LINKED_KEY, PARENT, { id: null, platform: null });
  const linked = catalog(3, restarted, [PRIOR_KEY, LINKED_KEY]);
  const staleList = catalog(99, started, [PRIOR_KEY]);
  try {
    linkGate.resolve(linked);
    assert.equal(await linking, "ok");
    listGate.resolve(staleList);
    await importing;
    assert.equal(store.view?.processGeneration, restarted);
    assert.equal(store.view?.revision, 3);
    assert.equal(store.parents[0]?.id, PARENT);
    assert.deepEqual(linkedKeys(store), ["a->parent", "b->parent"]);
  } finally {
    listGate.resolve(staleList);
    linkGate.resolve(linked);
    await Promise.allSettled([importing, linking]);
  }
});

test("an older platform list cannot replace a list that started later", async (t) => {
  const store = fixture();
  const older = deferred<PlatformAccountsView>();
  const newer = deferred<PlatformAccountsView>();
  let calls = 0;
  t.mock.method(platformAccountsApi, "list", () => (++calls === 1 ? older.promise : newer.promise));
  const first = store.load();
  const second = store.load();
  newer.resolve(labeled(5, 1, "newer"));
  await second;
  older.resolve(labeled(9, 1, "older"));
  await first;
  assert.equal(store.view?.revision, 5);
  assert.deepEqual(store.parents.map((parent) => parent.id), ["newer"]);
});

test("a platform list resolving after session teardown cannot repopulate parents", async (t) => {
  const store = fixture();
  useSessionStore();
  const pending = deferred<PlatformAccountsView>();
  t.mock.method(platformAccountsApi, "list", () => pending.promise);
  const loading = store.load();
  await flush();
  useSessionStore().dropSession();
  pending.resolve(view(99));
  await loading;
  await flush();
  assert.equal(store.view, null);
  assert.equal(store.loaded, false);
  assert.deepEqual(store.parents, []);
  assert.equal(store.error, "");
});

test("an import list resolving after session teardown cannot repopulate parents", async (t) => {
  const store = fixture();
  useSessionStore();
  const importGate = deferred<PlatformKeyImportResult>();
  const listGate = deferred<PlatformAccountsView>();
  t.mock.method(platformAccountsApi, "importKeys", () => importGate.promise);
  t.mock.method(platformAccountsApi, "list", () => listGate.promise);
  const importing = store.importKeys("parent");
  await flush();
  importGate.resolve(importResult(2));
  await flush();
  useSessionStore().dropSession();
  listGate.resolve(view(50));
  await importing;
  await flush();
  assert.equal(store.view, null);
  assert.equal(store.loaded, false);
  assert.deepEqual(store.parents, []);
  assert.equal(Boolean(store.importing.parent), false);
});

test("a destination read started by a platform save cannot repopulate after session teardown", async (t) => {
  const store = fixture();
  const destinations = useDestinationsStore();
  useSessionStore();
  const snapshot = deferred<{
    destinations: { id: string }[];
    credentials: [];
    cards: [];
    expectation: { expectedRevision: number; processGeneration: number };
  }>();
  const read = t.mock.method(routingCardsApi, "listSnapshot", () => snapshot.promise);
  t.mock.method(platformAccountsApi, "create", async () => view(2));
  const saving = store.createOrUpdate({
    kind: "new_api",
    name: "created",
    baseUrl: "https://example.test",
  }, null);
  await flush();
  assert.equal(await saving, "saved");
  assert.ok(read.mock.callCount() >= 1);
  useSessionStore().dropSession();
  snapshot.resolve({
    destinations: [{ id: "restored" }],
    credentials: [],
    cards: [],
    expectation: { expectedRevision: 9, processGeneration: 2 },
  });
  await flush();
  assert.equal(destinations.loaded, false);
  assert.deepEqual(destinations.destinations, []);
});
