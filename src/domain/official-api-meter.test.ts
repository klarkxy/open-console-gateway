import assert from "node:assert/strict";
import test from "node:test";
import {
  joinOfficialApiSpend,
  officialApiAccountMeter,
} from "./official-api-meter.ts";

test("unavailable balance never invents a wallet figure", () => {
  const meter = officialApiAccountMeter({
    balanceAvailable: false,
    balances: [{ currency: "CNY", granted: 1, observedAt: "2026-09-20T08:00:00Z", toppedUp: 2, total: 3 }],
    monthSpend: [{ amount: 1.5, currency: "CNY", pricedRequests: 4 }],
    lifetimeSpend: [{ amount: 9.2, currency: "CNY", pricedRequests: 12 }],
    unpricedRequests: 2,
  });
  assert.equal(meter.remainingEmpty, "unavailable");
  assert.equal(meter.remaining.length, 0);
  assert.equal(meter.monthSpend.length, 1);
  assert.equal(meter.lifetimeSpend[0]?.amount, 9.2);
  assert.equal(meter.unpriced, 2);
});

test("an unused public balance API is empty rather than zero", () => {
  const meter = officialApiAccountMeter({
    balanceAvailable: true,
    balances: [],
    monthSpend: [],
    lifetimeSpend: [],
    unpricedRequests: 0,
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
    monthSpend: [],
    lifetimeSpend: [],
    unpricedRequests: 0,
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
    monthSpend: [],
    lifetimeSpend: [{ amount: 2, currency: "CNY", pricedRequests: 3 }],
    unpricedRequests: 0,
  });
  assert.equal(gift.remaining[0]?.gift, 1.5);
  assert.equal(gift.lifetimeSpend.length, 1);
});

test("month spend joins original currencies and skips empty rows", () => {
  assert.equal(joinOfficialApiSpend([], (value, unit) => `${value} ${unit}`), null);
  assert.equal(
    joinOfficialApiSpend(
      [
        { amount: 1.2, currency: "CNY", pricedRequests: 3 },
        { amount: 0.4, currency: "USD", pricedRequests: 1 },
      ],
      (value, unit) => `${value} ${unit}`,
    ),
    "1.2 CNY · 0.4 USD",
  );
});
