import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import type {
  DestinationCredentialDto,
  DestinationDto,
  RoutingExplanation,
} from "../api/generated/dashboard-v4.ts";
import type { QuotaRecoveryDto } from "../api/dashboard-v4.ts";
import type { DestinationPatchInput } from "../api/destinations.ts";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useDestinationsStore } from "./destinations.ts";

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
      let body: unknown = null;
      if (typeof init.body === "string") {
        try {
          body = JSON.parse(init.body);
        } catch {
          body = init.body;
        }
      }
      calls.push({
        url: String(input),
        method: init.method ?? "GET",
        body,
        resolve: (payload, status = 200) => resolvePromise(new Response(
          JSON.stringify(payload),
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

function destinationDto(id: string, name = id): DestinationDto {
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
    catalog: [{
      enabled: true,
      preferred: "chat_completions",
      protocols: ["chat_completions"],
      publicModel: "lab-opus",
      upstreamModel: "vendor/opus",
      upstreamOverride: null,
    }],
    enabled: true,
    id,
    legacy: { kind: "custom_account", id: `acct-${id}` },
    maxCredentials: null,
    modelResolution: "public_only",
    name,
    observerCredentialId: null,
    plan: null,
    protocols: ["chat_completions"],
  };
}

function credentialDto(id: string, destinationId: string): DestinationCredentialDto {
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
    grants: { allowedEndpointIds: [], allowedOrigins: ["https://lab.example"] },
    hasSecret: true,
    id,
    lastError: null,
    legacyAccountId: `acct-${destinationId}`,
    name: id,
    notes: null,
    onboardingTask: null,
    purchaseDate: null,
    quotaPoolId: null,
    quotaRecovery: null,
    routingRank: 1,
    scope: { kind: "all" },
  };
}

function revisionBody(revision: number, processGeneration = 99): object {
  return { revision, processGeneration, pricingRevision: "p1" };
}

function snapshotBodies(
  destId: string,
  revision: number,
  name?: string,
  processGeneration = 99,
): [object, object] {
  return [
    { destinations: [destinationDto(destId, name)], revision: revisionBody(revision, processGeneration) },
    { credentials: [credentialDto(`cred-${destId}`, destId)], revision: revisionBody(revision, processGeneration) },
  ];
}

function resolvePair(
  calls: DeferredCall[],
  start: number,
  destId: string,
  revision: number,
  processGeneration = 99,
): void {
  const [dest, cred] = snapshotBodies(destId, revision, undefined, processGeneration);
  assert.ok(calls[start].url.endsWith("/routing/cards"));
  calls[start].resolve({ ...dest, ...cred, cards: [{ id: `card-${destId}`, destinationId: destId, credentialIds: [`cred-${destId}`] }] });
}

function patchInput(): DestinationPatchInput {
  return {
    authScheme: "bearer",
    endpointUrl: "https://lab.example/v1",
    models: [{ publicModel: "lab-opus", upstreamModel: "vendor/opus", upstreamOverride: null }],
    name: "Renamed",
    upstreamProtocol: "chat_completions",
  };
}

async function loadedStore(calls: DeferredCall[]): Promise<ReturnType<typeof useDestinationsStore>> {
  const store = useDestinationsStore();
  const load = store.load();
  await waitForCalls(calls, 1);
  resolvePair(calls, 0, "dest-1", 4);
  await load;
  return store;
}

test("patch commits the returned destination in place with the new CAS pair", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);
  assert.deepEqual(store.expectation, { expectedRevision: 4, processGeneration: 99 });

  const pending = store.patchDestination("dest-1", patchInput());
  await waitForCalls(calls, 2);
  const patch = calls[1]!;
  assert.equal(patch.method, "PATCH");
  assert.ok(patch.url.endsWith("/destinations/dest-1"));
  assert.deepEqual(patch.body, {
    ...patchInput(),
    expectedRevision: 4,
    processGeneration: 99,
  });
  patch.resolve({
    destination: destinationDto("dest-1", "Renamed"),
    credentials: [credentialDto("cred-dest-1", "dest-1")],
    revision: revisionBody(5),
  });

  const updated = await pending;
  assert.equal(updated.name, "Renamed");
  assert.equal(store.destinations.length, 1);
  assert.equal(store.destinations[0]?.name, "Renamed");
  assert.deepEqual(store.expectation, { expectedRevision: 5, processGeneration: 99 });
  // The authoritative credential receipt is committed with the destination.
  assert.equal(store.credentials[0]?.id, "cred-dest-1");
});

test("patch uses the editor-captured CAS pair after the store snapshot advances", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);
  const captured = { expectedRevision: 4, processGeneration: 99 };

  const reload = store.load();
  await waitForCalls(calls, 2);
  resolvePair(calls, 1, "dest-1", 5);
  await reload;
  assert.deepEqual(store.expectation, { expectedRevision: 5, processGeneration: 99 });

  const pending = store.patchDestination("dest-1", patchInput(), captured);
  await waitForCalls(calls, 3);
  assert.deepEqual(calls[2]!.body, {
    ...patchInput(),
    expectedRevision: 4,
    processGeneration: 99,
  });
  calls[2]!.resolve({
    destination: destinationDto("dest-1", "Renamed"),
    credentials: [credentialDto("cred-dest-1", "dest-1")],
    revision: revisionBody(6),
  });
  await pending;
});

test("patch conflict reloads the projection and rethrows without replaying", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const pending = store.patchDestination("dest-1", patchInput());
  await waitForCalls(calls, 2);
  calls[1]!.resolve({
    code: "revisionConflict",
    message: "revision conflict",
    currentRevision: 6,
    processGeneration: 99,
  }, 409);

  // runMutation refreshes tokens, then the store reloads the projection.
  await waitForCalls(calls, 3);
  const contract = calls[2]!;
  assert.ok(contract.url.endsWith("/contract"));
  contract.resolve(revisionBody(6));
  await waitForCalls(calls, 4);
  resolvePair(calls, 3, "dest-1", 6);

  await assert.rejects(pending, (error: unknown) => {
    assert.ok(error instanceof DashboardRequestError);
    assert.equal(error.status, 409);
    return true;
  });
  assert.equal(calls.filter((call) => call.method === "PATCH").length, 1);
  assert.deepEqual(store.expectation, { expectedRevision: 6, processGeneration: 99 });
});

test("delete removes the destination and its credentials and keeps the new revision", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const pending = store.deleteDestination("dest-1");
  await waitForCalls(calls, 2);
  const call = calls[1]!;
  assert.equal(call.method, "DELETE");
  assert.ok(call.url.endsWith("/destinations/dest-1"));
  assert.deepEqual(call.body, { expectedRevision: 4, processGeneration: 99 });
  call.resolve({ revision: revisionBody(7) });

  await pending;
  assert.equal(store.destinations.length, 0);
  assert.equal(store.credentials.length, 0);
  assert.deepEqual(store.expectation, { expectedRevision: 7, processGeneration: 99 });
});

test("delete surfaces the server error and keeps state when Keys still reference it", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const pending = store.deleteDestination("dest-1");
  await waitForCalls(calls, 2);
  calls[1]!.resolve({
    code: "invalidRequest",
    message: "custom destination still has 1 credential(s)",
    currentRevision: 4,
    processGeneration: 99,
  }, 400);

  await assert.rejects(pending, (error: unknown) => {
    assert.ok(error instanceof DashboardRequestError);
    assert.equal(error.status, 400);
    return true;
  });
  assert.equal(store.destinations.length, 1);
  assert.equal(store.credentials.length, 1);
  assert.deepEqual(store.expectation, { expectedRevision: 4, processGeneration: 99 });
});

test("patch receipt wins over a stale projection load started while it is pending", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const staleLoad = store.load();
  await waitForCalls(calls, 2);
  const pendingPatch = store.patchDestination("dest-1", patchInput());
  await waitForCalls(calls, 3);
  calls[2]!.resolve({
    destination: destinationDto("dest-1", "Renamed"),
    credentials: [credentialDto("cred-dest-1", "dest-1")],
    revision: revisionBody(5),
  });
  await pendingPatch;

  resolvePair(calls, 1, "dest-1", 4);
  await staleLoad;
  assert.equal(store.destinations[0]?.name, "Renamed");
  assert.deepEqual(store.expectation, { expectedRevision: 5, processGeneration: 99 });
});

test("clear blocks late destination patch and delete receipts from restoring session state", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const pendingPatch = store.patchDestination("dest-1", patchInput());
  await waitForCalls(calls, 2);
  store.clear();
  calls[1]!.resolve({
    destination: destinationDto("dest-1", "Renamed"),
    credentials: [credentialDto("cred-dest-1", "dest-1")],
    revision: revisionBody(5),
  });
  await pendingPatch;
  assert.equal(store.destinations.length, 0);
  assert.equal(store.expectation, null);

  const pendingDelete = store.deleteDestination("dest-1");
  await waitForCalls(calls, 3);
  store.clear();
  calls[2]!.resolve({ revision: revisionBody(6) });
  await pendingDelete;
  assert.equal(store.destinations.length, 0);
  assert.equal(store.credentials.length, 0);
  assert.equal(store.expectation, null);
});

function explanationBody(model: string): RoutingExplanation {
  return {
    clientProtocol: "chat_completions",
    conversationBinding: "not_evaluated",
    conversationSticky: false,
    eligible: [{
      accountId: "acct-dest-1",
      accountName: "Lab Key",
      adapterKind: "http",
      channel: "go",
      destinationId: "dest-1",
      destinationName: "Lab HTTP",
      providerId: "custom",
      resolvedModel: "vendor/opus",
      routingRank: 1,
      upstreamProtocol: "chat_completions",
    }],
    exclusions: [],
    expectedBasePolicyFirstPick: null,
    observedAt: "2026-09-20T00:00:00Z",
    requestedModel: model,
    resolved: {
      alias: model,
      kind: "alias",
      mappings: [{ providerId: "custom", routeable: true, upstreamModel: "vendor/opus" }],
    },
    revision: { revision: 4, processGeneration: 99, pricingRevision: "p1" },
    routingMode: "strict-priority",
    runtimeOnlyUncertainty: ["conversation_binding_not_evaluated"],
  };
}

test("explain caches per model+protocol and only the latest request commits", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);
  const key = store.explainKey("Lab-Opus", "chat_completions");
  assert.equal(key, "chat_completions lab-opus");

  const first = store.explainRouting("Lab-Opus", "chat_completions");
  const second = store.explainRouting("lab-opus", "chat_completions");
  await waitForCalls(calls, 3);
  assert.ok(calls[1]!.url.includes("/routing/explain?"));
  assert.ok(calls[1]!.url.includes("model=Lab-Opus"));
  assert.ok(calls[1]!.url.includes("clientProtocol=chat_completions"));
  assert.equal(store.explainLoading[key], true);

  // Newer request resolves first and commits.
  calls[2]!.resolve(explanationBody("lab-opus"));
  await second;
  assert.equal(store.explanations[key]?.requested_model, "lab-opus");
  assert.equal(store.explainLoading[key], undefined);

  // The stale earlier request resolves later and must not clobber.
  calls[1]!.resolve(explanationBody("STALE"));
  await first;
  assert.equal(store.explanations[key]?.requested_model, "lab-opus");
});

test("explain records the error for the view and keeps the last snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);
  const key = store.explainKey("lab-opus", "chat_completions");

  const pending = store.explainRouting("lab-opus", "chat_completions");
  await waitForCalls(calls, 2);
  calls[1]!.resolve({ code: "invalidRequest", message: "model is required" }, 400);
  await assert.rejects(pending);
  assert.ok(store.explainErrors[key]?.length);
  assert.equal(store.explanations[key], undefined);
});

test("clear wipes explanations and blocks a late commit after session drop", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);
  const key = store.explainKey("lab-opus", "chat_completions");

  const pending = store.explainRouting("lab-opus", "chat_completions");
  await waitForCalls(calls, 2);
  store.clear();
  assert.equal(store.destinations.length, 0);
  assert.deepEqual(store.explanations, {});
  assert.deepEqual(store.explainLoading, {});
  assert.deepEqual(store.explainErrors, {});

  calls[1]!.resolve(explanationBody("lab-opus"));
  await pending;
  assert.equal(store.explanations[key], undefined);
});


test("card layout uses the captured revision and commits all returned resources", async () => {
  setActivePinia(createPinia()); useControlPlaneStore();
  const calls = installDeferredFetch(); const store = await loadedStore(calls);
  const layout = [{ id: "empty", destinationId: "dest-1", credentialIds: [] }, { id: "populated", destinationId: "dest-1", credentialIds: ["cred-dest-1"] }];
  const pending = store.replaceRoutingCardLayout(layout, { expectedRevision: 3, processGeneration: 99 });
  await waitForCalls(calls, 2);
  assert.equal(calls[1].method, "PUT"); assert.ok(calls[1].url.endsWith("/routing/cards"));
  assert.deepEqual(calls[1].body, { cards: layout, expectedRevision: 3, processGeneration: 99 });
  const [dest, cred] = snapshotBodies("dest-1", 5);
  calls[1].resolve({ ...dest, ...cred, cards: layout }); await pending;
  assert.deepEqual(store.cards.map(c => c.id), ["empty", "populated"]);
  assert.equal(store.expectation?.expectedRevision, 5);
});

test("layout receipts cannot resurrect a logged out session", async () => {
  setActivePinia(createPinia()); useControlPlaneStore();
  const calls = installDeferredFetch(); const store = await loadedStore(calls);
  const layout = [{ id: "card", destinationId: "dest-1", credentialIds: ["cred-dest-1"] }];
  const pending = store.replaceRoutingCardLayout(layout); await waitForCalls(calls, 2);
  store.clear(); const [dest, cred] = snapshotBodies("dest-1", 5);
  calls[1].resolve({ ...dest, ...cred, cards: layout }); await pending;
  assert.deepEqual(store.cards, []); assert.deepEqual(store.destinations, []); assert.equal(store.loaded, false);
});

test("layout conflict reloads once without replaying the layout", async () => {
  setActivePinia(createPinia()); useControlPlaneStore();
  const calls = installDeferredFetch(); const store = await loadedStore(calls);
  const pending = store.replaceRoutingCardLayout([]);
  await waitForCalls(calls, 2);
  calls[1].resolve({ code: "revisionConflict", message: "conflict", currentRevision: 6, processGeneration: 99 }, 409);
  await waitForCalls(calls, 3); calls[2].resolve(revisionBody(6));
  await waitForCalls(calls, 4); resolvePair(calls, 3, "dest-1", 6);
  await assert.rejects(pending);
  assert.equal(calls.filter(c => c.method === "PUT").length, 1);
  assert.equal(store.cards[0].id, "card-dest-1");
  assert.equal(store.expectation?.expectedRevision, 6);
});

function quotaRecoveryDto(overrides: Partial<QuotaRecoveryDto> = {}): QuotaRecoveryDto {
  return {
    status: "waiting",
    reason: "quota_exhausted",
    window: "five_hours",
    observedAt: "2026-09-20T11:00:00Z",
    resetsAt: "2026-09-20T16:00:00Z",
    nextRetryAt: "2026-09-20T12:30:00Z",
    failureCount: 3,
    ...overrides,
  };
}

function quotaRetryBody(
  credentialId: string,
  destId: string,
  revision: number,
  recovery: QuotaRecoveryDto,
  processGeneration = 99,
): object {
  return {
    credential: { ...credentialDto(credentialId, destId), quotaRecovery: recovery },
    revision: revisionBody(revision, processGeneration),
  };
}

test("quota retry commits the returned Key in place and does not call model test", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const pending = store.retryQuotaRecovery("cred-dest-1");
  await waitForCalls(calls, 2);
  const call = calls[1]!;
  assert.equal(call.method, "POST");
  assert.ok(call.url.endsWith("/credentials/cred-dest-1/quota-retry"));
  assert.doesNotMatch(call.url, /test|models/i);
  assert.deepEqual(call.body, { expectedRevision: 4, processGeneration: 99 });
  call.resolve(quotaRetryBody("cred-dest-1", "dest-1", 5, quotaRecoveryDto({ status: "ready", failureCount: 3 })));

  const updated = await pending;
  assert.equal(updated.quota_recovery?.status, "ready");
  assert.equal(updated.quota_recovery?.failure_count, 3);
  assert.equal(store.credentials[0]?.quota_recovery?.status, "ready");
  assert.equal(store.credentials[0]?.enabled, true);
  assert.deepEqual(store.expectation, { expectedRevision: 5, processGeneration: 99 });
  assert.equal(calls.filter((row) => /test|models/i.test(row.url)).length, 0);
});

test("quota retry does not fan out to another Key on the same destination", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const load = store.load();
  await waitForCalls(calls, 1);
  const dest = destinationDto("dest-1");
  const waiting = {
    ...credentialDto("cred-a", "dest-1"),
    id: "cred-a",
    quotaPoolId: "pool-1",
    quotaRecovery: quotaRecoveryDto(),
  };
  const sibling = {
    ...credentialDto("cred-b", "dest-1"),
    id: "cred-b",
    quotaPoolId: "pool-1",
    routingRank: 2,
  };
  calls[0]!.resolve({
    destinations: [dest],
    credentials: [waiting, sibling],
    cards: [{ id: "card-dest-1", destinationId: "dest-1", credentialIds: ["cred-a", "cred-b"] }],
    revision: revisionBody(4),
  });
  await load;

  const pending = store.retryQuotaRecovery("cred-a");
  await waitForCalls(calls, 2);
  calls[1]!.resolve(quotaRetryBody("cred-a", "dest-1", 5, quotaRecoveryDto({ status: "ready" })));
  await pending;

  assert.equal(store.credentials.find((row) => row.id === "cred-a")?.quota_recovery?.status, "ready");
  assert.equal(store.credentials.find((row) => row.id === "cred-b")?.quota_recovery, null);
});

test("quota retry receipt wins over a stale projection poll", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const staleLoad = store.load();
  await waitForCalls(calls, 2);
  const pendingRetry = store.retryQuotaRecovery("cred-dest-1");
  await waitForCalls(calls, 3);
  calls[2]!.resolve(quotaRetryBody("cred-dest-1", "dest-1", 5, quotaRecoveryDto({ status: "ready" })));
  await pendingRetry;

  resolvePair(calls, 1, "dest-1", 4);
  await staleLoad;
  assert.equal(store.credentials[0]?.quota_recovery?.status, "ready");
  assert.deepEqual(store.expectation, { expectedRevision: 5, processGeneration: 99 });
});

test("quota retry conflict reloads the projection without replaying", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const pending = store.retryQuotaRecovery("cred-dest-1");
  await waitForCalls(calls, 2);
  calls[1]!.resolve({
    code: "revisionConflict",
    message: "revision conflict",
    currentRevision: 6,
    processGeneration: 99,
  }, 409);
  await waitForCalls(calls, 3);
  calls[2]!.resolve(revisionBody(6));
  await waitForCalls(calls, 4);
  resolvePair(calls, 3, "dest-1", 6);

  await assert.rejects(pending, (error: unknown) => {
    assert.ok(error instanceof DashboardRequestError);
    assert.equal(error.status, 409);
    return true;
  });
  assert.equal(calls.filter((call) => call.url.includes("/quota-retry")).length, 1);
  assert.deepEqual(store.expectation, { expectedRevision: 6, processGeneration: 99 });
});

test("clear blocks a late quota-retry receipt after logout", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const pending = store.retryQuotaRecovery("cred-dest-1");
  await waitForCalls(calls, 2);
  store.clear();
  calls[1]!.resolve(quotaRetryBody("cred-dest-1", "dest-1", 5, quotaRecoveryDto({ status: "ready" })));
  await pending;
  assert.equal(store.credentials.length, 0);
  assert.equal(store.expectation, null);
});

test("a retry receipt from a prior process does not clobber a newer snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const first = store.load();
  await waitForCalls(calls, 1);
  resolvePair(calls, 0, "dest-1", 10, 77);
  await first;
  assert.deepEqual(store.expectation, { expectedRevision: 10, processGeneration: 77 });

  const pendingRetry = store.retryQuotaRecovery("cred-dest-1");
  await waitForCalls(calls, 2);
  const restarted = store.load();
  await waitForCalls(calls, 3);
  resolvePair(calls, 2, "dest-1", 1, 78);
  await restarted;
  assert.equal(store.credentials[0]?.quota_recovery, null);
  assert.deepEqual(store.expectation, { expectedRevision: 1, processGeneration: 78 });

  calls[1]!.resolve(quotaRetryBody(
    "cred-dest-1",
    "dest-1",
    11,
    quotaRecoveryDto({ status: "ready" }),
    77,
  ));
  await pendingRetry;
  assert.equal(store.credentials[0]?.quota_recovery, null);
  assert.deepEqual(store.expectation, { expectedRevision: 1, processGeneration: 78 });
});

test("a patch receipt from a prior process does not clobber a newer snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const first = store.load();
  await waitForCalls(calls, 1);
  resolvePair(calls, 0, "dest-1", 10, 77);
  await first;

  const pendingPatch = store.patchDestination("dest-1", patchInput());
  await waitForCalls(calls, 2);
  const restarted = store.load();
  await waitForCalls(calls, 3);
  resolvePair(calls, 2, "dest-1", 1, 78);
  await restarted;
  assert.equal(store.destinations[0]?.name, "dest-1");
  assert.deepEqual(store.expectation, { expectedRevision: 1, processGeneration: 78 });

  calls[1]!.resolve({
    destination: destinationDto("dest-1", "Renamed"),
    credentials: [credentialDto("cred-dest-1", "dest-1")],
    revision: revisionBody(11, 77),
  });
  await pendingPatch;
  assert.equal(store.destinations[0]?.name, "dest-1");
  assert.deepEqual(store.expectation, { expectedRevision: 1, processGeneration: 78 });
});

test("a layout receipt from a prior process does not clobber a newer snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const first = store.load();
  await waitForCalls(calls, 1);
  resolvePair(calls, 0, "dest-1", 10, 77);
  await first;
  const priorCards = store.cards.map((card) => card.id);

  const layout = [{ id: "moved", destinationId: "dest-1", credentialIds: ["cred-dest-1"] }];
  const pendingLayout = store.replaceRoutingCardLayout(layout);
  await waitForCalls(calls, 2);
  const restarted = store.load();
  await waitForCalls(calls, 3);
  resolvePair(calls, 2, "dest-1", 1, 78);
  await restarted;
  assert.deepEqual(store.expectation, { expectedRevision: 1, processGeneration: 78 });

  const [dest, cred] = snapshotBodies("dest-1", 11, undefined, 77);
  calls[1]!.resolve({ ...dest, ...cred, cards: layout });
  await pendingLayout;
  assert.deepEqual(store.cards.map((card) => card.id), priorCards);
  assert.deepEqual(store.expectation, { expectedRevision: 1, processGeneration: 78 });
});

test("repeated quota retry posts the CAS endpoint again while waiting", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = await loadedStore(calls);

  const first = store.retryQuotaRecovery("cred-dest-1");
  await waitForCalls(calls, 2);
  calls[1]!.resolve(quotaRetryBody("cred-dest-1", "dest-1", 5, quotaRecoveryDto({ status: "waiting" })));
  await first;

  const second = store.retryQuotaRecovery("cred-dest-1");
  await waitForCalls(calls, 3);
  assert.ok(calls[2]!.url.endsWith("/credentials/cred-dest-1/quota-retry"));
  calls[2]!.resolve(quotaRetryBody("cred-dest-1", "dest-1", 6, quotaRecoveryDto({ status: "ready" })));
  await second;
  assert.equal(store.credentials[0]?.quota_recovery?.status, "ready");
  assert.equal(calls.filter((call) => call.url.includes("/quota-retry")).length, 2);
  assert.equal(calls.filter((call) => /test|models/i.test(call.url)).length, 0);
});
