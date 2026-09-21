import assert from "node:assert/strict";
import test from "node:test";
import {
  PAY_GO_METER_EMPTY,
  PAY_GO_METER_LABEL_KEYS,
  formatPayGoObservedAt,
} from "./pay-go-meter.ts";

test("pay-as-you-go meter labels stay on the three shared slots", () => {
  assert.equal(PAY_GO_METER_LABEL_KEYS.remaining, "余额");
  assert.equal(PAY_GO_METER_LABEL_KEYS.month, "本月");
  assert.equal(PAY_GO_METER_LABEL_KEYS.history, "历史");
  assert.equal(PAY_GO_METER_EMPTY, "—");
});

test("observation time is empty for missing values and compact otherwise", () => {
  assert.equal(formatPayGoObservedAt(0, "en-US"), "");
  assert.equal(formatPayGoObservedAt("", "en-US"), "");
  assert.equal(formatPayGoObservedAt("not-a-date", "en-US"), "");
  const fromUnix = formatPayGoObservedAt(1_753_000_000, "en-US");
  const fromIso = formatPayGoObservedAt("2026-09-20T08:07:00Z", "en-US");
  assert.ok(fromUnix.length > 0);
  assert.ok(fromIso.length > 0);
  assert.doesNotMatch(fromIso, /2026/);
});
