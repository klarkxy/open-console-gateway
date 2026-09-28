import assert from "node:assert/strict";
import test from "node:test";
import type { DestinationModelMetadataEntryView, ModelMetadataView } from "../api/destinations.ts";
import {
  MODEL_METADATA_ISSUE_KEYS,
  REASONING_EFFORT_LEVELS,
  buildModelMetadata,
  modelMetadataDraft,
  modelMetadataFingerprint,
  type ModelMetadataDraft,
  type ModelMetadataIssue,
} from "./model-metadata.ts";

function emptyView(): ModelMetadataView {
  return {
    name: null,
    context_window: null,
    max_output_tokens: null,
    input_modalities: null,
    output_modalities: null,
    reasoning: null,
    reasoning_efforts: null,
    tool_calling: null,
    parallel_tool_calls: null,
  };
}

function draft(patch: Partial<ModelMetadataDraft> = {}): ModelMetadataDraft {
  return { ...modelMetadataDraft(emptyView()), ...patch };
}

test("every issue code maps to a message key", () => {
  const issues: ModelMetadataIssue[] = [
    "invalid_name",
    "invalid_context_window",
    "invalid_max_output",
    "output_exceeds_context",
    "invalid_effort_spelling",
    "efforts_without_reasoning",
    "parallel_without_tool_calling",
  ];
  for (const issue of issues) {
    assert.equal(typeof MODEL_METADATA_ISSUE_KEYS[issue], "string");
  }
});

test("an untouched draft declares nothing: unknown stays absent", () => {
  const result = buildModelMetadata(modelMetadataDraft(emptyView()));
  assert.equal(result.kind, "save");
  assert.deepEqual(result.kind === "save" ? result.metadata : null, {});
});

test("a full draft round-trips through the view", () => {
  const view: ModelMetadataView = {
    name: "Private model",
    context_window: 262144,
    max_output_tokens: 32768,
    input_modalities: ["text", "image"],
    output_modalities: ["text"],
    reasoning: true,
    reasoning_efforts: { low: "low", xhigh: "max" },
    tool_calling: true,
    parallel_tool_calls: false,
  };
  const result = buildModelMetadata(modelMetadataDraft(view));
  assert.equal(result.kind, "save");
  if (result.kind !== "save") return;
  assert.deepEqual(result.metadata, {
    name: "Private model",
    contextWindow: 262144,
    maxOutputTokens: 32768,
    inputModalities: ["text", "image"],
    outputModalities: ["text"],
    reasoning: true,
    reasoningEfforts: { low: "low", xhigh: "max" },
    toolCalling: true,
    parallelToolCalls: false,
  });
});

test("unknown effort levels in a view never leak into the draft", () => {
  const result = modelMetadataDraft({
    ...emptyView(),
    reasoning_efforts: { low: "low", imaginary: "???", xhigh: "max" },
  });
  assert.deepEqual(result.reasoningEfforts, { low: "low", xhigh: "max" });
  assert.ok(!REASONING_EFFORT_LEVELS.includes("imaginary" as never));
});

test("zero, negative and fractional token limits are rejected", () => {
  for (const value of [0, -1, 1.5, Number.NaN]) {
    assert.deepEqual(buildModelMetadata(draft({ contextWindow: value })), {
      kind: "invalid",
      issue: "invalid_context_window",
    });
    assert.deepEqual(buildModelMetadata(draft({ maxOutputTokens: value })), {
      kind: "invalid",
      issue: "invalid_max_output",
    });
  }
});

test("output must not exceed the context window", () => {
  assert.deepEqual(buildModelMetadata(draft({ contextWindow: 100, maxOutputTokens: 200 })), {
    kind: "invalid",
    issue: "output_exceeds_context",
  });
  assert.equal(
    buildModelMetadata(draft({ contextWindow: 200, maxOutputTokens: 100 })).kind,
    "save",
  );
});

test("display name rejects blanks, control characters and oversized values", () => {
  assert.deepEqual(buildModelMetadata(draft({ name: "   " })), {
    kind: "invalid",
    issue: "invalid_name",
  });
  assert.deepEqual(buildModelMetadata(draft({ name: `bad${String.fromCharCode(7)}name` })), {
    kind: "invalid",
    issue: "invalid_name",
  });
  assert.deepEqual(buildModelMetadata(draft({ name: "x".repeat(201) })), {
    kind: "invalid",
    issue: "invalid_name",
  });
  assert.equal(buildModelMetadata(draft({ name: "x".repeat(200) })).kind, "save");
});

test("effort wire spellings follow the protocol alphabet", () => {
  assert.deepEqual(
    buildModelMetadata(draft({ reasoning: "yes", reasoningEfforts: { low: "low effort" } })),
    { kind: "invalid", issue: "invalid_effort_spelling" },
  );
  assert.deepEqual(
    buildModelMetadata(draft({ reasoning: "yes", reasoningEfforts: { low: "x".repeat(33) } })),
    { kind: "invalid", issue: "invalid_effort_spelling" },
  );
  assert.equal(
    buildModelMetadata(draft({ reasoning: "yes", reasoningEfforts: { xhigh: "max-1_v" } })).kind,
    "save",
  );
});

test("a non-reasoning model cannot declare effort levels", () => {
  assert.deepEqual(
    buildModelMetadata(draft({ reasoning: "no", reasoningEfforts: { low: "low" } })),
    { kind: "invalid", issue: "efforts_without_reasoning" },
  );
});

test("parallel tool calls require declared tool calling", () => {
  assert.deepEqual(
    buildModelMetadata(draft({ toolCalling: "no", parallelToolCalls: "yes" })),
    { kind: "invalid", issue: "parallel_without_tool_calling" },
  );
  assert.equal(
    buildModelMetadata(draft({ toolCalling: "yes", parallelToolCalls: "yes" })).kind,
    "save",
  );
});

test("fingerprint changes with facts, source, and presence", () => {
  const entry: DestinationModelMetadataEntryView = {
    public_model: "a",
    upstream_model: "a",
    metadata: { ...emptyView(), context_window: 8000 },
    source: "operator",
  };
  const base = modelMetadataFingerprint(entry);
  assert.notEqual(modelMetadataFingerprint({ ...entry, source: "upstream" }), base);
  assert.notEqual(
    modelMetadataFingerprint({ ...entry, metadata: { ...entry.metadata, context_window: 4000 } }),
    base,
  );
  assert.notEqual(modelMetadataFingerprint(null), base);
  assert.equal(modelMetadataFingerprint({ ...entry }), base);
});
