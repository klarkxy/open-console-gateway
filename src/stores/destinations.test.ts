import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import { dashboardApi } from "../api/dashboard.ts";
import {
  destinationsApi,
  presentDestination,
  presentDestinationCredential,
  type RoutingCardListSnapshot,
} from "../api/destinations.ts";
import type {
  DestinationCredentialDto,
  DestinationDto,
} from "../api/generated/dashboard-v4.ts";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useDestinationsStore } from "./destinations.ts";
import { useAccountsStore } from "./accounts.ts";
import { dropAllSnapshots } from "./persistence.ts";

interface DeferredCall {
  url: string;
  method: string;
  body: unknown;
  resolve: (body: object, status?: number) => void;
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
        resolve: (body, status = 200) => resolvePromise(new Response(
          JSON.stringify(body),
          { status, headers: { "Content-Type": "application/json" } },
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

function destinationDto(
  id: string,
  legacyId: string,
  extra: {
    name?: string;
    baseUrl?: string;
    legacyKind?: DestinationDto["legacy"]["kind"];
    observer?: boolean;
  } = {},
): DestinationDto {
  return {
    presentation: null,
    accountControls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
    adapter: "http",
    authScheme: "bearer",
    baseUrl: extra.baseUrl ?? "https://lab.example/v1",
    brandFamily: null,
    capabilities: {
      billingTierRequired: false,
      discoverableModels: true,
      externalIntegration: false,
      identityHeaders: false,
      managedSignup: false,
      observer: extra.observer ?? false,
      officialBalanceProbe: [],
      redirectPolicy: "no_follow",
      testable: true,
    },
    catalog: [],
    enabled: true,
    id,
    legacy: { kind: extra.legacyKind ?? "custom_account", id: legacyId },
    maxCredentials: extra.legacyKind === "platform_parent" ? null : 1,
    modelResolution: "public_only",
    name: extra.name ?? id,
    observerCredentialId: null,
    plan: null,
    protocols: ["chat_completions"],
    protocolRoutes: [],
  };
}

function credentialDto(
  id: string,
  destinationId: string,
  legacyAccountId: string,
  routingRank: number,
  name = id,
): DestinationCredentialDto {
  return {
    authState: "unknown",
    cooldowns: {
      fiveHourUntil: null,
      freeUntil: null,
      genericUntil: null,
      monthUntil: null,
      weekUntil: null,
    },
    destinationId,
    enabled: true,
    grants: { allowedEndpointIds: [], allowedOrigins: [] },
    hasSecret: true,
    id,
    lastError: null,
    legacyAccountId,
    name,
    notes: null,
    onboardingTask: null,
    purchaseDate: null,
    quotaPoolId: null,
    routingRank,
    scope: { kind: "all" },
  };
}

function destListBody(
  id: string,
  legacyId: string,
  revision: number,
  extra: Parameters<typeof destinationDto>[2] = {},
): object {
  return {
    destinations: [destinationDto(id, legacyId, extra)],
    revision: { revision, processGeneration: 99, pricingRevision: "p1" },
  };
}

function credListBody(
  destId: string,
  accountId: string,
  revision: number,
  name?: string,
): object {
  return {
    credentials: [credentialDto(`cred-${accountId}`, destId, accountId, 0, name ?? accountId)],
    revision: { revision, processGeneration: 99, pricingRevision: "p1" },
  };
}

function resolvePair(calls: DeferredCall[], start: number, destId: string, accountId: string, revision: number): void {
  resolvePairWith(calls, start, destListBody(destId, accountId, revision), credListBody(destId, accountId, revision));
}
function resolvePairWith(calls: DeferredCall[], start: number, destBody: object, credBody: object): void {
  const dest = destBody as { destinations: DestinationDto[]; revision: object };
  const cred = credBody as { credentials: DestinationCredentialDto[] };
  assert.ok(calls[start].url.endsWith("/routing/cards"));
  calls[start].resolve({ ...dest, ...cred, cards: dest.destinations.map(row => ({ id: `card-${row.id}`, destinationId: row.id,
    credentialIds: cred.credentials.filter(c => c.destinationId === row.id && c.id !== row.observerCredentialId).map(c => c.id) })) });
}

test("destinations store: a stale slower load does not clobber a newer one", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.load();
  store.invalidateReads();
  const second = store.load();
  await waitForCalls(calls, 2);

  resolvePair(calls, 1, "dest-b", "acc-b", 8);
  await second;
  assert.equal(store.destinations[0]?.id, "dest-b");
  assert.equal(store.credentialsByLegacyAccountId.get("acc-b")?.destination_id, "dest-b");
  assert.equal(store.destinationForAccount("acc-b")?.id, "dest-b");
  assert.equal(store.destinationForAccount("absent"), null);
  assert.deepEqual(store.expectation, { expectedRevision: 8, processGeneration: 99 });
  assert.equal(store.loading, false);

  resolvePair(calls, 0, "dest-a", "acc-a", 7);
  await first;
  assert.equal(store.destinations[0]?.id, "dest-b");
  assert.equal(store.byId.get("dest-a"), undefined);
  assert.equal(store.credentialsByLegacyAccountId.get("acc-a"), undefined);
  assert.equal(store.destinationForAccount("acc-a"), null);
  assert.equal(store.destinationForAccount("acc-b")?.id, "dest-b");
  assert.equal(store.error, "");
});

test("destinations store: a 409 refusal populates refusals and keeps the previous lists", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.load();
  await waitForCalls(calls, 1);
  resolvePair(calls, 0, "dest-ok", "acc-ok", 4);
  await first;
  assert.equal(store.destinations[0]?.id, "dest-ok");
  assert.equal(store.credentials.length, 1);

  const second = store.load();
  await waitForCalls(calls, 2);
  const dest = calls[1];
  dest.resolve({
    code: "destinationProjectionRefused",
    message: "projection refused",
    currentRevision: 5,
    processGeneration: 99,
    details: [{
      row: { kind: "account", id: "acc-bad", providerId: "custom" },
      error: "customAccountMissingEndpoint",
      detail: "missing custom_config",
    }],
  }, 409);

  await assert.rejects(second, (error: unknown) => {
    assert.ok(error instanceof DashboardRequestError);
    assert.equal(error.status, 409);
    assert.equal(error.code, "destinationProjectionRefused");
    return true;
  });

  assert.equal(store.destinations[0]?.id, "dest-ok");
  assert.equal(store.credentials[0]?.legacy_account_id, "acc-ok");
  assert.equal(store.loaded, true);
  assert.equal(store.error, "projection refused");
  assert.equal(store.refusals.length, 1);
  assert.deepEqual(store.refusals[0], {
    kind: "account",
    id: "acc-bad",
    providerId: "custom",
    error: "customAccountMissingEndpoint",
    detail: "missing custom_config",
  });
});

test("destinations store: clear() empties state and loaded is false", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const pending = store.load();
  await waitForCalls(calls, 1);
  resolvePair(calls, 0, "dest-ok", "acc-ok", 3);
  await pending;
  assert.equal(store.loaded, true);
  assert.equal(store.destinations.length, 1);

  store.clear();
  assert.deepEqual(store.destinations, []);
  assert.deepEqual(store.credentials, []);
  assert.equal(store.expectation, null);
  assert.equal(store.loaded, false);
  assert.equal(store.loading, false);
  assert.equal(store.error, "");
  assert.deepEqual(store.refusals, []);
  assert.equal(store.byId.size, 0);
  assert.equal(store.credentialsByLegacyAccountId.size, 0);
});

test("destinations store: first ordinary load failure records error and loaded stays false", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const pending = store.load();
  await waitForCalls(calls, 1);
  calls[0]!.reject(new Error("network down"));
  await assert.rejects(pending, /network down/);

  assert.equal(store.loaded, false);
  assert.equal(store.error, "network down");
  assert.equal(store.destinations.length, 0);
  assert.equal(store.credentials.length, 0);
  assert.deepEqual(store.refusals, []);
});

test("destinations store: revalidation ordinary failure keeps the successful snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.load();
  await waitForCalls(calls, 1);
  resolvePair(calls, 0, "dest-ok", "acc-ok", 4);
  await first;
  assert.equal(store.loaded, true);
  assert.equal(store.destinations[0]?.name, "dest-ok");

  const second = store.load();
  await waitForCalls(calls, 2);
  calls[1]!.reject(new Error("revalidation failed"));
  await assert.rejects(second, /revalidation failed/);

  assert.equal(store.loaded, true);
  assert.equal(store.destinations[0]?.id, "dest-ok");
  assert.equal(store.credentials[0]?.legacy_account_id, "acc-ok");
  assert.equal(store.error, "");
});

test("destinations store: refreshAfterMutation updates credential name after a rename", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.load();
  await waitForCalls(calls, 1);
  resolvePairWith(
    calls,
    0,
    destListBody("dest-go", "acc-go", 4, { name: "Old Name" }),
    credListBody("dest-go", "acc-go", 4, "Old Name"),
  );
  await first;
  assert.equal(store.credentials[0]?.name, "Old Name");

  const refreshed = store.refreshAfterMutation();
  await waitForCalls(calls, 2);
  resolvePairWith(
    calls,
    1,
    destListBody("dest-go", "acc-go", 5, { name: "Renamed" }),
    credListBody("dest-go", "acc-go", 5, "Renamed"),
  );
  await refreshed;
  assert.equal(store.credentials[0]?.name, "Renamed");
  assert.equal(store.destinations[0]?.name, "Renamed");
});

test("destinations store: refreshAfterMutation updates custom endpoint and name", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.load();
  await waitForCalls(calls, 1);
  resolvePairWith(
    calls,
    0,
    destListBody("dest-custom", "acc-custom", 4, {
      name: "Custom",
      baseUrl: "https://old.example/v1",
    }),
    credListBody("dest-custom", "acc-custom", 4, "Custom"),
  );
  await first;
  assert.equal(store.destinations[0]?.base_url, "https://old.example/v1");

  const refreshed = store.refreshAfterMutation();
  await waitForCalls(calls, 2);
  resolvePairWith(
    calls,
    1,
    destListBody("dest-custom", "acc-custom", 5, {
      name: "Custom Lab",
      baseUrl: "https://new.example/v1",
    }),
    credListBody("dest-custom", "acc-custom", 5, "Custom Lab"),
  );
  await refreshed;
  assert.equal(store.destinations[0]?.name, "Custom Lab");
  assert.equal(store.destinations[0]?.base_url, "https://new.example/v1");
  assert.equal(store.credentials[0]?.name, "Custom Lab");
});

test("destinations store: refreshAfterMutation adds a newly created platform destination", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.load();
  await waitForCalls(calls, 1);
  resolvePairWith(
    calls,
    0,
    {
      destinations: [],
      revision: { revision: 4, processGeneration: 99, pricingRevision: "p1" },
    },
    {
      credentials: [],
      revision: { revision: 4, processGeneration: 99, pricingRevision: "p1" },
    },
  );
  await first;
  assert.equal(store.destinations.length, 0);

  const refreshed = store.refreshAfterMutation();
  await waitForCalls(calls, 2);
  resolvePairWith(
    calls,
    1,
    destListBody("dest-plat", "plat-1", 5, {
      name: "Site",
      baseUrl: "https://newapi.example",
      legacyKind: "platform_parent",
      observer: true,
    }),
    {
      credentials: [],
      revision: { revision: 5, processGeneration: 99, pricingRevision: "p1" },
    },
  );
  await refreshed;
  assert.equal(store.destinations.length, 1);
  assert.equal(store.destinations[0]?.legacy.kind, "platform_parent");
  assert.equal(store.destinations[0]?.name, "Site");
  assert.equal(store.byId.has("dest-plat"), true);
});


test("destination cards and pending loads are cleared on logout", async () => {
  setActivePinia(createPinia()); useControlPlaneStore();
  const calls = installDeferredFetch(); const store = useDestinationsStore();
  const pending = store.load(); await waitForCalls(calls, 1); store.clear();
  resolvePair(calls, 0, "old-dest", "old-account", 2); await pending;
  assert.equal(store.loaded, false); assert.deepEqual(store.cards, []); assert.deepEqual(store.destinations, []);
});

test("account toggle refreshes the projected Key and revision before a layout write", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const initial = store.load();
  await waitForCalls(calls, 1);
  resolvePair(calls, 0, "dest-a", "acc-a", 7);
  await initial;

  const toggle = dashboardApi.toggleAccount("acc-a");
  await waitForCalls(calls, 2);
  assert.ok(calls[1]!.url.endsWith("/accounts/acc-a/toggle"));
  calls[1]!.resolve({
    account: { id: "acc-a", name: "Key", enabled: false, customConfig: null, modelCapabilities: [] },
    revision: { revision: 8, processGeneration: 99, pricingRevision: "p1" },
  });
  assert.equal((await toggle).enabled, false);

  const refresh = store.refreshAfterMutation();
  await waitForCalls(calls, 3);
  const credential = credentialDto("cred-acc-a", "dest-a", "acc-a", 0);
  resolvePairWith(calls, 2, destListBody("dest-a", "acc-a", 8), {
    credentials: [{ ...credential, enabled: false }],
    revision: { revision: 8, processGeneration: 99, pricingRevision: "p1" },
  });
  await refresh;
  assert.equal(store.credentialsByLegacyAccountId.get("acc-a")?.enabled, false);
  assert.deepEqual(store.expectation, { expectedRevision: 8, processGeneration: 99 });

  const layout = store.cards.map((card) => ({
    id: card.id, destinationId: card.destination_id, credentialIds: [...card.credential_ids],
  }));
  const save = store.replaceRoutingCardLayout(layout, store.expectation!);
  await waitForCalls(calls, 4);
  assert.deepEqual(calls[3]!.body, { cards: layout, expectedRevision: 8, processGeneration: 99 });
  resolvePairWith(calls, 3, destListBody("dest-a", "acc-a", 9), {
    credentials: [{ ...credential, enabled: false }],
    revision: { revision: 9, processGeneration: 99, pricingRevision: "p1" },
  });
  await save;
});

function detailFixture(): RoutingCardListSnapshot {
  return {
    destinations: ["a", "b"].map(id => presentDestination(destinationDto(`dest-${id}`, `acc-${id}`))),
    credentials: ["a", "b"].map(id => presentDestinationCredential(credentialDto(`cred-${id}`, `dest-${id}`, `acc-${id}`, 0))),
    cards: ["a", "b"].map(id => ({ id: `card-${id}`, destination_id: `dest-${id}`, credential_ids: [`cred-${id}`] })),
    expectation: { expectedRevision: 7, processGeneration: 99 },
  };
}

test("sparse destination detail merges siblings and preserves complete inventory and cards", () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
  const store = useDestinationsStore();
  store.commitReadSnapshot(detailFixture());
  const cards = store.cards;
  store.upsertDetailProjection({
    destinations: [presentDestination(destinationDto("dest-a", "acc-a", { name: "edited" }))],
    credentials: [presentDestinationCredential(credentialDto("cred-a", "dest-a", "acc-a", 0, "edited-key"))],
    expectation: { expectedRevision: 8, processGeneration: 99 },
  });
  assert.equal(store.loaded, true);
  assert.deepEqual(store.destinations.map(row => [row.id, row.name]), [["dest-a", "edited"], ["dest-b", "dest-b"]]);
  assert.deepEqual(store.credentials.map(row => [row.id, row.name]), [["cred-a", "edited-key"], ["cred-b", "cred-b"]]);
  assert.equal(store.cards, cards);
  assert.deepEqual(store.expectation, { expectedRevision: 8, processGeneration: 99 });
  store.clear();
});

test("sparse destination detail detaches pending inventory reads and remains incomplete until a full read", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const pending = store.load();
  await waitForCalls(calls, 1);
  store.upsertDetailProjection({
    destinations: [presentDestination(destinationDto("dest-a", "acc-a"))],
    expectation: { expectedRevision: 8, processGeneration: 99 },
  });
  assert.equal(store.loaded, false);
  assert.equal(store.loading, false);
  resolvePair(calls, 0, "old", "old", 7);
  await pending;
  assert.deepEqual(store.destinations.map(row => row.id), ["dest-a"]);
  const inventory = store.load({ maxAgeMs: 15_000 });
  await waitForCalls(calls, 2);
  resolvePair(calls, 1, "dest-b", "acc-b", 9);
  await inventory;
  assert.equal(store.loaded, true);
  assert.deepEqual(store.destinations.map(row => row.id), ["dest-b"]);
  store.clear();
});

test("sparse destination detail excludes credentials for confirmed removed accounts", () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
  const store = useDestinationsStore();
  useAccountsStore().removeAccount("acc-a");
  store.upsertDetailProjection({ credentials: detailFixture().credentials, expectation: detailFixture().expectation });
  assert.deepEqual(store.credentials.map(row => row.id), ["cred-b"]);
  assert.equal(store.loaded, false);
  store.clear();
  useAccountsStore().clearAccounts();
});

test("sparse destination detail respects backend and revision fences while a mutation is pending", async () => {
  setActivePinia(createPinia());
  const controlPlane = useControlPlaneStore();
  controlPlane.sync({ revision: 7, processGeneration: 99 });
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  store.commitReadSnapshot(detailFixture());
  const pending = store.patchDestination("dest-a", {
    name: "pending", authScheme: "bearer", endpointUrl: "https://lab.example/v1", models: [], upstreamProtocol: "chat_completions",
  });
  await waitForCalls(calls, 1);
  const detail = presentDestination(destinationDto("dest-a", "acc-a", { name: "new-detail" }));
  store.upsertDetailProjection({ destinations: [detail], expectation: { expectedRevision: 9, processGeneration: 99 } });
  calls[0]!.resolve({
    destination: destinationDto("dest-a", "acc-a", { name: "old-mutation" }),
    credentials: [], revision: { revision: 8, processGeneration: 99 },
  });
  await pending;
  assert.equal(store.byId.get("dest-a")?.name, "new-detail");
  store.upsertDetailProjection({ destinations: [{ ...detail, name: "old-detail" }], expectation: { expectedRevision: 8, processGeneration: 99 } });
  assert.equal(store.byId.get("dest-a")?.name, "new-detail");
  controlPlane.sync({ revision: 1, processGeneration: 100 });
  store.upsertDetailProjection({ destinations: [{ ...detail, name: "old-backend" }], expectation: { expectedRevision: 10, processGeneration: 99 } });
  store.upsertDetailProjection({ destinations: [{ ...detail, name: "unbound" }] });
  assert.equal(store.byId.get("dest-a")?.name, "new-detail");
  store.upsertDetailProjection({ destinations: [{ ...detail, name: "current-backend" }], expectation: { expectedRevision: 1, processGeneration: 100 } });
  assert.equal(store.byId.get("dest-a")?.name, "current-backend");
  store.clear();
});

test("sparse destination detail, full reads and subsequent mutations stay memory-only", async t => {
  dropAllSnapshots();
  const original = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  const backing = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: {
    getItem: (key: string) => backing.get(key) ?? null,
    setItem: (key: string, value: string) => backing.set(key, value),
    removeItem: (key: string) => backing.delete(key),
    get length() { return backing.size; },
    key: (index: number) => [...backing.keys()][index] ?? null,
  } });
  t.after(() => {
    dropAllSnapshots();
    if (original) Object.defineProperty(globalThis, "localStorage", original);
    else Reflect.deleteProperty(globalThis, "localStorage");
  });
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
  const store = useDestinationsStore();
  const detail = presentDestination(destinationDto("dest-a", "acc-a", { name: "edited" }));
  store.upsertDetailProjection({ destinations: [detail], expectation: { expectedRevision: 8, processGeneration: 99 } });
  assert.equal(backing.has("ocg.snapshot.v1:destinations"), false);
  assert.equal(store.loaded, false);
  t.mock.method(destinationsApi, "patch", async () => ({
    destination: detail,
    credentials: detailFixture().credentials,
    expectation: { expectedRevision: 9, processGeneration: 99 },
  }));
  await store.patchDestination("dest-a", {
    name: "edited", authScheme: "bearer", endpointUrl: "https://lab.example/v1", models: [], upstreamProtocol: "chat_completions",
  });
  assert.equal(backing.has("ocg.snapshot.v1:destinations"), false);
  assert.equal(store.loaded, false);
  store.commitReadSnapshot(detailFixture());
  store.upsertDetailProjection({ destinations: [detail], expectation: { expectedRevision: 8, processGeneration: 99 } });
  assert.equal(backing.has("ocg.snapshot.v1:destinations"), false);
  assert.deepEqual(store.destinations.map(row => [row.id, row.name]), [["dest-a", "edited"], ["dest-b", "dest-b"]]);
  assert.deepEqual(store.cards, detailFixture().cards);
  store.clear();
});

test("a full destination read cannot commit below an independently advanced same-process revision", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99 });
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  store.commitReadSnapshot(detailFixture());
  const pending = store.load();
  const operation = dashboardApi.getAccountsSnapshot();
  await waitForCalls(calls, 2);
  calls[1]!.resolve({ accounts: [], revision: 9, processGeneration: 99 });
  await operation;
  resolvePair(calls, 0, "stale", "stale", 8);
  await pending;
  assert.deepEqual(store.destinations.map(row => row.id), ["dest-a", "dest-b"]);
  assert.equal(store.expectation?.expectedRevision, 7);
  assert.equal(useControlPlaneStore().revision, 9);
  const recovery = store.load({ maxAgeMs: 15_000 });
  await waitForCalls(calls, 3);
  resolvePair(calls, 2, "current", "current", 9);
  await recovery;
  assert.deepEqual(store.destinations.map(row => row.id), ["current"]);
  store.clear();
});
