#!/usr/bin/env node
// Isolated native-protocol smoke. Does not build, probe, or call a remote provider.
// Installed DSH SDK + rendered current plugin -> this --cli binary -> loopback mocks.
// The mocks are the oracle: upstream fields must equal the original raw bytes.
// Optional --redaction-stress reuses this run. The oracle still serves the request.
// Optional --interleaved-stress implies that mode. Reasoning opens first and
// finishes after the secret text item. Content follows SDK chunk.index.

import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createServer } from "node:http";
import { access, cp, mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { basename, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { makeApi } from "../tools/gateway-lab/lib/dashboard.mjs";
import { registerSlots, startGatewayCli, waitForGateway } from "../tools/gateway-lab/lib/harness.mjs";
import { pickLoopbackPort, stopOwnedGateway } from "../tools/gateway-lab/lib/process.mjs";
import {
  CHAT_TOOL_ID,
  FINAL_TEXT,
  MESSAGES_TOOL_ID,
  PLAN_TEXT,
  RESPONSES_CALL_ID,
  RESPONSES_ITEM_ID,
  SYSTEM_TEXT,
  TOOL_ARGUMENTS,
  TOOL_RESULT_TEXT,
  protocolSse,
} from "./dsh-protocol-fixtures.mjs";

const execFileAsync = promisify(execFile);
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const pluginSource = join(repo, "integrations", "dsh-plugin");
const installedDshVersion = "0.2.0-rc.2";
const installedPiAiVersion = "0.87.1";
const encryptionKey = "ocg-native-protocol-smoke-dummy-not-a-secret";
const SIGNATURE_RAW = "signed-messages-raw";
const REDACTED_RAW = "redacted-thinking-data-raw";
const ENCRYPTED_RAW = "responses-encrypted-content-raw";
const NATIVE_RESPONSE_ID = "resp_native";
const NATIVE_REASONING_ID = "rs_native";
const NATIVE_MESSAGE_ID = "msg_native";
const VISIBLE_TEXT = "visible ocg value";
const STRESS_SECRET = "ocg";
const ENVELOPE = /^ocg-replay-v1:[0-9a-f]{64}:(.*)$/s;
const ZERO_DOMAIN = "0".repeat(64);
const syntheticSecrets = [encryptionKey];
const API_OF = Object.freeze({
  chat_completions: "openai-completions",
  responses: "openai-responses",
  messages: "anthropic-messages",
});
const ROUNDS = Object.freeze([
  ["chat", "chat_completions", "bearer", "native-chat", "upstream-chat", "/v1/chat/completions"],
  ["responses", "responses", "bearer", "native-responses", "upstream-responses", "/v1/responses"],
  ["messages", "messages", "x-api-key", "native-messages", "upstream-messages", "/v1/messages"],
]);

function readArgs() {
  const args = process.argv.slice(2);
  let cli = "";
  let redactionStress = false;
  let interleavedStress = false;
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === "--redaction-stress") {
      redactionStress = true;
      continue;
    }
    if (arg === "--interleaved-stress") {
      interleavedStress = true;
      redactionStress = true;
      continue;
    }
    if (arg === "--cli") {
      const value = args[index + 1] ?? "";
      if (!value || value.startsWith("-")) break;
      cli = resolve(value);
      index += 1;
      continue;
    }
    if (arg.startsWith("--cli=")) {
      const value = arg.slice("--cli=".length);
      if (!value || value.startsWith("-")) break;
      cli = resolve(value);
    }
  }
  if (!cli) throw new Error("pass --cli <ocg-manager-cli>; this smoke does not build a binary");
  return { cli, redactionStress, interleavedStress };
}

function dshBin() {
  if (process.env.OCG_DSH_SMOKE_BIN) return resolve(process.env.OCG_DSH_SMOKE_BIN);
  const appData = process.env.APPDATA;
  if (!appData) throw new Error("APPDATA is unavailable; cannot locate the installed DSH CLI");
  return join(appData, "npm", "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
}

async function exists(path) {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
}

function readBody(request) {
  return new Promise((resolveBody, reject) => {
    const chunks = [];
    request.on("data", (chunk) => chunks.push(chunk));
    request.on("end", () => resolveBody(Buffer.concat(chunks).toString("utf8")));
    request.on("error", reject);
  });
}

function protect(text) {
  let out = String(text ?? "");
  for (const secret of syntheticSecrets) {
    if (typeof secret === "string" && secret.length >= 8) out = out.split(secret).join("[redacted-synthetic]");
  }
  return out.replace(/sk-native-[a-z]+-dummy/g, "[redacted-synthetic]");
}

function runNode(args, options) {
  return execFileAsync(process.execPath, args, {
    encoding: "utf8",
    timeout: 180_000,
    windowsHide: true,
    maxBuffer: 4 * 1024 * 1024,
    ...options,
  });
}

function runnerEnv(dshHome) {
  return { ...process.env, DSH_HOME: dshHome };
}

function assertDeletableSmokeRoot(root) {
  const parent = resolve(tmpdir());
  const resolved = resolve(root);
  const fromParent = relative(parent, resolved);
  const name = basename(resolved);
  if (fromParent !== name || !name.startsWith("ocg-native-protocol-") || name === "ocg-native-protocol-") {
    throw new Error(`refusing recursive delete of ${resolved}; required direct child of ${parent} named ocg-native-protocol-*`);
  }
  return resolved;
}

function withZeroDomain(marker) {
  const match = ENVELOPE.exec(marker);
  assert.equal(typeof match?.[1], "string", "observed replay marker required before wrong-domain gateway call");
  return `ocg-replay-v1:${ZERO_DOMAIN}:${match[1]}`;
}

function dataEvents(events) {
  return `${events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("")}data: [DONE]\n\n`;
}

function anthropicEvents(events) {
  return events.map((event) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join("");
}

function messagesInterleavedSse(modelId) {
  const thinking = { type: "thinking", thinking: "", signature: "" };
  return anthropicEvents([
    {
      type: "message_start",
      message: {
        id: NATIVE_MESSAGE_ID, type: "message", role: "assistant", model: modelId, content: [],
        stop_reason: null, usage: { input_tokens: 1, output_tokens: 0 },
      },
    },
    { type: "content_block_start", index: 0, content_block: thinking },
    { type: "content_block_start", index: 1, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 1, delta: { type: "text_delta", text: VISIBLE_TEXT } },
    { type: "content_block_stop", index: 1 },
    { type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: PLAN_TEXT } },
    { type: "content_block_delta", index: 0, delta: { type: "signature_delta", signature: SIGNATURE_RAW } },
    { type: "content_block_stop", index: 0 },
    { type: "content_block_start", index: 2, content_block: { type: "redacted_thinking", data: REDACTED_RAW } },
    { type: "content_block_stop", index: 2 },
    { type: "content_block_start", index: 3, content_block: { type: "tool_use", id: MESSAGES_TOOL_ID, name: "echo", input: {} } },
    { type: "content_block_delta", index: 3, delta: { type: "input_json_delta", partial_json: JSON.stringify(TOOL_ARGUMENTS) } },
    { type: "content_block_stop", index: 3 },
    { type: "message_delta", delta: { stop_reason: "tool_use" }, usage: { output_tokens: 8 } },
    { type: "message_stop" },
  ]);
}

function responsesInterleavedSse(modelId) {
  const message = {
    type: "message", id: NATIVE_MESSAGE_ID, role: "assistant", status: "completed",
    content: [{ type: "output_text", text: VISIBLE_TEXT }],
  };
  const reasoning = {
    type: "reasoning", id: NATIVE_REASONING_ID,
    summary: [{ type: "summary_text", text: PLAN_TEXT }],
    encrypted_content: ENCRYPTED_RAW,
  };
  const call = {
    type: "function_call", id: RESPONSES_ITEM_ID, call_id: RESPONSES_CALL_ID,
    name: "echo", arguments: JSON.stringify(TOOL_ARGUMENTS),
  };
  return dataEvents([
    { type: "response.created", response: { id: NATIVE_RESPONSE_ID, status: "in_progress", model: modelId } },
    { type: "response.output_item.added", output_index: 0, item: { type: "reasoning", id: NATIVE_REASONING_ID } },
    { type: "response.output_item.added", output_index: 1, item: { type: "message", id: NATIVE_MESSAGE_ID, role: "assistant", status: "in_progress" } },
    { type: "response.content_part.added", item_id: NATIVE_MESSAGE_ID, output_index: 1, content_index: 0, part: { type: "output_text", text: "" } },
    { type: "response.output_text.delta", item_id: NATIVE_MESSAGE_ID, output_index: 1, content_index: 0, delta: VISIBLE_TEXT },
    { type: "response.output_text.done", item_id: NATIVE_MESSAGE_ID, output_index: 1, content_index: 0, text: VISIBLE_TEXT },
    { type: "response.content_part.done", item_id: NATIVE_MESSAGE_ID, output_index: 1, content_index: 0, part: { type: "output_text", text: VISIBLE_TEXT } },
    { type: "response.output_item.done", output_index: 1, item: message },
    { type: "response.reasoning_summary_part.added", item_id: NATIVE_REASONING_ID, output_index: 0, summary_index: 0, part: { type: "summary_text", text: "" } },
    { type: "response.reasoning_summary_text.delta", item_id: NATIVE_REASONING_ID, output_index: 0, summary_index: 0, delta: PLAN_TEXT },
    { type: "response.reasoning_summary_text.done", item_id: NATIVE_REASONING_ID, output_index: 0, summary_index: 0, text: PLAN_TEXT },
    { type: "response.reasoning_summary_part.done", item_id: NATIVE_REASONING_ID, output_index: 0, summary_index: 0, part: { type: "summary_text", text: PLAN_TEXT } },
    { type: "response.output_item.done", output_index: 0, item: reasoning },
    { type: "response.output_item.added", output_index: 2, item: { ...call, arguments: "" } },
    { type: "response.function_call_arguments.delta", item_id: RESPONSES_ITEM_ID, output_index: 2, delta: call.arguments },
    { type: "response.function_call_arguments.done", item_id: RESPONSES_ITEM_ID, output_index: 2, arguments: call.arguments },
    { type: "response.output_item.done", output_index: 2, item: call },
    {
      type: "response.completed",
      response: {
        id: NATIVE_RESPONSE_ID, status: "completed", model: modelId,
        output: [reasoning, message, call],
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
      },
    },
  ]);
}

function messagesToolSse(modelId, stress = false, interleaved = false) {
  if (interleaved) return messagesInterleavedSse(modelId);
  const thinkingIndex = stress ? 1 : 0;
  const redactedIndex = stress ? 2 : 1;
  const toolIndex = stress ? 3 : 2;
  const start = {
    type: "message_start",
    message: {
      id: "msg_smoke", type: "message", role: "assistant", model: modelId, content: [],
      stop_reason: null, usage: { input_tokens: 1, output_tokens: 1 },
    },
  };
  const events = [start];
  if (stress) {
    events.push(
      { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
      { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: VISIBLE_TEXT } },
      { type: "content_block_stop", index: 0 },
    );
  }
  events.push(
    { type: "content_block_start", index: thinkingIndex, content_block: { type: "thinking", thinking: "", signature: "" } },
    { type: "content_block_delta", index: thinkingIndex, delta: { type: "thinking_delta", thinking: PLAN_TEXT } },
    { type: "content_block_delta", index: thinkingIndex, delta: { type: "signature_delta", signature: SIGNATURE_RAW } },
    { type: "content_block_stop", index: thinkingIndex },
    { type: "content_block_start", index: redactedIndex, content_block: { type: "redacted_thinking", data: REDACTED_RAW } },
    { type: "content_block_stop", index: redactedIndex },
    { type: "content_block_start", index: toolIndex, content_block: { type: "tool_use", id: MESSAGES_TOOL_ID, name: "echo", input: {} } },
    { type: "content_block_delta", index: toolIndex, delta: { type: "input_json_delta", partial_json: JSON.stringify(TOOL_ARGUMENTS) } },
    { type: "content_block_stop", index: toolIndex },
    { type: "message_delta", delta: { stop_reason: "tool_use" }, usage: { output_tokens: 8 } },
    { type: "message_stop" },
  );
  return anthropicEvents(events);
}

function responsesToolSse(modelId, stress = false, interleaved = false) {
  if (interleaved) return responsesInterleavedSse(modelId);
  const reasoningIndex = stress ? 1 : 0;
  const callIndex = stress ? 2 : 1;
  const visible = {
    type: "message", id: "msg_visible", role: "assistant", status: "completed",
    content: [{ type: "output_text", text: VISIBLE_TEXT }],
  };
  const reasoning = {
    type: "reasoning", id: "rs_echo",
    summary: [{ type: "summary_text", text: PLAN_TEXT }],
    encrypted_content: ENCRYPTED_RAW,
  };
  const call = {
    type: "function_call", id: RESPONSES_ITEM_ID, call_id: RESPONSES_CALL_ID,
    name: "echo", arguments: JSON.stringify(TOOL_ARGUMENTS),
  };
  const events = [
    { type: "response.created", response: { id: "resp_smoke", status: "in_progress", model: modelId } },
  ];
  if (stress) {
    events.push(
      { type: "response.output_item.added", output_index: 0, item: { type: "message", id: "msg_visible", role: "assistant" } },
      { type: "response.output_text.delta", output_index: 0, delta: VISIBLE_TEXT },
      { type: "response.output_item.done", output_index: 0, item: visible },
    );
  }
  events.push(
    { type: "response.output_item.added", output_index: reasoningIndex, item: { type: "reasoning", id: "rs_echo", summary: [] } },
    { type: "response.reasoning_summary_text.delta", output_index: reasoningIndex, delta: PLAN_TEXT },
    { type: "response.output_item.done", output_index: reasoningIndex, item: reasoning },
    { type: "response.output_item.added", output_index: callIndex, item: { ...call, arguments: "" } },
    { type: "response.function_call_arguments.delta", output_index: callIndex, delta: call.arguments },
    { type: "response.output_item.done", output_index: callIndex, item: call },
    {
      type: "response.completed",
      response: {
        id: "resp_smoke", status: "completed", model: modelId,
        output: stress ? [visible, reasoning, call] : [reasoning, call],
        usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
      },
    },
  );
  return dataEvents(events);
}

function jsonUpstream(protocol, body, stress = false) {
  const replay = JSON.stringify(body).includes("tool_result") || JSON.stringify(body).includes("function_call_output");
  const usage = { input_tokens: 1, output_tokens: 1 };
  if (protocol === "messages") {
    const content = replay
      ? [{ type: "text", text: FINAL_TEXT }]
      : [
          ...(stress ? [{ type: "text", text: VISIBLE_TEXT }] : []),
          { type: "thinking", thinking: PLAN_TEXT, signature: SIGNATURE_RAW },
          { type: "redacted_thinking", data: REDACTED_RAW },
          { type: "tool_use", id: MESSAGES_TOOL_ID, name: "echo", input: TOOL_ARGUMENTS },
        ];
    return {
      id: replay ? "msg_final" : "msg_direct", type: "message", role: "assistant", model: body.model,
      content, stop_reason: replay ? "end_turn" : "tool_use", usage,
    };
  }
  const call = {
    type: "function_call", id: RESPONSES_ITEM_ID, call_id: RESPONSES_CALL_ID,
    name: "echo", arguments: JSON.stringify(TOOL_ARGUMENTS), status: "completed",
  };
  const reasoning = {
    type: "reasoning", id: "rs_echo", encrypted_content: ENCRYPTED_RAW,
    summary: [{ type: "summary_text", text: PLAN_TEXT }],
  };
  const visible = {
    type: "message", id: "msg_visible", role: "assistant", status: "completed",
    content: [{ type: "output_text", text: VISIBLE_TEXT }],
  };
  const output = replay
    ? [{ type: "message", role: "assistant", content: [{ type: "output_text", text: FINAL_TEXT }] }]
    : [...(stress ? [visible] : []), reasoning, call];
  return {
    id: replay ? "resp_final" : "resp_direct", object: "response", status: "completed",
    model: body.model, output, usage: { ...usage, total_tokens: 2 },
  };
}

function sseFor(protocol, phase, modelId, stress = false, interleaved = false) {
  if (phase === "final") return protocolSse(protocol, "final", modelId);
  if (protocol === "chat_completions") return protocolSse(protocol, "tool", modelId);
  if (protocol === "responses") return responsesToolSse(modelId, stress, interleaved);
  return messagesToolSse(modelId, stress, interleaved);
}

function parseSseData(sse) {
  return sse.split("\n\n").flatMap((block) => {
    const line = block.split("\n").find((item) => item.startsWith("data: "));
    if (!line || line === "data: [DONE]") return [];
    return [JSON.parse(line.slice("data: ".length))];
  });
}

function visibleProtocolTexts(value, out = []) {
  if (Array.isArray(value)) {
    for (const item of value) visibleProtocolTexts(item, out);
    return out;
  }
  if (!value || typeof value !== "object") return out;
  if ((value.type === "text" || value.type === "output_text" || value.type === "summary_text") && typeof value.text === "string") {
    out.push(value.text);
  }
  if (value.type === "thinking" && typeof value.thinking === "string") out.push(value.thinking);
  for (const [key, child] of Object.entries(value)) {
    if (key === "signature" || key === "data" || key === "encrypted_content" || key === "thinkingSignature" || key === "textSignature") continue;
    if (child && typeof child === "object") visibleProtocolTexts(child, out);
  }
  return out;
}

function assertVisibleRedacted(parsed, label) {
  const texts = visibleProtocolTexts(parsed);
  const ordinary = texts.filter((text) => text.includes("visible") || text.includes(VISIBLE_TEXT));
  assert.ok(ordinary.length > 0, `${label} ordinary visible text missing`);
  for (const text of texts) {
    assert.equal(text.includes(VISIBLE_TEXT), false, `${label} visible text`);
    assert.equal(text.includes(STRESS_SECRET), false, `${label} visible text`);
  }
  assert.ok(texts.includes(PLAN_TEXT), `${label} signed thinking`);
}

function assertStressOracle() {
  for (const raw of [PLAN_TEXT, SIGNATURE_RAW, REDACTED_RAW, ENCRYPTED_RAW]) {
    assert.equal(raw.includes(STRESS_SECRET), false, raw);
    assert.equal(raw.includes(VISIBLE_TEXT), false, raw);
  }
  const messages = parseSseData(messagesToolSse("upstream-messages", true));
  const starts = messages.filter((event) => event.type === "content_block_start");
  assert.deepEqual(starts.map((event) => [event.index, event.content_block.type]), [
    [0, "text"],
    [1, "thinking"],
    [2, "redacted_thinking"],
    [3, "tool_use"],
  ]);
  assert.equal(messages.find((event) => event.delta?.type === "text_delta")?.delta?.text, VISIBLE_TEXT);
  assert.equal(messages.find((event) => event.delta?.type === "thinking_delta")?.delta?.thinking, PLAN_TEXT);
  assert.equal(messages.find((event) => event.delta?.type === "signature_delta")?.delta?.signature, SIGNATURE_RAW);
  assert.equal(starts[2].content_block.data, REDACTED_RAW);
  assert.deepEqual(starts.filter((event) => event.content_block.type === "tool_use").map((event) => event.content_block.id), [MESSAGES_TOOL_ID]);
  const responses = parseSseData(responsesToolSse("upstream-responses", true));
  const added = responses.filter((event) => event.type === "response.output_item.added");
  assert.deepEqual(added.map((event) => [event.output_index, event.item.type]), [
    [0, "message"],
    [1, "reasoning"],
    [2, "function_call"],
  ]);
  const output = responses.find((event) => event.type === "response.completed")?.response?.output ?? [];
  assert.deepEqual(output.map((item) => item.type), ["message", "reasoning", "function_call"]);
  assert.equal(output.filter((item) => item.encrypted_content === ENCRYPTED_RAW).length, 1);
  assert.equal(output.filter((item) => item.type === "function_call" && item.id === RESPONSES_ITEM_ID && item.call_id === RESPONSES_CALL_ID).length, 1);
  assert.equal(new Set(output.map((item) => item.id)).size, output.length);
  assert.equal(output[0].content[0].text, VISIBLE_TEXT);
  assert.equal(output[1].summary[0].text, PLAN_TEXT);
  const messageJson = jsonUpstream("messages", { model: "upstream-messages" }, true);
  assert.deepEqual(messageJson.content.map((block) => block.type), ["text", "thinking", "redacted_thinking", "tool_use"]);
  assert.equal(messageJson.content[0].text, VISIBLE_TEXT);
  assert.equal(messageJson.content[1].thinking, PLAN_TEXT);
  assert.equal(messageJson.content[1].signature, SIGNATURE_RAW);
  assert.equal(messageJson.content[2].data, REDACTED_RAW);
  assert.deepEqual(messageJson.content.filter((block) => block.type === "tool_use").map((block) => block.id), [MESSAGES_TOOL_ID]);
  const responseJson = jsonUpstream("responses", { model: "upstream-responses" }, true);
  assert.deepEqual(responseJson.output.map((item) => item.type), ["message", "reasoning", "function_call"]);
  assert.equal(responseJson.output.filter((item) => item.encrypted_content === ENCRYPTED_RAW).length, 1);
  assert.equal(new Set(responseJson.output.map((item) => item.id)).size, responseJson.output.length);
  const replayMessages = jsonUpstream("messages", { messages: [{ content: [{ type: "tool_result" }] }] }, true);
  const replayResponses = jsonUpstream("responses", { input: [{ type: "function_call_output" }] }, true);
  assert.equal(JSON.stringify(replayMessages).includes(VISIBLE_TEXT), false);
  assert.equal(JSON.stringify(replayResponses).includes(VISIBLE_TEXT), false);
  assert.equal(messagesToolSse("upstream-messages").includes(VISIBLE_TEXT), false);
  assert.equal(responsesToolSse("upstream-responses").includes(VISIBLE_TEXT), false);
}

function assertInterleavedOracle() {
  for (const raw of [PLAN_TEXT, SIGNATURE_RAW, REDACTED_RAW, ENCRYPTED_RAW, NATIVE_RESPONSE_ID, NATIVE_REASONING_ID, NATIVE_MESSAGE_ID]) {
    assert.equal(raw.includes(STRESS_SECRET), false, raw);
  }
  assert.equal(responsesToolSse("upstream-responses", true).includes(NATIVE_REASONING_ID), false);
  assert.equal(messagesToolSse("upstream-messages", true).includes(NATIVE_MESSAGE_ID), false);
  const messages = parseSseData(messagesToolSse("upstream-messages", true, true));
  const messageStarts = messages.filter((event) => event.type === "content_block_start");
  assert.deepEqual(messageStarts.map((event) => [event.index, event.content_block.type]), [
    [0, "thinking"],
    [1, "text"],
    [2, "redacted_thinking"],
    [3, "tool_use"],
  ]);
  const textDeltaAt = messages.findIndex((event) => event.delta?.type === "text_delta");
  const textStopAt = messages.findIndex((event) => event.type === "content_block_stop" && event.index === 1);
  const thinkingDeltaAt = messages.findIndex((event) => event.delta?.type === "thinking_delta");
  assert.ok(messageStarts[0] && textDeltaAt > messages.indexOf(messageStarts[1]) && textStopAt > textDeltaAt && thinkingDeltaAt > textStopAt);
  assert.equal(messages[textDeltaAt].delta.text, VISIBLE_TEXT);
  assert.equal(messages[thinkingDeltaAt].delta.thinking, PLAN_TEXT);
  assert.equal(messages.find((event) => event.delta?.type === "signature_delta")?.delta?.signature, SIGNATURE_RAW);
  assert.equal(messageStarts[2].content_block.data, REDACTED_RAW);
  assert.equal(messageStarts[3].content_block.id, MESSAGES_TOOL_ID);
  assert.equal(messages.filter((event) => event.type === "message_delta").length, 1);
  assert.equal(messages.filter((event) => event.type === "message_stop").length, 1);
  assert.equal(messages.filter((event) => event.usage || event.message?.usage).length, 2);
  const responses = parseSseData(responsesToolSse("upstream-responses", true, true));
  assert.deepEqual(responses.map((event) => event.type), [
    "response.created",
    "response.output_item.added",
    "response.output_item.added",
    "response.content_part.added",
    "response.output_text.delta",
    "response.output_text.done",
    "response.content_part.done",
    "response.output_item.done",
    "response.reasoning_summary_part.added",
    "response.reasoning_summary_text.delta",
    "response.reasoning_summary_text.done",
    "response.reasoning_summary_part.done",
    "response.output_item.done",
    "response.output_item.added",
    "response.function_call_arguments.delta",
    "response.function_call_arguments.done",
    "response.output_item.done",
    "response.completed",
  ]);
  const responseIds = responses.map((event) => event.response?.id).filter(Boolean);
  assert.deepEqual(responseIds, [NATIVE_RESPONSE_ID, NATIVE_RESPONSE_ID]);
  assert.deepEqual(Object.keys(responses[1].item).sort(), ["id", "type"]);
  assert.equal(responses[1].output_index, 0);
  assert.equal(responses[1].item.id, NATIVE_REASONING_ID);
  assert.equal(responses[2].output_index, 1);
  assert.equal(responses[2].item.id, NATIVE_MESSAGE_ID);
  const textAt = responses.findIndex((event) => event.type === "response.output_text.delta");
  const summaryAt = responses.findIndex((event) => event.type === "response.reasoning_summary_text.delta");
  assert.ok(summaryAt > textAt);
  assert.equal(responses[textAt].delta, VISIBLE_TEXT);
  assert.equal(responses[textAt].output_index, 1);
  assert.equal(responses[summaryAt].delta, PLAN_TEXT);
  assert.equal(responses[summaryAt].output_index, 0);
  for (const event of responses) {
    if (event.item_id) {
      const expected = event.output_index === 0 ? NATIVE_REASONING_ID : event.output_index === 1 ? NATIVE_MESSAGE_ID : RESPONSES_ITEM_ID;
      assert.equal(event.item_id, expected, event.type);
    }
    if (event.item?.id) {
      const expected = event.item.type === "reasoning" ? NATIVE_REASONING_ID : event.item.type === "message" ? NATIVE_MESSAGE_ID : RESPONSES_ITEM_ID;
      assert.equal(event.item.id, expected, event.type);
    }
    if (event.item?.type === "function_call") {
      assert.equal(event.item.call_id, RESPONSES_CALL_ID);
    }
  }
  const output = responses.at(-1).response.output;
  assert.deepEqual(output.map((item) => item.type), ["reasoning", "message", "function_call"]);
  assert.equal(output[0].encrypted_content, ENCRYPTED_RAW);
  assert.equal(output[0].summary[0].text, PLAN_TEXT);
  assert.equal(output[1].content[0].text, VISIBLE_TEXT);
  assert.equal(responses.filter((event) => event.response?.usage).length, 1);
  assert.equal(responsesToolSse("upstream-responses", true, true).split(STRESS_SECRET).length - 1, 5);
  assert.equal(messagesToolSse("upstream-messages", true, true).split(STRESS_SECRET).length - 1, 1);
}

function phaseOf(protocol, body) {
  if (body?.stream === false) return "json";
  const blob = JSON.stringify(body ?? {});
  const replay = protocol === "chat_completions"
    ? blob.includes("\"role\":\"tool\"") || blob.includes("tool_calls")
    : protocol === "responses"
      ? blob.includes("function_call")
      : blob.includes("tool_result") || blob.includes("tool_use");
  return replay ? "final" : "tool";
}

function listen(protocol, secret, upstreamModel, inferencePath, stress = false, interleaved = false) {
  const hits = [];
  const server = createServer(async (request, response) => {
    try {
      const url = new URL(request.url ?? "/", "http://127.0.0.1");
      const text = request.method === "GET" || request.method === "HEAD" ? "" : await readBody(request);
      let body = null;
      if (text) {
        try {
          body = JSON.parse(text);
        } catch {
          body = { unparsed: true };
        }
      }
      const direct = request.headers["x-smoke-case"] === "wrong-domain";
      const authOk = protocol === "messages"
        ? request.headers["x-api-key"] === secret
        : request.headers.authorization === `Bearer ${secret}`;
      const hit = {
        protocol, method: request.method, pathname: url.pathname, authOk, direct, body,
        phase: request.method === "POST" ? phaseOf(protocol, body) : "other",
      };
      hits.push(hit);
      if (request.method === "GET" && url.pathname === "/v1/models") {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify({ object: "list", data: [{ id: upstreamModel }] }));
        return;
      }
      if (direct) {
        response.writeHead(200, { "content-type": "application/json" });
        response.end(JSON.stringify({ served: true }));
        return;
      }
      if (request.method === "POST" && url.pathname === inferencePath && body && !body.unparsed) {
        if (hit.phase === "json") {
          response.writeHead(200, { "content-type": "application/json" });
          response.end(JSON.stringify(jsonUpstream(protocol, body, stress)));
          return;
        }
        response.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
        response.end(sseFor(protocol, hit.phase, body.model, stress, interleaved));
        return;
      }
      response.writeHead(404, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: "unexpected", path: url.pathname }));
    } catch (error) {
      if (!response.headersSent) response.writeHead(500, { "content-type": "text/plain" });
      response.end(error instanceof Error ? error.message : String(error));
    }
  });
  return new Promise((resolveReady, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      resolveReady({
        server,
        hits,
        origin: `http://127.0.0.1:${address.port}`,
        close() {
          server.closeAllConnections?.();
          return new Promise((done) => server.close(() => done()));
        },
      });
    });
  });
}

function inferenceHits(mock) {
  return mock.hits.filter((hit) => hit.method === "POST" && hit.pathname !== "/v1/models" && !hit.direct);
}

function blocksOf(body) {
  const messages = Array.isArray(body?.messages) ? body.messages : [];
  return messages.flatMap((message) => (Array.isArray(message.content) ? message.content : []));
}

function assertRaw(value, raw, label) {
  assert.equal(value, raw, `${label} raw`);
  assert.equal(String(value).startsWith("ocg-replay-"), false, `${label} still enveloped`);
}

function assertSecondTurn(protocol, body) {
  const blob = JSON.stringify(body);
  assert.match(blob, /echo/, `${protocol} tools`);
  assert.match(blob, new RegExp(TOOL_RESULT_TEXT), `${protocol} tool result`);
  if (protocol === "chat_completions") {
    const assistant = (body.messages ?? []).find((message) => message.role === "assistant");
    assert.equal(assistant?.tool_calls?.[0]?.id, CHAT_TOOL_ID);
    const tool = (body.messages ?? []).find((message) => message.role === "tool");
    assert.equal(tool?.content, TOOL_RESULT_TEXT);
    return;
  }
  if (protocol === "responses") {
    const encrypted = (body.input ?? []).filter((item) => item?.type === "reasoning").map((item) => item.encrypted_content);
    assert.equal(encrypted.length, 1);
    assertRaw(encrypted[0], ENCRYPTED_RAW, "responses encrypted_content");
    assert.ok((body.input ?? []).some((item) => item?.type === "function_call" && item.call_id === RESPONSES_CALL_ID && item.name === "echo"));
    assert.ok((body.input ?? []).some((item) => item?.type === "function_call_output" && item.call_id === RESPONSES_CALL_ID));
    return;
  }
  const blocks = blocksOf(body);
  assertRaw(blocks.find((block) => block.type === "thinking")?.signature, SIGNATURE_RAW, "messages signature");
  assertRaw(blocks.find((block) => block.type === "redacted_thinking")?.data, REDACTED_RAW, "redacted_thinking.data");
  assert.ok(blocks.some((block) => block.type === "tool_use" && block.id === MESSAGES_TOOL_ID && block.name === "echo"));
}

function assertRounds(mocks, before) {
  const summary = {};
  for (const [name, protocol, , , upstreamModel, inferencePath] of ROUNDS) {
    const hits = inferenceHits(mocks[name]).slice(before[name]).filter((hit) => hit.phase !== "json");
    assert.deepEqual(hits.map((hit) => [hit.phase, hit.pathname, hit.authOk, hit.body?.model]), [
      ["tool", inferencePath, true, upstreamModel],
      ["final", inferencePath, true, upstreamModel],
    ], `${name} stream hits`);
    assert.match(JSON.stringify(hits[0].body?.tools ?? hits[0].body), /echo/);
    assertSecondTurn(protocol, hits[1].body);
    summary[name] = {
      hits: hits.length,
      model: hits[1].body?.model ?? null,
      path: hits[1].pathname,
      secondTurnRaw: protocol !== "chat_completions",
    };
  }
  return summary;
}

function collectStrings(value, out = []) {
  if (typeof value === "string") out.push(value);
  else if (Array.isArray(value)) for (const item of value) collectStrings(item, out);
  else if (value && typeof value === "object") for (const item of Object.values(value)) collectStrings(item, out);
  return out;
}

function clientEnvelope(payload, raw, label) {
  const found = collectStrings(payload).find((value) => {
    const match = ENVELOPE.exec(value);
    return match?.[1] === raw;
  });
  assert.equal(typeof found, "string", `gateway JSON did not envelope ${label}`);
  return found;
}

function echoTool(protocol) {
  if (protocol === "messages") {
    return [{ name: "echo", description: "Echo text", input_schema: { type: "object", properties: { text: { type: "string" } }, required: ["text"] } }];
  }
  return [{ type: "function", name: "echo", description: "Echo text", parameters: { type: "object", properties: { text: { type: "string" } }, required: ["text"] } }];
}

async function gatewayJson(base, pathName, key, body, messages) {
  const headers = { "content-type": "application/json", authorization: `Bearer ${key}` };
  if (messages) headers["anthropic-version"] = "2023-06-01";
  const response = await fetch(`${base}${pathName}`, { method: "POST", headers, body: JSON.stringify(body) });
  const text = await response.text();
  let parsed = null;
  try {
    parsed = JSON.parse(text);
  } catch {
    parsed = null;
  }
  return { status: response.status, parsed, text };
}

async function jsonDirect(base, key, mocks, stress = false) {
  const seen = {
    messages: inferenceHits(mocks.messages).filter((hit) => hit.phase === "json").length,
    responses: inferenceHits(mocks.responses).filter((hit) => hit.phase === "json").length,
  };
  const messagesFirst = {
    model: "native-messages", stream: false, max_tokens: 256, system: SYSTEM_TEXT,
    tools: echoTool("messages"),
    messages: [{ role: "user", content: [{ type: "text", text: "use echo" }] }],
  };
  const firstMessages = await gatewayJson(base, "/v1/messages", key, messagesFirst, true);
  assert.equal(firstMessages.status, 200, protect(firstMessages.text).slice(0, 400));
  const signature = clientEnvelope(firstMessages.parsed, SIGNATURE_RAW, "messages signature");
  const redacted = clientEnvelope(firstMessages.parsed, REDACTED_RAW, "redacted_thinking.data");
  if (stress) {
    assertVisibleRedacted(firstMessages.parsed, "messages json");
    const content = firstMessages.parsed?.content ?? [];
    assert.equal(content.filter((block) => block.type === "thinking").length, 1);
    assert.equal(content.filter((block) => block.type === "redacted_thinking").length, 1);
    assert.equal(content.filter((block) => block.type === "tool_use").length, 1);
    assert.equal(content.find((block) => block.type === "thinking")?.signature, signature);
    assert.equal(content.find((block) => block.type === "redacted_thinking")?.data, redacted);
    assert.equal(content.find((block) => block.type === "thinking")?.thinking, PLAN_TEXT);
  }
  const messagesSecond = {
    ...messagesFirst,
    messages: [
      messagesFirst.messages[0],
      {
        role: "assistant",
        content: [
          { type: "thinking", thinking: PLAN_TEXT, signature },
          { type: "redacted_thinking", data: redacted },
          { type: "tool_use", id: MESSAGES_TOOL_ID, name: "echo", input: TOOL_ARGUMENTS },
        ],
      },
      { role: "user", content: [{ type: "tool_result", tool_use_id: MESSAGES_TOOL_ID, content: TOOL_RESULT_TEXT }] },
    ],
  };
  const secondMessages = await gatewayJson(base, "/v1/messages", key, messagesSecond, true);
  assert.equal(secondMessages.status, 200, protect(secondMessages.text).slice(0, 400));
  const responsesFirst = {
    model: "native-responses", stream: false, store: false,
    tools: echoTool("responses"),
    input: [{ role: "user", content: [{ type: "input_text", text: "use echo" }] }],
  };
  const firstResponses = await gatewayJson(base, "/v1/responses", key, responsesFirst, false);
  assert.equal(firstResponses.status, 200, protect(firstResponses.text).slice(0, 400));
  const encrypted = clientEnvelope(firstResponses.parsed, ENCRYPTED_RAW, "encrypted_content");
  if (stress) {
    assertVisibleRedacted(firstResponses.parsed, "responses json");
    const output = firstResponses.parsed?.output ?? [];
    const reasoning = output.filter((item) => item?.type === "reasoning");
    assert.equal(reasoning.length, 1);
    assert.equal(reasoning[0].encrypted_content, encrypted);
    assert.equal(reasoning[0].summary?.[0]?.text, PLAN_TEXT);
    assert.equal(output.filter((item) => item?.type === "function_call").length, 1);
  }
  const responsesSecond = {
    ...responsesFirst,
    input: [
      responsesFirst.input[0],
      { type: "reasoning", id: "rs_echo", summary: [{ type: "summary_text", text: PLAN_TEXT }], encrypted_content: encrypted },
      { type: "function_call", id: RESPONSES_ITEM_ID, call_id: RESPONSES_CALL_ID, name: "echo", arguments: JSON.stringify(TOOL_ARGUMENTS) },
      { type: "function_call_output", call_id: RESPONSES_CALL_ID, output: TOOL_RESULT_TEXT },
    ],
  };
  const secondResponses = await gatewayJson(base, "/v1/responses", key, responsesSecond, false);
  assert.equal(secondResponses.status, 200, protect(secondResponses.text).slice(0, 400));
  const jsonHits = (name) => inferenceHits(mocks[name]).filter((hit) => hit.phase === "json");
  const messageHits = jsonHits("messages").slice(seen.messages);
  const responseHits = jsonHits("responses").slice(seen.responses);
  assert.equal(messageHits.length, 2);
  assert.equal(responseHits.length, 2);
  assertSecondTurn("messages", messageHits[1].body);
  assertSecondTurn("responses", responseHits[1].body);
  return {
    messages: { hits: 2, path: messageHits[1].pathname },
    responses: { hits: 2, path: responseHits[1].pathname },
    upstreamRaw: true,
    bound: { signature, redacted, encrypted },
  };
}

function hitCounts(mocks) {
  return Object.fromEntries(ROUNDS.map(([name]) => [name, inferenceHits(mocks[name]).length]));
}

async function gatewayWrongDomain(base, key, bound) {
  const messages = await gatewayJson(base, "/v1/messages", key, {
    model: "native-messages",
    stream: false,
    max_tokens: 256,
    system: SYSTEM_TEXT,
    tools: echoTool("messages"),
    messages: [
      { role: "user", content: [{ type: "text", text: "use echo" }] },
      {
        role: "assistant",
        content: [
          { type: "thinking", thinking: PLAN_TEXT, signature: withZeroDomain(bound.signature) },
          { type: "redacted_thinking", data: withZeroDomain(bound.redacted) },
          { type: "tool_use", id: MESSAGES_TOOL_ID, name: "echo", input: TOOL_ARGUMENTS },
        ],
      },
      { role: "user", content: [{ type: "tool_result", tool_use_id: MESSAGES_TOOL_ID, content: TOOL_RESULT_TEXT }] },
    ],
  }, true);
  const responses = await gatewayJson(base, "/v1/responses", key, {
    model: "native-responses",
    stream: false,
    store: false,
    tools: echoTool("responses"),
    input: [
      { role: "user", content: [{ type: "input_text", text: "use echo" }] },
      {
        type: "reasoning",
        id: "rs_echo",
        summary: [{ type: "summary_text", text: PLAN_TEXT }],
        encrypted_content: withZeroDomain(bound.encrypted),
      },
      {
        type: "function_call",
        id: RESPONSES_ITEM_ID,
        call_id: RESPONSES_CALL_ID,
        name: "echo",
        arguments: JSON.stringify(TOOL_ARGUMENTS),
      },
      { type: "function_call_output", call_id: RESPONSES_CALL_ID, output: TOOL_RESULT_TEXT },
    ],
  }, false);
  return {
    messagesStatus: messages.status,
    responsesStatus: responses.status,
    messagesBody: protect(messages.text).slice(0, 240),
    responsesBody: protect(responses.text).slice(0, 240),
  };
}

async function responsesClientToMessages(base, key, mocks) {
  const before = hitCounts(mocks);
  const firstBody = {
    model: "native-messages",
    stream: false,
    store: false,
    tools: echoTool("responses"),
    input: [{ role: "user", content: [{ type: "input_text", text: "use echo" }] }],
  };
  const first = await gatewayJson(base, "/v1/responses", key, firstBody, false);
  const paths = (name) => inferenceHits(mocks[name]).slice(before[name]).map((hit) => hit.pathname);
  if (first.status !== 200) {
    throw new Error(`responses client to messages upstream blocked: HTTP ${first.status} messagePaths=${paths("messages").join(",") || "none"} responsePaths=${paths("responses").join(",") || "none"} body=${protect(first.text).slice(0, 500)}`);
  }
  const output = Array.isArray(first.parsed?.output) ? first.parsed.output : [];
  const reasoning = output.filter((item) => item?.type === "reasoning" && typeof item.encrypted_content === "string");
  const calls = output.filter((item) => item?.type === "function_call" && item.call_id);
  if (reasoning.length === 0 || calls.length === 0) {
    const kinds = output.map((item) => item?.type ?? "null").join(",");
    throw new Error(`responses client to messages upstream returned no reasoning item and tool call: types=${kinds} body=${protect(JSON.stringify(first.parsed)).slice(0, 500)}`);
  }
  const second = await gatewayJson(base, "/v1/responses", key, {
    ...firstBody,
    input: [
      firstBody.input[0],
      ...reasoning,
      ...calls,
      ...calls.map((call) => ({ type: "function_call_output", call_id: call.call_id, output: TOOL_RESULT_TEXT })),
    ],
  }, false);
  const messageHits = inferenceHits(mocks.messages).slice(before.messages);
  const responseHits = inferenceHits(mocks.responses).slice(before.responses);
  if (second.status !== 200 || responseHits.length !== 0 || messageHits.length < 2) {
    throw new Error(`responses client to messages upstream second turn blocked: HTTP ${second.status} messagePaths=${messageHits.map((hit) => hit.pathname).join(",") || "none"} responsePaths=${responseHits.map((hit) => hit.pathname).join(",") || "none"} body=${protect(second.text).slice(0, 500)}`);
  }
  assertSecondTurn("messages", messageHits[messageHits.length - 1].body);
  return {
    clientPath: "/v1/responses",
    publicModel: "native-messages",
    upstreamPaths: messageHits.map((hit) => hit.pathname),
    rawRestored: true,
  };
}

function rejectSchema(catalog) {
  const rows = new Map((catalog?.data ?? []).map((row) => [row?.id, row]));
  const selected = [];
  for (const [, protocol, , publicModel] of ROUNDS) {
    const row = rows.get(publicModel);
    const version = row?.ocg?.schemaVersion;
    if (version === 1) throw new Error(`rejected schema 1 binary for ${publicModel}`);
    if (version !== 2) throw new Error(`rejected non-schema-2 catalog for ${publicModel}: ${String(version)}`);
    if (row.ocg?.protocols?.preferred !== protocol) {
      throw new Error(`${publicModel} preferred ${String(row.ocg?.protocols?.preferred)} != ${protocol}`);
    }
    if (!row.ocg.protocols.supported?.includes(protocol)) throw new Error(`${publicModel} preferred protocol is not supported`);
    selected.push({ id: publicModel, schemaVersion: 2, preferred: protocol });
  }
  return selected;
}

function runnerSource(bin, renderedIndex, stress = false, interleaved = false) {
  const models = ROUNDS.map(([, , , publicModel]) => publicModel);
  return `
    process.argv[1] = ${JSON.stringify(bin)};
    const echo = [{ name: "echo", description: "Echo text", parameters: { type: "object", properties: { text: { type: "string" } }, required: ["text"] } }];
    let stored;
    const credentials = {
      async set(ref, value) { stored = { ref, value }; },
      async resolve(ref) { return ref === stored?.ref ? { value: stored.value } : undefined; },
    };
    const { findPackageJSON } = await import("node:module");
    const { dirname: pathDirname, join: pathJoin } = await import("node:path");
    const { pathToFileURL: toFileUrl } = await import("node:url");
    function loadDsh(name, entry) {
      const manifest = findPackageJSON(name, toFileUrl(process.argv[1]).href);
      if (!manifest) throw new Error("missing " + name);
      return import(toFileUrl(pathJoin(pathDirname(manifest), entry)).href);
    }
    const [{ Context }, dshLlm] = await Promise.all([
      loadDsh("@deepseek-ai/cordis", "lib/index.js"),
      loadDsh("@deepseek-ai/dsh-llm", "lib/index.js"),
    ]);
    const ctx = new Context();
    new dshLlm.LlmRuntime(ctx);
    ctx.provide("credentials", credentials);
    const registerAdapter = ctx.llm.registerAdapter.bind(ctx.llm);
    ctx.llm.registerAdapter = (providers, value) => {
      ctx.adapter = value;
      return registerAdapter(providers, value);
    };
    function user(id, text) { return { id, role: "user", content: [{ type: "text", text }], source: { kind: "user" } }; }
    function projectChunk(chunk) {
      const row = { type: chunk?.type ?? null };
      if (typeof chunk?.index === "number") row.index = chunk.index;
      if (typeof chunk?.blockType === "string") row.blockType = chunk.blockType;
      if (chunk?.type === "block-end") {
        row.blockType = chunk.block?.type ?? null;
        if (typeof chunk.block?.id === "string") row.id = chunk.block.id.slice(0, 80);
      }
      if (chunk?.type === "tool-call-delta" && typeof chunk.id === "string") row.id = chunk.id.slice(0, 80);
      if (chunk?.type === "finish") {
        row.kind = chunk.reason?.kind ?? null;
        row.code = chunk.reason?.failure?.code ?? null;
      }
      return row;
    }
    function summarizeBlock(block) {
      if (!block || typeof block !== "object") return { type: null };
      const row = { type: block.type ?? null };
      if (typeof block.id === "string") row.id = block.id.slice(0, 80);
      if (typeof block.name === "string") row.name = block.name.slice(0, 80);
      if (typeof block.text === "string" && block.text.length > 0) row.text = block.text.slice(0, 120);
      if (block.redacted === true) row.redacted = true;
      return row;
    }
    function summarizeReplayBlock(block) {
      if (!block || typeof block !== "object") return { type: null };
      const signature = typeof block.thinkingSignature === "string"
        ? block.thinkingSignature
        : typeof block.textSignature === "string"
          ? block.textSignature
          : typeof block.thoughtSignature === "string"
            ? block.thoughtSignature
            : "";
      const marker = /^ocg-replay-v1:[0-9a-f]{64}:(.*)$/s.exec(signature);
      let cipher = null;
      if (!marker && signature.startsWith("{")) {
        try {
          const item = JSON.parse(signature);
          const encrypted = item?.encrypted_content;
          const encryptedMarker = typeof encrypted === "string" ? /^ocg-replay-v1:[0-9a-f]{64}:(.*)$/s.exec(encrypted) : null;
          cipher = encryptedMarker ? encryptedMarker[1] : typeof encrypted === "string" && encrypted.length > 0 ? "present" : null;
        } catch { cipher = "unparsed"; }
      }
      return {
        type: block.type ?? null,
        redacted: block.redacted === true,
        signature: marker ? "envelope" : signature ? "other" : "absent",
        payload: marker ? marker[1] : null,
        cipher,
      };
    }
    function placeBlocks(chunks) {
      const ends = [];
      const seen = new Set();
      let error = null;
      for (const chunk of chunks) {
        if (chunk?.type !== "block-end") continue;
        const index = chunk.index;
        if (!Number.isInteger(index) || index < 0) {
          error = error ?? ("block-end index " + String(index));
        } else if (seen.has(index)) {
          error = error ?? ("duplicate block-end index " + index);
        } else {
          seen.add(index);
        }
        ends.push({ index: Number.isInteger(index) ? index : -1, block: chunk.block });
      }
      if (!error) ends.sort((left, right) => left.index - right.index);
      return { error, blocks: ends.map((item) => item.block) };
    }
    function streamEdges(chunks) {
      const edges = [];
      for (const chunk of chunks) {
        if (chunk?.type !== "block-start" && chunk?.type !== "block-end") continue;
        const row = { type: chunk.type, index: Number.isInteger(chunk.index) ? chunk.index : null };
        const blockType = chunk.blockType ?? chunk.block?.type ?? null;
        if (typeof blockType === "string") row.blockType = blockType;
        const id = chunk.block?.id ?? chunk.id;
        if (typeof id === "string" && id.length > 0) row.id = id.slice(0, 80);
        edges.push(row);
      }
      return edges;
    }
    function coherenceOf(edges) {
      const open = new Map();
      let aligned = true;
      for (const edge of edges ?? []) {
        if (!Number.isInteger(edge.index)) aligned = false;
        if (edge.type === "block-start") {
          if (open.has(edge.index)) aligned = false;
          open.set(edge.index, edge.blockType ?? null);
        } else {
          const started = open.get(edge.index);
          if (started === undefined || (edge.blockType && started && started !== edge.blockType)) aligned = false;
          open.delete(edge.index);
        }
      }
      return { aligned: aligned && open.size === 0, open: [...open.keys()] };
    }
    function diagnose(turn) {
      const response = turn?.replayState?.response ?? null;
      const header = {};
      if (response && typeof response === "object") {
        for (const key of ["kind", "version", "api", "provider", "model", "stopReason", "responseId", "responseModel"]) {
          if (response[key] !== undefined) header[key] = response[key];
        }
      }
      const replayBlocks = Array.isArray(turn?.replayState?.blocks) ? turn.replayState.blocks : null;
      return JSON.stringify({
        finishKind: turn?.finishKind ?? null,
        thrown: turn?.thrown?.code ?? null,
        http: turn?.http ?? [],
        counts: {
          content: Array.isArray(turn?.blocks) ? turn.blocks.length : null,
          replay: replayBlocks ? replayBlocks.length : null,
        },
        coherence: coherenceOf(turn?.edges),
        edges: turn?.edges ?? [],
        chunks: turn?.chunkTrace ?? [],
        blocks: (turn?.blocks ?? []).map(summarizeBlock),
        replay: {
          response: Object.keys(header).length > 0 ? header : null,
          blocks: replayBlocks ? replayBlocks.map(summarizeReplayBlock) : null,
        },
      });
    }
    function collected(chunks, thrown) {
      const finish = chunks.find((chunk) => chunk.type === "finish");
      const trace = chunks.slice(0, 80).map(projectChunk);
      if (chunks.length > 80) trace.push({ type: "truncated", count: chunks.length });
      const placed = placeBlocks(chunks);
      return {
        thrown: thrown ?? (placed.error ? { code: "BLOCK_INDEX", message: placed.error } : null),
        finishKind: finish?.reason?.kind ?? null,
        finishCode: finish?.reason?.failure?.code ?? null,
        finishMessage: finish?.reason?.failure?.message ?? null,
        blocks: placed.blocks,
        edges: streamEdges(chunks),
        replayState: finish?.replayState ?? null,
        chunkTrace: trace,
      };
    }
    async function collect(stream) {
      const chunks = [];
      try { for await (const chunk of stream) chunks.push(chunk); }
      catch (error) {
        return collected(chunks, { code: error?.code ?? null, message: error instanceof Error ? error.message : String(error) });
      }
      return collected(chunks, null);
    }
    function expectKind(label, turn, kind) {
      if (turn.thrown) throw new Error(label + " threw " + turn.thrown.code + ": " + turn.thrown.message + "\\n" + diagnose(turn));
      if (turn.finishKind !== kind) throw new Error(label + " finish " + turn.finishKind + ": " + turn.finishMessage + "\\n" + diagnose(turn));
      return turn;
    }
    function assistant(id, model, turn) {
      return { id, role: "assistant", content: turn.blocks, source: { kind: "model", provider: "ocg", model, replayState: turn.replayState } };
    }
    function toolMessage(turn) {
      const call = turn.blocks.find((block) => block.type === "tool-call");
      if (!call) throw new Error("missing tool call");
      return { id: "tool-" + call.id, role: "tool", toolCallId: call.id, content: [{ type: "text", text: ${JSON.stringify(TOOL_RESULT_TEXT)} }], source: { kind: "tool", callId: call.id } };
    }
    function carried(state, raw) {
      let found = null;
      const walk = (value) => {
        if (found || value == null) return;
        if (typeof value === "string") {
          const match = /^ocg-replay-v1:[0-9a-f]{64}:(.*)$/s.exec(value);
          if (match && match[1] === raw) found = value;
          return;
        }
        if (Array.isArray(value)) value.forEach(walk);
        else if (typeof value === "object") Object.values(value).forEach(walk);
      };
      walk(state);
      return found;
    }
    function nestedEncrypted(state, raw) {
      let found = null;
      const walk = (value) => {
        if (found || value == null || typeof value !== "object") return;
        if (typeof value.thinkingSignature === "string") {
          try {
            const item = JSON.parse(value.thinkingSignature);
            const content = item?.encrypted_content;
            const match = typeof content === "string" ? /^ocg-replay-v1:[0-9a-f]{64}:(.*)$/s.exec(content) : null;
            if (match && match[1] === raw) found = content;
          } catch { /* messages signatures are direct markers, not this JSON item */ }
        }
        for (const child of Object.values(value)) walk(child);
      };
      walk(state);
      return found;
    }
    function captured(turn) {
      const response = turn.replayState?.response ?? null;
      return { api: response?.api ?? null, version: response?.version ?? null };
    }
    const plugin = await import(${JSON.stringify(pathToFileURL(renderedIndex).href)});
    await plugin.apply(ctx);
    const adapter = ctx.adapter;
    const serviceListed = await ctx.llm.listModels("ocg");
    const listed = await adapter.listModels("ocg");
    const resolved = {};
    const resolveErrors = {};
    for (const model of serviceListed) {
      try {
        resolved[model.id] = await ctx.llm.resolveModelInfo("ocg", model.id);
        await ctx.llm.prepareCall({ provider: "ocg", model: model.id });
      } catch (error) {
        resolveErrors[model.id] = error?.code ?? null;
      }
    }
    if (Object.values(resolveErrors).includes("INVALID_MODEL_REASONING")) {
      throw new Error("public model load rejected an empty effort menu: " + JSON.stringify(resolveErrors));
    }
    function effortView(info) {
      if (!info || !Object.hasOwn(info, "reasoning")) return null;
      return info.reasoning.efforts.map((effort) => ({ id: effort.id, name: effort.name }));
    }
    let posts = 0;
    const baseFetch = globalThis.fetch.bind(globalThis);
    globalThis.fetch = async (input, init) => {
      if ((init?.method ?? "GET") !== "GET") posts += 1;
      return baseFetch(input, init);
    };
    const messagesMenu = effortView(resolved["native-messages"]);
    const undeclaredEffort = messagesMenu ? "not-a-level" : "high";
    const postsBeforeUndeclared = posts;
    let undeclared = { threw: false, code: null, message: null };
    try {
      await ctx.llm.prepareCall({ provider: "ocg", model: "native-messages", reasoningEffort: undeclaredEffort });
    } catch (error) {
      undeclared = { threw: true, code: error?.code ?? null, message: error instanceof Error ? error.message : String(error) };
    }
    const undeclaredPosts = posts - postsBeforeUndeclared;
    const gate = {
      serviceIds: serviceListed.map((model) => model.id),
      menus: Object.fromEntries(${JSON.stringify(models)}.map((id) => [id, effortView(resolved[id]) ?? null])),
      resolveErrors,
      undeclaredEffort,
      undeclared,
      undeclaredPosts,
    };
    async function prepare(id) { return adapter.prepareCall("ocg", id); }
    const prepared = {};
    for (const id of ${JSON.stringify(models)}) prepared[id] = await prepare(id);
    let missing = { threw: false, message: null };
    try { await prepare("missing-model"); }
    catch (error) { missing = { threw: true, message: error instanceof Error ? error.message : String(error) }; }
    const wrongEnvelope = {
      response: { kind: "pi-ai", version: 2, api: "openai-responses", provider: "ocg", model: "native-chat", stopReason: "stop" },
      blocks: [{ type: "reasoning", thinkingSignature: "sig" }],
    };
    const wrongTurn = await collect(prepared["native-chat"].stream({
      provider: "ocg", model: "native-chat", system: ${JSON.stringify(SYSTEM_TEXT)}, tools: echo,
      messages: [
        user("u-wrong", "wrong-tuple"),
        { id: "a-wrong", role: "assistant", content: [{ type: "reasoning", text: "secret" }], source: { kind: "model", provider: "ocg", model: "native-chat", replayState: wrongEnvelope } },
      ],
    }));
    const httpTrace = [];
    let recordHttp = false;
    const originalFetch = globalThis.fetch.bind(globalThis);
    globalThis.fetch = async (input, init) => {
      const response = await originalFetch(input, init);
      if (!recordHttp) return response;
      let path = "unparsed";
      try {
        const raw = typeof input === "string" ? input : input?.url ?? "";
        path = new URL(raw, "http://127.0.0.1").pathname.slice(0, 80);
      } catch { path = "unparsed"; }
      const headers = {};
      const contentType = response.headers.get("content-type");
      if (contentType) headers["content-type"] = contentType.slice(0, 80);
      if (httpTrace.length < 8) httpTrace.push({ method: init?.method ?? "GET", path, status: response.status, headers });
      return response;
    };
    async function turn(id, messages) {
      const from = httpTrace.length;
      recordHttp = true;
      try {
        const result = await collect(prepared[id].stream({ provider: "ocg", model: id, system: ${JSON.stringify(SYSTEM_TEXT)}, tools: echo, messages }));
        result.http = httpTrace.slice(from);
        return result;
      } finally {
        recordHttp = false;
      }
    }
    async function pair(id) {
      const first = expectKind(id + " tool", await turn(id, [user("u-" + id, "use echo")]), "tool-calls");
      try {
        const second = expectKind(id + " final", await turn(id, [user("u-" + id, "use echo"), assistant("a-" + id, id, first), toolMessage(first)]), "stop");
        return { first, second };
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new Error(message + "\\nfirst-turn " + diagnose(first));
      }
    }
    const chat = await pair("native-chat");
    const responses = await pair("native-responses");
    const messages = await pair("native-messages");
    const textOf = (turn) => turn.blocks.some((block) => block?.type === "text" && block.text === ${JSON.stringify(FINAL_TEXT)});
    let redaction = null;
    if (${stress ? "true" : "false"}) {
      const phrase = ${JSON.stringify(VISIBLE_TEXT)};
      const plan = ${JSON.stringify(PLAN_TEXT)};
      const signatureRaw = ${JSON.stringify(SIGNATURE_RAW)};
      const redactedRaw = ${JSON.stringify(REDACTED_RAW)};
      const encryptedRaw = ${JSON.stringify(ENCRYPTED_RAW)};
      const marker = /^ocg-replay-v1:[0-9a-f]{64}:(.*)$/s;
      function payloads(state, raw) {
        const found = [];
        const walk = (value) => {
          if (typeof value === "string") {
            const match = marker.exec(value);
            if (match && match[1] === raw) found.push(value);
            return;
          }
          if (Array.isArray(value)) value.forEach(walk);
          else if (value && typeof value === "object") Object.values(value).forEach(walk);
        };
        walk(state);
        return found;
      }
      function cipherPayloads(state) {
        const found = [];
        const walk = (value) => {
          if (!value || typeof value !== "object") return;
          if (typeof value.thinkingSignature === "string") {
            try {
              const item = JSON.parse(value.thinkingSignature);
              if (typeof item.encrypted_content === "string") found.push(item.encrypted_content);
            } catch { /* Messages signatures are direct markers, not this JSON item */ }
          }
          for (const child of Object.values(value)) {
            if (child && typeof child === "object") walk(child);
          }
        };
        walk(state);
        return found;
      }
      function requireVisible(label, turn) {
        const texts = turn.blocks.filter((block) => block?.type === "text").map((block) => String(block.text ?? ""));
        if (texts.length !== 1 || texts[0].includes(phrase) || texts[0].includes("ocg") || !texts[0].includes("visible") || !texts[0].includes("value")) {
          throw new Error(label + " ordinary visible text was not redacted");
        }
      }
      function requireReasoning(label, turn) {
        const texts = turn.blocks.filter((block) => block?.type === "reasoning").map((block) => String(block.text ?? ""));
        if (!texts.includes(plan) || texts.some((text) => text.includes("ocg") || text.includes(phrase))) {
          throw new Error(label + " signed thinking text changed");
        }
      }
      function requireTool(label, turn) {
        const calls = turn.blocks.filter((block) => block?.type === "tool-call");
        if (calls.length !== 1) throw new Error(label + " tool call count " + calls.length);
      }
      function requireMarker(label, state, raw) {
        const found = payloads(state, raw);
        const payload = found.length === 1 ? marker.exec(found[0]) : null;
        if (!payload || payload[1] !== raw || raw.includes("ocg")) throw new Error(label + " opaque marker count " + found.length);
      }
      requireVisible("messages", messages.first);
      requireVisible("responses", responses.first);
      requireReasoning("messages", messages.first);
      requireReasoning("responses", responses.first);
      requireTool("messages", messages.first);
      requireTool("responses", responses.first);
      const interleaved = ${interleaved ? "true" : "false"};
      const messageOrder = ${JSON.stringify(interleaved ? "reasoning,text,reasoning,tool-call" : "text,reasoning,reasoning,tool-call")};
      const responseOrder = ${JSON.stringify(interleaved ? "reasoning,text,tool-call" : "text,reasoning,tool-call")};
      if (messages.first.blocks.map((block) => block.type).join(",") !== messageOrder) {
        throw new Error("messages block order " + messages.first.blocks.map((block) => block.type).join(","));
      }
      if (responses.first.blocks.map((block) => block.type).join(",") !== responseOrder) {
        throw new Error("responses block order " + responses.first.blocks.map((block) => block.type).join(","));
      }
      requireMarker("messages signature", messages.first.replayState, signatureRaw);
      requireMarker("messages redacted", messages.first.replayState, redactedRaw);
      const ciphers = cipherPayloads(responses.first.replayState);
      const cipher = ciphers.length === 1 ? marker.exec(ciphers[0]) : null;
      if (!cipher || cipher[1] !== encryptedRaw) throw new Error("responses cipher count " + ciphers.length);
      if (messages.second.finishKind !== "stop" || responses.second.finishKind !== "stop" || messages.first.finishKind !== "tool-calls" || responses.first.finishKind !== "tool-calls") {
        throw new Error("stress stop was not valid");
      }
      redaction = { nativeSdkRedactedVisibleText: true, messagesOpaque: true, responsesCipherOnce: true };
      if (interleaved) {
        const identify = (turn) => turn.blocks.map((block) => ({ type: block?.type ?? null, id: block?.id ?? null }));
        redaction.interleaved = true;
        redaction.order = { responses: responseOrder, messages: messageOrder };
        redaction.coherence = {
          responses: { ...coherenceOf(responses.first.edges), edges: responses.first.edges },
          messages: { ...coherenceOf(messages.first.edges), edges: messages.first.edges },
        };
        redaction.ids = {
          responseId: responses.first.replayState?.response?.responseId ?? null,
          messageId: messages.first.replayState?.response?.responseId ?? null,
          responses: identify(responses.first),
          messages: identify(messages.first),
        };
      }
    }
    const report = {
      gate,
      credentialRef: stored?.ref ?? null,
      models: listed.map((model) => ({ id: model.id, descriptorApi: model.api ?? null, schemaVersion: model.ocg?.schemaVersion ?? null, preferred: model.ocg?.protocols?.preferred ?? null })),
      captured: { chat: captured(chat.first), responses: captured(responses.first), messages: captured(messages.first) },
      missing,
      wrongTuple: { code: wrongTurn.thrown?.code ?? wrongTurn.finishCode ?? null, message: wrongTurn.thrown?.message ?? wrongTurn.finishMessage ?? null },
      finishes: { chat: [chat.first.finishKind, chat.second.finishKind], responses: [responses.first.finishKind, responses.second.finishKind], messages: [messages.first.finishKind, messages.second.finishKind] },
      ordinaryText: { chat: textOf(chat.second), responses: textOf(responses.second), messages: textOf(messages.second) },
      envelopes: {
        signature: carried(messages.first, ${JSON.stringify(SIGNATURE_RAW)}),
        redacted: carried(messages.first, ${JSON.stringify(REDACTED_RAW)}),
        encrypted: nestedEncrypted(responses.first, ${JSON.stringify(ENCRYPTED_RAW)}),
      },
    };
    if (redaction) report.redaction = redaction;
    process.stdout.write(JSON.stringify(report));
  `;
}

function assertSdk(runtime) {
  assert.equal(runtime.credentialRef, "OCG_GATEWAY_KEY");
  for (const [, , , publicModel] of ROUNDS) {
    assert.ok(runtime.gate.serviceIds.includes(publicModel), publicModel);
    assert.equal(runtime.gate.resolveErrors[publicModel] ?? null, null, publicModel);
    const menu = runtime.gate.menus[publicModel];
    assert.ok(menu === null || (Array.isArray(menu) && menu.length > 0 && menu.every((effort) => effort.id && effort.name)), publicModel);
  }
  assert.equal(Object.values(runtime.gate.resolveErrors).includes("INVALID_MODEL_REASONING"), false);
  assert.equal(runtime.gate.undeclared.threw, true);
  assert.equal(runtime.gate.undeclared.code, "UNSUPPORTED_REASONING_EFFORT");
  assert.equal(runtime.gate.undeclaredPosts, 0);
  assert.equal(runtime.missing.threw, true);
  assert.equal(runtime.wrongTuple.code, "OCG_PROTOCOL_REPLAY_REFUSED");
  for (const [name, protocol, , publicModel] of ROUNDS) {
    const model = runtime.models.find((item) => item.id === publicModel);
    assert.ok(model, publicModel);
    assert.equal(model.schemaVersion, 2, publicModel);
    assert.equal(model.preferred, protocol, publicModel);
    assert.equal(model.descriptorApi ?? null, null, `${publicModel} listModels descriptor has no api`);
    assert.equal(runtime.captured[name].version, 2, `${name} replayState.response.version`);
    assert.equal(runtime.captured[name].api, API_OF[protocol], `${publicModel} replayState.response.api`);
    assert.deepEqual(runtime.finishes[name], ["tool-calls", "stop"], name);
    assert.equal(runtime.ordinaryText[name], true, name);
  }
  assert.match(runtime.envelopes.signature, ENVELOPE);
  assert.match(runtime.envelopes.redacted, ENVELOPE);
  assert.match(runtime.envelopes.encrypted, ENVELOPE);
  assert.equal(ENVELOPE.exec(runtime.envelopes.signature)[1], SIGNATURE_RAW);
  assert.equal(ENVELOPE.exec(runtime.envelopes.redacted)[1], REDACTED_RAW);
  assert.equal(ENVELOPE.exec(runtime.envelopes.encrypted)[1], ENCRYPTED_RAW);
}

async function main() {
  const { cli: cliPath, redactionStress, interleavedStress } = readArgs();
  if (!(await exists(cliPath))) throw new Error(`CLI binary not found: ${cliPath}. This smoke does not build one.`);
  const bin = dshBin();
  if (!(await exists(bin))) throw new Error(`DSH CLI is missing: ${bin}`);
  const dshRoot = dirname(dirname(bin));
  const dshPackage = JSON.parse(await readFile(join(dshRoot, "package.json"), "utf8"));
  const piPackage = JSON.parse(await readFile(join(dshRoot, "node_modules", "@earendil-works", "pi-ai", "package.json"), "utf8"));
  assert.equal(dshPackage.version, installedDshVersion);
  assert.equal(piPackage.version, installedPiAiVersion);
  if (redactionStress) assertStressOracle();
  if (interleavedStress) assertInterleavedOracle();
  const root = await mkdtemp(join(tmpdir(), "ocg-native-protocol-"));
  assertDeletableSmokeRoot(root);
  const pluginDir = join(root, "plugin");
  const dataDir = join(root, "data");
  const logDir = join(root, "logs");
  const dshHome = join(root, "dsh-home");
  const mocks = {};
  let gatewayPid = null;
  let gatewayPort = null;
  let cleaned = false;
  async function cleanup() {
    if (cleaned) return { verified: true, idempotent: true };
    const resolved = assertDeletableSmokeRoot(root);
    cleaned = true;
    for (const mock of Object.values(mocks)) {
      if (mock) await mock.close().catch(() => {});
    }
    let proof = { verified: false };
    try {
      proof = await stopOwnedGateway({ pid: gatewayPid, port: gatewayPort, lab: null, listeners: [] });
    } finally {
      await rm(resolved, { recursive: true, force: true });
    }
    return proof;
  }
  try {
    if (/\s/.test(root)) throw new Error(`isolated smoke path contains whitespace: ${root}`);
    await Promise.all([
      cp(pluginSource, pluginDir, { recursive: true }),
      mkdir(dataDir),
      mkdir(logDir),
      mkdir(dshHome),
    ]);
    for (const [name, protocol, , , upstreamModel, inferencePath] of ROUNDS) {
      const secret = redactionStress ? STRESS_SECRET : `sk-native-${name}-dummy`;
      mocks[name] = await listen(protocol, secret, upstreamModel, inferencePath, redactionStress, interleavedStress);
      mocks[name].secret = secret;
    }
    const wrong = await fetch(`${mocks.chat.origin}/v1/chat/completions`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${mocks.chat.secret}`,
        "x-smoke-case": "wrong-domain",
      },
      body: JSON.stringify({
        model: "native-chat",
        stream: false,
        messages: [{ role: "user", content: "direct" }],
        domain: "ocg-replay-v1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:foreign",
      }),
    });
    assert.equal(wrong.status, 200);
    gatewayPort = await pickLoopbackPort();
    const launched = await startGatewayCli({
      cliPath, dataDir, logDir, port: gatewayPort, encryptionKey,
    });
    gatewayPid = launched.pid;
    const gatewayBase = `http://127.0.0.1:${gatewayPort}`;
    const connection = await waitForGateway(gatewayBase, gatewayPid, launched.stderr);
    const gatewayKey = connection.primaryKey;
    syntheticSecrets.push(gatewayKey);
    const api = makeApi(gatewayBase, null, () => gatewayKey);
    await registerSlots(api, ROUNDS.map(([name, protocol, auth, publicModel, upstreamModel]) => ({
      slot: name,
      url: `${mocks[name].origin}/v1`,
      protocol,
      auth,
      secret: mocks[name].secret,
      publicModel,
      model: upstreamModel,
    })));
    const catalogResponse = await fetch(`${gatewayBase}/v1/models`, { headers: { authorization: `Bearer ${gatewayKey}` } });
    const catalogText = await catalogResponse.text();
    assert.equal(catalogResponse.status, 200, catalogText.slice(0, 400));
    const catalog = JSON.parse(catalogText);
    const selected = rejectSchema(catalog);
    const renderedIndex = join(pluginDir, "index.js");
    const rendered = (await readFile(renderedIndex, "utf8"))
      .replaceAll("__OCG_GATEWAY_V1_URL__", `${gatewayBase}/v1`)
      .replaceAll("__OCG_CREDENTIAL_BOOTSTRAP_PATH_JSON__", JSON.stringify(join(root, "credential-handoff")));
    await writeFile(renderedIndex, rendered);
    await writeFile(join(root, "credential-handoff"), gatewayKey);
    const before = Object.fromEntries(ROUNDS.map(([name]) => [name, inferenceHits(mocks[name]).length]));
    const runner = join(root, "runtime-check.mjs");
    await writeFile(runner, runnerSource(bin, renderedIndex, redactionStress, interleavedStress));
    const runtime = JSON.parse((await runNode([runner], { env: runnerEnv(dshHome) })).stdout);
    for (const [name] of ROUNDS) {
      const added = inferenceHits(mocks[name]).slice(before[name]);
      assert.equal(added.some((hit) => JSON.stringify(hit.body).includes("wrong-tuple")), false, `${name} preflight`);
      assert.equal(added.some((hit) => hit.body?.model === "missing-model"), false, `${name} missing model`);
    }
    assertSdk(runtime);
    if (redactionStress) {
      assert.equal(runtime.redaction?.nativeSdkRedactedVisibleText, true);
      assert.equal(runtime.redaction?.messagesOpaque, true);
      assert.equal(runtime.redaction?.responsesCipherOnce, true);
    }
    const stream = assertRounds(mocks, before);
    const direct = await jsonDirect(gatewayBase, gatewayKey, mocks, redactionStress);
    const beforeForeign = hitCounts(mocks);
    const foreign = await gatewayWrongDomain(gatewayBase, gatewayKey, direct.bound);
    assert.notEqual(foreign.messagesStatus, 200, `gateway messages wrong domain served: ${foreign.messagesBody}`);
    assert.notEqual(foreign.responsesStatus, 200, `gateway responses wrong domain served: ${foreign.responsesBody}`);
    if (redactionStress) {
      assert.equal(foreign.messagesStatus, 400, `gateway messages wrong domain: ${foreign.messagesBody}`);
      assert.equal(foreign.responsesStatus, 400, `gateway responses wrong domain: ${foreign.responsesBody}`);
    }
    assert.deepEqual(hitCounts(mocks), beforeForeign, "gateway wrong domain reached an upstream mock");
    const beforeMissing = hitCounts(mocks);
    const rejected = await gatewayJson(gatewayBase, "/v1/chat/completions", gatewayKey, {
      model: "missing-model", stream: false, messages: [{ role: "user", content: "no" }],
    }, false);
    assert.notEqual(rejected.status, 200, "wrong model was served");
    assert.deepEqual(hitCounts(mocks), beforeMissing, "missing model reached an upstream mock");
    const cross = await responsesClientToMessages(gatewayBase, gatewayKey, mocks);
    const proof = await cleanup();
    assert.equal(proof.verified, true, protect(JSON.stringify(proof)));
    const selectedIds = new Set(selected.map((item) => item.id));
    const report = {
      status: "pass",
      builtBinary: false,
      source: {
        renderedPlugin: "integrations/dsh-plugin",
        schemaGuard: "reject-schema-1",
        selected,
      },
      nativeSdk: {
        dshVersion: dshPackage.version,
        piAiVersion: piPackage.version,
        isolatedDshHome: "root/dsh-home",
        descriptorApi: Object.fromEntries(runtime.models.filter((model) => selectedIds.has(model.id)).map((model) => [model.id, model.descriptorApi ?? null])),
        capturedApi: runtime.captured,
        finishes: runtime.finishes,
        ordinaryText: runtime.ordinaryText,
        wrongTuple: runtime.wrongTuple.code,
        missingModelRejected: runtime.missing.threw,
        modelGate: {
          serviceIds: runtime.gate.serviceIds,
          menus: runtime.gate.menus,
          resolveErrors: runtime.gate.resolveErrors,
          undeclaredEffort: runtime.gate.undeclaredEffort,
          undeclaredCode: runtime.gate.undeclared.code,
          undeclaredPosts: runtime.gate.undeclaredPosts,
        },
        sawEnvelopes: true,
      },
      gateway: {
        cliPath,
        port: gatewayPort,
        catalogStatus: 200,
        wrongModelStatus: rejected.status,
        wrongDomain: {
          messagesStatus: foreign.messagesStatus,
          responsesStatus: foreign.responsesStatus,
        },
        cleanup: proof.verified,
      },
      mock: {
        stream,
        jsonDirect: {
          messages: direct.messages,
          responses: direct.responses,
          upstreamRaw: direct.upstreamRaw,
        },
        directMockForeignDomain: "served",
        gatewayWrongDomain: {
          messagesStatus: foreign.messagesStatus,
          responsesStatus: foreign.responsesStatus,
          upstreamUnchanged: true,
        },
        responsesClientToMessages: cross,
        zeroHttpWrongModel: true,
      },
    };
    if (interleavedStress) {
      report.interleavedStress = {
        impliesRedactionStress: true,
        order: {
          responses: ["reasoning", "text", "tool"],
          messages: ["reasoning", "text", "reasoning", "tool"],
        },
        coherence: runtime.redaction?.coherence ?? null,
        ids: runtime.redaction?.ids ?? null,
        nativeSdkRedactedVisibleText: true,
        rawOpaqueRoundTrip: true,
        directJsonEnvelope: true,
        foreign400ZeroPost: true,
        oracle: "serves-upstream-bytes",
        foreign: {
          messagesStatus: foreign.messagesStatus,
          responsesStatus: foreign.responsesStatus,
          upstreamPosts: 0,
        },
      };
    }
    if (redactionStress && !interleavedStress) {
      report.redactionStress = {
        semantics: {
          credential: STRESS_SECRET,
          visibleText: VISIBLE_TEXT,
          placement: "before-native-opaque",
          order: ["text", "reasoning", "tool"],
          safeUnchanged: [PLAN_TEXT, SIGNATURE_RAW, REDACTED_RAW, ENCRYPTED_RAW],
          oracle: "serves-upstream-bytes",
        },
        nativeSdkRedactedVisibleText: true,
        rawOpaqueRoundTrip: true,
        directJsonEnvelope: true,
        foreign400ZeroPost: true,
        foreign: {
          messagesStatus: foreign.messagesStatus,
          responsesStatus: foreign.responsesStatus,
          upstreamPosts: 0,
        },
        source: {
          smoke: "scripts/ocg-native-client-protocol-smoke.mjs",
          plugin: "integrations/dsh-plugin",
          protocolFixtures: "scripts/dsh-protocol-fixtures.mjs",
        },
      };
    }
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
  } catch (error) {
    let tail = "";
    try {
      tail = protect((await readFile(join(logDir, "gateway.stderr.log"), "utf8")).slice(-1500));
    } catch {
      tail = "";
    }
    error.cleanup = await cleanup().catch((cleanupError) => ({ verified: false, error: protect(cleanupError) }));
    error.message = protect(tail ? `${error.message}\n${tail}` : error.message);
    if (error.stdout) error.stdout = protect(error.stdout);
    if (error.stderr) error.stderr = protect(error.stderr);
    throw error;
  }
}

main().catch((error) => {
  const stdout = error?.stdout ? `\n${protect(error.stdout)}` : "";
  const stderr = error?.stderr ? `\n${protect(error.stderr)}` : "";
  const text = error instanceof Error ? `${error.stack ?? error.message}` : String(error);
  console.error(`${protect(text)}${stdout}${stderr}`);
  process.exitCode = 1;
});
