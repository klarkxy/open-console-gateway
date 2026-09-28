import assert from "node:assert/strict";
import test from "node:test";
import { isLocalMutationBusy } from "../stores/controlPlane.ts";
import { installFetchMock, setupControlPlane, v3AccountDto } from "../test-helpers/dashboard-v3-fetch.ts";
import { dashboardApi } from "./dashboard.ts";

function deferred<T>(): { promise: Promise<T>; resolve: (value: T | PromiseLike<T>) => void } {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

test("distinct verbs on one account never share a promise: delete behind update rejects busy", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<object>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/accounts/acct-1") && req.method === "PATCH") return gate.promise;
    if (req.url.endsWith("/accounts/acct-1") && req.method === "DELETE") {
      return { account: null, revision: 9, processGeneration: 99 };
    }
    throw new Error(`unexpected request ${req.method} ${req.url}`);
  });
  const update = dashboardApi.updateAccount("acct-1", { name: "Renamed" });
  // Same target while the update is in flight: every other intent — a
  // different verb (delete, toggle) or a duplicate update — rejects as busy
  // instead of being reported successful through the update's promise.
  await assert.rejects(dashboardApi.deleteAccount("acct-1"), isLocalMutationBusy);
  await assert.rejects(dashboardApi.toggleAccount("acct-1"), isLocalMutationBusy);
  await assert.rejects(dashboardApi.updateAccount("acct-1", { name: "Again" }), isLocalMutationBusy);
  gate.resolve({ account: v3AccountDto("acct-1", { name: "Renamed" }), revision: 8, processGeneration: 99 });
  const updated = await update;
  assert.equal(updated.name, "Renamed");
  // Only the PATCH was dispatched; the rejected intents left no trace.
  assert.deepEqual(requests.map((req) => req.method), ["PATCH"]);
  // Once the update settles, the target frees and the delete dispatches as
  // its own operation on the receipt's fresh tokens.
  await dashboardApi.deleteAccount("acct-1");
  assert.deepEqual(requests.map((req) => req.method), ["PATCH", "DELETE"]);
  assert.deepEqual(requests[1]?.body, { expectedRevision: 8, processGeneration: 99 });
});

test("writes for different accounts are not busy: they serialize on fresh CAS tokens", async () => {
  setupControlPlane(7, 99);
  const gate = deferred<object>();
  const requests = installFetchMock((req) => {
    if (req.url.endsWith("/accounts/acct-1") && req.method === "PATCH") return gate.promise;
    if (req.url.endsWith("/accounts/acct-2/toggle") && req.method === "POST") {
      return { account: v3AccountDto("acct-2", { id: "acct-2", enabled: false }), revision: 9, processGeneration: 99 };
    }
    throw new Error(`unexpected request ${req.method} ${req.url}`);
  });
  const first = dashboardApi.updateAccount("acct-1", { notes: "n" });
  const second = dashboardApi.toggleAccount("acct-2");
  await flush();
  // A different target is accepted, not rejected; it queues behind the lane.
  assert.deepEqual(requests.map((req) => req.method), ["PATCH"]);
  assert.deepEqual(requests[0]?.body, { notes: "n", expectedRevision: 7, processGeneration: 99 });
  gate.resolve({ account: v3AccountDto("acct-1"), revision: 8, processGeneration: 99 });
  await first;
  await second;
  // The toggle dispatched after the update receipt synced its revision, so
  // two quick edits cannot self-conflict on the shared CAS pair.
  assert.deepEqual(requests.map((req) => req.method), ["PATCH", "POST"]);
  assert.deepEqual(requests[1]?.body, { expectedRevision: 8, processGeneration: 99 });
});
