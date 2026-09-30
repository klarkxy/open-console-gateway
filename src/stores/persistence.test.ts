import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import {
  dropAllSnapshots,
  dropSnapshot,
  flushSnapshots,
  readSnapshot,
  writeSnapshot,
} from "./persistence.ts";
import { useAccountsStore } from "./accounts.ts";
import { useDestinationsStore } from "./destinations.ts";
import { useProvidersStore } from "./providers.ts";
import type { Account } from "../api/dashboard.ts";

function installLocalStorage(): Map<string, string> {
  const backing = new Map<string, string>();
  const storage: Storage = {
    get length() { return backing.size; },
    clear: () => backing.clear(),
    getItem: (key) => backing.get(key) ?? null,
    key: (index) => [...backing.keys()][index] ?? null,
    removeItem: (key) => { backing.delete(key); },
    setItem: (key, value) => { backing.set(key, String(value)); },
  };
  Object.defineProperty(globalThis, "localStorage", { value: storage, configurable: true });
  return backing;
}

function seed(key: string, data: unknown): void {
  globalThis.localStorage.setItem(`ocg.snapshot.v1:${key}`, JSON.stringify({ v: 1, data }));
}

test("a scheduled write reads back after flush, keeping the latest payload", () => {
  const backing = installLocalStorage();
  writeSnapshot("k", { n: 1 });
  writeSnapshot("k", { n: 2 });
  // Debounced: nothing is stored until the timer or a flush lands.
  assert.equal(backing.size, 0);
  flushSnapshots();
  assert.deepEqual(readSnapshot("k"), { n: 2 });
});

test("corrupt, wrong-version, and validator-rejected entries read as null", () => {
  installLocalStorage();
  globalThis.localStorage.setItem("ocg.snapshot.v1:bad", "not json");
  globalThis.localStorage.setItem("ocg.snapshot.v1:old", JSON.stringify({ v: 99, data: [1] }));
  seed("shape", { not: "a list" });
  assert.equal(readSnapshot("bad"), null);
  assert.equal(readSnapshot("old"), null);
  assert.equal(
    readSnapshot("shape", (data) => (Array.isArray(data) ? data : null)),
    null,
  );
});

test("dropSnapshot removes the stored entry and cancels its pending write", () => {
  const backing = installLocalStorage();
  seed("k", [1]);
  writeSnapshot("pending", [2]);
  dropSnapshot("k");
  dropSnapshot("pending");
  flushSnapshots();
  assert.equal(backing.size, 0);
});

test("dropAllSnapshots removes only ocg.snapshot keys", () => {
  const backing = installLocalStorage();
  seed("a", [1]);
  seed("b", [2]);
  globalThis.localStorage.setItem("ocg-theme", "dark");
  dropAllSnapshots();
  assert.deepEqual([...backing.keys()], ["ocg-theme"]);
});

test("accounts store hydrates from a persisted snapshot, then clears it", () => {
  installLocalStorage();
  const account = { id: "a1", name: "Cached" } as unknown as Account;
  seed("accounts", [account]);
  setActivePinia(createPinia());
  const store = useAccountsStore();
  assert.equal(store.accounts[0]?.id, "a1");
  assert.equal(store.loaded, true);
  store.clearAccounts();
  assert.equal(readSnapshot("accounts"), null);
  assert.equal(store.loaded, false);
});

test("accounts store persists setAccounts commits", () => {
  installLocalStorage();
  setActivePinia(createPinia());
  const store = useAccountsStore();
  store.setAccounts([{ id: "a2" } as unknown as Account]);
  flushSnapshots();
  assert.equal((readSnapshot("accounts") as Account[])[0]?.id, "a2");
});

test("destinations store persists a committed snapshot and hydrates it", () => {
  installLocalStorage();
  setActivePinia(createPinia());
  const expectation = { expectedRevision: 3, processGeneration: 9 };
  useDestinationsStore().commitSnapshot({
    destinations: [{ id: "d1" } as never],
    credentials: [{ id: "c1" } as never],
    cards: [{ id: "card1" } as never],
    expectation,
  });
  flushSnapshots();
  setActivePinia(createPinia());
  const hydrated = useDestinationsStore();
  assert.equal(hydrated.loaded, true);
  assert.equal(hydrated.destinations[0]?.id, "d1");
  assert.equal(hydrated.expectation?.expectedRevision, 3);
  hydrated.clear();
  assert.equal(readSnapshot("destinations"), null);
});

test("providers store hydrates the persisted projection and clear drops it", () => {
  installLocalStorage();
  seed("providers", {
    catalog: [{ provider_id: "p1" }],
    contracts: { revision: 5, process_generation: 9 },
    connections: [{ id: "conn1" }],
    aliasUnpublished: ["hidden-model"],
  });
  setActivePinia(createPinia());
  const store = useProvidersStore();
  assert.equal(store.catalog?.[0]?.provider_id, "p1");
  assert.equal(store.connections?.[0]?.id, "conn1");
  assert.deepEqual(store.aliasUnpublished, ["hidden-model"]);
  store.clear();
  assert.equal(readSnapshot("providers"), null);
  assert.equal(store.catalog, null);
});

test("a malformed providers snapshot hydrates as empty, never throws", () => {
  installLocalStorage();
  seed("providers", { catalog: "not-a-list" });
  setActivePinia(createPinia());
  const store = useProvidersStore();
  assert.equal(store.catalog, null);
});
