import assert from "node:assert/strict";
import test from "node:test";
import { parseModelCatalog, describeOcgModel, THINKING_LEVELS } from "../integrations/dsh-plugin/model-catalog.js";
const options = { providerId: "open-console-gateway", baseUrl: "http://127.0.0.1:9042/v1" };
const parse = (...rows) => parseModelCatalog({ data: rows }, options);
const declared = (extra = {}) => ({ id: "private-alias", name: "Private model", ocg: {
  schemaVersion: 1, status: "declared", contextWindow: 262144, maxOutputTokens: 32768,
  inputModalities: ["text", "image"], reasoning: true, reasoningEfforts: { low: "low", high: "high", xhigh: "max" }, ...extra,
} });

test("concrete model facts survive without a recognizable vendor/model ID", () => {
  const { models: [m], metadata, modelErrors } = parse(declared());
  assert.equal(m.contextWindow, 262144); assert.equal(m.maxTokens, 32768);
  assert.equal(m.name, "Private model"); assert.deepEqual(m.input, ["text", "image"]);
  assert.equal(m.api, "openai-completions"); assert.equal(m.baseUrl, options.baseUrl);
  assert.equal(modelErrors.size, 0); assert.deepEqual(metadata.get(m.id).fallbacks, []);
});

test("all seven thinking levels are decided, with wire renames preserved", () => {
  const { models: [m] } = parse(declared());
  assert.equal(m.reasoning, true); assert.deepEqual(Object.keys(m.thinkingLevelMap), THINKING_LEVELS);
  assert.deepEqual(m.thinkingLevelMap, { off: null, minimal: null, low: "low", medium: null, high: "high", xhigh: "max", max: null });
  assert.equal(m.compat.thinkingFormat, "openai"); assert.equal(m.compat.supportsReasoningEffort, true);
});

test("reasoning=true alone never manufactures selectable tiers", () => {
  const { models: [m] } = parse(declared({ reasoningEfforts: undefined }));
  assert.equal(m.reasoning, false); assert.ok(Object.values(m.thinkingLevelMap).every((v) => v === null));
});

test("legacy ID-only catalog stays usable but fallback context is not a declared fact", () => {
  const { models: [m], metadata } = parse({ id: "model" });
  const info = describeOcgModel({ id: "model", context: { contextWindow: m.contextWindow } }, metadata.get("model"));
  assert.equal(info.context.contextWindow, undefined);
  assert.equal(info.ocg.contextWindow, undefined); assert.ok(info.ocg.fallbacks.includes("contextWindow"));
  assert.equal(m.reasoning, false);
});

test("bad limits, future schemas, and invalid tiers isolate errors to their row", () => {
  for (const bad of [declared({ contextWindow: -1 }), declared({ contextWindow: 0 }), declared({ contextWindow: "262144" }),
    declared({ contextWindow: 100, maxOutputTokens: 200 }), declared({ schemaVersion: 2 }),
    declared({ reasoningEfforts: { ultra: "ultra" } }), declared({ reasoning: false }),
    declared({ reasoningEfforts: { high: "\nsecret" } })]) {
    const result = parse(bad, { id: "healthy", contextWindow: 8000, maxTokens: 1000 });
    assert.equal(result.models.length, 2); assert.equal(result.modelErrors.size, 1);
    assert.ok(result.modelErrors.has("private-alias")); assert.equal(result.models[1].contextWindow, 8000);
  }
});

test("duplicate identities fail closed and empty catalogs are valid", () => {
  assert.equal(parse({ id: "a" }, { id: "a" }).modelErrors.get("a"), "Duplicate OCG model ID");
  assert.equal(parse().models.length, 0); assert.throws(() => parseModelCatalog({}, options));
});

test("non-text-only models do not acquire fabricated text capabilities", () => {
  assert.ok(parse(declared({ inputModalities: ["audio"] })).modelErrors.has("private-alias"));
  assert.ok(parse(declared({ outputModalities: ["image"] })).modelErrors.has("private-alias"));
});

test("metadata snapshots are not shared with callers", () => {
  const { metadata } = parse(declared());
  const original = metadata.get("private-alias");
  const shown = describeOcgModel({ id: "private-alias", context: { contextWindow: 262144 } }, original);
  shown.ocg.reasoningEfforts.high = "changed";
  assert.equal(original.reasoningEfforts.high, "high"); assert.equal(shown.context.contextWindow, 262144);
});

test("explicit Off wire spelling survives; Off is never added implicitly", () => {
  const { models: [m] } = parse(declared({ reasoningEfforts: { off: "none", high: "high" } }));
  assert.equal(m.thinkingLevelMap.off, "none"); assert.equal(m.thinkingLevelMap.low, null);
});
