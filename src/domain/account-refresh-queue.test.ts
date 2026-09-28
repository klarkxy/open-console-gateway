import assert from "node:assert/strict";
import test from "node:test";
import { createAccountRefreshQueue, platformRefreshBinding, waitForAccountRefreshIdle } from "./account-refresh-queue.ts";
import type { PlatformAccount } from "../api/platform-accounts.ts";
import { ref } from "vue";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(yes => { resolve = yes; });
  return { promise, resolve };
}

test("manual and automatic jobs run serially and duplicate accounts share completion", async () => {
  const states = new Map();
  const queue = createAccountRefreshQueue((id, state) => state ? states.set(id, state) : states.delete(id));
  const gate = deferred();
  const calls: string[] = [];
  const first = queue.enqueue("auto-a", () => true, async () => { calls.push("a"); await gate.promise; });
  const second = queue.enqueue("manual-b", () => true, async () => { calls.push("b"); });
  assert.equal(queue.enqueue("manual-b", () => true, async () => { calls.push("duplicate"); }), second);
  assert.deepEqual(calls, ["a"]);
  assert.equal(states.get("auto-a"), "running");
  assert.equal(states.get("manual-b"), "queued");
  gate.resolve();
  await Promise.all([first, second]);
  assert.deepEqual(calls, ["a", "b"]);
  assert.equal(states.size, 0);
});

test("a failed refresh rejects its caller and the next account still runs", async () => {
  const queue = createAccountRefreshQueue(() => {});
  const gate = deferred();
  const first = queue.enqueue("a", () => true, async () => { await gate.promise; throw new Error("offline"); });
  let ran = false;
  const second = queue.enqueue("b", () => true, async () => { ran = true; });
  const rejected = assert.rejects(first, /offline/);
  gate.resolve();
  await Promise.all([rejected, second]);
  assert.equal(ran, true);
});

test("deletion or rebinding before execution skips obsolete queued work", async () => {
  const queue = createAccountRefreshQueue(() => {});
  const gate = deferred();
  let binding = "old";
  const first = queue.enqueue("a", () => true, () => gate.promise);
  let ran = false;
  const second = queue.enqueue("b", () => binding === "old", async () => { ran = true; });
  binding = "new";
  gate.resolve();
  await Promise.all([first, second]);
  assert.equal(ran, false);
});

test("logout clears waiting jobs and stale completion cannot clear a new session job", async () => {
  const states = new Map();
  const queue = createAccountRefreshQueue((id, state) => state ? states.set(id, state) : states.delete(id));
  const old = deferred();
  const fresh = deferred();
  let oldCurrent!: () => boolean;
  const first = queue.enqueue("a", () => true, async current => { oldCurrent = current; await old.promise; });
  let waitingRan = false;
  const waiting = queue.enqueue("b", () => true, async () => { waitingRan = true; });
  queue.reset();
  await waiting;
  assert.equal(waitingRan, false);
  assert.equal(oldCurrent(), false);
  assert.equal(states.size, 0);
  const next = queue.enqueue("a", () => true, () => fresh.promise);
  assert.equal(states.get("a"), "queued");
  old.resolve();
  await first;
  assert.equal(states.get("a"), "running");
  fresh.resolve();
  await next;
  assert.equal(states.size, 0);
});

test("a parent observation revision does not discard the next queued linked Key", async () => {
  const queue = createAccountRefreshQueue(() => {});
  const gate = deferred();
  let parent = { id: "p", kind: "new_api", baseUrl: "https://example.test", version: 1 } as PlatformAccount;
  const binding = platformRefreshBinding(parent);
  const first = queue.enqueue("platform:p", () => true, async () => {
    await gate.promise;
    parent = { ...parent, version: 2 };
  });
  let childRan = false;
  const child = queue.enqueue("key", () => platformRefreshBinding(parent) === binding, async () => { childRan = true; });
  gate.resolve();
  await Promise.all([first, child]);
  assert.equal(childRan, true);
  assert.notEqual(platformRefreshBinding({ ...parent, baseUrl: "https://different.test" }), binding);
  assert.notEqual(platformRefreshBinding(undefined), binding);
});

test("a queued platform refresh waits for an unrelated edit, then executes once", async () => {
  const blocked = ref(true);
  const current = ref(true);
  const queue = createAccountRefreshQueue(() => {});
  const calls: string[] = [];
  const first = queue.enqueue("a", () => current.value, async valid => {
    if (await waitForAccountRefreshIdle(() => blocked.value, valid)) calls.push("a");
  });
  const next = queue.enqueue("b", () => true, async () => { calls.push("b"); });
  await Promise.resolve();
  assert.deepEqual(calls, []);
  blocked.value = false;
  await Promise.all([first, next]);
  assert.deepEqual(calls, ["a", "b"]);
});

test("logout releases a lock wait without sending the waiting refresh", async () => {
  const blocked = ref(true);
  const current = ref(true);
  const waiting = waitForAccountRefreshIdle(() => blocked.value, () => current.value);
  current.value = false;
  assert.equal(await waiting, false);
});
