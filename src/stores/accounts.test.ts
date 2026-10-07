import assert from "node:assert/strict";
import test from "node:test";
import type { Account } from "../api/dashboard.ts";
import { dashboardApi } from "../api/dashboard.ts";
import { installFetchMock, setupControlPlane, v3AccountDto } from "../test-helpers/dashboard-v3-fetch.ts";
import { useAccountsStore } from "./accounts.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useSessionStore } from "./session.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function body(id: string, processGeneration = 99) {
  return { accounts: [v3AccountDto(id)], revision: 7, processGeneration };
}

test("account page reuse requires a successful read; concurrent callers share payload and keep content rendered", async () => {
  setupControlPlane();
  const gate = deferred<object>();
  const requests = installFetchMock(() => gate.promise);
  const store = useAccountsStore();
  const first = store.loadPresented();
  const second = store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 1);
  gate.resolve(body("one"));
  assert.deepEqual((await first).map(row => row.id), ["one"]);
  assert.deepEqual((await second).map(row => row.id), ["one"]);
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 1);
  const refresh = store.loadPresented();
  assert.equal(requests.length, 2);
  assert.equal(store.loaded, true);
  assert.deepEqual(store.accounts.map(row => row.id), ["one"]);
  await refresh;
  store.clearAccounts();
});

test("failed revalidation retains the list but cannot be reused as fresh", async () => {
  setupControlPlane();
  let response = async (): Promise<object> => body("one");
  const requests = installFetchMock(() => response());
  const store = useAccountsStore();
  await store.loadPresented();
  response = async () => { throw new Error("offline"); };
  await assert.rejects(store.loadPresented());
  assert.equal(store.loaded, true);
  assert.deepEqual(store.accounts.map(row => row.id), ["one"]);
  response = async () => body("two");
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 3);
  assert.deepEqual(store.accounts.map(row => row.id), ["two"]);
  store.clearAccounts();
});

test("mutation commits detach pending account reads and invalidate completed freshness", async () => {
  setupControlPlane();
  const old = deferred<object>();
  const next = deferred<object>();
  const requests = installFetchMock(() => requests.length === 1 ? old.promise : next.promise);
  const store = useAccountsStore();
  const first = store.loadPresented();
  store.setAccounts([{ id: "saved" } as Account]);
  const second = store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 2);
  old.resolve(body("old"));
  await first;
  assert.deepEqual(store.accounts.map(row => row.id), ["saved"]);
  next.resolve(body("new"));
  await second;
  assert.deepEqual(store.accounts.map(row => row.id), ["new"]);
  store.setAccounts([{ id: "saved-again" } as Account]);
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 3);
  store.clearAccounts();
});

test("session and backend identity changes bypass freshness and detach old flights", async () => {
  setupControlPlane();
  const gate = deferred<object>();
  const requests = installFetchMock(() => requests.length === 1 ? gate.promise : body("current", 100));
  const store = useAccountsStore();
  const abandoned = store.loadPresented();
  useSessionStore().dropSession();
  useControlPlaneStore().sync({ revision: 7, processGeneration: 100 });
  await store.loadPresented({ maxAgeMs: 15_000 });
  gate.resolve(body("abandoned"));
  await abandoned;
  assert.deepEqual(store.accounts.map(row => row.id), ["current"]);
  useControlPlaneStore().sync({ revision: 7, processGeneration: 101 });
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 3);
  store.clearAccounts();
});

test("legacy account cache is discarded and the first page read must validate inventory", async t => {
  setupControlPlane();
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: {
    getItem: () => { throw new Error("business snapshots must not be read"); },
    removeItem() {},
  } });
  t.after(() => {
    if (original) Object.defineProperty(globalThis, "localStorage", original);
    else Reflect.deleteProperty(globalThis, "localStorage");
  });
  const requests = installFetchMock(() => body("validated"));
  const store = useAccountsStore();
  assert.equal(store.loaded, false);
  assert.equal(store.accounts.length, 0);
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 1);
  assert.deepEqual(store.accounts.map(row => row.id), ["validated"]);
  store.clearAccounts();
});

test("a lazy account detail preserves inventory completeness and still requires an inventory read", async () => {
  setupControlPlane();
  const requests = installFetchMock(() => body("full"));
  const store = useAccountsStore();
  store.upsertDetailAccount({ id: "detail" } as Account);
  assert.equal(store.loaded, false);
  assert.deepEqual(store.accounts.map(row => row.id), ["detail"]);
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 1);
  assert.equal(store.loaded, true);
  store.upsertDetailAccount({ id: "full" } as Account);
  assert.equal(store.loaded, true);
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 2);
  store.clearAccounts();
});

test("account inventory commits the response that discovers a restarted backend", async () => {
  setupControlPlane();
  const requests = installFetchMock(() => body("after-restart", 100));
  const store = useAccountsStore();
  const result = await store.loadPresented();
  assert.deepEqual(result.map(row => row.id), ["after-restart"]);
  assert.deepEqual(store.accounts.map(row => row.id), ["after-restart"]);
  assert.equal(store.loaded, true);
  assert.equal(useControlPlaneStore().processGeneration, 100);
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 1);
  store.clearAccounts();
});

test("an account response withheld from the old backend cannot commit after an independent restart discovery", async () => {
  setupControlPlane();
  const gate = deferred<object>();
  const requests = installFetchMock(() => requests.length === 1 ? gate.promise : body("current", 100));
  const store = useAccountsStore();
  store.commitPresented([{ id: "before" } as Account]);
  const old = store.loadPresented();
  const discovered = await dashboardApi.getAccountsSnapshot();
  assert.equal(discovered.expectation.processGeneration, 100);
  gate.resolve(body("old", 99));
  assert.deepEqual((await old).map(row => row.id), ["old"]);
  assert.deepEqual(store.accounts.map(row => row.id), ["before"]);
  assert.equal(useControlPlaneStore().processGeneration, 100);
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 3);
  assert.deepEqual(store.accounts.map(row => row.id), ["current"]);
  store.clearAccounts();
});

test("a restarted account inventory response cannot overwrite a row mutation or session teardown", async () => {
  setupControlPlane();
  let gate = deferred<object>();
  installFetchMock(() => gate.promise);
  const store = useAccountsStore();
  const pending = store.loadPresented();
  store.upsertDetailAccount({ id: "saved" } as Account);
  gate.resolve(body("after-restart", 100));
  await pending;
  assert.deepEqual(store.accounts.map(row => row.id), ["saved"]);
  assert.equal(store.loaded, false);
  gate = deferred<object>();
  const abandoned = store.loadPresented();
  useSessionStore().dropSession();
  gate.resolve(body("another-restart", 101));
  await abandoned;
  assert.deepEqual(store.accounts, []);
  assert.equal(store.loaded, false);
  assert.equal(useControlPlaneStore().processGeneration, null);
});

test("account full reads cannot commit a lower revision after an independent confirmed operation", async () => {
  setupControlPlane();
  const pending = deferred<object>();
  const requests = installFetchMock(() => {
    if (requests.length === 1) return body("before");
    if (requests.length === 2) return pending.promise;
    return { ...body("current"), revision: 9 };
  });
  const store = useAccountsStore();
  await store.loadPresented();
  const read = store.loadPresented();
  await dashboardApi.getAccountsSnapshot();
  pending.resolve({ ...body("stale"), revision: 8 });
  assert.deepEqual((await read).map(row => row.id), ["stale"]);
  assert.deepEqual(store.accounts.map(row => row.id), ["before"]);
  assert.equal(useControlPlaneStore().revision, 9);
  await store.loadPresented({ maxAgeMs: 15_000 });
  assert.equal(requests.length, 4);
  assert.deepEqual(store.accounts.map(row => row.id), ["current"]);
  store.clearAccounts();
});
