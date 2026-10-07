import assert from "node:assert/strict";
import test from "node:test";
import { officialApiAccountMeter } from "./official-api-meter.ts";

test("official balances use the server meter without inventing a missing balance", () => {
  const meter = { remainingEmpty: "not_queried" as const, remaining: [] };
  assert.equal(officialApiAccountMeter({ meter }), meter);
});

test("a recorded zero and its native currency survive presentation", () => {
  const meter = { remainingEmpty: null, remaining: [{ currency: "USD", total: 0, gift: null, observedAt: "2026-10-07T00:00:00Z" }] };
  assert.equal(officialApiAccountMeter({ meter }), meter);
  assert.equal(officialApiAccountMeter({ meter }).remaining[0]?.total, 0);
});
