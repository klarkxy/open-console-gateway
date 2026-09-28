import assert from "node:assert/strict";
import test from "node:test";
import { installFetchMock, setupControlPlane, type RecordedRequest } from "../test-helpers/dashboard-v3-fetch.ts";
import { useProvidersStore } from "./providers.ts";

test("CPA catalog retains a good snapshot on error and drops late session results", async () => {
  setupControlPlane(7, 99);
  let response: () => Promise<object> = async () => ({ models: [{ id: "current", enabled: true }] });
  installFetchMock(() => response());
  const store = useProvidersStore();
  await store.loadCpaModels();
  assert.equal(store.cpaModels?.[0]?.id, "current");
  response = async () => { throw new Error("offline"); };
  await assert.rejects(store.loadCpaModels());
  assert.equal(store.cpaModels?.[0]?.id, "current");
  const gate = deferred<object>();
  response = () => gate.promise;
  const pending = store.loadCpaModels();
  store.clear();
  gate.resolve({ models: [{ id: "abandoned", enabled: true }] });
  await pending;
  assert.equal(store.cpaModels, null);
});

test("a late earlier CPA catalog read cannot overwrite the newer snapshot", async () => {
  setupControlPlane(7, 99);
  const gates = [deferred<object>(), deferred<object>()];
  let index = 0;
  installFetchMock(() => gates[index++]!.promise);
  const store = useProvidersStore();
  const first = store.loadCpaModels();
  const second = store.loadCpaModels();
  gates[1]!.resolve({ models: [{ id: "new", enabled: true }] });
  await second;
  gates[0]!.resolve({ models: [{ id: "old", enabled: true }] });
  await first;
  assert.equal(store.cpaModels?.[0]?.id, "new");
});

function deferred<T = void>(): { promise: Promise<T>; resolve: (value: T | PromiseLike<T>) => void; reject: (error: unknown) => void } {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function publicationResponse(unpublished: string[], revision: number): object {
  return {
    revision: { revision, processGeneration: 99 },
    unpublished,
  };
}

interface PublicationFixture {
  requests: RecordedRequest[];
  patches: () => RecordedRequest[];
  gates: Map<string, Array<ReturnType<typeof deferred<object>>>>;
  store: ReturnType<typeof useProvidersStore>;
}

/**
 * Alias publication backend: GET returns `initial`; PATCH applies the row
 * change to the current list and returns the new authoritative list. Named
 * rows can be held with deferred gates to shape interleavings.
 */
function publicationFixture(initial: string[], held: string[] = []): PublicationFixture {
  let current = [...initial];
  let revision = 7;
  const gates = new Map<string, Array<ReturnType<typeof deferred<object>>>>();
  const requests = installFetchMock((req) => {
    if (!req.url.endsWith("/alias-publication")) throw new Error(`unexpected request ${req.url}`);
    if (req.method === "GET") return publicationResponse(current, revision);
    const publicModel = String(req.body?.publicModel ?? "");
    const published = Boolean(req.body?.published);
    const apply = (): object => {
      if (Number(req.body?.expectedRevision) !== revision) {
        return new Response(JSON.stringify({
          message: "revision conflict",
          code: "revisionConflict",
          currentRevision: revision,
          processGeneration: 99,
        }), { status: 409, headers: { "Content-Type": "application/json" } });
      }
      current = published
        ? current.filter((name) => name.toLocaleLowerCase() !== publicModel.toLocaleLowerCase())
        : [...new Set([...current, publicModel])];
      revision += 1;
      return publicationResponse(current, revision);
    };
    if (held.includes(publicModel)) {
      const gate = deferred<object>();
      const list = gates.get(publicModel) ?? [];
      list.push(gate);
      gates.set(publicModel, list);
      return gate.promise.then(apply);
    }
    return apply();
  });
  return {
    requests,
    patches: () => requests.filter((req) => req.method === "PATCH"),
    gates,
    store: useProvidersStore(),
  };
}

async function loadInitial(fixture: PublicationFixture): Promise<void> {
  await fixture.store.loadAliasPublication();
  assert.equal(fixture.store.aliasPublicationReady, true);
}

test("rapid toggles on different rows keep each accepted receipt", async () => {
  setupControlPlane(7, 99);
  const fixture = publicationFixture([], ["alpha"]);
  await loadInitial(fixture);

  const first = fixture.store.setAliasPublished("alpha", false);
  const second = fixture.store.setAliasPublished("beta", false);
  await flush();
  // Both rows show their optimistic overlay while the lane serializes.
  assert.deepEqual(fixture.store.aliasUnpublished, ["alpha", "beta"]);
  assert.equal(fixture.patches().length, 1);
  fixture.gates.get("alpha")![0]!.resolve({});
  await Promise.all([first, second]);
  // Each accepted receipt committed; neither rolled the other back.
  assert.deepEqual(fixture.store.aliasUnpublished.sort(), ["alpha", "beta"]);
  assert.equal(fixture.patches().length, 2);
  // The second write dispatched on the first receipt's revision.
  assert.equal(fixture.patches()[0]?.body?.expectedRevision, 7);
  assert.equal(fixture.patches()[1]?.body?.expectedRevision, 8);
  assert.equal(fixture.store.aliasPublicationSaveError, "");
});

test("a failed toggle reconciles only its own overlay", async () => {
  setupControlPlane(7, 99);
  const fixture = publicationFixture([], ["alpha"]);
  await loadInitial(fixture);

  const first = fixture.store.setAliasPublished("alpha", false);
  const second = fixture.store.setAliasPublished("beta", false);
  await flush();
  fixture.gates.get("alpha")![0]!.reject(new Error("network down"));
  await first;
  // beta's optimistic overlay survives alpha's failure.
  assert.deepEqual(fixture.store.aliasUnpublished, ["beta"]);
  assert.notEqual(fixture.store.aliasPublicationSaveError, "");
  await second;
  assert.deepEqual(fixture.store.aliasUnpublished.sort(), ["beta"]);
});

test("same-row duplicate toggles dispatch once", async () => {
  setupControlPlane(7, 99);
  const fixture = publicationFixture([]);
  await loadInitial(fixture);

  const first = fixture.store.setAliasPublished("alpha", false);
  const duplicate = fixture.store.setAliasPublished("ALPHA", false);
  await Promise.all([first, duplicate]);
  assert.equal(fixture.patches().length, 1);
  assert.deepEqual(fixture.store.aliasUnpublished, ["alpha"]);
});

test("a failed initial read keeps the switch disabled without discarding later success", async () => {
  setupControlPlane(7, 99);
  let failReads = true;
  let current: string[] = [];
  installFetchMock((req) => {
    if (!req.url.endsWith("/alias-publication")) throw new Error(`unexpected request ${req.url}`);
    if (req.method === "GET") {
      if (failReads) throw new Error("read down");
      return publicationResponse(current, 7);
    }
    current = [...current, String(req.body?.publicModel ?? "")];
    return publicationResponse(current, 8);
  });
  const store = useProvidersStore();
  await store.loadAliasPublication();
  assert.equal(store.aliasPublicationReady, false);
  assert.notEqual(store.aliasPublicationLoadError, "");

  failReads = false;
  await store.loadAliasPublication();
  assert.equal(store.aliasPublicationReady, true);
  assert.equal(store.aliasPublicationLoadError, "");
  // A failed later read retains the last committed list.
  failReads = true;
  await store.loadAliasPublication();
  assert.equal(store.aliasPublicationReady, true);
  assert.notEqual(store.aliasPublicationLoadError, "");
});

test("logout clears publication state and a late receipt cannot commit", async () => {
  setupControlPlane(7, 99);
  const fixture = publicationFixture([], ["alpha"]);
  await loadInitial(fixture);
  assert.deepEqual(fixture.store.aliasUnpublished, []);

  const pending = fixture.store.setAliasPublished("alpha", false);
  await flush();
  assert.deepEqual(fixture.store.aliasUnpublished, ["alpha"]);
  fixture.store.clear();
  assert.equal(fixture.store.aliasPublicationReady, false);
  assert.deepEqual(fixture.store.aliasUnpublished, []);
  assert.deepEqual(fixture.store.aliasPublicationPending, []);

  fixture.gates.get("alpha")![0]!.resolve({});
  await pending;
  // The receipt landed on the wire before logout but must not repopulate state.
  assert.equal(fixture.store.aliasPublicationReady, false);
  assert.deepEqual(fixture.store.aliasUnpublished, []);
});
