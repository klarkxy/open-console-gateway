import assert from "node:assert/strict";
import test from "node:test";
import type { PricingModel } from "../api/dashboard.ts";
import {
  buildPricingTableRows,
  formatPricingMultiplier,
  formatPricingRate,
} from "./pricing-view.ts";

const pricingLabels = {
  highspeed: "高速别名",
  minimaxM3Upper: "> 512K 输入",
  priorityService: "优先服务",
  minimaxM3UpperPriority: "> 512K 输入 + 优先服务",
};

function pricingModel(overrides: Partial<PricingModel>): PricingModel {
  return {
    model_id: "example-model",
    display_name: "Example Model",
    input: 1,
    output: 2,
    cache_read: 0.5,
    cache_write: 0.75,
    usage: 60,
    quota_multiplier: 1,
    min_input_tokens: null,
    max_input_tokens: null,
    adjustments: [],
    ...overrides,
  };
}

function assertRatesClose(actual: Array<number | null | undefined>, expected: number[]): void {
  assert.equal(actual.length, expected.length);
  actual.forEach((value, index) => {
    assert.ok(typeof value === "number" && Math.abs(value - (expected[index] ?? 0)) < 1e-12);
  });
}

test("pricing rates use two decimals and expose tiny exact values", () => {
  assert.deepEqual(formatPricingRate(null, "en-US"), { label: "—", exact: null });
  assert.deepEqual(formatPricingRate(0, "en-US"), { label: "$0.00", exact: null });
  assert.deepEqual(formatPricingRate(1.236, "en-US"), { label: "$1.24", exact: null });
  assert.deepEqual(formatPricingRate(0.003625, "en-US"), {
    label: "<$0.01",
    exact: "$0.003625",
  });
});

test("pricing multipliers use a compact display", () => {
  assert.equal(formatPricingMultiplier(1.5), "×1.5");
});

test("DeepSeek peak rows nest under the off-peak root", () => {
  const rows = buildPricingTableRows([
    pricingModel({
      model_id: "deepseek-v4-flash",
      display_name: "DeepSeek V4 Flash (Peak)",
      time_window: "peak",
      input: 0.44,
      output: 1.32,
    }),
    pricingModel({
      model_id: "deepseek-v4-flash",
      display_name: "DeepSeek V4 Flash (Off-Peak)",
      time_window: "off_peak",
      input: 0.22,
      output: 0.66,
    }),
  ], pricingLabels);

  assert.equal(rows.length, 1);
  const flash = rows[0];
  assert.equal(flash?.kind, "group");
  assert.equal(flash?.display_name, "DeepSeek V4 Flash");
  assert.equal(flash?.input, 0.22);
  assert.deepEqual(flash?.children?.map(({ display_name, input }) => [display_name, input]), [
    ["Peak", 0.44],
  ]);
  const keys = [flash?.row_key, ...(flash?.children ?? []).map((child) => child.row_key)];
  assert.equal(new Set(keys).size, keys.length);
});

test("pricing rows group duplicate tiers under stable unique keys", () => {
  const rows = buildPricingTableRows([
    pricingModel({
      model_id: "qwen3.7-plus",
      display_name: "Qwen3.7 Plus (> 256K tokens)",
      min_input_tokens: 256_001,
      input: 3,
      output: 6,
    }),
    pricingModel({
      model_id: "qwen3.7-plus",
      display_name: "Qwen3.7 Plus (≤ 256K tokens)",
      max_input_tokens: 256_000,
    }),
    pricingModel({ model_id: "glm-5.2", display_name: "GLM-5.2" }),
  ], pricingLabels);

  assert.equal(rows.length, 2);
  const qwen = rows[0];
  assert.equal(qwen?.kind, "group");
  assert.equal(qwen?.display_name, "Qwen3.7 Plus");
  assert.equal(qwen?.input, 1);
  assert.equal(qwen?.output, 2);
  assert.equal(qwen?.editable_multiplier, true);
  assert.deepEqual(qwen?.children?.map(({ display_name }) => display_name), ["> 256K tokens"]);
  assert.equal(qwen?.children?.[0]?.input, 3);
  assert.equal(qwen?.children?.[0]?.output, 6);
  assert.ok(qwen?.children?.every(({ model_id, editable_multiplier }) => (
    model_id === "qwen3.7-plus" && !editable_multiplier
  )));
  const keys = rows.flatMap((row) => [row.row_key, ...(row.children ?? []).map((child) => child.row_key)]);
  assert.equal(new Set(keys).size, keys.length);
});

test("MiniMax roots retain standard rates while every upgrade is materialized below", () => {
  const rows = buildPricingTableRows([
    pricingModel({
      model_id: "minimax-m3",
      display_name: "MiniMax M3",
      input: 0.3,
      output: 1.2,
      cache_read: 0.06,
      cache_write: null,
      adjustments: [
        { label: ">512K input", multiplier: 2, applies_to: "input,output,cache_read,cache_write" },
        { label: "priority service tier", multiplier: 1.5, applies_to: "input,output,cache_read,cache_write" },
        { label: ">512K + priority", multiplier: 3, applies_to: "input,output,cache_read,cache_write" },
      ],
    }),
    pricingModel({
      model_id: "minimax-m2.7",
      display_name: "MiniMax M2.7",
      input: 0.3,
      output: 1.2,
      cache_read: 0.06,
      cache_write: 0.375,
      adjustments: [{ label: "highspeed alias", multiplier: 2, applies_to: "input,output" }],
    }),
  ], pricingLabels);

  const m3 = rows[0];
  assert.equal(m3?.input, 0.3);
  assert.equal(m3?.output, 1.2);
  assert.deepEqual(m3?.children?.map(({ display_name }) => display_name), [
    "> 512K 输入",
    "优先服务",
    "> 512K 输入 + 优先服务",
  ]);
  assertRatesClose(m3?.children?.map(({ input }) => input) ?? [], [0.6, 0.45, 0.9]);
  assertRatesClose(m3?.children?.map(({ output }) => output) ?? [], [2.4, 1.8, 3.6]);
  assertRatesClose(m3?.children?.map(({ cache_read }) => cache_read) ?? [], [0.12, 0.09, 0.18]);
  assert.ok(m3?.children?.every(({ cache_write, editable_multiplier }) => (
    cache_write === null && !editable_multiplier
  )));

  const m27 = rows[1];
  assert.equal(m27?.input, 0.3);
  assert.equal(m27?.output, 1.2);
  assert.deepEqual(m27?.children?.map(({ display_name }) => display_name), ["高速别名"]);
  assert.equal(m27?.children?.[0]?.input, 0.6);
  assert.equal(m27?.children?.[0]?.output, 2.4);
  assert.equal(m27?.children?.[0]?.cache_read, 0.06);
  assert.equal(m27?.children?.[0]?.cache_write, 0.375);
});

test("unknown adjustment labels are preserved without a model-id branch", () => {
  const rows = buildPricingTableRows([
    pricingModel({
      model_id: "future-model",
      display_name: "Future Model",
      adjustments: [{ label: "weekend boost", multiplier: 1.25, applies_to: "input,output" }],
    }),
  ], pricingLabels);
  assert.equal(rows[0]?.kind, "group");
  assert.equal(rows[0]?.children?.[0]?.display_name, "weekend boost");
  assert.equal(rows[0]?.children?.[0]?.input, 1.25);
  assert.equal(rows[0]?.children?.[0]?.output, 2.5);
});
