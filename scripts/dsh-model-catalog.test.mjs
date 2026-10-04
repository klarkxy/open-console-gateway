import assert from "node:assert/strict";
import test from "node:test";
import {
  parseModelCatalog,
  describeOcgModel,
  messagesBaseUrl,
  refuseCrossProtocolReplay,
  refuseDegradingReplay,
  refuseUndeclaredMessagesReasoning,
  THINKING_LEVELS,
  REJECTED_MODEL_API,
  PROTOCOL_REPLAY_CODE,
  REPLAY_METADATA_CODE,
  MESSAGES_REASONING_CODE,
} from "../integrations/dsh-plugin/model-catalog.js";

const options = { providerId: "ocg", baseUrl: "http://127.0.0.1:9042/ocg/v1" };
const parse = (...rows) => parseModelCatalog({ data: rows }, options);
const protocols = (preferred, supported = [preferred]) => ({ preferred, supported });
const declared = (extra = {}, id = "private-alias") => ({
  id,
  name: "Private model",
  ocg: {
    schemaVersion: 2,
    status: "declared",
    contextWindow: 262144,
    maxOutputTokens: 32768,
    inputModalities: ["text", "image"],
    reasoning: true,
    reasoningEfforts: { low: "low", high: "high", xhigh: "max" },
    protocols: protocols("chat_completions", ["chat_completions", "responses"]),
    ...extra,
  },
});
const healthy = {
  id: "healthy",
  ocg: {
    schemaVersion: 2,
    protocols: protocols("chat_completions"),
    contextWindow: 8000,
    maxOutputTokens: 1000,
  },
};

test("preferred protocol selects the pi-ai api and only Messages drops a trailing /v1", () => {
  assert.equal(messagesBaseUrl(options.baseUrl), "http://127.0.0.1:9042/ocg");
  assert.equal(messagesBaseUrl(`${options.baseUrl}/`), "http://127.0.0.1:9042/ocg");
  assert.throws(() => messagesBaseUrl("http://127.0.0.1:9042/ocg/v1/models"));
  assert.throws(() => messagesBaseUrl("http://127.0.0.1:9042/v10"));
  const { models, modelErrors } = parse(
    declared(),
    declared({ protocols: protocols("responses") }, "responses-model"),
    declared({ protocols: protocols("messages"), reasoningEfforts: undefined }, "messages-model"),
  );
  assert.equal(modelErrors.size, 0);
  assert.equal(models[0].api, "openai-completions");
  assert.equal(models[0].baseUrl, options.baseUrl);
  assert.equal(models[1].api, "openai-responses");
  assert.equal(models[1].baseUrl, options.baseUrl);
  assert.equal(models[2].api, "anthropic-messages");
  assert.equal(models[2].baseUrl, "http://127.0.0.1:9042/ocg");
  assert.equal(models[1].compat, undefined);
  assert.equal(models[2].compat, undefined);
  assert.equal(models[2].compat?.forceAdaptiveThinking, undefined);
});

test("chat keeps an explicit effort map and does not advertise a developer role", () => {
  const { models: [model], metadata } = parse(declared());
  assert.equal(model.reasoning, true);
  assert.deepEqual(Object.keys(model.thinkingLevelMap), THINKING_LEVELS);
  assert.deepEqual(model.thinkingLevelMap, {
    off: null, minimal: null, low: "low", medium: null, high: "high", xhigh: "max", max: null,
  });
  assert.equal(model.compat.thinkingFormat, "openai");
  assert.equal(model.compat.supportsReasoningEffort, true);
  assert.equal(model.compat.supportsStore, false);
  assert.equal(model.compat.supportsDeveloperRole, false);
  assert.deepEqual(metadata.get(model.id).protocols, {
    preferred: "chat_completions",
    supported: ["chat_completions", "responses"],
  });
  assert.deepEqual(metadata.get(model.id).fallbacks, []);
});

test("reasoning=true without an effort map stays capable and offers no invented level", () => {
  for (const preferred of ["chat_completions", "responses", "messages"]) {
    const { models: [model], metadata } = parse(declared({
      protocols: protocols(preferred),
      reasoningEfforts: undefined,
    }));
    assert.equal(model.reasoning, true, preferred);
    assert.ok(Object.values(model.thinkingLevelMap).every((value) => value === null), preferred);
    assert.equal(metadata.get(model.id).reasoning, true);
    assert.equal(metadata.get(model.id).reasoningEfforts, undefined);
    if (preferred === "chat_completions") assert.equal(model.compat.supportsReasoningEffort, false);
    else assert.equal(model.compat, undefined);
  }
});

test("a messages model keeps a legacy chat effort map and offers no native menu", () => {
  const { models: [model], metadata, modelErrors } = parse(declared({ protocols: protocols("messages") }));
  assert.equal(modelErrors.size, 0);
  assert.equal(model.api, "anthropic-messages");
  assert.equal(model.reasoning, true);
  assert.equal(model.compat, undefined);
  assert.deepEqual(model.thinkingLevelMap, Object.fromEntries(THINKING_LEVELS.map((level) => [level, null])));
  assert.equal(metadata.get(model.id).reasoning, true);
  assert.equal(metadata.get(model.id).status, "declared");
  assert.deepEqual(metadata.get(model.id).reasoningEfforts, { low: "low", high: "high", xhigh: "max" });
  assert.deepEqual(metadata.get(model.id).protocols, { preferred: "messages", supported: ["messages"] });
});

test("missing, old, and invalid protocol rows are not executable chat models", () => {
  const rows = [
    { id: "id-only" },
    { id: "schema-1", ocg: { schemaVersion: 1, protocols: protocols("chat_completions"), reasoning: true } },
    { id: "schema-future", ocg: { schemaVersion: 3, protocols: protocols("chat_completions") } },
    { id: "no-protocol", ocg: { schemaVersion: 2, contextWindow: 1000, maxOutputTokens: 100 } },
    { id: "empty-supported", ocg: { schemaVersion: 2, protocols: { preferred: "chat_completions", supported: [] } } },
    { id: "preferred-outside", ocg: { schemaVersion: 2, protocols: { preferred: "responses", supported: ["chat_completions"] } } },
    { id: "unknown-protocol", ocg: { schemaVersion: 2, protocols: protocols("gemini") } },
  ];
  for (const row of rows) {
    const result = parse(row, healthy);
    assert.equal(result.models.length, 2, row.id);
    assert.equal(result.modelErrors.has(row.id), true, row.id);
    assert.equal(result.models[0].api, REJECTED_MODEL_API, row.id);
    assert.notEqual(result.models[0].api, "openai-completions");
    assert.equal(result.models[1].id, "healthy");
    assert.equal(result.models[1].api, "openai-completions");
    assert.equal(result.models[1].contextWindow, 8000);
  }
});

test("bad limits and tiers isolate errors without inventing a chat call", () => {
  for (const bad of [
    declared({ contextWindow: -1 }),
    declared({ contextWindow: 0 }),
    declared({ contextWindow: "262144" }),
    declared({ contextWindow: 100, maxOutputTokens: 200 }),
    declared({ reasoningEfforts: { ultra: "ultra" } }),
    declared({ reasoning: false }),
    declared({ reasoningEfforts: { high: "\nsecret" } }),
  ]) {
    const result = parse(bad, healthy);
    assert.equal(result.models.length, 2);
    assert.equal(result.modelErrors.size, 1);
    assert.equal(result.models[0].api, REJECTED_MODEL_API);
    assert.equal(result.models[1].contextWindow, 8000);
  }
});

test("duplicate identities fail closed and empty catalogs are valid", () => {
  assert.equal(parse(healthy, { ...healthy }).modelErrors.get("healthy"), "Duplicate OCG model ID");
  assert.equal(parse().models.length, 0);
  assert.throws(() => parseModelCatalog({}, options));
});

test("non-text-only models do not acquire fabricated text capabilities", () => {
  assert.ok(parse(declared({ inputModalities: ["audio"] })).modelErrors.has("private-alias"));
  assert.ok(parse(declared({ outputModalities: ["image"] })).modelErrors.has("private-alias"));
  assert.ok(parse(declared({ inputModalities: [] })).modelErrors.has("private-alias"));
});

test("metadata snapshots are not shared with callers", () => {
  const { metadata } = parse(declared());
  const original = metadata.get("private-alias");
  const shown = describeOcgModel({ id: "private-alias", context: { contextWindow: 262144 } }, original);
  shown.ocg.reasoningEfforts.high = "changed";
  assert.equal(original.reasoningEfforts.high, "high");
  assert.equal(shown.context.contextWindow, 262144);
});

test("explicit Off wire spelling survives; Off is never added implicitly", () => {
  const { models: [model] } = parse(declared({ reasoningEfforts: { off: "none", high: "high" } }));
  assert.equal(model.thinkingLevelMap.off, "none");
  assert.equal(model.thinkingLevelMap.low, null);
});

test("unknown reasoning controls stay distinguishable from an explicit empty offer", () => {
  const absent = parse(declared({ reasoning: undefined, reasoningEfforts: undefined }));
  assert.equal(absent.metadata.get("private-alias").reasoning, undefined);
  assert.equal(absent.metadata.get("private-alias").reasoningEfforts, undefined);
  assert.equal(absent.models[0].reasoning, false);
  const empty = parse(declared({ reasoningEfforts: {} }));
  assert.deepEqual(empty.metadata.get("private-alias").reasoningEfforts, {});
  assert.equal(empty.models[0].reasoning, true);
});

function piDescriptor(id, efforts) {
  return {
    provider: "ocg",
    id,
    name: "Private model",
    inputModalities: ["text"],
    context: { contextWindow: 262144 },
    ...(efforts === undefined ? {} : { reasoning: { efforts } }),
  };
}

test("an empty public effort menu is omitted and no level is guessed", () => {
  for (const preferred of ["messages", "chat_completions", "responses"]) {
    for (const reasoningEfforts of [undefined, {}]) {
      const parsed = parse(declared({
        protocols: protocols(preferred),
        reasoningEfforts,
      }, "menu-model"));
      const metadata = parsed.metadata.get("menu-model");
      const source = piDescriptor("menu-model", []);
      const shown = describeOcgModel(source, metadata);
      assert.equal(parsed.models[0].reasoning, true, preferred);
      assert.ok(Object.values(parsed.models[0].thinkingLevelMap).every((value) => value === null), preferred);
      assert.equal(Object.hasOwn(shown, "reasoning"), false, `${preferred} public menu`);
      assert.equal(shown.ocg.reasoning, true, preferred);
      assert.deepEqual(shown.ocg.reasoningEfforts, reasoningEfforts);
      assert.equal(JSON.stringify(shown).includes("\"efforts\""), false, preferred);
      assert.equal(source.reasoning.efforts.length, 0);
    }
  }
  const legacy = parse(declared({ protocols: protocols("messages") }, "legacy-messages"));
  const metadata = legacy.metadata.get("legacy-messages");
  const shown = describeOcgModel(piDescriptor("legacy-messages", []), metadata);
  assert.equal(Object.hasOwn(shown, "reasoning"), false);
  assert.equal(shown.ocg.reasoning, true);
  assert.deepEqual(shown.ocg.reasoningEfforts, { low: "low", high: "high", xhigh: "max" });
  assert.deepEqual(legacy.models[0].thinkingLevelMap, Object.fromEntries(THINKING_LEVELS.map((level) => [level, null])));
  const listed = parse(declared({ reasoningEfforts: [] }, "listed-empty"));
  assert.ok(Object.values(listed.models[0].thinkingLevelMap).every((value) => value === null));
  assert.deepEqual(listed.metadata.get("listed-empty").reasoningEfforts, {});
  const listedShown = describeOcgModel(piDescriptor("listed-empty", []), listed.metadata.get("listed-empty"));
  assert.equal(Object.hasOwn(listedShown, "reasoning"), false);
});

test("a nonempty public effort menu stays exact, including a malformed menu", () => {
  const menu = {
    efforts: [
      { id: "low", name: "Low" },
      { id: "high", name: "High" },
      { id: "xhigh", name: "Xhigh" },
    ],
    defaultEffort: "high",
  };
  const { metadata } = parse(declared());
  const source = { provider: "ocg", id: "private-alias", name: "Private model", reasoning: menu };
  const shown = describeOcgModel(source, metadata.get("private-alias"));
  assert.equal(shown.reasoning, menu);
  const malformed = { efforts: [{ id: "high" }] };
  const passed = describeOcgModel({ ...source, reasoning: malformed }, metadata.get("private-alias"));
  assert.equal(passed.reasoning, malformed);
  assert.equal(parse(declared({ reasoningEfforts: { ultra: "ultra" } })).models[0].api, REJECTED_MODEL_API);
});

test("unknown context is omitted from the public descriptor", () => {
  const row = declared({ contextWindow: undefined, inputModalities: undefined });
  const { models: [model], metadata } = parse(row);
  const original = { id: row.id, context: { contextWindow: model.contextWindow } };
  const info = describeOcgModel(original, metadata.get(row.id));
  assert.equal(info.context, undefined);
  assert.equal(Object.hasOwn(info, "context"), false);
  assert.equal(original.context.contextWindow, model.contextWindow);
  assert.equal(info.ocg.contextWindow, undefined);
  assert.ok(info.ocg.fallbacks.includes("contextWindow"));
});

test("an output-only limit is preserved without a contradictory public context", () => {
  const { models: [model], metadata } = parse({
    id: "output-only",
    maxTokens: 262144,
    ocg: { schemaVersion: 2, protocols: protocols("responses"), maxOutputTokens: 262144 },
  });
  assert.equal(model.api, "openai-responses");
  assert.equal(model.maxTokens, 262144);
  assert.ok(model.contextWindow >= model.maxTokens);
  assert.equal(metadata.get("output-only").contextWindow, undefined);
});

test("any provider, api, or model mismatch refuses opaque native history", () => {
  const model = { id: "private-alias", provider: "ocg", api: "openai-completions" };
  const thinking = {
    role: "assistant",
    provider: "ocg",
    model: "private-alias",
    api: "openai-completions",
    content: [{ type: "thinking", thinking: "kept", thinkingSignature: "sig" }],
  };
  const refuse = (next, message = thinking) => assert.throws(
    () => refuseCrossProtocolReplay(next, { messages: [message] }),
    (error) => {
      assert.equal(error.code, PROTOCOL_REPLAY_CODE);
      assert.match(error.message, /cannot replay/);
      return true;
    },
  );
  assert.doesNotThrow(() => refuseCrossProtocolReplay(model, { messages: [thinking] }));
  refuse({ ...model, api: "anthropic-messages" });
  refuse({ ...model, id: "other-model" });
  refuse({ ...model, provider: "other" });
  refuse(model, {
    ...thinking,
    model: "other-model",
    content: [{ type: "toolCall", id: "call", name: "echo", arguments: {}, thoughtSignature: "sig" }],
  });
  refuse(model, {
    ...thinking,
    api: "openai-responses",
    content: [{ type: "thinking", thinking: "", redacted: true }],
  });
  assert.doesNotThrow(() => refuseCrossProtocolReplay(model, {
    messages: [{ ...thinking, model: "other-model", content: [{ type: "text", text: "plain" }] }],
  }));
  assert.doesNotThrow(() => refuseCrossProtocolReplay(model, {
    messages: [{ ...thinking, provider: "other", content: [{ type: "thinking", thinking: "kept", thinkingSignature: "  " }] }],
  }));
  assert.doesNotThrow(() => refuseCrossProtocolReplay(model, {
    messages: [{
      ...thinking,
      api: "anthropic-messages",
      content: [{ type: "toolCall", id: "call", name: "echo", arguments: {} }],
    }],
  }));
});

test("harness replay metadata is rejected before it can be stripped to plaintext", () => {
  const model = { id: "private-alias", provider: "ocg", api: "openai-completions" };
  const envelope = (blocks, extra = {}) => ({
    response: {
      kind: "pi-ai",
      version: 2,
      api: "openai-completions",
      provider: "ocg",
      model: "private-alias",
      stopReason: "stop",
      ...extra,
    },
    blocks,
  });
  const assistant = (content, replayState, source = {}) => ({
    role: "assistant",
    content,
    source: { kind: "model", provider: "ocg", model: "private-alias", replayState, ...source },
  });
  const metadata = (messages) => assert.throws(
    () => refuseDegradingReplay(model, messages),
    (error) => {
      assert.equal(error.code, REPLAY_METADATA_CODE);
      return true;
    },
  );
  const tuple = (messages) => assert.throws(
    () => refuseDegradingReplay(model, messages),
    (error) => {
      assert.equal(error.code, PROTOCOL_REPLAY_CODE);
      return true;
    },
  );
  assert.doesNotThrow(() => refuseDegradingReplay(model, [
    { role: "assistant", content: [{ type: "text", text: "plain" }], source: { kind: "model", provider: "ocg", model: "other" } },
  ]));
  assert.doesNotThrow(() => refuseDegradingReplay(model, [
    assistant([{ type: "reasoning", text: "kept" }], envelope([{ type: "reasoning", thinkingSignature: "sig" }])),
  ]));
  assert.doesNotThrow(() => refuseDegradingReplay(model, [
    assistant(
      [{ type: "text", text: "plain" }],
      envelope([{ type: "text" }], { model: "other-model" }),
      { model: "other-model" },
    ),
  ]));
  metadata([assistant([{ type: "text", text: "x" }], { kind: "nope" })]);
  metadata([assistant([{ type: "text", text: "x" }], {
    response: { kind: "other", version: 2, api: "openai-completions", provider: "ocg", model: "private-alias", stopReason: "stop" },
    blocks: [{ type: "text" }],
  })]);
  metadata([assistant([{ type: "text", text: "x" }], {
    response: { kind: "pi-ai", version: 1, api: "openai-completions", provider: "ocg", model: "private-alias", stopReason: "stop" },
    blocks: [{ type: "text" }],
  })]);
  metadata([assistant([{ type: "text", text: "x" }], envelope([{ type: "text" }, { type: "reasoning" }]))]);
  metadata([assistant([{ type: "text", text: "x" }], envelope([{ type: "reasoning" }]))]);
  metadata([assistant([{ type: "text", text: "x" }], envelope([{ type: "text", thinkingSignature: 1 }]))]);
  metadata([assistant([{ type: "text", text: "x" }], envelope([{ type: "text" }]), { provider: "other" })]);
  tuple([assistant(
    [{ type: "reasoning", text: "kept" }],
    envelope([{ type: "reasoning", thinkingSignature: "sig" }], { api: "anthropic-messages" }),
  )]);
  tuple([assistant(
    [{ type: "reasoning", text: "kept" }],
    envelope([{ type: "reasoning", redacted: true }], { model: "other-model" }),
    { model: "other-model" },
  )]);
  tuple([assistant(
    [{ type: "tool-call", id: "call", name: "echo", arguments: "{}" }],
    envelope([{ type: "tool-call", thoughtSignature: "sig" }], { provider: "other" }),
    { provider: "other" },
  )]);
});

test("a selected messages reasoning level is refused instead of rewritten", () => {
  const model = { api: "anthropic-messages", id: "messages-model" };
  assert.throws(() => refuseUndeclaredMessagesReasoning(model, { reasoning: "high" }), (error) => {
    assert.equal(error.code, MESSAGES_REASONING_CODE);
    return true;
  });
  assert.doesNotThrow(() => refuseUndeclaredMessagesReasoning(model, {}));
  assert.doesNotThrow(() => refuseUndeclaredMessagesReasoning(
    { api: "openai-responses", id: "responses-model" },
    { reasoning: "high" },
  ));
});
