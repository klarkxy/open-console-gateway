import assert from "node:assert/strict";
import test from "node:test";
import { createReadLifecycle, readProcessIsCurrent, readSnapshotIsCurrent } from "./readLifecycle.ts";

test("read process fence accepts discovery payloads and rejects abandoned origins or unbound late payloads", () => {
  assert.equal(readProcessIsCurrent(100, 100, 99), true);
  assert.equal(readProcessIsCurrent(99, 100, 99), false);
  assert.equal(readProcessIsCurrent(99, 99, 99), true);
  assert.equal(readProcessIsCurrent(100, 100, null), true);
  assert.equal(readProcessIsCurrent(undefined, 100, 99), false);
  assert.equal(readProcessIsCurrent(undefined, 99, 99), true);
  assert.equal(readProcessIsCurrent(undefined, null, null), true);
});

test("full reads reject a revision behind the current process while identity-less fixtures require a stable origin", () => {
  assert.equal(readSnapshotIsCurrent(99, 8, { processGeneration: 99, revision: 9 }, 99), false);
  assert.equal(readSnapshotIsCurrent(99, 9, { processGeneration: 99, revision: 9 }, 99), true);
  assert.equal(readSnapshotIsCurrent(100, 1, { processGeneration: 100, revision: 1 }, 99), true);
  assert.equal(readSnapshotIsCurrent(undefined, undefined, { processGeneration: 99, revision: 9 }, 99), true);
  assert.equal(readSnapshotIsCurrent(undefined, undefined, { processGeneration: 100, revision: 1 }, 99), false);
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test("identical pending reads share a flight; default reads never reuse completed freshness", async () => {
  const reads = createReadLifecycle(() => 100);
  const gate = deferred<number>();
  let count = 0;
  const read = async () => { count++; const value = await gate.promise; reads.markSuccessful("row"); return value; };
  const first = reads.run("row", undefined, () => 1, read);
  const second = reads.run("row", undefined, () => 1, read);
  assert.equal(first, second);
  gate.resolve(2);
  assert.deepEqual(await Promise.all([first, second]), [2, 2]);
  assert.equal(await reads.run("row", { maxAgeMs: 10 }, () => 3, read), 3);
  assert.equal(count, 1);
  await reads.run("row", undefined, () => 3, read);
  assert.equal(count, 2);
});

test("failure, clock reversal and expiration cannot qualify as fresh", async () => {
  let time = 100;
  const reads = createReadLifecycle(() => time);
  let count = 0;
  const read = async () => { count++; reads.markSuccessful("row"); return count; };
  await reads.run("row", undefined, () => 0, read);
  time = 99;
  await reads.run("row", { maxAgeMs: 10 }, () => 0, read);
  time = 109;
  await reads.run("row", { maxAgeMs: 10 }, () => 0, read);
  await assert.rejects(reads.run("row", undefined, () => 0, async () => { throw new Error("offline"); }));
  await reads.run("row", { maxAgeMs: 10 }, () => 0, read);
  assert.equal(count, 4);
});

test("invalidating a resource detaches its flight without releasing a newer pending read", async () => {
  const reads = createReadLifecycle();
  const old = deferred<number>();
  const next = deferred<number>();
  const first = reads.run("row", undefined, () => 0, () => old.promise);
  reads.invalidate("row");
  const second = reads.run("row", undefined, () => 0, () => next.promise);
  old.resolve(1);
  await first;
  const joined = reads.run("row", undefined, () => 0, async () => 3);
  assert.equal(joined, second);
  next.resolve(2);
  assert.equal(await joined, 2);
});
