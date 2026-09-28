import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
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

function metadataBody(
  destinationId: string,
  revision: number,
  metadata: object,
  source: string,
): object {
  return {
    destinationId,
    models: [{ publicModel: "model-a", upstreamModel: "model-a", source, metadata }],
    revision: { revision, processGeneration: 99, pricingRevision: "p1" },
  };
}

test("model metadata: only the latest load commits per destination", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.loadModelMetadata("dest-a");
  const second = store.loadModelMetadata("dest-a");
  await waitForCalls(calls, 2);
  assert.ok(calls.every((call) => call.url.endsWith("/destinations/dest-a/model-metadata") && call.method === "GET"));

  calls[1].resolve(metadataBody("dest-a", 8, { contextWindow: 64000 }, "upstream"));
  await second;
  assert.equal(store.modelMetadata["dest-a"]?.models[0]?.metadata.context_window, 64000);
  assert.deepEqual(store.modelMetadata["dest-a"]?.expectation, { expectedRevision: 8, processGeneration: 99 });
  assert.equal(store.modelMetadataLoading["dest-a"], undefined);

  calls[0].resolve(metadataBody("dest-a", 7, { contextWindow: 32000 }, "operator"));
  await first;
  assert.equal(store.modelMetadata["dest-a"]?.models[0]?.metadata.context_window, 64000);
  assert.deepEqual(store.modelMetadata["dest-a"]?.expectation, { expectedRevision: 8, processGeneration: 99 });
});

test("model metadata: a declaration PUT carries CAS and commits the receipt in place", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const pending = store.declareModelMetadata(
    "dest-a",
    "model-a",
    { contextWindow: 262144, inputModalities: ["text", "image"] },
    { expectedRevision: 8, processGeneration: 99 },
  );
  await waitForCalls(calls, 1);
  const call = calls[0];
  assert.ok(call.url.endsWith("/destinations/dest-a/model-metadata"));
  assert.equal(call.method, "PUT");
  assert.deepEqual(call.body, {
    expectedRevision: 8,
    processGeneration: 99,
    publicModel: "model-a",
    metadata: { contextWindow: 262144, inputModalities: ["text", "image"] },
  });

  call.resolve(metadataBody("dest-a", 9, { contextWindow: 262144, inputModalities: ["text", "image"] }, "operator"));
  await pending;
  assert.equal(store.modelMetadata["dest-a"]?.models[0]?.source, "operator");
  assert.deepEqual(store.modelMetadata["dest-a"]?.models[0]?.metadata.input_modalities, ["text", "image"]);
  assert.deepEqual(store.expectation, { expectedRevision: 9, processGeneration: 99 });
});

test("model metadata: a pending load cannot clobber a finished declaration", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const load = store.loadModelMetadata("dest-a");
  const put = store.declareModelMetadata(
    "dest-a",
    "model-a",
    { contextWindow: 131072 },
    { expectedRevision: 8, processGeneration: 99 },
  );
  await waitForCalls(calls, 2);
  const [loadCall, putCall] = calls;
  assert.equal(loadCall.method, "GET");
  assert.equal(putCall.method, "PUT");

  putCall.resolve(metadataBody("dest-a", 9, { contextWindow: 131072 }, "operator"));
  await put;
  assert.equal(store.modelMetadata["dest-a"]?.models[0]?.metadata.context_window, 131072);

  loadCall.resolve(metadataBody("dest-a", 8, { contextWindow: 32000 }, "unknown"));
  await load;
  assert.equal(store.modelMetadata["dest-a"]?.models[0]?.metadata.context_window, 131072);
  assert.deepEqual(store.expectation, { expectedRevision: 9, processGeneration: 99 });
});

test("model metadata: clear drops cached entries", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const load = store.loadModelMetadata("dest-a");
  await waitForCalls(calls, 1);
  calls[0].resolve(metadataBody("dest-a", 8, { contextWindow: 64000 }, "upstream"));
  await load;
  assert.ok(store.modelMetadata["dest-a"]);

  store.clear();
  assert.deepEqual(store.modelMetadata, {});
  assert.deepEqual(store.modelMetadataErrors, {});
});
