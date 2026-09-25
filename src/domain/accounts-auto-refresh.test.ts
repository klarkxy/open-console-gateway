import assert from "node:assert/strict";
import test from "node:test";
import { ACCOUNT_AUTO_REFRESH_MS, billingObservedAt, createAccountsAutoRefresh, type AccountRefreshTarget } from "./accounts-auto-refresh.ts";
import type { BillingStatus } from "../api/billing.ts";

function fixture() {
  let at = 1_800_000_000_000;
  let allowed = true;
  const calls: string[] = [];
  let targets: AccountRefreshTarget[] = ["a", "b", "c"].map(id => ({
    id, binding: "v1", observedAt: 0, nextAllowedAt: 0, busy: false,
    refresh: async () => { calls.push(id); },
  }));
  const controller = createAccountsAutoRefresh({ allowed: () => allowed, targets: () => targets, now: () => at });
  return { controller, calls, targets, now: () => at,
    advance: (ms = ACCOUNT_AUTO_REFRESH_MS) => { at += ms; },
    allow: (value: boolean) => { allowed = value; },
    replace: (value: AccountRefreshTarget[]) => { targets = value; },
  };
}

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

test("overlapping triggers stay serial and hiding stops the remaining pass", async () => {
  const f = fixture();
  let finish!: () => void;
  f.targets[0]!.refresh = async () => { f.calls.push("a"); await new Promise<void>(resolve => { finish = resolve; }); };
  const first = f.controller.run();
  await f.controller.run(); assert.deepEqual(f.calls, ["a"]);
  f.allow(false); finish(); await first;
  assert.deepEqual(f.calls, ["a"]);
  f.allow(true); await f.controller.run(); assert.deepEqual(f.calls, ["a", "b", "c"]);
});

test("deactivation and logout invalidate an in-flight continuation", async () => {
  for (const operation of ["pause", "reset"] as const) {
    const f = fixture(); let finish!: () => void; let current!: () => boolean;
    f.targets[0]!.refresh = async guard => { current = guard; await new Promise<void>(resolve => { finish = resolve; }); };
    const first = f.controller.run(); f.controller[operation]();
    assert.equal(current(), false); finish(); await first;
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


test("each automatic attempt reconciles the projection before another starts", async () => {
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
  assert.deepEqual(events, ["a", "projection", "b", "projection"]);
});
