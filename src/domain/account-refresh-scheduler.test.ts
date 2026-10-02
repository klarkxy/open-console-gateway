import assert from "node:assert/strict";
import test from "node:test";
import { createAccountRefreshScheduler } from "./account-refresh-scheduler.ts";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(yes => { resolve = yes; });
  return { promise, resolve };
}
const shared = { exclusive: false, priority: "background" } as const;

test("four observations run concurrently and fast accounts do not wait for a slow one", async () => {
  const states = new Map();
  const queue = createAccountRefreshScheduler((id, state) => state ? states.set(id, state) : states.delete(id));
  const gates = Array.from({ length: 6 }, deferred);
  const calls: number[] = [];
  const jobs = gates.map((gate, index) => queue.enqueue(String(index), () => true, async () => {
    calls.push(index); await gate.promise;
  }, shared));
  assert.deepEqual(calls, [0, 1, 2, 3]);
  gates[1]!.resolve(); await jobs[1];
  assert.deepEqual(calls, [0, 1, 2, 3, 4]);
  assert.equal(states.get("0"), "running");
  assert.equal(states.has("1"), false);
  gates.forEach(gate => gate.resolve()); await Promise.all(jobs);
  assert.equal(states.size, 0);
});

test("exclusive writes fence observations and preserve the serial default", async () => {
  const queue = createAccountRefreshScheduler(() => {});
  const firstGate = deferred(); const writeGate = deferred();
  const calls: string[] = [];
  const first = queue.enqueue("quota", () => true, async () => { calls.push("quota"); await firstGate.promise; }, shared);
  const write = queue.enqueue("models", () => true, async () => { calls.push("models"); await writeGate.promise; });
  const next = queue.enqueue("next", () => true, async () => { calls.push("next"); }, shared);
  assert.deepEqual(calls, ["quota"]);
  firstGate.resolve(); await first;
  assert.deepEqual(calls, ["quota", "models"]);
  writeGate.resolve(); await Promise.all([write, next]);
  assert.deepEqual(calls, ["quota", "models", "next"]);
});

test("a manual duplicate promotes queued background work without a second request", async () => {
  const queue = createAccountRefreshScheduler(() => {}, 1);
  const gate = deferred(); const calls: string[] = [];
  const first = queue.enqueue("a", () => true, () => gate.promise, shared);
  const background = queue.enqueue("b", () => true, async () => { calls.push("b"); }, shared);
  const promoted = queue.enqueue("c", () => true, async () => { calls.push("c"); }, shared);
  assert.equal(queue.enqueue("c", () => true, async () => { assert.fail("duplicate ran"); }, { exclusive: false }), promoted);
  gate.resolve(); await Promise.all([first, background, promoted]);
  assert.deepEqual(calls, ["c", "b"]);
});

test("rejection and stale pending jobs release capacity", async () => {
  const queue = createAccountRefreshScheduler(() => {}, 1);
  const gate = deferred(); let current = true;
  const first = queue.enqueue("a", () => true, async () => { await gate.promise; throw new Error("offline"); }, shared);
  const rejected = assert.rejects(first, /offline/);
  const stale = queue.enqueue("b", () => current, async () => { assert.fail("stale ran"); }, shared);
  let ran = false;
  const next = queue.enqueue("c", () => true, async () => { ran = true; }, shared);
  current = false; gate.resolve();
  await Promise.all([rejected, stale, next]); assert.equal(ran, true);
});

test("reset preserves the concurrency bound and old completion cannot clear a new job", async () => {
  const states = new Map();
  const queue = createAccountRefreshScheduler((id, state) => state ? states.set(id, state) : states.delete(id), 1);
  const old = deferred(); const fresh = deferred(); let oldCurrent!: () => boolean;
  const first = queue.enqueue("a", () => true, async current => { oldCurrent = current; await old.promise; }, shared);
  const waiting = queue.enqueue("b", () => true, async () => { assert.fail("logged-out job ran"); }, shared);
  queue.reset(); await waiting; assert.equal(oldCurrent(), false);
  const next = queue.enqueue("a", () => true, () => fresh.promise, shared);
  assert.equal(states.get("a"), "queued");
  old.resolve(); await first; assert.equal(states.get("a"), "running");
  fresh.resolve(); await next; assert.equal(states.size, 0);
});

test("invalid concurrency fails instead of leaving jobs permanently queued", () => {
  for (const limit of [0, -1, 1.5, NaN, Infinity]) {
    assert.throws(() => createAccountRefreshScheduler(() => {}, limit), RangeError);
  }
});
