import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import type {
  DestinationCredentialDto,
  DestinationDto,
} from "../api/generated/dashboard-v4.ts";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useDestinationsStore } from "./destinations.ts";

interface DeferredCall {
  url: string;
  method: string;
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

function destinationDto(id: string, legacyId: string): DestinationDto {
  return {
    adapter: "http",
    authScheme: "bearer",
    baseUrl: "https://lab.example/v1",
    brandFamily: null,
    capabilities: {
      billingTierRequired: false,
      discoverableModels: true,
      externalIntegration: false,
      identityHeaders: false,
      managedSignup: false,
      observer: false,
      officialBalanceProbe: [],
      redirectPolicy: "no_follow",
      testable: true,
    },
    catalog: [],
    enabled: true,
    id,
    legacy: { kind: "custom_account", id: legacyId },
    maxCredentials: 1,
    name: id,
    observerCredentialId: null,
    plan: null,
    protocols: ["chat_completions"],
  };
}

function credentialDto(
  id: string,
  destinationId: string,
  legacyAccountId: string,
  routingRank: number,
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
    name: id,
    notes: null,
    onboardingTask: null,
    purchaseDate: null,
    quotaPoolId: null,
    routingRank,
    scope: { kind: "all" },
  };
}

function destListBody(id: string, legacyId: string, revision: number): object {
  return {
    destinations: [destinationDto(id, legacyId)],
    revision: { revision, processGeneration: 99, pricingRevision: "p1" },
  };
}

function credListBody(
  destId: string,
  accountId: string,
  revision: number,
): object {
  return {
    credentials: [credentialDto(`cred-${accountId}`, destId, accountId, 0)],
    revision: { revision, processGeneration: 99, pricingRevision: "p1" },
  };
}

function resolvePair(
  calls: DeferredCall[],
  start: number,
  destId: string,
  accountId: string,
  revision: number,
): void {
  const slice = calls.slice(start, start + 2);
  const dest = slice.find((call) => call.url.endsWith("/destinations"));
  const cred = slice.find((call) => call.url.endsWith("/credentials"));
  assert.ok(dest && cred, "expected destination and credential fetches");
  dest.resolve(destListBody(destId, accountId, revision));
  cred.resolve(credListBody(destId, accountId, revision));
}

test("destinations store: a stale slower load does not clobber a newer one", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.load();
  const second = store.load();
  await waitForCalls(calls, 4);

  resolvePair(calls, 2, "dest-b", "acc-b", 8);
  await second;
  assert.equal(store.destinations[0]?.id, "dest-b");
  assert.equal(store.credentialsByLegacyAccountId.get("acc-b")?.destination_id, "dest-b");
  assert.deepEqual(store.expectation, { expectedRevision: 8, processGeneration: 99 });
  assert.equal(store.loading, false);

  resolvePair(calls, 0, "dest-a", "acc-a", 7);
  await first;
  assert.equal(store.destinations[0]?.id, "dest-b");
  assert.equal(store.byId.get("dest-a"), undefined);
  assert.equal(store.credentialsByLegacyAccountId.get("acc-a"), undefined);
  assert.equal(store.error, "");
});

test("destinations store: a 409 refusal populates refusals and keeps the previous lists", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.load();
  await waitForCalls(calls, 2);
  resolvePair(calls, 0, "dest-ok", "acc-ok", 4);
  await first;
  assert.equal(store.destinations[0]?.id, "dest-ok");
  assert.equal(store.credentials.length, 1);

  const second = store.load();
  await waitForCalls(calls, 4);
  const dest = calls.slice(2).find((call) => call.url.endsWith("/destinations"));
  const cred = calls.slice(2).find((call) => call.url.endsWith("/credentials"));
  assert.ok(dest && cred);
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
  cred.resolve(credListBody("dest-ok", "acc-ok", 5));

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
  await waitForCalls(calls, 2);
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
