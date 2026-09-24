import {
  CREATED_AT,
  MARKER,
  TOOL_MARKER,
  TOOL_NAME,
  UTF8_MARKER,
  familyOfProtocol,
  headerValue,
  isObject,
  promptDigest,
  sha256,
  sseEvent,
} from "./common.mjs";

const CHAT_ROLES = new Set(["system", "user", "assistant", "tool", "developer"]);
const MESSAGES_ROLES = new Set(["user", "assistant"]);
const RESPONSES_ROLES = new Set(["user", "assistant", "system", "developer"]);
export const AUTH_ERRORS = new Set([
  "missing_authorization",
  "invalid_authorization",
  "missing_x_api_key",
  "missing_anthropic_version",
  "redundant_credential_headers",
  "wrong_secret",
]);

export function pushError(errors, code) {
  if (!errors.includes(code)) errors.push(code);
}

export function extractSecret(headers, auth) {
  const authorization = headerValue(headers.authorization);
  const xApiKey = headerValue(headers["x-api-key"]);
  if (auth === "bearer") {
    const match = /^Bearer[ \t]+(\S+)$/i.exec(authorization);
    return match ? match[1] : "";
  }
  return xApiKey;
}

export function inspectAuth(headers, slot) {
  const authorization = headerValue(headers.authorization);
  const xApiKey = headerValue(headers["x-api-key"]);
  const anthropicVersion = headerValue(headers["anthropic-version"]);
  const errors = [];
  let authHeader = "none";
  if (authorization && xApiKey) authHeader = "both";
  else if (authorization) authHeader = "authorization";
  else if (xApiKey) authHeader = "x-api-key";
  const authHash = sha256(authorization || xApiKey || "");
  const secret = extractSecret(headers, slot.auth);

  if (authorization && xApiKey) pushError(errors, "redundant_credential_headers");
  if (slot.auth === "bearer") {
    if (xApiKey) pushError(errors, "redundant_credential_headers");
    if (!authorization) pushError(errors, "missing_authorization");
    else {
      const match = /^Bearer[ \t]+(\S+)$/i.exec(authorization);
      if (!match) pushError(errors, "invalid_authorization");
      else if (match[1] !== slot.secret) pushError(errors, "wrong_secret");
    }
  } else {
    if (authorization) pushError(errors, "redundant_credential_headers");
    if (!xApiKey) pushError(errors, "missing_x_api_key");
    else if (xApiKey !== slot.secret) pushError(errors, "wrong_secret");
    if (!anthropicVersion) pushError(errors, "missing_anthropic_version");
  }
  return { authHeader, authHash, errors, secret };
}

export function collectToolNames(tools) {
  if (!Array.isArray(tools)) return [];
  const names = [];
  for (const tool of tools) {
    if (!isObject(tool)) continue;
    if (typeof tool.function?.name === "string" && tool.function.name) {
      names.push(tool.function.name);
      continue;
    }
    if (typeof tool.name === "string" && tool.name) names.push(tool.name);
    if (Array.isArray(tool.functionDeclarations)) {
      for (const decl of tool.functionDeclarations) {
        if (isObject(decl) && typeof decl.name === "string" && decl.name) names.push(decl.name);
      }
    }
    if (Array.isArray(tool.tools)) names.push(...collectToolNames(tool.tools));
  }
  return names;
}

function textFromContent(content, family) {
  if (typeof content === "string") return [content];
  if (!Array.isArray(content)) return [];
  const texts = [];
  for (const part of content) {
    if (typeof part === "string") {
      texts.push(part);
      continue;
    }
    if (!isObject(part)) continue;
    const type = typeof part.type === "string" ? part.type : "";
    if (family === "chat" && (type === "text" || type === "output_text" || type === "input_text" || !type)) {
      if (typeof part.text === "string") texts.push(part.text);
    } else if (family === "messages" && (type === "text" || !type) && typeof part.text === "string") {
      texts.push(part.text);
    } else if (
      family === "responses" &&
      (type === "input_text" || type === "output_text" || type === "text" || !type) &&
      typeof part.text === "string"
    ) {
      texts.push(part.text);
    }
  }
  return texts;
}

export function extractUserTexts(slotName, body) {
  const texts = [];
  if (slotName === "chat" || slotName === "messages" || slotName === "alpha" || slotName === "bravo" || slotName === "charlie") {
    const family = slotName === "messages" ? "messages" : "chat";
    const messages = Array.isArray(body.messages) ? body.messages : [];
    for (const message of messages) {
      if (!isObject(message) || message.role !== "user") continue;
      texts.push(...textFromContent(message.content, family));
    }
    return texts;
  }
  if (typeof body.input === "string") {
    texts.push(body.input);
    return texts;
  }
  if (!Array.isArray(body.input)) return texts;
  for (const item of body.input) {
    if (typeof item === "string") {
      texts.push(item);
      continue;
    }
    if (!isObject(item)) continue;
    const type = typeof item.type === "string" ? item.type : "message";
    if (type === "input_text" && typeof item.text === "string") {
      texts.push(item.text);
      continue;
    }
    if (type !== "message") continue;
    const role = typeof item.role === "string" ? item.role : "user";
    if (role !== "user") continue;
    texts.push(...textFromContent(item.content, "responses"));
  }
  return texts;
}

function collectRoles(slotName, body) {
  const roles = [];
  const messagesFamily = slotName === "messages" || slotName === "chat" || slotName === "alpha" || slotName === "bravo" || slotName === "charlie";
  if (messagesFamily) {
    if (!Array.isArray(body.messages)) return roles;
    for (const message of body.messages) {
      if (isObject(message) && typeof message.role === "string") roles.push(message.role);
    }
    return roles;
  }
  if (typeof body.input === "string") {
    roles.push("user");
    return roles;
  }
  if (!Array.isArray(body.input)) return roles;
  for (const item of body.input) {
    if (typeof item === "string") {
      roles.push("user");
      continue;
    }
    if (!isObject(item)) continue;
    if (typeof item.role === "string") roles.push(item.role);
    else if (item.type === "function_call") roles.push("assistant");
    else if (item.type === "function_call_output") roles.push("tool");
    else if (item.type === "message" || item.type == null) roles.push("user");
  }
  return roles;
}

function contentLooksNative(content, family, role) {
  if (typeof content === "string") return true;
  if (content == null) return family === "chat" && (role === "assistant" || role === "tool");
  if (!Array.isArray(content)) return false;
  for (const part of content) {
    if (typeof part === "string") continue;
    if (!isObject(part)) return false;
    const type = part.type;
    if (type != null && typeof type !== "string") return false;
    if (["text", "input_text", "output_text"].includes(type) && typeof part.text !== "string") return false;
    if (family === "chat") {
      if (type && !["text", "image_url", "output_text", "input_text", "image", "file", "input_audio"].includes(type)) {
        if (typeof part.text !== "string" && !isObject(part.image_url) && typeof part.image_url !== "string") {
          return false;
        }
      }
      continue;
    }
    if (family === "messages") {
      if (!type) {
        if (typeof part.text !== "string") return false;
        continue;
      }
      if (!["text", "image", "tool_use", "tool_result", "thinking", "redacted_thinking", "document"].includes(type)) {
        return false;
      }
      continue;
    }
    if (type && !["input_text", "output_text", "text", "input_image", "refusal", "output_image"].includes(type)) {
      return false;
    }
  }
  return true;
}

function validateMessagesFamily(body, errors) {
  if (Object.prototype.hasOwnProperty.call(body, "input")) pushError(errors, "wrong_family_input");
  if (!Object.prototype.hasOwnProperty.call(body, "messages")) {
    pushError(errors, "missing_messages");
    return;
  }
  if (!Array.isArray(body.messages)) {
    pushError(errors, "malformed_content");
    return;
  }
  if (body.messages.length === 0) pushError(errors, "empty_messages");
}

function validateChatShape(body, errors) {
  validateMessagesFamily(body, errors);
  if (!Array.isArray(body.messages)) return;
  for (const message of body.messages) {
    if (!isObject(message)) {
      pushError(errors, "malformed_content");
      continue;
    }
    if (typeof message.role !== "string" || !CHAT_ROLES.has(message.role)) pushError(errors, "malformed_role");
    if (message.role === "tool" && typeof message.tool_call_id !== "string") pushError(errors, "malformed_content");
    if (!contentLooksNative(message.content, "chat", message.role)) pushError(errors, "malformed_content");
    if (message.tool_calls != null && !Array.isArray(message.tool_calls)) pushError(errors, "malformed_content");
  }
}

function validateMessagesShape(body, errors) {
  validateMessagesFamily(body, errors);
  if (body.system != null && typeof body.system !== "string" && !Array.isArray(body.system)) {
    pushError(errors, "malformed_content");
  }
  if (!Array.isArray(body.messages)) return;
  for (const message of body.messages) {
    if (!isObject(message)) {
      pushError(errors, "malformed_content");
      continue;
    }
    if (typeof message.role !== "string" || !MESSAGES_ROLES.has(message.role)) pushError(errors, "malformed_role");
    if (!contentLooksNative(message.content, "messages", message.role)) pushError(errors, "malformed_content");
  }
}

function validateResponsesItem(item, errors) {
  if (typeof item === "string") return;
  if (!isObject(item)) {
    pushError(errors, "malformed_content");
    return;
  }
  const type = typeof item.type === "string" ? item.type : "message";
  if (
    [
      "function_call",
      "function_call_output",
      "custom_tool_call",
      "custom_tool_call_output",
      "reasoning",
      "item_reference",
      "tool_search_call",
      "tool_search_output",
      "web_search_call",
      "input_text",
    ].includes(type)
  ) {
    return;
  }
  if (type !== "message") {
    pushError(errors, "malformed_content");
    return;
  }
  const role = typeof item.role === "string" ? item.role : "user";
  if (!RESPONSES_ROLES.has(role)) pushError(errors, "malformed_role");
  if (item.content != null && !contentLooksNative(item.content, "responses", role)) {
    pushError(errors, "malformed_content");
  }
}

function validateResponsesShape(body, errors) {
  if (Object.prototype.hasOwnProperty.call(body, "messages")) pushError(errors, "wrong_family_messages");
  if (!Object.prototype.hasOwnProperty.call(body, "input")) {
    pushError(errors, "missing_input");
    return;
  }
  if (typeof body.input === "string") {
    if (body.input.length === 0) pushError(errors, "empty_input");
    return;
  }
  if (!Array.isArray(body.input)) {
    pushError(errors, "malformed_content");
    return;
  }
  if (body.input.length === 0) pushError(errors, "empty_input");
  for (const item of body.input) validateResponsesItem(item, errors);
}

export function validateBody(slot, body) {
  const errors = [];
  if (!isObject(body)) {
    return {
      errors: ["malformed_content"],
      model: null,
      stream: false,
      store: undefined,
      bodyKeys: [],
      roles: [],
      toolNames: [],
      textMarkerPresent: false,
    };
  }
  const bodyKeys = Object.keys(body);
  const model = typeof body.model === "string" ? body.model : null;
  const stream = body.stream === true;
  const store = Object.prototype.hasOwnProperty.call(body, "store") ? body.store : undefined;
  if (body.stream !== undefined && typeof body.stream !== "boolean") pushError(errors, "malformed_stream");
  const allowedModels = new Set([slot.model, ...(Array.isArray(slot.catalog) ? slot.catalog : [])]);
  if (!allowedModels.has(model)) pushError(errors, "wrong_model");
  if (body.tools != null && !Array.isArray(body.tools)) pushError(errors, "malformed_content");
  const family = familyOfProtocol(slot.protocol);
  if (family === "chat") validateChatShape(body, errors);
  else if (family === "messages") validateMessagesShape(body, errors);
  else {
    validateResponsesShape(body, errors);
    if (store !== false) pushError(errors, "store_not_false");
  }
  const roles = collectRoles(slot.slot, body);
  const toolNames = collectToolNames(body.tools);
  const userText = extractUserTexts(slot.slot, body).join("\n");
  const textMarkerPresent = userText.includes(MARKER);
  const toolMarkerPresent = userText.includes(TOOL_MARKER);
  const utf8MarkerPresent = userText.includes(UTF8_MARKER);
  const toolEvidence = extractToolEvidence(slot, body);
  const hasToolResult = toolEvidence.hasToolResult;
  if (!textMarkerPresent) pushError(errors, "missing_lab_probe");
  return {
    errors,
    model,
    stream,
    store,
    bodyKeys,
    roles,
    toolNames,
    textMarkerPresent,
    toolMarkerPresent,
    utf8MarkerPresent,
    hasToolResult,
    toolCallIds: toolEvidence.toolCallIds,
    argumentDigest: toolEvidence.argumentDigest,
    resultDigest: toolEvidence.resultDigest,
  };
}

export function chatUsage() {
  return { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 };
}

export function responsesUsage() {
  return { input_tokens: 1, output_tokens: 1, total_tokens: 2 };
}

export function responsesObject(slot, { status, output, usage, completedAt, model, incompleteDetails = null }) {
  return {
    id: "resp-lab",
    object: "response",
    created_at: CREATED_AT,
    status,
    background: false,
    completed_at: status === "completed" ? completedAt : null,
    error: null,
    incomplete_details: incompleteDetails,
    instructions: null,
    max_output_tokens: null,
    model: model ?? slot.model,
    output,
    parallel_tool_calls: true,
    previous_response_id: null,
    reasoning: { effort: null, summary: null },
    store: false,
    temperature: null,
    text: { format: { type: "text" } },
    tool_choice: "auto",
    tools: [],
    usage,
  };
}

function detectToolResult(slot, body) {
  return extractToolEvidence(slot, body).hasToolResult;
}

export function extractToolEvidence(_slot, body) {
  const toolCallIds = [];
  const argumentParts = [];
  const resultParts = [];
  if (Array.isArray(body?.messages)) {
    for (const message of body.messages) {
      if (!isObject(message)) continue;
      if (Array.isArray(message.tool_calls)) {
        for (const call of message.tool_calls) {
          if (call?.id) toolCallIds.push(call.id);
          const args = call?.function?.arguments ?? call?.arguments;
          if (typeof args === "string") argumentParts.push(args);
        }
      }
      if (message.role === "tool") {
        if (message.tool_call_id) toolCallIds.push(message.tool_call_id);
        if (typeof message.content === "string") resultParts.push(message.content);
      }
      if (Array.isArray(message.content)) {
        for (const part of message.content) {
          if (!isObject(part)) continue;
          if (part.type === "tool_use" && part.id) {
            toolCallIds.push(part.id);
            if (part.input != null) argumentParts.push(typeof part.input === "string" ? part.input : JSON.stringify(part.input));
          }
          if (part.type === "tool_result") {
            if (part.tool_use_id) toolCallIds.push(part.tool_use_id);
            if (typeof part.content === "string") resultParts.push(part.content);
          }
        }
      }
    }
  }
  if (Array.isArray(body?.input)) {
    for (const item of body.input) {
      if (!isObject(item)) continue;
      if (item.type === "function_call") {
        if (item.call_id || item.id) toolCallIds.push(item.call_id || item.id);
        if (typeof item.arguments === "string") argumentParts.push(item.arguments);
      }
      if (item.type === "function_call_output") {
        if (item.call_id) toolCallIds.push(item.call_id);
        if (typeof item.output === "string") resultParts.push(item.output);
      }
    }
  }
  return {
    hasToolResult: resultParts.length > 0,
    toolCallIds: [...new Set(toolCallIds)],
    argumentDigest: argumentParts.length ? promptDigest(argumentParts.join("")) : null,
    resultDigest: resultParts.length ? promptDigest(resultParts.join("")) : null,
  };
}

export function jsonToolCall(slot) {
  const family = familyOfProtocol(slot.protocol);
  const args = JSON.stringify({ value: "ping" });
  if (family === "chat") {
    return {
      id: "chatcmpl-lab",
      object: "chat.completion",
      created: CREATED_AT,
      model: slot.model,
      choices: [
        {
          index: 0,
          message: {
            role: "assistant",
            content: null,
            tool_calls: [{ id: "call_lab_echo", type: "function", function: { name: TOOL_NAME, arguments: args } }],
          },
          finish_reason: "tool_calls",
        },
      ],
      usage: chatUsage(),
    };
  }
  if (family === "messages") {
    return {
      id: "msg-lab",
      type: "message",
      role: "assistant",
      model: slot.model,
      content: [{ type: "tool_use", id: "call_lab_echo", name: TOOL_NAME, input: { value: "ping" } }],
      stop_reason: "tool_use",
      stop_sequence: null,
      usage: { input_tokens: 1, output_tokens: 1 },
    };
  }
  return responsesObject(slot, {
    status: "completed",
    output: [
      {
        type: "function_call",
        id: "call_lab_echo",
        call_id: "call_lab_echo",
        name: TOOL_NAME,
        arguments: args,
        status: "completed",
      },
    ],
    usage: responsesUsage(),
    completedAt: CREATED_AT,
  });
}

export function streamToolCall(slot) {
  const family = familyOfProtocol(slot.protocol);
  if (family === "chat") {
    const chunk = (delta, finishReason, usage) => {
      const body = {
        id: "chatcmpl-lab",
        object: "chat.completion.chunk",
        created: CREATED_AT,
        model: slot.model,
        choices: [{ index: 0, delta, finish_reason: finishReason }],
      };
      if (usage) body.usage = usage;
      return sseEvent(null, body);
    };
    return (
      chunk({
        role: "assistant",
        content: null,
        tool_calls: [{ index: 0, id: "call_lab_echo", type: "function", function: { name: TOOL_NAME, arguments: "" } }],
      }, null) +
      chunk({ tool_calls: [{ index: 0, function: { arguments: "{\"val" } }] }, null) +
      chunk({ tool_calls: [{ index: 0, function: { arguments: "ue\":\"ping\"}" } }] }, null) +
      chunk({}, "tool_calls", chatUsage()) +
      sseEvent(null, "[DONE]")
    );
  }
  if (family === "messages") {
    return (
      sseEvent("message_start", {
        type: "message_start",
        message: {
          id: "msg-lab",
          type: "message",
          role: "assistant",
          model: slot.model,
          content: [],
          stop_reason: null,
          stop_sequence: null,
          usage: { input_tokens: 1, output_tokens: 0 },
        },
      }) +
      sseEvent("content_block_start", {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", id: "call_lab_echo", name: TOOL_NAME, input: {} },
      }) +
      sseEvent("content_block_delta", {
        type: "content_block_delta",
        index: 0,
        delta: { type: "input_json_delta", partial_json: "{\"value\":\"ping\"}" },
      }) +
      sseEvent("content_block_stop", { type: "content_block_stop", index: 0 }) +
      sseEvent("message_delta", {
        type: "message_delta",
        delta: { stop_reason: "tool_use", stop_sequence: null },
        usage: { output_tokens: 1 },
      }) +
      sseEvent("message_stop", { type: "message_stop" })
    );
  }
  const call = {
    type: "function_call",
    id: "call_lab_echo",
    call_id: "call_lab_echo",
    name: TOOL_NAME,
    arguments: "",
    status: "in_progress",
  };
  const done = { ...call, arguments: JSON.stringify({ value: "ping" }), status: "completed" };
  const created = responsesObject(slot, { status: "in_progress", output: [], usage: null, completedAt: null });
  const completed = responsesObject(slot, { status: "completed", output: [done], usage: responsesUsage(), completedAt: CREATED_AT });
  return [
    ["response.created", { response: created }],
    ["response.output_item.added", { output_index: 0, item: call }],
    ["response.function_call_arguments.delta", { output_index: 0, delta: "{\"val" }],
    ["response.function_call_arguments.delta", { output_index: 0, delta: "ue\":\"ping\"}" }],
    ["response.function_call_arguments.done", { output_index: 0, arguments: JSON.stringify({ value: "ping" }) }],
    ["response.output_item.done", { output_index: 0, item: done }],
    ["response.completed", { response: completed }],
  ]
    .map(([type, fields], sequenceNumber) => sseEvent(type, { type, sequence_number: sequenceNumber, ...fields }))
    .join("");
}

export function jsonSuccess(slot, { omitUsage = false, text, toolCall = false } = {}) {
  if (toolCall) return jsonToolCall(slot);
  const family = familyOfProtocol(slot.protocol);
  const content = text ?? slot.ok;
  if (family === "chat") {
    const body = {
      id: "chatcmpl-lab",
      object: "chat.completion",
      created: CREATED_AT,
      model: slot.model,
      choices: [{ index: 0, message: { role: "assistant", content }, finish_reason: "stop" }],
    };
    if (!omitUsage) body.usage = chatUsage();
    return body;
  }
  if (family === "messages") {
    const body = {
      id: "msg-lab",
      type: "message",
      role: "assistant",
      model: slot.model,
      content: [{ type: "text", text: content }],
      stop_reason: "end_turn",
      stop_sequence: null,
    };
    if (!omitUsage) body.usage = { input_tokens: 1, output_tokens: 1 };
    return body;
  }
  const item = {
    type: "message",
    id: "msg_0",
    status: "completed",
    role: "assistant",
    content: [{ type: "output_text", text: content, annotations: [], logprobs: [] }],
  };
  return responsesObject(slot, {
    status: "completed",
    output: [item],
    usage: omitUsage ? null : responsesUsage(),
    completedAt: CREATED_AT,
  });
}

export function chatSse(slot, { omitUsage = false, omitEnd = false, text } = {}) {
  const content = text ?? slot.ok;
  const chunk = (delta, finishReason, usage) => {
    const body = {
      id: "chatcmpl-lab",
      object: "chat.completion.chunk",
      created: CREATED_AT,
      model: slot.model,
      choices: [{ index: 0, delta, finish_reason: finishReason }],
    };
    if (usage) body.usage = usage;
    return sseEvent(null, body);
  };
  let body = chunk({ role: "assistant", content }, null);
  if (!omitEnd) {
    body += chunk({}, "stop", omitUsage ? undefined : chatUsage());
    body += sseEvent(null, "[DONE]");
  }
  return body;
}

export function messagesSse(slot, { omitUsage = false, omitEnd = false, text } = {}) {
  const content = text ?? slot.ok;
  let body =
    sseEvent("message_start", {
      type: "message_start",
      message: {
        id: "msg-lab",
        type: "message",
        role: "assistant",
        model: slot.model,
        content: [],
        stop_reason: null,
        stop_sequence: null,
        usage: omitUsage ? undefined : { input_tokens: 1, output_tokens: 0 },
      },
    }) +
    sseEvent("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }) +
    sseEvent("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: content } }) +
    sseEvent("content_block_stop", { type: "content_block_stop", index: 0 }) +
    sseEvent("message_delta", {
      type: "message_delta",
      delta: { stop_reason: "end_turn", stop_sequence: null },
      usage: omitUsage ? undefined : { output_tokens: 1 },
    });
  if (!omitEnd) body += sseEvent("message_stop", { type: "message_stop" });
  return body;
}

export function responsesSse(slot, { omitUsage = false, omitEnd = false, text } = {}) {
  const content = text ?? slot.ok;
  const itemId = "msg_0";
  const addedItem = { type: "message", id: itemId, status: "in_progress", role: "assistant", content: [] };
  const completedItem = {
    type: "message",
    id: itemId,
    status: "completed",
    role: "assistant",
    content: [{ type: "output_text", text: content, annotations: [], logprobs: [] }],
  };
  const created = responsesObject(slot, { status: "in_progress", output: [], usage: null, completedAt: null });
  const completed = responsesObject(slot, {
    status: "completed",
    output: [completedItem],
    usage: omitUsage ? null : responsesUsage(),
    completedAt: CREATED_AT,
  });
  const events = [
    ["response.created", { response: created }],
    ["response.output_item.added", { output_index: 0, item: addedItem }],
    [
      "response.content_part.added",
      {
        item_id: itemId,
        output_index: 0,
        content_index: 0,
        part: { type: "output_text", text: "", annotations: [], logprobs: [] },
      },
    ],
    ["response.output_text.delta", { item_id: itemId, output_index: 0, content_index: 0, delta: content, logprobs: [] }],
    ["response.output_text.done", { item_id: itemId, output_index: 0, content_index: 0, text: content, logprobs: [] }],
    [
      "response.content_part.done",
      {
        item_id: itemId,
        output_index: 0,
        content_index: 0,
        part: { type: "output_text", text: content, annotations: [], logprobs: [] },
      },
    ],
    ["response.output_item.done", { output_index: 0, item: completedItem }],
  ];
  if (!omitEnd) events.push(["response.completed", { response: completed }]);
  return events
    .map(([type, fields], sequenceNumber) => sseEvent(type, { type, sequence_number: sequenceNumber, ...fields }))
    .join("");
}

export function streamBody(slot, options = {}) {
  if (options.toolCall) return streamToolCall(slot);
  const family = familyOfProtocol(slot.protocol);
  if (family === "chat") return chatSse(slot, options);
  if (family === "messages") return messagesSse(slot, options);
  return responsesSse(slot, options);
}

export function errorPayload(slot, errors, status) {
  const message = errors[0] || "invalid_request";
  const type = status === 401 ? "authentication_error" : "invalid_request_error";
  if (familyOfProtocol(slot.protocol) === "messages") return { type: "error", error: { type, message } };
  return { error: { message, type } };
}

export function statusFor(errors) {
  if (
    errors.includes("method_not_post") ||
    errors.includes("content_type_not_json") ||
    errors.includes("body_too_large") ||
    errors.includes("invalid_json")
  ) {
    return 400;
  }
  if (errors.some((code) => AUTH_ERRORS.has(code))) return 401;
  return 400;
}

export function modelsCatalog(slot) {
  const ids = slot.catalog?.length ? slot.catalog : [slot.model];
  return {
    object: "list",
    data: ids.map((id) => ({
      id,
      object: "model",
      created: CREATED_AT,
      owned_by: "gateway-lab",
    })),
  };
}

export function splitChunks(text, size) {
  const out = [];
  for (let i = 0; i < text.length; i += size) out.push(text.slice(i, i + size));
  return out;
}
