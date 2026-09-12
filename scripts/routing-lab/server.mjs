import http from "node:http";
import { createHash } from "node:crypto";

const HOST = "127.0.0.1";
const BODY_LIMIT = 1024 * 1024;
const MAX_LOG = 2000;
const MARKER = "LAB_PROBE";
const CREATED_AT = 0;

const LISTENER_IDS = Object.freeze(["alpha", "bravo", "charlie"]);

const SLOT_DEFS = Object.freeze([
  {
    slot: "chat",
    listener: "alpha",
    protocol: "chat_completions",
    path: "/chat/v1/chat/completions",
    model: "upstream-chat",
    secret: "sk-lab-chat",
    auth: "bearer",
    ok: "LAB_OK_chat",
    publicModel: "lab-chat",
  },
  {
    slot: "responses",
    listener: "bravo",
    protocol: "responses",
    path: "/responses/v1/responses",
    model: "upstream-responses",
    secret: "sk-lab-responses",
    auth: "bearer",
    ok: "LAB_OK_responses",
    publicModel: "lab-responses",
  },
  {
    slot: "messages",
    listener: "charlie",
    protocol: "messages",
    path: "/messages/v1/messages",
    model: "upstream-messages",
    secret: "sk-lab-messages",
    auth: "x-api-key",
    ok: "LAB_OK_messages",
    publicModel: "lab-messages",
  },
  {
    slot: "alpha",
    listener: "alpha",
    protocol: "chat_completions",
    path: "/chat/v1/chat/completions",
    model: "upstream-alpha",
    secret: "sk-lab-alpha",
    auth: "bearer",
    ok: "LAB_OK_alpha",
    publicModel: "lab-route",
  },
  {
    slot: "bravo",
    listener: "bravo",
    protocol: "chat_completions",
    path: "/chat/v1/chat/completions",
    model: "upstream-bravo",
    secret: "sk-lab-bravo",
    auth: "bearer",
    ok: "LAB_OK_bravo",
    publicModel: "lab-route",
  },
  {
    slot: "charlie",
    listener: "charlie",
    protocol: "chat_completions",
    path: "/chat/v1/chat/completions",
    model: "upstream-charlie",
    secret: "sk-lab-charlie",
    auth: "bearer",
    ok: "LAB_OK_charlie",
    publicModel: "lab-route",
  },
]);

const CHAT_ROLES = new Set(["system", "user", "assistant", "tool", "developer"]);
const MESSAGES_ROLES = new Set(["user", "assistant"]);
const RESPONSES_ROLES = new Set(["user", "assistant", "system", "developer"]);
const AUTH_ERRORS = new Set([
  "missing_authorization",
  "invalid_authorization",
  "missing_x_api_key",
  "missing_anthropic_version",
  "redundant_credential_headers",
  "wrong_secret",
]);
const LAB_PATHS = new Set(["/_lab/health", "/_lab/requests", "/_lab/reset", "/_lab/script", "/_lab/journal"]);

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function pushError(errors, code) {
  if (!errors.includes(code)) errors.push(code);
}

function headerValue(value) {
  if (value == null) return "";
  if (Array.isArray(value)) return value.map(String).join("\n").trim();
  return String(value).trim();
}

function pathnameOf(req) {
  try {
    return new URL(req.url || "/", "http://127.0.0.1").pathname;
  } catch {
    return "";
  }
}

function isJsonContentType(value) {
  if (!value) return false;
  const media = String(Array.isArray(value) ? value[0] : value)
    .split(";")[0]
    .trim()
    .toLowerCase();
  return media === "application/json";
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function readBody(req, limit) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    let oversized = false;
    req.on("data", (chunk) => {
      size += chunk.length;
      if (size > limit) {
        oversized = true;
        return;
      }
      chunks.push(chunk);
    });
    req.on("end", () => {
      resolve({ buf: oversized ? Buffer.alloc(0) : Buffer.concat(chunks), oversized });
    });
    req.on("error", reject);
  });
}

function extractSecret(headers, auth) {
  const authorization = headerValue(headers.authorization);
  const xApiKey = headerValue(headers["x-api-key"]);
  if (auth === "bearer") {
    const match = /^Bearer[ \t]+(\S+)$/i.exec(authorization);
    return match ? match[1] : "";
  }
  return xApiKey;
}

function inspectAuth(headers, slot) {
  const authorization = headerValue(headers.authorization);
  const xApiKey = headerValue(headers["x-api-key"]);
  const anthropicVersion = headerValue(headers["anthropic-version"]);
  const errors = [];
  let authHeader = "none";
  if (authorization && xApiKey) authHeader = "both";
  else if (authorization) authHeader = "authorization";
  else if (xApiKey) authHeader = "x-api-key";
  const authHash = sha256(authorization || xApiKey || "");

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
  return { authHeader, authHash, errors, secret: extractSecret(headers, slot.auth) };
}

function collectToolNames(tools) {
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

function extractUserTexts(slotName, body) {
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
    else if (item.type === "message" || item.type == null) roles.push("user");
  }
  return roles;
}

function familyOf(slot) {
  if (slot.protocol === "messages") return "messages";
  if (slot.protocol === "responses") return "responses";
  return "chat";
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

function validateBody(slot, body) {
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
  if (model !== slot.model) pushError(errors, "wrong_model");
  if (body.tools != null && !Array.isArray(body.tools)) pushError(errors, "malformed_content");
  const family = familyOf(slot);
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
  if (!textMarkerPresent) pushError(errors, "missing_lab_probe");
  return { errors, model, stream, store, bodyKeys, roles, toolNames, textMarkerPresent };
}

function chatUsage() {
  return { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 };
}

function responsesUsage() {
  return { input_tokens: 1, output_tokens: 1, total_tokens: 2 };
}

function responsesObject(slot, { status, output, usage, completedAt }) {
  return {
    id: "resp-lab",
    object: "response",
    created_at: CREATED_AT,
    status,
    background: false,
    completed_at: completedAt,
    error: null,
    incomplete_details: null,
    instructions: null,
    max_output_tokens: null,
    model: slot.model,
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

function jsonSuccess(slot) {
  const family = familyOf(slot);
  if (family === "chat") {
    return {
      id: "chatcmpl-lab",
      object: "chat.completion",
      created: CREATED_AT,
      model: slot.model,
      choices: [{ index: 0, message: { role: "assistant", content: slot.ok }, finish_reason: "stop" }],
      usage: chatUsage(),
    };
  }
  if (family === "messages") {
    return {
      id: "msg-lab",
      type: "message",
      role: "assistant",
      model: slot.model,
      content: [{ type: "text", text: slot.ok }],
      stop_reason: "end_turn",
      stop_sequence: null,
      usage: { input_tokens: 1, output_tokens: 1 },
    };
  }
  const item = {
    type: "message",
    id: "msg_0",
    status: "completed",
    role: "assistant",
    content: [{ type: "output_text", text: slot.ok, annotations: [], logprobs: [] }],
  };
  return responsesObject(slot, { status: "completed", output: [item], usage: responsesUsage(), completedAt: CREATED_AT });
}

function sseEvent(event, data) {
  const prefix = event ? `event: ${event}\n` : "";
  return `${prefix}data: ${typeof data === "string" ? data : JSON.stringify(data)}\n\n`;
}

function chatSse(slot) {
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
  return chunk({ role: "assistant", content: slot.ok }, null) + chunk({}, "stop", chatUsage()) + sseEvent(null, "[DONE]");
}

function messagesSse(slot) {
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
    sseEvent("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }) +
    sseEvent("content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: slot.ok } }) +
    sseEvent("content_block_stop", { type: "content_block_stop", index: 0 }) +
    sseEvent("message_delta", {
      type: "message_delta",
      delta: { stop_reason: "end_turn", stop_sequence: null },
      usage: { output_tokens: 1 },
    }) +
    sseEvent("message_stop", { type: "message_stop" })
  );
}

function responsesSse(slot) {
  const itemId = "msg_0";
  const addedItem = { type: "message", id: itemId, status: "in_progress", role: "assistant", content: [] };
  const completedItem = {
    type: "message",
    id: itemId,
    status: "completed",
    role: "assistant",
    content: [{ type: "output_text", text: slot.ok, annotations: [], logprobs: [] }],
  };
  const created = responsesObject(slot, { status: "in_progress", output: [], usage: null, completedAt: null });
  const completed = responsesObject(slot, {
    status: "completed",
    output: [completedItem],
    usage: responsesUsage(),
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
    ["response.output_text.delta", { item_id: itemId, output_index: 0, content_index: 0, delta: slot.ok, logprobs: [] }],
    ["response.output_text.done", { item_id: itemId, output_index: 0, content_index: 0, text: slot.ok, logprobs: [] }],
    [
      "response.content_part.done",
      {
        item_id: itemId,
        output_index: 0,
        content_index: 0,
        part: { type: "output_text", text: slot.ok, annotations: [], logprobs: [] },
      },
    ],
    ["response.output_item.done", { output_index: 0, item: completedItem }],
    ["response.completed", { response: completed }],
  ];
  return events
    .map(([type, fields], sequenceNumber) => sseEvent(type, { type, sequence_number: sequenceNumber, ...fields }))
    .join("");
}

function streamBody(slot) {
  const family = familyOf(slot);
  if (family === "chat") return chatSse(slot);
  if (family === "messages") return messagesSse(slot);
  return responsesSse(slot);
}

function errorPayload(slot, errors, status) {
  const message = errors[0] || "invalid_request";
  const type = status === 401 ? "authentication_error" : "invalid_request_error";
  if (familyOf(slot) === "messages") return { type: "error", error: { type, message } };
  return { error: { message, type } };
}

function statusFor(errors) {
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

function writeJson(res, status, body) {
  const payload = JSON.stringify(body);
  res.writeHead(status, {
    "content-type": "application/json",
    "content-length": Buffer.byteLength(payload),
  });
  res.end(payload);
}

function writeSse(res, body) {
  res.writeHead(200, {
    "content-type": "text/event-stream",
    "cache-control": "no-cache",
  });
  res.end(body);
}

function resolveSlot(listenerId, path, headers) {
  const onListener = SLOT_DEFS.filter((slot) => slot.listener === listenerId && slot.path === path);
  if (onListener.length === 0) return null;
  const authorization = headerValue(headers.authorization);
  const xApiKey = headerValue(headers["x-api-key"]);
  const bearer = /^Bearer[ \t]+(\S+)$/i.exec(authorization)?.[1] ?? "";
  const bySecret = onListener.find((slot) => slot.secret === bearer || slot.secret === xApiKey);
  return bySecret ?? onListener[0];
}

function normalizeScript(item) {
  if (!item || typeof item !== "object") return { kind: "success" };
  const kind = item.kind === "http" || item.kind === "drop" || item.kind === "success" ? item.kind : "success";
  if (kind === "http") {
    const status = Number.parseInt(item.status, 10);
    return {
      kind: "http",
      status: Number.isFinite(status) ? status : 500,
      body: item.body ?? { error: { message: item.message || "scripted_http", type: "api_error" } },
    };
  }
  return { kind };
}

export function createLab({ host = HOST } = {}) {
  const journal = [];
  const queues = new Map(LISTENER_IDS.map((id) => [id, []]));
  const servers = [];
  const listeners = [];

  function remember(row) {
    journal.push(row);
    if (journal.length > MAX_LOG) journal.shift();
  }

  function reset() {
    journal.length = 0;
    for (const id of LISTENER_IDS) queues.set(id, []);
  }

  function script(listenerId, queue) {
    if (!LISTENER_IDS.includes(listenerId)) throw new Error(`unknown listener ${listenerId}`);
    queues.set(listenerId, (Array.isArray(queue) ? queue : [queue]).map(normalizeScript));
  }

  function snapshot() {
    return journal.map((row) => ({ ...row, errors: [...row.errors], bodyKeys: [...row.bodyKeys], roles: [...row.roles], toolNames: [...row.toolNames] }));
  }

  function takeScript(listenerId) {
    const queue = queues.get(listenerId) ?? [];
    if (queue.length === 0) return { kind: "success" };
    return queue.shift();
  }

  async function handleInference(req, res, listener, slot, path) {
    const raw = await readBody(req, BODY_LIMIT);
    const errors = [];
    if ((req.url || "").includes("?")) pushError(errors, "query_not_allowed");
    if (req.method !== "POST") pushError(errors, "method_not_post");
    if (req.method === "POST" && !isJsonContentType(req.headers["content-type"])) {
      pushError(errors, "content_type_not_json");
    }
    if (raw.oversized) pushError(errors, "body_too_large");

    const auth = inspectAuth(req.headers, slot);
    for (const code of auth.errors) pushError(errors, code);

    let parsed = null;
    let bodyInfo = {
      model: null,
      stream: false,
      store: undefined,
      bodyKeys: [],
      roles: [],
      toolNames: [],
      textMarkerPresent: false,
      bodyHash: sha256(raw.oversized ? "" : raw.buf),
    };

    if (!raw.oversized && req.method === "POST") {
      if (raw.buf.length === 0) pushError(errors, "invalid_json");
      else {
        try {
          parsed = JSON.parse(raw.buf.toString("utf8"));
        } catch {
          pushError(errors, "invalid_json");
        }
      }
    }

    if (!raw.oversized && req.method === "POST" && !errors.includes("invalid_json")) {
      const checked = validateBody(slot, parsed);
      for (const code of checked.errors) pushError(errors, code);
      bodyInfo = { ...checked, bodyHash: bodyInfo.bodyHash };
    }

    const scripted = takeScript(listener.id);
    const row = {
      sequence: journal.length + 1,
      t: new Date().toISOString(),
      listener: listener.id,
      port: listener.port,
      slot: slot.slot,
      protocol: slot.protocol,
      path,
      model: bodyInfo.model,
      stream: bodyInfo.stream,
      store: bodyInfo.store,
      valid: errors.length === 0,
      errors: [...errors],
      bodyKeys: bodyInfo.bodyKeys,
      roles: bodyInfo.roles,
      textMarkerPresent: bodyInfo.textMarkerPresent,
      authHeader: auth.authHeader,
      authHash: auth.authHash,
      bodyHash: bodyInfo.bodyHash,
      toolNames: bodyInfo.toolNames,
      scriptKind: scripted.kind,
      scriptStatus: scripted.kind === "http" ? scripted.status : scripted.kind === "success" ? 200 : null,
    };
    remember(row);

    if (scripted.kind === "drop") {
      req.socket.destroy();
      return;
    }
    if (scripted.kind === "http") {
      writeJson(res, scripted.status, scripted.body);
      return;
    }
    if (errors.length) {
      const status = statusFor(errors);
      writeJson(res, status, errorPayload(slot, errors, status));
      return;
    }
    if (bodyInfo.stream) writeSse(res, streamBody(slot));
    else writeJson(res, 200, jsonSuccess(slot));
  }

  async function handleLab(req, res, listener, path) {
    if (req.method !== "GET") await readBody(req, BODY_LIMIT);
    if (path === "/_lab/health") {
      if (req.method !== "GET") {
        writeJson(res, 400, { error: { message: "method_not_allowed" } });
        return;
      }
      writeJson(res, 200, { ok: true, listener: listener.id, port: listener.port });
      return;
    }
    if (path === "/_lab/requests" || path === "/_lab/journal") {
      if (req.method !== "GET") {
        writeJson(res, 400, { error: { message: "method_not_allowed" } });
        return;
      }
      writeJson(res, 200, { requests: snapshot() });
      return;
    }
    if (path === "/_lab/reset") {
      if (req.method !== "POST") {
        writeJson(res, 400, { error: { message: "method_not_allowed" } });
        return;
      }
      reset();
      writeJson(res, 200, { ok: true });
      return;
    }
    if (path === "/_lab/script") {
      if (req.method !== "POST") {
        writeJson(res, 400, { error: { message: "method_not_allowed" } });
        return;
      }
      writeJson(res, 400, { error: { message: "use_in_process_script_api" } });
    }
  }

  async function handleUnknown(req, res, listener) {
    const raw = await readBody(req, BODY_LIMIT);
    const path = pathnameOf(req);
    const authorization = headerValue(req.headers.authorization);
    const xApiKey = headerValue(req.headers["x-api-key"]);
    remember({
      sequence: journal.length + 1,
      t: new Date().toISOString(),
      listener: listener.id,
      port: listener.port,
      slot: "unknown",
      protocol: "unknown",
      path,
      model: null,
      stream: false,
      store: undefined,
      valid: false,
      errors: ["unknown_route"],
      bodyKeys: [],
      roles: [],
      textMarkerPresent: false,
      authHeader: authorization && xApiKey ? "both" : authorization ? "authorization" : xApiKey ? "x-api-key" : "none",
      authHash: sha256(authorization || xApiKey || ""),
      bodyHash: sha256(raw.buf),
      toolNames: [],
      scriptKind: "success",
      scriptStatus: 404,
    });
    writeJson(res, 404, { error: { message: "unknown_route" } });
  }

  function attach(listener) {
    const server = http.createServer((req, res) => {
      const path = pathnameOf(req);
      const slot = resolveSlot(listener.id, path, req.headers);
      const run = slot
        ? handleInference(req, res, listener, slot, path)
        : LAB_PATHS.has(path)
          ? handleLab(req, res, listener, path)
          : handleUnknown(req, res, listener);
      Promise.resolve(run).catch(() => {
        if (!res.headersSent) writeJson(res, 400, { error: { message: "invalid_request" } });
        else res.end();
      });
    });
    return server;
  }

  function listenOne(id) {
    return new Promise((resolve, reject) => {
      const listener = { id, host, port: 0, url: "" };
      const server = attach(listener);
      server.on("error", reject);
      server.listen(0, host, () => {
        const address = server.address();
        listener.port = address.port;
        listener.url = `http://${host}:${address.port}`;
        servers.push(server);
        listeners.push(listener);
        resolve(listener);
      });
    });
  }

  async function start() {
    for (const id of LISTENER_IDS) await listenOne(id);
    return {
      host,
      listeners: listeners.map((item) => ({ ...item })),
      slots: SLOT_DEFS.map((slot) => {
        const listener = listeners.find((item) => item.id === slot.listener);
        return { ...slot, url: `${listener.url}${slot.path}`, listenerUrl: listener.url, port: listener.port };
      }),
    };
  }

  function close() {
    return Promise.all(
      servers.map(
        (server) =>
          new Promise((resolve) => {
            if (typeof server.closeAllConnections === "function") server.closeAllConnections();
            server.close(() => resolve());
          }),
      ),
    );
  }

  return { start, close, reset, script, snapshot, listeners, journal };
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function selfCheck() {
  const chat = SLOT_DEFS.find((slot) => slot.slot === "chat");
  const responses = SLOT_DEFS.find((slot) => slot.slot === "responses");
  const ok = validateBody(chat, {
    model: chat.model,
    messages: [{ role: "user", content: [{ type: "text", text: `ping ${MARKER}` }] }],
    tools: [{ type: "function", function: { name: "lookup", parameters: { type: "object" } } }],
  });
  assert(ok.errors.length === 0, `chat native should pass: ${ok.errors}`);
  const responsesOk = validateBody(responses, {
    model: responses.model,
    store: false,
    input: MARKER,
  });
  assert(responsesOk.errors.length === 0, `responses store=false should pass: ${responsesOk.errors}`);
  const responsesStore = validateBody(responses, { model: responses.model, input: MARKER });
  assert(responsesStore.errors.includes("store_not_false"), "responses without store=false must fail");
  const bearer = inspectAuth({ authorization: `Bearer ${chat.secret}` }, chat);
  assert(bearer.errors.length === 0, "chat bearer");
}

if (process.argv.includes("--self-check")) {
  selfCheck();
  process.stdout.write("self-check ok\n");
  process.exit(0);
}

if (import.meta.url === `file://${process.argv[1].replaceAll("\\", "/")}` || process.argv[1]?.endsWith("server.mjs")) {
  if (!process.argv.includes("--self-check") && process.argv.includes("--listen")) {
    const lab = createLab();
    const started = await lab.start();
    process.stdout.write(`${JSON.stringify({ listeners: started.listeners }, null, 2)}\n`);
  }
}
