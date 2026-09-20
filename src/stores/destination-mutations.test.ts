import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { DashboardRequestError } from "../api/dashboard-v3.ts";
import type {
  DestinationCredentialDto,
  DestinationDto,
  RoutingExplanation,
} from "../api/generated/dashboard-v4.ts";
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
    routingRank: 1,
    scope: { kind: "all" },
  };
}

function revisionBody(revision: number): object {
  return { revision, processGeneration: 99, pricingRevision: "p1" };
}

function snapshotBodies(destId: string, revision: number, name?: string): [object, object] {
  return [
    { destinations: [destinationDto(destId, name)], revision: revisionBody(revision) },
    { credentials: [credentialDto(`cred-${destId}`, destId)], revision: revisionBody(revision) },
  ];
}

function resolvePair(calls: DeferredCall[], start: number, destId: string, revision: number): void {
  const slice = calls.slice(start, start + 2);
  const dest = slice.find((call) => call.url.endsWith("/destinations"));
  const cred = slice.find((call) => call.url.endsWith("/credentials"));
  assert.ok(dest && cred, "expected destination and credential fetches");
  const [destBody, credBody] = snapshotBodies(destId, revision);
  dest.resolve(destBody);
  cred.resolve(credBody);
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
  await waitForCalls(calls, 2);
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
  await waitForCalls(calls, 3);
  const patch = calls[2]!;
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
  await waitForCalls(calls, 4);
  resolvePair(calls, 2, "dest-1", 5);
  await reload;
  assert.deepEqual(store.expectation, { expectedRevision: 5, processGeneration: 99 });

  const pending = store.patchDestination("dest-1", patchInput(), captured);
  await waitForCalls(calls, 5);
  assert.deepEqual(calls[4]!.body, {
    ...patchInput(),
    expectedRevision: 4,
    processGeneration: 99,
  });
  calls[4]!.resolve({
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
  await waitForCalls(calls, 3);
  calls[2]!.resolve({
    code: "revisionConflict",
    message: "revision conflict",
    currentRevision: 6,
    processGeneration: 99,
  }, 409);

  // runMutation refreshes tokens, then the store reloads the projection.
  await waitForCalls(calls, 4);
  const contract = calls[3]!;
  assert.ok(contract.url.endsWith("/contract"));
  contract.resolve(revisionBody(6));
  await waitForCalls(calls, 6);
  resolvePair(calls, 4, "dest-1", 6);

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
  await waitForCalls(calls, 3);
  const call = calls[2]!;
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
  await waitForCalls(calls, 3);
  calls[2]!.resolve({
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
  await waitForCalls(calls, 4);
  const pendingPatch = store.patchDestination("dest-1", patchInput());
  await waitForCalls(calls, 5);
  calls[4]!.resolve({
    destination: destinationDto("dest-1", "Renamed"),
    credentials: [credentialDto("cred-dest-1", "dest-1")],
    revision: revisionBody(5),
  });
  await pendingPatch;

  resolvePair(calls, 2, "dest-1", 4);
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
  await waitForCalls(calls, 3);
  store.clear();
  calls[2]!.resolve({
    destination: destinationDto("dest-1", "Renamed"),
    credentials: [credentialDto("cred-dest-1", "dest-1")],
    revision: revisionBody(5),
  });
  await pendingPatch;
  assert.equal(store.destinations.length, 0);
  assert.equal(store.expectation, null);

  const pendingDelete = store.deleteDestination("dest-1");
  await waitForCalls(calls, 4);
  store.clear();
  calls[3]!.resolve({ revision: revisionBody(6) });
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
  await waitForCalls(calls, 4);
  assert.ok(calls[2]!.url.includes("/routing/explain?"));
  assert.ok(calls[2]!.url.includes("model=Lab-Opus"));
  assert.ok(calls[2]!.url.includes("clientProtocol=chat_completions"));
  assert.equal(store.explainLoading[key], true);

  // Newer request resolves first and commits.
  calls[3]!.resolve(explanationBody("lab-opus"));
  await second;
  assert.equal(store.explanations[key]?.requested_model, "lab-opus");
  assert.equal(store.explainLoading[key], undefined);

  // The stale earlier request resolves later and must not clobber.
  calls[2]!.resolve(explanationBody("STALE"));
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
  await waitForCalls(calls, 3);
  calls[2]!.resolve({ code: "invalidRequest", message: "model is required" }, 400);
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
  await waitForCalls(calls, 3);
  store.clear();
  assert.equal(store.destinations.length, 0);
  assert.deepEqual(store.explanations, {});
  assert.deepEqual(store.explainLoading, {});
  assert.deepEqual(store.explainErrors, {});

  calls[2]!.resolve(explanationBody("lab-opus"));
  await pending;
  assert.equal(store.explanations[key], undefined);
});
