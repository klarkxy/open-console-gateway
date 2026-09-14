import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useControlPlaneStore } from "./controlPlane.ts";
import { useIdentitiesStore } from "./identities.ts";
import type { IdentitySummary } from "../api/generated/dashboard-v4.ts";

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

function identitySummary(accountId: string, siblingId?: string): IdentitySummary {
  const credential = (id: string, legacyId: string): IdentitySummary["credentials"][number] => ({
    bindings: [{
      allowedEndpointIds: [],
      allowedOrigins: [],
      connectionId: "conn-1",
      enabled: true,
      id: `bind-${legacyId}`,
      modelScope: { kind: "all" },
      routingRank: 0,
    }],
    credential: {
      authState: "unknown",
      authStateVersion: 1,
      enabled: true,
      expiresAt: null,
      hasMaterial: true,
      id,
      materialKind: "api_key",
      purpose: "inference",
      secretRef: "opaque",
      version: 1,
    },
    lastError: null,
    legacy: { id: legacyId, kind: "account" },
    onboardingTask: null,
    quotaPoolId: null,
    quotaWindows: [],
    subject: "account_credential",
    subscription: null,
  });
  return {
    credentials: siblingId
      ? [credential("cred-1", accountId), credential("cred-2", siblingId)]
      : [credential("cred-1", accountId)],
    declaredRelations: [],
    identity: {
      authorityRef: null,
      enabled: true,
      id: "ident-1",
      identityConfidence: "opaque",
      label: "Go",
      notes: null,
    },
    legacy: { id: accountId, kind: "account" },
  };
}

function listBody(accountId: string, revision: number, siblingId?: string): object {
  return {
    identities: [identitySummary(accountId, siblingId)],
    revision: { revision, processGeneration: 99, pricingRevision: "p1" },
  };
}

test("identities store: an older load resolving last does not clobber newer state", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useIdentitiesStore();

  const first = store.loadPresented();
  const second = store.loadPresented();
  await waitForCalls(calls, 2);

  calls[1]!.resolve(listBody("acc-b", 8, "acc-c"));
  await second;
  assert.equal(store.byAccountId.get("acc-b")?.legacy.id, "acc-b");
  assert.equal(store.byAccountId.get("acc-c")?.legacy.id, "acc-b");
  assert.equal(store.loading, false);

  calls[0]!.resolve(listBody("acc-a", 7));
  const stale = await first;
  assert.equal(stale[0]?.legacy.id, "acc-a");
  assert.equal(store.byAccountId.get("acc-a"), undefined);
  assert.equal(store.byAccountId.get("acc-b")?.identity.id, "ident-1");
  assert.equal(store.error, "");
});

test("identities store: a stale load failure does not overwrite a fresh success", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useIdentitiesStore();

  const first = store.loadPresented();
  const second = store.loadPresented();
  await waitForCalls(calls, 2);

  calls[1]!.resolve(listBody("acc-b", 8));
  await second;
  calls[0]!.reject(new Error("stale failure"));
  await assert.rejects(first, /stale failure/);

  assert.equal(store.error, "");
  assert.equal(store.byAccountId.get("acc-b")?.legacy.id, "acc-b");
  assert.equal(store.loading, false);
});

test("identities store applies identities and the GET pair atomically and ignores a stale generation", async () => {
  setActivePinia(createPinia());
  const control = useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useIdentitiesStore();

  const first = store.loadPresented();
  const second = store.loadPresented();
  await waitForCalls(calls, 2);

  calls[1]!.resolve(listBody("acc-b", 8, "acc-c"));
  await second;
  assert.deepEqual(store.snapshotExpectation, { expectedRevision: 8, processGeneration: 99 });
  assert.equal(store.byAccountId.get("acc-b")?.legacy.id, "acc-b");

  control.sync({ revision: 12, processGeneration: 99, pricingRevision: "p9" });
  assert.deepEqual(store.snapshotExpectation, { expectedRevision: 8, processGeneration: 99 });
  assert.equal(control.revision, 12);

  calls[0]!.resolve(listBody("acc-a", 7));
  await first;
  assert.equal(store.byAccountId.get("acc-a"), undefined);
  assert.deepEqual(store.snapshotExpectation, { expectedRevision: 8, processGeneration: 99 });
});

test("identities store: a stale load error does not apply a partial snapshot pair", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useIdentitiesStore();

  const first = store.loadPresented();
  const second = store.loadPresented();
  await waitForCalls(calls, 2);

  calls[1]!.resolve(listBody("acc-b", 8));
  await second;

  calls[0]!.reject(new Error("stale failure"));
  await assert.rejects(first, /stale failure/);
  assert.deepEqual(store.snapshotExpectation, { expectedRevision: 8, processGeneration: 99 });
});

test("identities store: a failed load does not write identities without a pair", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore();
  const calls = installDeferredFetch();
  const store = useIdentitiesStore();

  const pending = store.loadPresented();
  await waitForCalls(calls, 1);
  calls[0]!.reject(new Error("unavailable"));
  await assert.rejects(pending, /unavailable/);
  assert.equal(store.error, "unavailable");
  assert.equal(store.snapshotExpectation, null);
  assert.equal(store.identities.length, 0);
  assert.equal(store.loaded, false);
});
