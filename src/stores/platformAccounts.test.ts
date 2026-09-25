import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useDestinationsStore } from "./destinations.ts";
import { usePlatformAccountsStore } from "./platformAccounts.ts";
import type { PlatformAccountsView, PlatformLink } from "../api/platform-accounts.ts";
import { presentDestination } from "../api/destinations.ts";
import type { DestinationDto } from "../api/generated/dashboard-v4.ts";

interface DeferredCall {
  url: string;
  method: string;
  body: unknown;
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
        body: typeof init.body === "string" ? JSON.parse(init.body) : null,
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

function parentDestination(name: string): DestinationDto {
  return {
    accountControls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
    adapter: "http",
    authScheme: "bearer",
    baseUrl: "https://example.test",
    brandFamily: null,
    capabilities: {
      billingTierRequired: false, discoverableModels: true, externalIntegration: false,
      identityHeaders: false, managedSignup: false, observer: true,
      officialBalanceProbe: [], redirectPolicy: "no_follow", testable: true,
    },
    catalog: [], enabled: true, id: "dest-parent", legacy: { kind: "platform_parent", id: "parent-1" },
    maxCredentials: null, modelResolution: "public_only", name,
    observerCredentialId: null, plan: null, protocols: ["chat_completions"], protocolRoutes: [],
  };
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
  destinations.commitSnapshot({ destinations: [], credentials: [], cards: [], expectation: { expectedRevision: 7, processGeneration: 99 } });
  const store = usePlatformAccountsStore();

  const pending = store.createOrUpdate({
    kind: "new_api",
    name: "New site",
    baseUrl: "https://new.example.test",
  }, null);
  await waitForCalls(calls, 1);
  calls[0]!.resolve(listBody("parent-new", 8, 99));
  await waitForCalls(calls, 2);
  calls[1]!.reject(new Error("destination refresh failed"));

  assert.equal(await pending, "saved_refresh_failed");
  assert.equal(store.parents[0]?.id, "parent-new", "the committed platform view is retained");
  assert.equal(store.destinationRefreshError, "destination refresh failed");
  assert.equal(destinations.loaded, true, "the prior destination snapshot stays rendered");
  assert.deepEqual(destinations.destinations, []);
  assert.deepEqual(destinations.credentials, []);
});

test("platform rename refreshes the destination revision before a layout write", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: "p1" });
  const calls = installDeferredFetch();
  const destinations = useDestinationsStore();
  destinations.commitSnapshot({
    destinations: [presentDestination(parentDestination("Site"))], credentials: [],
    cards: [{ id: "card-parent", destination_id: "dest-parent", credential_ids: [] }],
    expectation: { expectedRevision: 7, processGeneration: 99 },
  });
  const store = usePlatformAccountsStore();
  store.acceptView(platformView("parent-1", 7, 99));

  const save = store.createOrUpdate({ kind: "new_api", name: "Renamed", baseUrl: "https://example.test" }, store.parents[0]!);
  await waitForCalls(calls, 1);
  assert.equal(calls[0]!.method, "PUT");
  calls[0]!.resolve({
    ...platformView("parent-1", 8, 99),
    accounts: [{ ...platformView("parent-1", 8, 99).accounts[0]!, name: "Renamed" }],
  });
  await waitForCalls(calls, 2);
  assert.ok(calls[1]!.url.endsWith("/routing/cards"));
  assert.equal(destinations.expectation?.expectedRevision, 7, "the old revision is not paired with an optimistic rename");
  calls[1]!.resolve({
    destinations: [parentDestination("Renamed")], credentials: [],
    cards: [{ id: "card-parent", destinationId: "dest-parent", credentialIds: [] }],
    revision: { revision: 8, processGeneration: 99, pricingRevision: "p1" },
  });
  assert.equal(await save, "saved");
  assert.equal(destinations.destinations[0]?.name, "Renamed");
  assert.deepEqual(destinations.expectation, { expectedRevision: 8, processGeneration: 99 });

  const layout = [{ id: "card-parent", destinationId: "dest-parent", credentialIds: [] }];
  const reorder = destinations.replaceRoutingCardLayout(layout, destinations.expectation!);
  await waitForCalls(calls, 3);
  assert.deepEqual(calls[2]!.body, { cards: layout, expectedRevision: 8, processGeneration: 99 });
  calls[2]!.resolve({
    destinations: [parentDestination("Renamed")], credentials: [],
    cards: [{ id: "card-parent", destinationId: "dest-parent", credentialIds: [] }],
    revision: { revision: 9, processGeneration: 99, pricingRevision: "p1" },
  });
  await reorder;
});

test("platform edit reports projection refresh failure and retains its last coherent snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: "p1" });
  const calls = installDeferredFetch();
  const destinations = useDestinationsStore();
  destinations.commitSnapshot({
    destinations: [presentDestination(parentDestination("Site"))], credentials: [], cards: [],
    expectation: { expectedRevision: 7, processGeneration: 99 },
  });
  const store = usePlatformAccountsStore();
  store.acceptView(platformView("parent-1", 7, 99));

  const save = store.createOrUpdate({ kind: "new_api", name: "Renamed", baseUrl: "https://example.test" }, store.parents[0]!);
  await waitForCalls(calls, 1);
  calls[0]!.resolve(platformView("parent-1", 8, 99));
  await waitForCalls(calls, 2);
  calls[1]!.reject(new Error("projection unavailable"));

  assert.equal(await save, "saved_refresh_failed");
  assert.equal(store.destinationRefreshError, "projection unavailable");
  assert.equal(destinations.destinations[0]?.name, "Site");
  assert.deepEqual(destinations.expectation, { expectedRevision: 7, processGeneration: 99 });
});


test("late platform child refresh cannot repopulate a logged-out store", async () => {
  setActivePinia(createPinia());
  const calls = installDeferredFetch();
  useControlPlaneStore().sync({ revision: 1, processGeneration: 1 });
  const store = usePlatformAccountsStore();
  store.acceptView(platformView("parent", 1, 1));
  const pending = store.refreshChild("parent", "child");
  await waitForCalls(calls, 1);
  store.clear();
  calls[0]!.resolve(platformView("parent", 2, 1));
  assert.equal(await pending, "error");
  assert.equal(store.view, null);
  assert.deepEqual(store.refreshing, {});
});
