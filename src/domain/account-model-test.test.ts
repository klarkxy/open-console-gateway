import assert from "node:assert/strict";
import test from "node:test";
import { filterAccountTestModels, type AccountTestModel } from "./account-model-test.ts";

const models: AccountTestModel[] = [
  { modelId: "provider/alpha", alias: "Alpha", protocol: "chat_completions" },
  { modelId: "provider/beta", alias: "Beta", protocol: "messages" },
];
test("draft filter matches server candidates by raw id and alias while preserving order", () => {
  assert.deepEqual(filterAccountTestModels(models, "BETA"), [models[1]]);
  assert.deepEqual(filterAccountTestModels(models, "provider/a"), [models[0]]);
  assert.deepEqual(filterAccountTestModels(models, "  "), models);
  assert.deepEqual(filterAccountTestModels(models, "unknown"), []);
  assert.deepEqual(models.map(row => row.modelId), ["provider/alpha", "provider/beta"]);
});
