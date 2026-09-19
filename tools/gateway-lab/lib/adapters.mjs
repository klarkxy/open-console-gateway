import { CREATED_AT, LIVE_MODEL, familyOfProtocol, isObject, sseEvent } from "./common.mjs";
import { chatUsage, responsesObject, responsesUsage } from "./protocol.mjs";

export function mapUsageFields(usage) {
  if (!usage || typeof usage !== "object") return null;
  const inputTokens = usage.prompt_tokens ?? usage.input_tokens;
  const outputTokens = usage.completion_tokens ?? usage.output_tokens;
  const totalTokens = usage.total_tokens;
  if (inputTokens == null && outputTokens == null && totalTokens == null) return null;
  return {
    input_tokens: inputTokens,
    output_tokens: outputTokens,
    total_tokens: totalTokens,
  };
}

function asText(content, family = "chat") {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  const parts = [];
  for (const part of content) {
    if (typeof part === "string") {
      parts.push(part);
      continue;
    }
    if (!isObject(part)) continue;
    const type = typeof part.type === "string" ? part.type : "";
    if (typeof part.text === "string" && (!type || ["text", "input_text", "output_text"].includes(type))) {
      parts.push(part.text);
    }
  }
  return parts.join("");
}

function openaiTools(tools) {
  if (!Array.isArray(tools)) return undefined;
  const out = [];
  for (const tool of tools) {
    if (!isObject(tool)) continue;
    if (tool.type === "function" && isObject(tool.function) && typeof tool.function.name === "string") {
      out.push({
        type: "function",
        function: {
          name: tool.function.name,
          description: tool.function.description,
          parameters: tool.function.parameters ?? { type: "object", properties: {} },
        },
      });
      continue;
    }
    if (typeof tool.name === "string") {
      out.push({
        type: "function",
        function: {
          name: tool.name,
          description: tool.description,
          parameters: tool.parameters ?? tool.input_schema ?? { type: "object", properties: {} },
        },
      });
    }
  }
  return out.length ? out : undefined;
}

export function chatToCanonical(body) {
  const messages = [];
  for (const message of Array.isArray(body.messages) ? body.messages : []) {
    if (!isObject(message)) continue;
    const mapped = {
      role: message.role,
      content: message.content == null ? null : asText(message.content, "chat") || message.content,
    };
    if (message.tool_call_id) mapped.tool_call_id = message.tool_call_id;
    if (Array.isArray(message.tool_calls)) {
      mapped.tool_calls = message.tool_calls.map((call) => ({
        id: call.id,
        name: call.function?.name ?? call.name,
        arguments: typeof call.function?.arguments === "string" ? call.function.arguments : JSON.stringify(call.function?.arguments ?? call.arguments ?? {}),
      }));
    }
    messages.push(mapped);
  }
  return {
    family: "chat",
    model: body.model,
    stream: body.stream === true,
    messages,
    tools: openaiTools(body.tools),
    max_tokens: body.max_tokens ?? body.max_completion_tokens,
  };
}

export function messagesToCanonical(body) {
  const messages = [];
  if (typeof body.system === "string" && body.system) {
    messages.push({ role: "system", content: body.system });
  } else if (Array.isArray(body.system)) {
    const text = asText(body.system, "messages");
    if (text) messages.push({ role: "system", content: text });
  }
  for (const message of Array.isArray(body.messages) ? body.messages : []) {
    if (!isObject(message)) continue;
    if (Array.isArray(message.content)) {
      const texts = [];
      const toolCalls = [];
      const toolResults = [];
      for (const part of message.content) {
        if (!isObject(part)) continue;
        if (part.type === "text" && typeof part.text === "string") texts.push(part.text);
        else if (part.type === "tool_use") {
          toolCalls.push({
            id: part.id,
            name: part.name,
            arguments: JSON.stringify(part.input ?? {}),
          });
        } else if (part.type === "tool_result") {
          const content = typeof part.content === "string" ? part.content : JSON.stringify(part.content ?? "");
          toolResults.push({ role: "tool", tool_call_id: part.tool_use_id, content });
        }
      }
      if (toolResults.length) messages.push(...toolResults);
      if (texts.length || toolCalls.length) {
        messages.push({
          role: message.role,
          content: texts.join("") || null,
          tool_calls: toolCalls.length ? toolCalls : undefined,
        });
      }
    } else {
      messages.push({ role: message.role, content: asText(message.content, "messages") });
    }
  }
  return {
    family: "messages",
    model: body.model,
    stream: body.stream === true,
    messages,
    tools: openaiTools(body.tools),
    max_tokens: body.max_tokens,
  };
}

export function responsesToCanonical(body) {
  const messages = [];
  if (typeof body.instructions === "string" && body.instructions) {
    messages.push({ role: "system", content: body.instructions });
  }
  const pushInput = (item) => {
    if (typeof item === "string") {
      messages.push({ role: "user", content: item });
      return;
    }
    if (!isObject(item)) return;
    const type = typeof item.type === "string" ? item.type : "message";
    if (type === "input_text" && typeof item.text === "string") {
      messages.push({ role: "user", content: item.text });
      return;
    }
    if (type === "function_call") {
      messages.push({
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: item.call_id || item.id,
            name: item.name,
            arguments: typeof item.arguments === "string" ? item.arguments : JSON.stringify(item.arguments ?? {}),
          },
        ],
      });
      return;
    }
    if (type === "function_call_output") {
      messages.push({
        role: "tool",
        tool_call_id: item.call_id,
        content: typeof item.output === "string" ? item.output : JSON.stringify(item.output ?? ""),
      });
      return;
    }
    if (type === "message" || item.role) {
      messages.push({
        role: item.role || "user",
        content: asText(item.content, "responses") || (typeof item.content === "string" ? item.content : ""),
      });
    }
  };
  if (typeof body.input === "string") pushInput(body.input);
  else if (Array.isArray(body.input)) for (const item of body.input) pushInput(item);
  return {
    family: "responses",
    model: body.model,
    stream: body.stream === true,
    messages,
    tools: openaiTools(body.tools),
    max_tokens: body.max_output_tokens ?? body.max_tokens,
    store: body.store,
  };
}

export function toCanonical(protocol, body) {
  const family = familyOfProtocol(protocol);
  if (family === "messages") return messagesToCanonical(body);
  if (family === "responses") return responsesToCanonical(body);
  return chatToCanonical(body);
}

export function canonicalToChatRequest(canonical, { liveModel = LIVE_MODEL, maxTokens = 512 } = {}) {
  const messages = canonical.messages.map((message) => {
    const out = { role: message.role, content: message.content };
    if (message.tool_call_id) out.tool_call_id = message.tool_call_id;
    if (Array.isArray(message.tool_calls)) {
      out.tool_calls = message.tool_calls.map((call) => ({
        id: call.id,
        type: "function",
        function: { name: call.name, arguments: call.arguments },
      }));
    }
    return out;
  });
  const body = {
    model: liveModel,
    messages,
    stream: canonical.stream === true,
    max_tokens: canonical.max_tokens || maxTokens,
  };
  if (canonical.tools) body.tools = canonical.tools;
  return body;
}

export function rewriteProtocolModel(family, payload, model) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) return payload;
  const out = { ...payload };
  if (Object.prototype.hasOwnProperty.call(out, "model")) out.model = model;
  if (family === "messages" && isObject(out.message) && Object.prototype.hasOwnProperty.call(out.message, "model")) {
    out.message = { ...out.message, model };
  }
  if (family === "responses" && isObject(out.response) && Object.prototype.hasOwnProperty.call(out.response, "model")) {
    out.response = { ...out.response, model };
  }
  return out;
}

function assistantFromChatMessage(message) {
  const toolCalls = Array.isArray(message?.tool_calls)
    ? message.tool_calls.map((call) => ({
        id: call.id,
        name: call.function?.name ?? call.name,
        arguments: typeof call.function?.arguments === "string" ? call.function.arguments : JSON.stringify(call.function?.arguments ?? {}),
      }))
    : [];
  return {
    role: "assistant",
    content: message?.content ?? null,
    tool_calls: toolCalls.length ? toolCalls : undefined,
    finish_reason: message?.finish_reason,
  };
}

export function chatResponseToCanonical(payload) {
  const choice = payload?.choices?.[0];
  const message = choice?.message ?? {};
  const assistant = assistantFromChatMessage(message);
  assistant.finish_reason = choice?.finish_reason ?? assistant.finish_reason;
  return {
    id: payload?.id || "chatcmpl-lab",
    model: payload?.model,
    assistant,
    usage: payload?.usage || null,
  };
}

export function canonicalToChatJson(result, upstreamModel, { synthesizeUsage = false } = {}) {
  const message = { role: "assistant", content: result.assistant.content ?? null };
  if (result.assistant.tool_calls) {
    message.tool_calls = result.assistant.tool_calls.map((call) => ({
      id: call.id,
      type: "function",
      function: { name: call.name, arguments: call.arguments },
    }));
  }
  const finish = result.assistant.tool_calls?.length ? "tool_calls" : "stop";
  const body = {
    id: result.id || "chatcmpl-lab",
    object: "chat.completion",
    created: CREATED_AT,
    model: upstreamModel,
    choices: [{ index: 0, message, finish_reason: result.assistant.finish_reason || finish }],
  };
  if (result.usage) body.usage = result.usage;
  else if (synthesizeUsage) body.usage = chatUsage();
  return body;
}

export function canonicalToMessagesJson(result, upstreamModel, { synthesizeUsage = false } = {}) {
  const content = [];
  if (typeof result.assistant.content === "string" && result.assistant.content) {
    content.push({ type: "text", text: result.assistant.content });
  }
  if (result.assistant.tool_calls) {
    for (const call of result.assistant.tool_calls) {
      let input = {};
      try {
        input = JSON.parse(call.arguments || "{}");
      } catch {
        input = { raw: call.arguments };
      }
      content.push({ type: "tool_use", id: call.id, name: call.name, input });
    }
  }
  const body = {
    id: result.id || "msg-lab",
    type: "message",
    role: "assistant",
    model: upstreamModel,
    content,
    stop_reason: result.assistant.tool_calls?.length ? "tool_use" : "end_turn",
    stop_sequence: null,
  };
  const mapped = mapUsageFields(result.usage);
  if (mapped) body.usage = mapped;
  else if (synthesizeUsage) body.usage = { input_tokens: 1, output_tokens: 1 };
  return body;
}

export function canonicalToResponsesJson(result, slot, { synthesizeUsage = false } = {}) {
  const output = [];
  if (typeof result.assistant.content === "string" && result.assistant.content) {
    output.push({
      type: "message",
      id: "msg_0",
      status: "completed",
      role: "assistant",
      content: [{ type: "output_text", text: result.assistant.content, annotations: [], logprobs: [] }],
    });
  }
  if (result.assistant.tool_calls) {
    for (const call of result.assistant.tool_calls) {
      output.push({
        type: "function_call",
        id: call.id,
        call_id: call.id,
        name: call.name,
        arguments: call.arguments,
        status: "completed",
      });
    }
  }
  const usage = mapUsageFields(result.usage) || (synthesizeUsage ? responsesUsage() : null);
  const incomplete = result.assistant?.finish_reason === "length";
  return responsesObject(slot, {
    status: incomplete ? "incomplete" : "completed",
    output,
    usage,
    completedAt: incomplete ? null : CREATED_AT,
    model: slot.model,
    incompleteDetails: incomplete ? { reason: "max_output_tokens" } : null,
  });
}

export function canonicalToClientJson(protocol, result, slot, opts = {}) {
  const family = familyOfProtocol(protocol);
  if (family === "messages") return canonicalToMessagesJson(result, slot.model, opts);
  if (family === "responses") return canonicalToResponsesJson(result, slot, opts);
  return canonicalToChatJson(result, slot.model, opts);
}

export function parseSseBlock(raw) {
  let event = null;
  const dataLines = [];
  for (const line of raw.split(/\r?\n/)) {
    if (line.startsWith("event:")) event = line.slice(6).trim();
    else if (line.startsWith("data:")) dataLines.push(line.slice(5).trimStart());
  }
  if (!dataLines.length && event == null) return null;
  return { event, data: dataLines.join("\n") };
}

export function createSseParser(onEvent) {
  let buf = "";
  return {
    push(chunk) {
      buf += chunk;
      buf = buf.replaceAll("\r\n", "\n");
      let idx;
      while ((idx = buf.indexOf("\n\n")) >= 0) {
        const raw = buf.slice(0, idx);
        buf = buf.slice(idx + 2);
        const parsed = parseSseBlock(raw);
        if (parsed) onEvent(parsed);
      }
    },
    flush() {
      if (buf.trim()) {
        const parsed = parseSseBlock(buf);
        if (parsed) onEvent(parsed);
        buf = "";
      }
    },
  };
}

function emitChatChunk(model, delta, finishReason, usage) {
  const body = {
    id: "chatcmpl-lab",
    object: "chat.completion.chunk",
    created: CREATED_AT,
    model,
    choices: [{ index: 0, delta, finish_reason: finishReason }],
  };
  if (usage) body.usage = usage;
  return sseEvent(null, body);
}

export function createLiveStreamTranslator({ protocol, slot, onWrite, onEnd }) {
  const family = familyOfProtocol(protocol);
  let started = false;
  let textStarted = false;
  let seq = 0;
  let acc = "";
  const toolAcc = new Map();
  let finished = false;
  let pendingReason = null;
  let pendingUsage = null;

  function emit(event, data) {
    onWrite(event ? sseEvent(event, data) : sseEvent(null, data));
  }

  function ensureStart() {
    if (started) return;
    started = true;
    if (family === "messages") {
      const startUsage = mapUsageFields(pendingUsage);
      const start = {
        type: "message_start",
        message: {
          id: "msg-lab",
          type: "message",
          role: "assistant",
          model: slot.model,
          content: [],
          stop_reason: null,
          stop_sequence: null,
        },
      };
      if (startUsage && startUsage.input_tokens != null) {
        start.message.usage = { input_tokens: startUsage.input_tokens, output_tokens: 0 };
      }
      emit("message_start", start);
    } else if (family === "responses") {
      const created = responsesObject(slot, { status: "in_progress", output: [], usage: null, completedAt: null });
      emit("response.created", { type: "response.created", sequence_number: seq++, response: created });
    }
  }

  function ensureText() {
    ensureStart();
    if (textStarted) return;
    textStarted = true;
    if (family === "messages") {
      emit("content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } });
    } else if (family === "responses") {
      emit("response.output_item.added", {
        type: "response.output_item.added",
        sequence_number: seq++,
        output_index: 0,
        item: { type: "message", id: "msg_0", status: "in_progress", role: "assistant", content: [] },
      });
      emit("response.content_part.added", {
        type: "response.content_part.added",
        sequence_number: seq++,
        item_id: "msg_0",
        output_index: 0,
        content_index: 0,
        part: { type: "output_text", text: "", annotations: [], logprobs: [] },
      });
    }
  }

  function onChatPayload(payload) {
    if (payload === "[DONE]") {
      finish();
      return;
    }
    let parsed = payload;
    if (typeof payload === "string") {
      try {
        parsed = JSON.parse(payload);
      } catch {
        return;
      }
    }
    const choice = parsed?.choices?.[0] || {};
    const delta = choice.delta || {};
    if (family === "chat") {
      onWrite(emitChatChunk(slot.model, delta, choice.finish_reason ?? null, parsed.usage));
      return;
    }
    if (typeof delta.content === "string" && delta.content) {
      ensureText();
      acc += delta.content;
      if (family === "messages") {
        emit("content_block_delta", {
          type: "content_block_delta",
          index: 0,
          delta: { type: "text_delta", text: delta.content },
        });
      } else {
        emit("response.output_text.delta", {
          type: "response.output_text.delta",
          sequence_number: seq++,
          item_id: "msg_0",
          output_index: 0,
          content_index: 0,
          delta: delta.content,
          logprobs: [],
        });
      }
    }
    if (Array.isArray(delta.tool_calls)) {
      ensureStart();
      for (const call of delta.tool_calls) {
        const index = call.index ?? 0;
        const current = toolAcc.get(index) || { id: call.id, name: call.function?.name, arguments: "" };
        if (call.id) current.id = call.id;
        if (call.function?.name) current.name = call.function.name;
        if (typeof call.function?.arguments === "string") current.arguments += call.function.arguments;
        toolAcc.set(index, current);
      }
    }
    if (choice.finish_reason) {
      pendingReason = choice.finish_reason;
      if (parsed.usage) pendingUsage = parsed.usage;
    }
    if (parsed.usage) pendingUsage = parsed.usage;
  }

  function mapStopReason(reason) {
    if (reason === "tool_calls") return "tool_use";
    if (reason === "length") return "max_tokens";
    if (reason === "stop" || !reason) return toolAcc.size ? "tool_use" : "end_turn";
    return reason;
  }

  function finish(usage, reason) {
    if (finished) return;
    finished = true;
    const actualUsage = usage || pendingUsage;
    const actualReason = reason || pendingReason;
    if (family === "chat") {
      onWrite(sseEvent(null, "[DONE]"));
      onEnd();
      return;
    }
    if (family === "messages") {
      if (textStarted) emit("content_block_stop", { type: "content_block_stop", index: 0 });
      let blockIndex = textStarted ? 1 : 0;
      for (const call of toolAcc.values()) {
        let input = {};
        try {
          input = JSON.parse(call.arguments || "{}");
        } catch {
          input = { raw: call.arguments };
        }
        emit("content_block_start", {
          type: "content_block_start",
          index: blockIndex,
          content_block: { type: "tool_use", id: call.id, name: call.name, input },
        });
        emit("content_block_stop", { type: "content_block_stop", index: blockIndex });
        blockIndex += 1;
      }
      const delta = {
        type: "message_delta",
        delta: { stop_reason: mapStopReason(actualReason), stop_sequence: null },
      };
      const mapped = mapUsageFields(actualUsage);
      if (mapped) {
        const usage = {};
        if (mapped.input_tokens != null) usage.input_tokens = mapped.input_tokens;
        if (mapped.output_tokens != null) usage.output_tokens = mapped.output_tokens;
        if (Object.keys(usage).length) delta.usage = usage;
      }
      emit("message_delta", delta);
      emit("message_stop", { type: "message_stop" });
      onEnd();
      return;
    }
    if (textStarted) {
      emit("response.output_text.done", {
        type: "response.output_text.done",
        sequence_number: seq++,
        item_id: "msg_0",
        output_index: 0,
        content_index: 0,
        text: acc,
        logprobs: [],
      });
      emit("response.content_part.done", {
        type: "response.content_part.done",
        sequence_number: seq++,
        item_id: "msg_0",
        output_index: 0,
        content_index: 0,
        part: { type: "output_text", text: acc, annotations: [], logprobs: [] },
      });
      emit("response.output_item.done", {
        type: "response.output_item.done",
        sequence_number: seq++,
        output_index: 0,
        item: {
          type: "message",
          id: "msg_0",
          status: "completed",
          role: "assistant",
          content: [{ type: "output_text", text: acc, annotations: [], logprobs: [] }],
        },
      });
    }
    const output = [];
    if (acc) {
      output.push({
        type: "message",
        id: "msg_0",
        status: "completed",
        role: "assistant",
        content: [{ type: "output_text", text: acc, annotations: [], logprobs: [] }],
      });
    }
    for (const call of toolAcc.values()) {
      output.push({
        type: "function_call",
        id: call.id,
        call_id: call.id,
        name: call.name,
        arguments: call.arguments,
        status: "completed",
      });
    }
    const incomplete = actualReason === "length";
    const completed = responsesObject(slot, {
      status: incomplete ? "incomplete" : "completed",
      output,
      usage: mapUsageFields(actualUsage),
      completedAt: incomplete ? null : CREATED_AT,
      incompleteDetails: incomplete ? { reason: "max_output_tokens" } : null,
    });
    const eventName = incomplete ? "response.incomplete" : "response.completed";
    emit(eventName, { type: eventName, sequence_number: seq++, response: completed });
    onEnd();
    void reason;
  }

  return {
    onChatPayload,
    finish: () => finish(),
    get finished() {
      return finished;
    },
  };
}

export function liveErrorPayload(protocol, status, message) {
  const type = status === 401 ? "authentication_error" : "api_error";
  if (familyOfProtocol(protocol) === "messages") return { type: "error", error: { type, message } };
  return { error: { message, type } };
}
