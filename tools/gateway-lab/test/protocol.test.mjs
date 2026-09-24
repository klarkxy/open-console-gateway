import assert from "node:assert/strict";
import test from "node:test";
import { SLOT_DEFS } from "../lib/profile.mjs";
import { inspectAuth, jsonSuccess, streamBody, validateBody } from "../lib/protocol.mjs";
import { MARKER } from "../lib/common.mjs";
import { selfCheck } from "../lib/lab.mjs";

const chat = SLOT_DEFS.find((slot) => slot.slot === "chat");
const responses = SLOT_DEFS.find((slot) => slot.slot === "responses");
const messages = SLOT_DEFS.find((slot) => slot.slot === "messages");

test("self-check accepts native chat and responses store=false", () => {
  selfCheck();
});

test("chat JSON success is a protocol-valid completion with usage", () => {
  const body = jsonSuccess(chat);
  assert.equal(body.object, "chat.completion");
  assert.equal(body.model, "upstream-chat");
  assert.equal(body.choices[0].message.content, "LAB_OK_chat");
  assert.equal(body.usage.total_tokens, 2);
});

test("messages and responses JSON include usage and content", () => {
  const msg = jsonSuccess(messages);
  assert.equal(msg.type, "message");
  assert.equal(msg.content[0].text, "LAB_OK_messages");
  assert.ok(msg.usage.input_tokens >= 1);
  const resp = jsonSuccess(responses);
  assert.equal(resp.object, "response");
  assert.equal(resp.status, "completed");
  assert.ok(resp.usage.total_tokens >= 1);
});

test("SSE streams include content, terminal event, and usage", () => {
  const chatSse = streamBody(chat);
  assert.match(chatSse, /LAB_OK_chat/);
  assert.match(chatSse, /\[DONE\]/);
  assert.match(chatSse, /prompt_tokens/);
  const msgSse = streamBody(messages);
  assert.match(msgSse, /message_start/);
  assert.match(msgSse, /message_stop/);
  assert.match(msgSse, /LAB_OK_messages/);
  const respSse = streamBody(responses);
  assert.match(respSse, /response.completed/);
  assert.match(respSse, /LAB_OK_responses/);
});

test("responses without store=false is invalid", () => {
  const result = validateBody(responses, { model: responses.model, input: MARKER });
  assert.ok(result.errors.includes("store_not_false"));
});

test("wrong model and missing probe fail validation", () => {
  const wrong = validateBody(chat, { model: "nope", messages: [{ role: "user", content: MARKER }] });
  assert.ok(wrong.errors.includes("wrong_model"));
  const missing = validateBody(chat, { model: chat.model, messages: [{ role: "user", content: "hello" }] });
  assert.ok(missing.errors.includes("missing_lab_probe"));
});

test("bearer and x-api-key auth match slot secrets", () => {
  assert.equal(inspectAuth({ authorization: `Bearer ${chat.secret}` }, chat).errors.length, 0);
  assert.ok(inspectAuth({ authorization: "Bearer wrong" }, chat).errors.includes("wrong_secret"));
  assert.equal(
    inspectAuth({ "x-api-key": messages.secret, "anthropic-version": "2023-06-01" }, messages).errors.length,
    0,
  );
});
