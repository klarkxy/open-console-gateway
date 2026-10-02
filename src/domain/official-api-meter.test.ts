import assert from "node:assert/strict";
import test from "node:test";
import { officialApiAccountMeter } from "./official-api-meter.ts";

test("unavailable balance never invents a wallet figure", () => {
  const meter = officialApiAccountMeter({
    balanceAvailable: false,
    balances: [{ currency: "CNY", granted: 1, observedAt: "2026-09-20T08:00:00Z", toppedUp: 2, total: 3 }],
  });
  assert.equal(meter.remainingEmpty, "unavailable");
  assert.equal(meter.remaining.length, 0);
});

test("an unused public balance API is empty rather than zero", () => {
  const meter = officialApiAccountMeter({
    balanceAvailable: true,
    balances: [],
  });
  assert.equal(meter.remainingEmpty, "not_queried");
  assert.equal(meter.remaining.length, 0);
});

test("gift is omitted when granted is zero and kept when it adds a fact", () => {
  const none = officialApiAccountMeter({
    balanceAvailable: true,
    balances: [{
      currency: "CNY",
      granted: 0,
      observedAt: "2026-09-20T08:07:06Z",
      toppedUp: 9.19,
      total: 9.19,
    }],
  });
  assert.equal(none.remainingEmpty, null);
  assert.equal(none.remaining[0]?.total, 9.19);
  assert.equal(none.remaining[0]?.gift, null);

  const gift = officialApiAccountMeter({
    balanceAvailable: true,
    balances: [{
      currency: "CNY",
      granted: 1.5,
      observedAt: "2026-09-20T08:07:06Z",
      toppedUp: 7.69,
      total: 9.19,
    }],
  });
  assert.equal(gift.remainingEmpty, null);
  assert.equal(gift.remaining[0]?.gift, 1.5);
  assert.equal(gift.remaining[0]?.total, 9.19);
});

test("a non-finite balance is empty rather than a zero wallet", () => {
  const dropped = officialApiAccountMeter({
    balanceAvailable: true,
    balances: [{
      currency: "CNY",
      granted: 1,
      observedAt: "2026-09-20T08:00:00Z",
      toppedUp: 1,
      total: Number.NaN,
    }],
  });
  assert.equal(dropped.remainingEmpty, "not_queried");
  assert.equal(dropped.remaining.length, 0);

  const kept = officialApiAccountMeter({
    balanceAvailable: true,
    balances: [
      {
        currency: "CNY",
        granted: 1,
        observedAt: "2026-09-20T08:00:00Z",
        toppedUp: 1,
        total: Number.NaN,
      },
      {
        currency: "USD",
        granted: 0,
        observedAt: "2026-09-20T08:00:00Z",
        toppedUp: 4.5,
        total: 4.5,
      },
    ],
  });
  assert.equal(kept.remainingEmpty, null);
  assert.equal(kept.remaining.length, 1);
  assert.equal(kept.remaining[0]?.total, 4.5);
  assert.equal(kept.remaining[0]?.gift, null);
});
