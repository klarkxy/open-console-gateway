import assert from "node:assert/strict";
import test from "node:test";
import { createRevalidateGate } from "./revalidate.ts";

test("the first revalidation always runs and the TTL window suppresses repeats", () => {
  const gate = createRevalidateGate(15_000);
  assert.equal(gate.shouldRun(1_000), true);
  gate.record(1_000);
  assert.equal(gate.shouldRun(15_999), false);
  assert.equal(gate.shouldRun(16_000), true);
});

test("record restarts the window and reset reopens the gate", () => {
  const gate = createRevalidateGate(15_000);
  gate.record(10_000);
  gate.record(20_000);
  assert.equal(gate.shouldRun(34_999), false);
  assert.equal(gate.shouldRun(35_000), true);
  gate.reset();
  assert.equal(gate.shouldRun(20_500), true);
});
