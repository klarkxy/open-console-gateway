import assert from "node:assert/strict";
import test from "node:test";
import { normalizeChatReasoningReplay, chatStreamOptions } from "../integrations/dsh-plugin/model-catalog.js";

test("Chat replay removes streaming indices without changing history or opaque fields", () => {
  const details = [
    { type: "reasoning.text", text: "plan", signature: "signed-by-provider", format: "unknown", index: 0 },
    { type: "reasoning.encrypted", data: "opaque-exact-bytes", id: "rs-1", format: "vendor-v1", index: 1, vendor: { index: 7 } },
    { type: "reasoning.summary", summary: "summary", index: 2 },
    { type: "reasoning.text", text: "already valid" },
  ];
  const user = { role: "user", content: "continue", metadata: { index: 9 } };
  const payload = { messages: [{ role: "assistant", content: "answer", reasoning_details: details }, user], metadata: { index: 3 } };
  const before = structuredClone(payload);
  const result = normalizeChatReasoningReplay(payload);
  assert.deepEqual(result.messages[0].reasoning_details, [
    { type: "reasoning.text", text: "plan", signature: "signed-by-provider", format: "unknown" },
    { type: "reasoning.encrypted", data: "opaque-exact-bytes", id: "rs-1", format: "vendor-v1", vendor: { index: 7 } },
    { type: "reasoning.summary", summary: "summary" },
    details[3],
  ]);
  assert.deepEqual(payload, before);
  assert.equal(result.messages[1], user);
  assert.equal(result.metadata, payload.metadata);
  assert.equal(normalizeChatReasoningReplay(result), result);
});

test("Chat replay leaves unrelated and malformed fields to the SDK or upstream", () => {
  for (const payload of [undefined, null, [], {}, { input: [{ type: "reasoning", index: 0 }] }, {
    messages: [{ role: "assistant", reasoning_details: [null, "invalid", { type: "reasoning.text", text: "plain" }] },
      { role: "tool", index: 0, reasoning_details: [{ index: 1 }] }],
  }]) {
    assert.equal(normalizeChatReasoningReplay(payload), payload);
  }
});

test("Chat normalization composes with async replacement and mutation payload hooks", async () => {
  const model = { id: "old-thread" };
  const payload = { messages: [{ role: "assistant", reasoning_details: [{ type: "reasoning.text", text: "old", index: 0 }] }] };
  for (const replace of [false, true]) {
    let calls = 0;
    const options = { signal: new AbortController().signal, async onPayload(value, actualModel) {
      calls += 1;
      assert.equal(actualModel, model);
      if (replace) return { ...value, temperature: 0.5 };
      value.temperature = 0.5;
    } };
    const wrapped = chatStreamOptions(model, {}, options);
    assert.equal(wrapped.signal, options.signal);
    const result = await wrapped.onPayload(structuredClone(payload), model);
    assert.equal(calls, 1);
    assert.equal(result.temperature, 0.5);
    assert.equal(result.messages[0].reasoning_content, "old");
    assert.equal(Object.hasOwn(result.messages[0], "reasoning_details"), false);
  }
  assert.deepEqual(await chatStreamOptions(model, {}, undefined).onPayload(payload, model), {
    messages: [{ role: "assistant", reasoning_content: "old" }],
  });
  const failure = new Error("custom hook failed");
  await assert.rejects(chatStreamOptions(model, {}, { onPayload() { throw failure; } }).onPayload(payload, model), (error) => error === failure);
});

test("old unsigned unknown-format text replays through reasoning_content without duplicate text", () => {
  const details = [
    { type: "reasoning.text", text: "first", format: "unknown", index: 0 },
    { type: "reasoning.text", text: " second", format: "unknown", index: 1 },
  ];
  for (const reasoning_content of [undefined, "", "first second"]) {
    const message = { role: "assistant", content: "answer", reasoning_details: details, reasoning_content };
    const before = structuredClone(message);
    const normalized = normalizeChatReasoningReplay({ messages: [message] });
    assert.equal(normalized.messages[0].reasoning_content, "first second");
    assert.equal(Object.hasOwn(normalized.messages[0], "reasoning_details"), false);
    assert.deepEqual(message, before);
  }
});

test("signed, encrypted, identified, and vendor-format details are never lowered to plain text", () => {
  for (const extra of [{ signature: "signature-bytes" }, { data: "encrypted-bytes" }, { id: "provider-id" },
    { format: "anthropic-v1" }, { vendor: { index: 1 } }]) {
    const detail = { type: "reasoning.text", text: "plan", format: "unknown", index: 0, ...extra };
    const result = normalizeChatReasoningReplay({ messages: [{ role: "assistant", reasoning_details: [detail] }] });
    const expected = { ...detail };
    delete expected.index;
    assert.deepEqual(result.messages[0].reasoning_details, [expected]);
    assert.equal(Object.hasOwn(result.messages[0], "reasoning_content"), false);
  }
  const conflict = { role: "assistant", reasoning_content: "different", reasoning_details: [{ type: "reasoning.text", text: "plan" }] };
  assert.equal(normalizeChatReasoningReplay({ messages: [conflict] }).messages[0], conflict);
});
