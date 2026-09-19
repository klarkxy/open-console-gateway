import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useDestinationsStore } from "./destinations.ts";
import { usePlatformAccountsStore } from "./platformAccounts.ts";
import type { PlatformAccountsView, PlatformLink } from "../api/platform-accounts.ts";

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

function platformLink(accountId: string, parentId: string): PlatformLink {
  return {
    accountId,
    platformAccountId: parentId,
    group: { id: null, platform: null, subscriptionType: null, autoGroups: [], verified: false },
    snapshot: null,
  };
}

function platformView(
  accountId: string,
  revision: number,
  processGeneration: number,
  links: PlatformLink[] = [],
): PlatformAccountsView {
  return {
    accounts: [{
      id: accountId,
      kind: "new_api",
      name: "Site",
      baseUrl: "https://example.test",
      hasUserCredential: true,
      version: 1,
      snapshot: null,
    }],
    links,
    revision,
    processGeneration,
  };
}

function listBody(
  accountId: string,
  revision: number,
  processGeneration: number,
  links: PlatformLink[] = [],
): object {
  return platformView(accountId, revision, processGeneration, links);
}

test("platform accounts store: a stale older-revision snapshot from the same process generation is rejected while a different generation is adopted", () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = usePlatformAccountsStore();

  store.acceptView(platformView("parent-new", 5, 1));
  store.acceptView(platformView("parent-old", 4, 1));
  assert.equal(store.view?.revision, 5);
  assert.equal(store.view?.processGeneration, 1);
  assert.equal(store.parents[0]?.id, "parent-new");

  store.acceptView(platformView("parent-other-gen", 1, 2));
  assert.equal(store.view?.revision, 1);
  assert.equal(store.view?.processGeneration, 2);
  assert.equal(store.parents[0]?.id, "parent-other-gen");
});

test("platform accounts store: an accepted snapshot supersedes an in-flight load", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = usePlatformAccountsStore();

  const pending = store.load();
  await waitForCalls(calls, 1);

  store.acceptView(platformView("accepted", 10, 1));
  assert.equal(store.parents[0]?.id, "accepted");
  assert.equal(store.loading, false);

  calls[0]!.resolve(listBody("slow-load", 99, 1));
  const stale = await pending;
  assert.equal(stale.accounts[0]?.id, "slow-load");
  assert.equal(store.parents[0]?.id, "accepted");
  assert.equal(store.view?.revision, 10);
  assert.equal(store.error, "");
});

test("platform accounts store: clear() empties state and loaded is false", () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = usePlatformAccountsStore();

  store.setPendingLink({ accountId: "acc-1", parentId: "parent-1" });
  store.acceptView(platformView("parent-1", 3, 1, [platformLink("acc-2", "parent-1")]));
  assert.equal(store.loaded, true);
  assert.equal(store.parents.length, 1);
  assert.equal(store.links.length, 1);

  store.clear();
  assert.equal(store.view, null);
  assert.equal(store.parents.length, 0);
  assert.equal(store.links.length, 0);
  assert.equal(store.loaded, false);
  assert.equal(store.loading, false);
  assert.equal(store.error, "");
  assert.equal(store.pendingLink, null);
  assert.equal(store.mutating, false);
});

test("platform accounts store: an accepted view containing the pending account's link clears pendingLink", () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const store = usePlatformAccountsStore();

  store.setPendingLink({ accountId: "acc-1", parentId: "parent-1" });
  store.acceptView(platformView("parent-other", 2, 1));
  assert.equal(store.pendingLink?.accountId, "acc-1");

  store.acceptView(platformView("parent-1", 3, 1, [platformLink("acc-1", "parent-1")]));
  assert.equal(store.pendingLink, null);
  assert.equal(store.linkForAccount("acc-1")?.platformAccountId, "parent-1");
});

test("platform accounts store: a committed create reports a destination refresh failure without losing either snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const destinations = useDestinationsStore();
  destinations.commitSnapshot([], [], { expectedRevision: 7, processGeneration: 99 });
  const store = usePlatformAccountsStore();

  const pending = store.createOrUpdate({
    kind: "new_api",
    name: "New site",
    baseUrl: "https://new.example.test",
  }, null);
  await waitForCalls(calls, 1);
  calls[0]!.resolve(listBody("parent-new", 8, 99));
  await waitForCalls(calls, 3);
  calls[1]!.reject(new Error("destination refresh failed"));
  calls[2]!.resolve({
    credentials: [],
    revision: { revision: 8, processGeneration: 99 },
  });

  assert.equal(await pending, "saved_refresh_failed");
  assert.equal(store.parents[0]?.id, "parent-new", "the committed platform view is retained");
  assert.equal(store.destinationRefreshError, "destination refresh failed");
  assert.equal(destinations.loaded, true, "the prior destination snapshot stays rendered");
  assert.deepEqual(destinations.destinations, []);
  assert.deepEqual(destinations.credentials, []);
});
