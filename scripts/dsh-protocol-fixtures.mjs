// Loopback SSE for the installed pi-ai 0.87.1 parsers.
// Chat and Responses are OpenAI `data:` events. Messages are Anthropic
// `event:` + `data:` lines and do not use `data: [DONE]`.

export const SYSTEM_TEXT = "ocg-system";
export const PLAN_TEXT = "plan";
export const FINAL_TEXT = "smoke-ok";
export const TOOL_RESULT_TEXT = "echoed";
export const TOOL_ARGUMENTS = Object.freeze({ text: "hi" });
export const CHAT_TOOL_ID = "call_echo";
export const RESPONSES_CALL_ID = "call_echo";
export const RESPONSES_ITEM_ID = "fc_echo";
export const MESSAGES_TOOL_ID = "toolu_echo";
export const MESSAGES_SIGNATURE = "sig-1";
export const FLIP_PATH = "/ocg/__fixture__/flip";

export const EFFORTS = Object.freeze({ low: "low", high: "high", xhigh: "max" });

function row(id, preferred, supported, extra = {}) {
  return {
    id,
    ocg: {
      schemaVersion: 2,
      contextWindow: 262144,
      maxOutputTokens: 32768,
      inputModalities: ["text", "image"],
      reasoning: true,
      protocols: { preferred, supported },
      ...extra,
    },
  };
}

export function catalogPayload(generation = 1) {
  const chat = generation === 1
    ? row("smoke-chat", "chat_completions", ["chat_completions", "responses"], { reasoningEfforts: EFFORTS })
    : row("smoke-chat", "messages", ["messages"]);
  return {
    object: "list",
    data: [
      chat,
      row("smoke-responses", "responses", ["responses"], { reasoningEfforts: EFFORTS }),
      row("smoke-messages", "messages", ["messages"], { reasoningEfforts: EFFORTS }),
      row("smoke-responses-plain", "responses", ["responses"]),
      { id: "legacy-model" },
    ],
  };
}

function dataEvents(events) {
  return `${events.map((event) => `data: ${JSON.stringify(event)}\n\n`).join("")}data: [DONE]\n\n`;
}

function anthropicEvents(events) {
  return events.map((event) => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join("");
}

function chatChunk(modelId, delta, finishReason, usage) {
  return {
    id: "chatcmpl-smoke",
    object: "chat.completion.chunk",
    created: 0,
    model: modelId,
    choices: [{ index: 0, delta, finish_reason: finishReason }],
    ...(usage ? { usage } : {}),
  };
}

function chatSse(phase, modelId) {
  const usage = { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 };
  if (phase === "tool") {
    return dataEvents([
      chatChunk(modelId, { role: "assistant", reasoning_content: PLAN_TEXT }, null),
      chatChunk(modelId, {
        tool_calls: [{
          index: 0,
          id: CHAT_TOOL_ID,
          type: "function",
          function: { name: "echo", arguments: JSON.stringify(TOOL_ARGUMENTS) },
        }],
      }, null),
      chatChunk(modelId, {}, "tool_calls", usage),
    ]);
  }
  return dataEvents([
    chatChunk(modelId, { role: "assistant", content: FINAL_TEXT }, null),
    chatChunk(modelId, {}, "stop", usage),
  ]);
}

function responsesSse(phase, modelId) {
  const completed = (output) => ({
    type: "response.completed",
    response: {
      id: "resp_smoke",
      status: "completed",
      output,
      usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 },
    },
  });
  if (phase === "tool") {
    const reasoning = {
      type: "reasoning",
      id: "rs_echo",
      summary: [{ type: "summary_text", text: PLAN_TEXT }],
    };
    return dataEvents([
      { type: "response.created", response: { id: "resp_smoke", model: modelId } },
      { type: "response.output_item.added", output_index: 0, item: { type: "reasoning", id: "rs_echo" } },
      { type: "response.reasoning_summary_text.delta", output_index: 0, delta: PLAN_TEXT },
      { type: "response.output_item.done", output_index: 0, item: reasoning },
      {
        type: "response.output_item.added",
        output_index: 1,
        item: { type: "function_call", id: RESPONSES_ITEM_ID, call_id: RESPONSES_CALL_ID, name: "echo", arguments: "" },
      },
      { type: "response.function_call_arguments.delta", output_index: 1, delta: JSON.stringify(TOOL_ARGUMENTS) },
      {
        type: "response.output_item.done",
        output_index: 1,
        item: {
          type: "function_call",
          id: RESPONSES_ITEM_ID,
          call_id: RESPONSES_CALL_ID,
          name: "echo",
          arguments: JSON.stringify(TOOL_ARGUMENTS),
        },
      },
      completed([]),
    ]);
  }
  const message = {
    type: "message",
    id: "msg_smoke",
    role: "assistant",
    status: "completed",
    content: [{ type: "output_text", text: FINAL_TEXT }],
  };
  return dataEvents([
    { type: "response.created", response: { id: "resp_final", model: modelId } },
    { type: "response.output_item.added", output_index: 0, item: { type: "message", id: "msg_smoke", role: "assistant" } },
    { type: "response.output_text.delta", output_index: 0, delta: FINAL_TEXT },
    { type: "response.output_item.done", output_index: 0, item: message },
    completed([message]),
  ]);
}

function messagesSse(phase, modelId) {
  const start = {
    type: "message_start",
    message: {
      id: "msg_smoke",
      type: "message",
      role: "assistant",
      model: modelId,
      content: [],
      stop_reason: null,
      stop_sequence: null,
      usage: { input_tokens: 1, output_tokens: 1 },
    },
  };
  if (phase === "tool") {
    return anthropicEvents([
      start,
      { type: "content_block_start", index: 0, content_block: { type: "thinking", thinking: "", signature: "" } },
      { type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: PLAN_TEXT } },
      { type: "content_block_delta", index: 0, delta: { type: "signature_delta", signature: MESSAGES_SIGNATURE } },
      { type: "content_block_stop", index: 0 },
      {
        type: "content_block_start",
        index: 1,
        content_block: { type: "tool_use", id: MESSAGES_TOOL_ID, name: "echo", input: {} },
      },
      {
        type: "content_block_delta",
        index: 1,
        delta: { type: "input_json_delta", partial_json: JSON.stringify(TOOL_ARGUMENTS) },
      },
      { type: "content_block_stop", index: 1 },
      { type: "message_delta", delta: { stop_reason: "tool_use", stop_sequence: null }, usage: { output_tokens: 8 } },
      { type: "message_stop" },
    ]);
  }
  return anthropicEvents([
    start,
    { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } },
    { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: FINAL_TEXT } },
    { type: "content_block_stop", index: 0 },
    { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 2 } },
    { type: "message_stop" },
  ]);
}

export function protocolSse(protocol, phase, modelId) {
  if (protocol === "chat_completions") return chatSse(phase, modelId);
  if (protocol === "responses") return responsesSse(phase, modelId);
  if (protocol === "messages") return messagesSse(phase, modelId);
  throw new Error(`Unknown loopback protocol: ${protocol}`);
}
