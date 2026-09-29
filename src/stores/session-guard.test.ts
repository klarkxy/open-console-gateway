import assert from "node:assert/strict";
import test from "node:test";
import { createPinia, setActivePinia } from "pinia";
import { installWindowDashboard } from "../test-helpers/dashboard-v3-fetch.ts";
import { useConnectionStore } from "./connection.ts";
import { useControlPlaneStore } from "./controlPlane.ts";

/**
 * Isolated from review-races.test.ts, which hangs under the worktree's node
 * test runner. Covers the session guard on primary-key rotation.
 */

interface DeferredCall {
  url: string;
  method: string;
  resolve: (body: object) => void;
}

function installDeferredFetch(): DeferredCall[] {
  installWindowDashboard();
  const calls: DeferredCall[] = [];
  Object.defineProperty(globalThis, "fetch", {
    configurable: true,
    value: (input: string, init: RequestInit = {}) => new Promise<Response>((resolvePromise) => {
      calls.push({
        url: String(input),
        method: init.method ?? "GET",
        resolve: (body) => resolvePromise(new Response(
          JSON.stringify(body),
          { headers: { "Content-Type": "application/json" } },
        )),
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

test("regeneratePrimaryKey does not refetch plaintext after logout", async () => {
  setActivePinia(createPinia());
  useControlPlaneStore().sync({ revision: 7, processGeneration: 99, pricingRevision: null });
  const calls = installDeferredFetch();
  const store = useConnectionStore();

  const rotation = store.regeneratePrimaryKey();
  await waitForCalls(calls, 1);
  assert.equal(calls[0]!.method, "POST");
  // Session teardown lands while the mutation is still in flight.
  store.clearSecrets();
  calls[0]!.resolve({ revision: 8, processGeneration: 99 });

  await assert.rejects(rotation, /session ended/);
  assert.equal(calls.length, 1, "must not start GET /connection after session teardown");
  assert.equal(store.info, null);
  assert.equal(store.loading, false);
});
