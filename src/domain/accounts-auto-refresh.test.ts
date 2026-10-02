import assert from "node:assert/strict";
import test from "node:test";
import { ACCOUNT_AUTO_REFRESH_MS, billingObservedAt, createAccountsAutoRefresh, type AccountRefreshTarget } from "./accounts-auto-refresh.ts";
import type { BillingStatus } from "../api/billing.ts";

function fixture(concurrency = 1) {
  let at = 1_800_000_000_000;
  let allowed = true;
  const calls: string[] = [];
  let targets: AccountRefreshTarget[] = ["a", "b", "c"].map(id => ({
    id, binding: "v1", observedAt: 0, nextAllowedAt: 0, busy: false,
    refresh: async () => { calls.push(id); },
  }));
  const controller = createAccountsAutoRefresh({ allowed: () => allowed, targets: () => targets, now: () => at, concurrency });
  return { controller, calls, targets, now: () => at,
    advance: (ms = ACCOUNT_AUTO_REFRESH_MS) => { at += ms; },
    allow: (value: boolean) => { allowed = value; },
    replace: (value: AccountRefreshTarget[]) => { targets = value; },
  };
}

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(yes => { resolve = yes; });
  return { promise, resolve };
}

test("indexed targets are scanned once and rechecked individually after I/O", async () => {
  let scans = 0;
  let lookups = 0;
  const targets = ["a", "b"].map((id) => ({
    id, binding: "v1", observedAt: 0, nextAllowedAt: 0, busy: false,
    refresh: async () => {},
  }));
  const controller = createAccountsAutoRefresh({
    allowed: () => true,
    targets: () => ({
      ids: () => { scans += 1; return targets.map((target) => target.id); },
      current: (id) => { lookups += 1; return targets.find((target) => target.id === id); },
    }),
  });
  await controller.run();
  assert.equal(scans, 1);
  assert.equal(lookups, 2);
});

test("all due accounts refresh once; entry and timer ticks share freshness", async () => {
  const f = fixture();
  await f.controller.run(); await f.controller.run();
  assert.deepEqual(f.calls, ["a", "b", "c"]);
  f.advance(); await f.controller.run();
  assert.deepEqual(f.calls, ["a", "b", "c", "a", "b", "c"]);
});

test("fresh observations, busy rows and server throttle defer work", async () => {
  const f = fixture();
  f.targets[0]!.observedAt = f.now();
  f.targets[1]!.busy = true;
  f.targets[2]!.nextAllowedAt = f.now() + 2 * ACCOUNT_AUTO_REFRESH_MS;
  await f.controller.run(); assert.deepEqual(f.calls, []);
  f.advance(); f.targets[1]!.busy = false;
  await f.controller.run(); assert.deepEqual(f.calls, ["a", "b"]);
  f.advance(); await f.controller.run(); assert.deepEqual(f.calls, ["a", "b", "a", "b", "c"]);
});

test("failures preserve progress and back off from completion", async () => {
  const f = fixture();
  f.targets[0]!.refresh = async () => { f.calls.push("a"); f.advance(); throw new Error("offline"); };
  await f.controller.run(); await f.controller.run();
  assert.deepEqual(f.calls, ["a", "b", "c"]);
});

test("overlapping triggers share a pass and hiding stops unstarted work", async () => {
  const f = fixture();
  const gate = deferred();
  f.targets[0]!.refresh = async () => { f.calls.push("a"); await gate.promise; };
  const first = f.controller.run();
  await f.controller.run(); assert.deepEqual(f.calls, ["a"]);
  f.allow(false); gate.resolve(); await first;
  assert.deepEqual(f.calls, ["a"]);
  f.allow(true); await f.controller.run(); assert.deepEqual(f.calls, ["a", "b", "c"]);
});

test("deactivation and logout invalidate an in-flight continuation", async () => {
  for (const operation of ["pause", "reset"] as const) {
    const f = fixture(); const gate = deferred(); let current!: () => boolean;
    f.targets[0]!.refresh = async guard => { current = guard; await gate.promise; };
    const first = f.controller.run(); f.controller[operation]();
    assert.equal(current(), false); gate.resolve(); await first;
    assert.deepEqual(f.calls, []);
  }
});

test("targets are rechecked after I/O; rebound accounts bypass old attempt state", async () => {
  const f = fixture();
  f.targets[0]!.refresh = async () => { f.calls.push("a"); f.replace([f.targets[0]!, f.targets[2]!]); };
  await f.controller.run(); assert.deepEqual(f.calls, ["a", "c"]);
  f.targets[2]!.binding = "v2";
  await f.controller.run(); assert.deepEqual(f.calls, ["a", "c", "c"]);
});

test("local estimate updates do not count as an upstream observation", () => {
  const status = { usage: { syncState: null, quotaWindows: [{ observedAt: null, updatedAt: "2026-09-25T10:00:00Z" }], creditBalances: [] }, cash: null } as unknown as BillingStatus;
  assert.equal(billingObservedAt(status), 0);
  status.usage!.quotaWindows[0]!.observedAt = "2026-09-25T09:00:00Z";
  assert.equal(billingObservedAt(status), Date.parse("2026-09-25T09:00:00Z"));
});

test("automatic attempts reconcile the shared projection only once per pass", async () => {
  const events: string[] = [];
  const controller = createAccountsAutoRefresh({
    allowed: () => true,
    targets: () => ["a", "b"].map(id => ({
      id, binding: "v1", observedAt: 0, nextAllowedAt: 0, busy: false,
      refresh: async () => { events.push(id); if (id === "b") throw new Error("offline"); },
    })),
    afterRefresh: async () => { events.push("projection"); },
  });
  await controller.run();
  assert.deepEqual(events, ["a", "b", "projection"]);
});

test("a four-worker pool advances fast accounts before the slow first account completes", async () => {
  const gates = Array.from({ length: 6 }, deferred);
  const calls: number[] = [];
  let projections = 0;
  const targets = gates.map((gate, index) => ({
    id: String(index), binding: "v1", observedAt: 0, nextAllowedAt: 0, busy: false,
    refresh: async () => { calls.push(index); await gate.promise; },
  }));
  const controller = createAccountsAutoRefresh({
    allowed: () => true, targets: () => targets, concurrency: 4,
    afterRefresh: async () => { projections++; },
  });
  const pass = controller.run();
  assert.deepEqual(calls, [0, 1, 2, 3]);
  gates[1]!.resolve(); await Promise.resolve(); await Promise.resolve();
  assert.deepEqual(calls, [0, 1, 2, 3, 4]);
  assert.equal(projections, 0);
  gates.forEach(gate => gate.resolve()); await pass;
  assert.equal(projections, 1);
});

test("reset starts a new session without an old completion clearing its running identity", async () => {
  const old = deferred(); const fresh = deferred();
  let count = 0; let projections = 0;
  const target: AccountRefreshTarget = {
    id: "a", binding: "v1", observedAt: 0, nextAllowedAt: 0, busy: false,
    refresh: () => ++count === 1 ? old.promise : fresh.promise,
  };
  const controller = createAccountsAutoRefresh({
    allowed: () => true, targets: () => [target],
    afterRefresh: async () => { projections++; },
  });
  const first = controller.run(); controller.reset();
  const second = controller.run(); assert.equal(count, 2);
  old.resolve(); await first;
  await controller.run(); assert.equal(count, 2); assert.equal(projections, 0);
  fresh.resolve(); await second; assert.equal(projections, 1);
});

test("a queued continuation uses a captured binding even if the original object is mutated", async () => {
  const f = fixture(); const gate = deferred(); let current!: () => boolean;
  f.targets[0]!.refresh = async guard => { current = guard; await gate.promise; };
  const pass = f.controller.run();
  f.targets[0]!.binding = "v2";
  assert.equal(current(), false);
  gate.resolve(); await pass;
});
