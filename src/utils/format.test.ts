import assert from "node:assert/strict";
import test from "node:test";
import { DEFAULT_LOCALE, setLocale } from "../i18n/index.ts";
import { formatCost, formatNumber, useClipboard } from "./format.ts";

test("formatNumber uses the active locale grouping", async () => {
  await setLocale("en-US");
  assert.equal(formatNumber(1234567.89), "1,234,567.89");
  await setLocale("de-DE");
  assert.equal(formatNumber(1234.5), new Intl.NumberFormat("de-DE").format(1234.5));
  assert.match(formatNumber(1234.5), /1\.234,5/);
  assert.doesNotMatch(formatNumber(1234.5), /1,234\.5/);
  await setLocale(DEFAULT_LOCALE);
});

test("formatCost defaults tiny values to four decimals", () => {
  setLocale("en-US");
  assert.equal(formatCost(0), "$0.00");
  assert.equal(formatCost(1.5), "$1.50");
  assert.equal(formatCost(0.001), "$0.0010");
  setLocale(DEFAULT_LOCALE);
});

test("useClipboard copies, tracks target, and cleans up timers", async () => {
  setLocale("zh-CN");
  const original = globalThis.navigator;
  const writes: string[] = [];
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: {
      clipboard: {
        writeText: async (value: string) => {
          writes.push(value);
        },
      },
    },
  });

  try {
    const { copiedTarget, copy, cleanup } = useClipboard(20);
    await assert.rejects(() => copy("k", "", "Key"), /没有可复制的内容/);
    const result = await copy("gateway-key", "ocg-secret", "Key");
    assert.deepEqual(result, { target: "gateway-key", label: "Key" });
    assert.equal(writes.at(-1), "ocg-secret");
    assert.equal(copiedTarget.value, "gateway-key");
    await new Promise((resolve) => setTimeout(resolve, 30));
    assert.equal(copiedTarget.value, null);
    cleanup();
  } finally {
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: original,
    });
    setLocale(DEFAULT_LOCALE);
  }
});

test("useClipboard rejects environments without clipboard support", async () => {
  setLocale("zh-CN");
  const original = globalThis.navigator;
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: {},
  });
  try {
    const { copy } = useClipboard();
    await assert.rejects(() => copy("k", "value", "Key"), /剪贴板/);
  } finally {
    Object.defineProperty(globalThis, "navigator", {
      configurable: true,
      value: original,
    });
    setLocale(DEFAULT_LOCALE);
  }
});
