import assert from "node:assert/strict";
import test from "node:test";
import { dateTimeFormatter, numberFormatter } from "./intl-cache.ts";

test("repeated calls with the same locale and options return the same formatter and output", () => {
  const options = { maximumFractionDigits: 6 };
  const first = numberFormatter("en-US", options);
  const second = numberFormatter("en-US", { maximumFractionDigits: 6 });
  assert.equal(first, second);
  assert.equal(first.format(1234.5), second.format(1234.5));
  assert.equal(numberFormatter("en-US", options).format(0.000001), "0.000001");
});

test("different options or locales never share a cached formatter", () => {
  const plain = numberFormatter("en-US", { maximumFractionDigits: 6 });
  const currency = numberFormatter("en-US", {
    style: "currency",
    currency: "USD",
    currencyDisplay: "narrowSymbol",
  });
  assert.notEqual(plain, currency);
  assert.equal(plain.format(12.5), "12.5");
  assert.equal(currency.format(12.5), "$12.50");
  assert.notEqual(numberFormatter("de-DE", { maximumFractionDigits: 6 }), plain);
  assert.equal(numberFormatter("de-DE", { maximumFractionDigits: 6 }).format(1234.5), "1.234,5");
});

test("the runtime default locale stays distinct from an explicit locale", () => {
  const fallback = numberFormatter(undefined, { maximumFractionDigits: 2 });
  const explicit = numberFormatter("en-US", { maximumFractionDigits: 2 });
  assert.notEqual(fallback, explicit);
  assert.equal(fallback.resolvedOptions().maximumFractionDigits, 2);
});

test("date formatters cache per locale and options", () => {
  const options = { year: "numeric", month: "2-digit", day: "2-digit", timeZone: "UTC" } as const;
  const date = new Date("2026-03-04T05:06:07Z");
  const zh = dateTimeFormatter("zh-CN", options);
  assert.equal(zh, dateTimeFormatter("zh-CN", { ...options }));
  assert.equal(zh.format(date), dateTimeFormatter("zh-CN", { ...options }).format(date));
  assert.notEqual(zh, dateTimeFormatter("en-US", options));
});

test("invalid options keep throwing on every call so call-site fallbacks still run", () => {
  // A structurally invalid currency tag must throw both before and after any
  // cache entry exists, because callers branch to a plain-suffix fallback in
  // `catch`.
  const badCurrency = { style: "currency", currency: "US" } as Intl.NumberFormatOptions;
  assert.throws(() => numberFormatter("en-US", badCurrency), RangeError);
  assert.throws(() => numberFormatter("en-US", badCurrency), RangeError);
  assert.throws(() => numberFormatter("", { maximumFractionDigits: 2 }), RangeError);
  assert.throws(() => numberFormatter("", { maximumFractionDigits: 2 }), RangeError);
});
