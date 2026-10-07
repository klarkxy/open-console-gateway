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
  store.invalidateReads();
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

test("model metadata: the aggregate catalog read replaces the map in one call", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const pending = store.loadAllModelMetadata();
  await waitForCalls(calls, 1);
  assert.ok(calls[0].url.endsWith("/model-metadata") && calls[0].method === "GET");
  calls[0].resolve({
    destinations: [
      metadataBody("dest-a", 9, { contextWindow: 64000 }, "upstream"),
      metadataBody("dest-b", 9, { inputModalities: ["text"] }, "unknown"),
    ],
    revision: { revision: 9, processGeneration: 99, pricingRevision: "p1" },
  });
  await pending;
  assert.deepEqual(Object.keys(store.modelMetadata).sort(), ["dest-a", "dest-b"]);
  assert.equal(store.modelMetadata["dest-a"]?.models[0]?.metadata.context_window, 64000);
  assert.deepEqual(store.modelMetadata["dest-b"]?.models[0]?.metadata.input_modalities, ["text"]);
});

test("model metadata: only the latest aggregate load commits", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();

  const first = store.loadAllModelMetadata();
  store.invalidateReads();
  const second = store.loadAllModelMetadata();
  await waitForCalls(calls, 2);

  calls[1].resolve({
    destinations: [metadataBody("dest-b", 9, { contextWindow: 128000 }, "operator")],
    revision: { revision: 9, processGeneration: 99, pricingRevision: "p1" },
  });
  await second;
  assert.deepEqual(Object.keys(store.modelMetadata), ["dest-b"]);

  calls[0].resolve({
    destinations: [metadataBody("dest-a", 9, { contextWindow: 32000 }, "upstream")],
    revision: { revision: 9, processGeneration: 99, pricingRevision: "p1" },
  });
  await first;
  assert.deepEqual(Object.keys(store.modelMetadata), ["dest-b"]);
});

test("model metadata: aggregate singleflight seeds individual freshness and removes departed entries", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const first = store.loadAllModelMetadata();
  const joined = store.loadAllModelMetadata();
  await waitForCalls(calls, 1);
  calls[0]!.resolve({
    destinations: [metadataBody("dest-a", 7, { contextWindow: 32000 }, "operator")],
    revision: { revision: 7, processGeneration: 99 },
  });
  await Promise.all([first, joined]);
  const cached = await store.loadModelMetadata("dest-a", { maxAgeMs: 15_000 });
  assert.equal(cached.destination_id, "dest-a");
  assert.equal(calls.length, 1);
  const reload = store.loadAllModelMetadata();
  await waitForCalls(calls, 2);
  calls[1]!.resolve({ destinations: [], revision: { revision: 8, processGeneration: 99 } });
  await reload;
  const departed = store.loadModelMetadata("dest-a", { maxAgeMs: 15_000 });
  await waitForCalls(calls, 3);
  calls[2]!.resolve(metadataBody("dest-a", 8, { contextWindow: 64000 }, "operator"));
  assert.equal((await departed).models[0]?.metadata.context_window, 64000);
  store.clear();
});

test("model metadata: an older aggregate cannot overwrite a newer individual snapshot", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const aggregate = store.loadAllModelMetadata();
  const individual = store.loadModelMetadata("dest-a");
  await waitForCalls(calls, 2);
  calls[1]!.resolve(metadataBody("dest-a", 8, { contextWindow: 64000 }, "operator"));
  await individual;
  calls[0]!.resolve({
    destinations: [metadataBody("dest-a", 7, { contextWindow: 32000 }, "operator")],
    revision: { revision: 7, processGeneration: 99 },
  });
  await aggregate;
  assert.equal(store.modelMetadata["dest-a"]?.models[0]?.metadata.context_window, 64000);
  store.clear();
});

test("model metadata: an aggregate supersedes older individual reads and their freshness", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const individual = store.loadModelMetadata("dest-a");
  const aggregate = store.loadAllModelMetadata();
  await waitForCalls(calls, 2);
  calls[1]!.resolve({
    destinations: [metadataBody("dest-a", 8, { contextWindow: 64000 }, "operator")],
    revision: { revision: 8, processGeneration: 99 },
  });
  await aggregate;
  calls[0]!.resolve(metadataBody("dest-a", 7, { contextWindow: 32000 }, "operator"));
  await individual;
  assert.equal(store.modelMetadata["dest-a"]?.models[0]?.metadata.context_window, 64000);
  assert.equal(store.modelMetadataLoading["dest-a"], undefined);
  await store.loadModelMetadata("dest-a", { maxAgeMs: 15_000 });
  assert.equal(calls.length, 2);
  store.clear();
});

test("model metadata: a declaration receipt invalidates reads started while the write was pending", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useDestinationsStore();
  const write = store.declareModelMetadata("dest-a", "model-a", { contextWindow: 64000 }, { expectedRevision: 7, processGeneration: 99 });
  const read = store.loadModelMetadata("dest-a");
  await waitForCalls(calls, 2);
  calls[0]!.resolve(metadataBody("dest-a", 8, { contextWindow: 64000 }, "operator"));
  await write;
  calls[1]!.resolve(metadataBody("dest-a", 7, { contextWindow: 32000 }, "operator"));
  await read;
  assert.equal(store.modelMetadata["dest-a"]?.models[0]?.metadata.context_window, 64000);
  const recovery = store.loadModelMetadata("dest-a", { maxAgeMs: 15_000 });
  await waitForCalls(calls, 3);
  calls[2]!.resolve(metadataBody("dest-a", 8, { contextWindow: 64000 }, "operator"));
  await recovery;
  store.clear();
});
