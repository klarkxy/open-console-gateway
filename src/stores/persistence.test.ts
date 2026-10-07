import assert from "node:assert/strict";
import test, { type TestContext } from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { dropAllSnapshots, dropSnapshot } from "./persistence.ts";
import { useAccountsStore } from "./accounts.ts";
import { useDestinationsStore } from "./destinations.ts";
import { useProvidersStore } from "./providers.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import type { Account } from "../api/dashboard.ts";

function installLocalStorage(t: TestContext): Map<string, string> {
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  const backing = new Map<string, string>();
  const storage: Storage = {
    get length() { return backing.size; },
    clear: () => backing.clear(),
    getItem: () => { throw new Error("retired business snapshots must never be read"); },
    key: index => [...backing.keys()][index] ?? null,
    removeItem: key => { backing.delete(key); },
    setItem: (key, value) => { backing.set(key, String(value)); },
  };
  Object.defineProperty(globalThis, "localStorage", { value: storage, configurable: true });
  t.after(() => {
    if (original) Object.defineProperty(globalThis, "localStorage", original);
    else Reflect.deleteProperty(globalThis, "localStorage");
  });
  return backing;
}

function freshStores() {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
  return { accounts: useAccountsStore(), destinations: useDestinationsStore(), providers: useProvidersStore() };
}

test("single-resource cleanup discards only its exact legacy snapshot key", t => {
  const backing = installLocalStorage(t);
  backing.set("ocg.snapshot.v1:accounts", "old");
  backing.set("ocg.snapshot.v1:providers", "other");
  backing.set("ocg-theme", "dark");
  dropSnapshot("accounts");
  assert.deepEqual([...backing.entries()], [["ocg.snapshot.v1:providers", "other"], ["ocg-theme", "dark"]]);
});

test("startup cleanup removes the exact retired namespace and preserves all UI preferences", t => {
  const backing = installLocalStorage(t);
  for (const key of ["accounts", "providers", "destinations", "unknown-legacy-resource"]) backing.set(`ocg.snapshot.v1:${key}`, "not json");
  const preferences = new Map([
    ["ocg-theme", "dark"], ["ocg-locale", "zh-CN"], ["ocg-sidebar", "collapsed"],
    ["ocg.snapshot.v2:accounts", "unrelated-version"], ["ocg.snapshot.v1", "unrelated-key"],
  ]);
  for (const [key, value] of preferences) backing.set(key, value);
  dropAllSnapshots();
  assert.deepEqual(backing, preferences);
});

test("store creation discards legacy projections and starts with no complete business inventory", t => {
  const backing = installLocalStorage(t);
  backing.set("ocg.snapshot.v1:accounts", JSON.stringify({ v: 1, data: [{ id: "old-account" }] }));
  backing.set("ocg.snapshot.v1:destinations", JSON.stringify({ v: 1, data: { destinations: [{ id: "old-destination" }], credentials: [], cards: [] } }));
  backing.set("ocg.snapshot.v1:providers", JSON.stringify({ v: 1, data: { catalog: [{ provider_id: "old-provider" }], aliasUnpublished: ["old-model"] } }));
  const stores = freshStores();
  assert.equal(backing.size, 0);
  assert.deepEqual(stores.accounts.accounts, []);
  assert.equal(stores.accounts.loaded, false);
  assert.deepEqual(stores.destinations.destinations, []);
  assert.deepEqual(stores.destinations.cards, []);
  assert.equal(stores.destinations.loaded, false);
  assert.equal(stores.destinations.expectation, null);
  assert.equal(stores.providers.catalog, null);
  assert.equal(stores.providers.contracts, null);
  assert.equal(stores.providers.connections, null);
  assert.equal(stores.providers.aliasPublicationReady, false);
});

test("complete and sparse business commits remain in memory without writing browser storage", async t => {
  const backing = installLocalStorage(t);
  backing.set("ocg-theme", "dark");
  const stores = freshStores();
  stores.accounts.upsertDetailAccount({ id: "detail" } as Account);
  assert.equal(stores.accounts.loaded, false);
  stores.accounts.commitPresented([{ id: "full" } as Account]);
  stores.accounts.upsertAccount({ id: "full", name: "saved" } as Account);
  stores.destinations.commitReadSnapshot({
    destinations: [{ id: "destination" } as never], credentials: [], cards: [],
    expectation: { expectedRevision: 7, processGeneration: 99 },
  });
  stores.providers.commitReadProjection({ catalog: [], contracts: { providers: [], custom_endpoints: [], revision: 7, process_generation: 99 }, connections: [] });
  assert.equal(stores.accounts.loaded, true);
  assert.equal(stores.accounts.accounts[0]?.name, "saved");
  assert.equal(stores.destinations.loaded, true);
  assert.equal(stores.destinations.destinations[0]?.id, "destination");
  assert.deepEqual(stores.providers.catalog, []);
  // Cover the retired debounced writer as well as synchronous writes.
  await new Promise(resolve => setTimeout(resolve, 600));
  assert.deepEqual([...backing.entries()], [["ocg-theme", "dark"]]);
  const next = freshStores();
  assert.deepEqual(next.accounts.accounts, []);
  assert.equal(next.accounts.loaded, false);
  assert.deepEqual(next.destinations.destinations, []);
  assert.equal(next.destinations.loaded, false);
  assert.equal(next.providers.catalog, null);
});

test("teardown deletes legacy entries reintroduced after startup without touching preferences", t => {
  const backing = installLocalStorage(t);
  const stores = freshStores();
  for (const key of ["accounts", "destinations", "providers"]) backing.set(`ocg.snapshot.v1:${key}`, "stale-cache");
  backing.set("ocg-locale", "en");
  stores.accounts.clearAccounts();
  stores.destinations.clear();
  stores.providers.clear();
  assert.deepEqual([...backing.entries()], [["ocg-locale", "en"]]);
});

test("restricted browser storage does not block cleanup or memory-only store creation", t => {
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, get: () => { throw new Error("denied"); } });
  t.after(() => {
    if (original) Object.defineProperty(globalThis, "localStorage", original);
    else Reflect.deleteProperty(globalThis, "localStorage");
  });
  assert.doesNotThrow(() => { dropSnapshot("accounts"); dropAllSnapshots(); });
  const stores = freshStores();
  assert.equal(stores.accounts.loaded, false);
  assert.equal(stores.destinations.loaded, false);
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: {
    removeItem() { throw new Error("denied"); },
    get length() { throw new Error("denied"); },
  } });
  assert.doesNotThrow(() => { dropSnapshot("accounts"); dropAllSnapshots(); stores.accounts.clearAccounts(); });
});
