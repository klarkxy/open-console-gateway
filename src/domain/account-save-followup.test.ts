import assert from "node:assert/strict";
import test from "node:test";
import { runAccountSaveFollowup, type AccountSaveFollowup } from "./account-save-followup.ts";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

function fixture(overrides: Partial<AccountSaveFollowup> = {}) {
  const calls: string[] = [];
  const state = { current: true, notified: 0 };
  const input: AccountSaveFollowup = {
    kind: "created", accountId: "acct-1", hasUsageDisplay: true, usageReady: true,
    isCurrent: () => state.current,
    refreshProjection: async () => { calls.push("projection"); return true; },
    notifyProjectionFailure: () => { state.notified++; },
    loadConnections: async () => { calls.push("connections"); },
    refreshCatalogForNewProvider: async () => { calls.push("catalog"); },
    loadUsage: async id => { assert.equal(id, "acct-1"); calls.push("usage"); },
    ...overrides,
  };
  return { input, calls, state };
}

test("slow projection and failed catalog never postpone usage or reject a confirmed save", async () => {
  const projection = deferred<boolean>();
  const f = fixture({
    refreshProjection: () => projection.promise,
    refreshCatalogForNewProvider: async () => { throw new Error("catalog offline"); },
  });
  const done = runAccountSaveFollowup(f.input);
  assert.deepEqual(f.calls, ["connections", "usage"]);
  assert.equal(f.state.notified, 0);
  projection.resolve(false);
  await done;
  assert.equal(f.state.notified, 1);
});

test("a thrown projection failure reports independently of all other reads", async () => {
  const f = fixture({ refreshProjection: async () => { throw new Error("offline"); } });
  await runAccountSaveFollowup(f.input);
  assert.deepEqual(f.calls, ["connections", "catalog", "usage"]);
  assert.equal(f.state.notified, 1);
});

test("a stale context starts no reads and a late failure reports nothing after logout", async () => {
  const projection = deferred<boolean>();
  const f = fixture({ refreshProjection: () => projection.promise });
  const done = runAccountSaveFollowup(f.input);
  f.state.current = false;
  const started = [...f.calls];
  await runAccountSaveFollowup(f.input);
  assert.deepEqual(f.calls, started);
  projection.resolve(false);
  await done;
  assert.equal(f.state.notified, 0);
});

test("updated saves only read the projection and current usage", async () => {
  const f = fixture({ kind: "updated" });
  await runAccountSaveFollowup(f.input);
  assert.deepEqual(f.calls, ["projection", "usage"]);
});

test("new accounts without ready usage skip the usage read", async () => {
  const f = fixture({ usageReady: false });
  await runAccountSaveFollowup(f.input);
  assert.deepEqual(f.calls, ["projection", "connections", "catalog"]);
});
