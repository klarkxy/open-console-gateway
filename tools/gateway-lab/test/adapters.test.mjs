import assert from "node:assert/strict";
import test from "node:test";
import {
  canonicalToChatJson,
  canonicalToChatRequest,
  canonicalToMessagesJson,
  canonicalToResponsesJson,
  chatResponseToCanonical,
  chatToCanonical,
  createLiveStreamTranslator,
  messagesToCanonical,
  responsesToCanonical,
  rewriteProtocolModel,
} from "../lib/adapters.mjs";
import { LIVE_MODEL } from "../lib/common.mjs";

const TOOL_ARGS = JSON.stringify({ value: "ping", model: "do-not-rewrite-me" });

test("chat text, tool call, and tool result round-trip to Chat Completions outbound", () => {
  const body = {
    model: "upstream-chat",
    messages: [
      { role: "system", content: "sys" },
      { role: "user", content: [{ type: "text", text: "call it" }] },
      {
        role: "assistant",
        content: null,
        tool_calls: [{ id: "c1", type: "function", function: { name: "lab_echo", arguments: TOOL_ARGS } }],
      },
      { role: "tool", tool_call_id: "c1", content: "{\"value\":\"ping\"}" },
    ],
    tools: [{ type: "function", function: { name: "lab_echo", parameters: { type: "object" } } }],
  };
  const canonical = chatToCanonical(body);
  const outbound = canonicalToChatRequest(canonical, { liveModel: LIVE_MODEL, maxTokens: 512 });
  assert.equal(outbound.model, LIVE_MODEL);
  assert.equal(outbound.max_tokens, 512);
  assert.equal(outbound.messages[2].tool_calls[0].function.arguments, TOOL_ARGS);
  assert.equal(outbound.messages[3].tool_call_id, "c1");
  assert.equal(outbound.tools[0].function.name, "lab_echo");
});

test("messages tool_use and tool_result convert without rewriting tool input", () => {
  const body = {
    model: "upstream-messages",
    system: "sys",
    messages: [
      { role: "user", content: [{ type: "text", text: "hi" }] },
      {
        role: "assistant",
        content: [{ type: "tool_use", id: "tu1", name: "lab_echo", input: { value: "ping", model: "keep" } }],
      },
      {
        role: "user",
        content: [{ type: "tool_result", tool_use_id: "tu1", content: "pong" }],
      },
    ],
    tools: [{ name: "lab_echo", input_schema: { type: "object" } }],
  };
  const canonical = messagesToCanonical(body);
  const outbound = canonicalToChatRequest(canonical);
  assert.equal(outbound.model, LIVE_MODEL);
  const assistant = outbound.messages.find((item) => item.role === "assistant");
  assert.equal(JSON.parse(assistant.tool_calls[0].function.arguments).model, "keep");
  const tool = outbound.messages.find((item) => item.role === "tool");
  assert.equal(tool.tool_call_id, "tu1");
});

test("responses input, function_call, and function_call_output convert", () => {
  const body = {
    model: "upstream-responses",
    store: false,
    input: [
      { type: "message", role: "user", content: [{ type: "input_text", text: "hi" }] },
      { type: "function_call", call_id: "f1", name: "lab_echo", arguments: TOOL_ARGS },
      { type: "function_call_output", call_id: "f1", output: "pong" },
    ],
    tools: [{ type: "function", name: "lab_echo", parameters: { type: "object" } }],
  };
  const canonical = responsesToCanonical(body);
  const outbound = canonicalToChatRequest(canonical);
  assert.equal(outbound.messages.some((item) => item.role === "tool" && item.tool_call_id === "f1"), true);
  assert.equal(outbound.messages.find((item) => item.role === "assistant").tool_calls[0].function.arguments, TOOL_ARGS);
});

test("remote chat response maps back and only protocol model fields are rewritten", () => {
  const remote = {
    id: "chatcmpl-x",
    model: LIVE_MODEL,
    choices: [
      {
        index: 0,
        finish_reason: "tool_calls",
        message: {
          role: "assistant",
          content: null,
          tool_calls: [{ id: "c1", type: "function", function: { name: "lab_echo", arguments: TOOL_ARGS } }],
        },
      },
    ],
    usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 },
  };
  const result = chatResponseToCanonical(remote);
  const chat = canonicalToChatJson(result, "upstream-chat");
  assert.equal(chat.model, "upstream-chat");
  assert.equal(chat.choices[0].message.tool_calls[0].function.arguments, TOOL_ARGS);
  const messages = canonicalToMessagesJson(result, "upstream-messages");
  assert.equal(messages.model, "upstream-messages");
  assert.equal(messages.content[0].type, "tool_use");
  assert.equal(messages.content[0].input.model, "do-not-rewrite-me");
  const responses = canonicalToResponsesJson(result, { model: "upstream-responses" });
  assert.equal(responses.model, "upstream-responses");
  assert.equal(responses.output[0].arguments, TOOL_ARGS);
});

test("rewriteProtocolModel does not walk tool arguments", () => {
  const payload = {
    model: LIVE_MODEL,
    choices: [{ message: { tool_calls: [{ function: { arguments: TOOL_ARGS } }] } }],
  };
  const rewritten = rewriteProtocolModel("chat", payload, "upstream-chat");
  assert.equal(rewritten.model, "upstream-chat");
  assert.equal(rewritten.choices[0].message.tool_calls[0].function.arguments, TOOL_ARGS);
});

test("messages conversion preserves actual input usage and does not default to 1", () => {
  const result = {
    assistant: { content: "ok", finish_reason: "stop" },
    usage: { prompt_tokens: 17, completion_tokens: 4, total_tokens: 21 },
  };
  const messages = canonicalToMessagesJson(result, "upstream-messages");
  assert.equal(messages.usage.input_tokens, 17);
  assert.equal(messages.usage.output_tokens, 4);
  const unknown = canonicalToMessagesJson({ assistant: { content: "ok" }, usage: null }, "upstream-messages");
  assert.equal(unknown.usage, undefined);
});

test("messages SSE preserves late input_tokens=17 and output_tokens=4", () => {
  const frames = [];
  const translator = createLiveStreamTranslator({
    protocol: "messages",
    slot: { model: "upstream-messages" },
    onWrite: (chunk) => frames.push(chunk),
    onEnd: () => {},
  });
  translator.onChatPayload({
    choices: [{ index: 0, delta: { content: "hi" }, finish_reason: null }],
  });
  translator.onChatPayload({
    choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
    usage: { prompt_tokens: 17, completion_tokens: 4, total_tokens: 21 },
  });
  translator.onChatPayload("[DONE]");
  const objects = frames
    .join("")
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data: "))
    .map((line) => JSON.parse(line.slice(6)));
  const usages = objects.flatMap((item) => [item.usage, item.message?.usage].filter(Boolean));
  assert.equal(
    usages.some((usage) => usage.input_tokens === 17),
    true,
    `missing input_tokens=17 in ${JSON.stringify(usages)}`,
  );
  assert.equal(
    usages.some((usage) => usage.output_tokens === 4),
    true,
    `missing output_tokens=4 in ${JSON.stringify(usages)}`,
  );
  assert.equal(
    usages.some((usage) => usage.input_tokens === 1),
    false,
    `invented input_tokens=1 in ${JSON.stringify(usages)}`,
  );
});

test("responses length finish_reason maps to incomplete not completed", () => {
  const result = {
    assistant: { content: "cut", finish_reason: "length" },
    usage: { prompt_tokens: 2, completion_tokens: 8, total_tokens: 10 },
  };
  const json = canonicalToResponsesJson(result, { model: "upstream-responses" });
  assert.equal(json.status, "incomplete");
  assert.equal(json.incomplete_details.reason, "max_output_tokens");
  const frames = [];
  const translator = createLiveStreamTranslator({
    protocol: "responses",
    slot: { model: "upstream-responses" },
    onWrite: (chunk) => frames.push(chunk),
    onEnd: () => {},
  });
  translator.onChatPayload({
    choices: [{ delta: { content: "cut" }, finish_reason: "length" }],
    usage: { prompt_tokens: 2, completion_tokens: 8, total_tokens: 10 },
  });
  translator.onChatPayload("[DONE]");
  const text = frames.join("");
  assert.equal(text.includes("response.completed"), false);
  assert.equal(text.includes("response.incomplete"), true);
});
