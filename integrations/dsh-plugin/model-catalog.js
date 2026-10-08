// Pure /v1/models -> pi-ai translation. Do not guess capabilities from names,
// and do not turn a missing protocol into an executable Chat model.
export const THINKING_LEVELS = Object.freeze(["off", "minimal", "low", "medium", "high", "xhigh", "max"]);
export const REJECTED_MODEL_API = "ocg-rejected";
export const PROTOCOL_REPLAY_CODE = "OCG_PROTOCOL_REPLAY_REFUSED";
export const REPLAY_METADATA_CODE = "OCG_REPLAY_METADATA_REJECTED";
export const MESSAGES_REASONING_CODE = "OCG_MESSAGES_REASONING_UNDECLARED";
const FALLBACK_CONTEXT = 128_000;
const FALLBACK_OUTPUT = 16_384;
const PROTOCOL_API = Object.freeze({
  chat_completions: "openai-completions",
  responses: "openai-responses",
  messages: "anthropic-messages",
});
const object = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const positive = (value) => Number.isSafeInteger(value) && value > 0;

function tokenLimit(source, keys) {
  for (const key of keys) {
    if (source[key] === undefined || source[key] === null) continue;
    if (!positive(source[key])) throw new Error(`Invalid model token limit: ${key}`);
    return source[key];
  }
  return undefined;
}

function modalities(value, label) {
  if (value === undefined || value === null) return undefined;
  if (!Array.isArray(value) || value.length === 0 || new Set(value).size !== value.length
    || value.some((v) => !["text", "image", "audio", "video"].includes(v))) {
    throw new Error(`Invalid model ${label} modalities`);
  }
  return [...value];
}

function reasoning(source) {
  const declared = object(source.reasoning) ? source.reasoning.supported : source.reasoning;
  if (declared !== undefined && declared !== null && typeof declared !== "boolean") {
    throw new Error("Invalid model reasoning support");
  }
  const raw = source.reasoningEfforts ?? (object(source.reasoning) ? source.reasoning.efforts : undefined);
  const map = Object.fromEntries(THINKING_LEVELS.map((level) => [level, null]));
  let hasEfforts = false;
  if (raw !== undefined && raw !== null) {
    hasEfforts = true;
    const pairs = Array.isArray(raw) ? raw.map((level) => [level, level])
      : object(raw) ? Object.entries(raw) : undefined;
    if (!pairs || pairs.length > THINKING_LEVELS.length || new Set(pairs.map(([k]) => k)).size !== pairs.length) {
      throw new Error("Invalid model reasoning efforts");
    }
    for (const [level, wire] of pairs) {
      if (!THINKING_LEVELS.includes(level) || typeof wire !== "string" || !/^[A-Za-z0-9_-]{1,32}$/.test(wire)) {
        throw new Error("Unsupported model reasoning effort or wire spelling");
      }
      map[level] = wire;
    }
    if (declared !== true && pairs.length > 0) throw new Error("Reasoning efforts require explicit reasoning support");
  }
  const expressible = Object.values(map).some((value) => value !== null);
  return {
    // Capability is the explicit boolean. An effort menu is a separate fact.
    enabled: declared === true,
    map,
    declared,
    hasEfforts,
    expressible,
  };
}

export function messagesBaseUrl(v1Base) {
  if (typeof v1Base !== "string" || v1Base.length === 0) {
    throw new Error("Messages base URL is missing a trailing /v1");
  }
  const withoutTrailingSlash = v1Base.endsWith("/") ? v1Base.slice(0, -1) : v1Base;
  if (!withoutTrailingSlash.endsWith("/v1")) {
    throw new Error("Messages base URL is missing a trailing /v1");
  }
  const root = withoutTrailingSlash.slice(0, -3);
  if (root.length === 0 || root.endsWith("/")) {
    throw new Error("Messages base URL is missing a trailing /v1");
  }
  return root;
}

function protocolsOf(source) {
  const protocols = source.protocols;
  if (!object(protocols)) throw new Error("Missing OCG model protocol");
  const supported = protocols.supported;
  if (!Array.isArray(supported) || supported.length === 0) throw new Error("OCG model protocols.supported is empty");
  if (new Set(supported).size !== supported.length) throw new Error("OCG model protocols.supported contains duplicates");
  for (const name of supported) {
    if (!Object.hasOwn(PROTOCOL_API, name)) throw new Error("Unsupported OCG model protocol");
  }
  if (!Object.hasOwn(PROTOCOL_API, protocols.preferred) || !supported.includes(protocols.preferred)) {
    throw new Error("OCG model preferred protocol is not supported");
  }
  return {
    preferred: protocols.preferred,
    supported: [...supported],
    api: PROTOCOL_API[protocols.preferred],
  };
}

function baseUrlFor(api, baseUrl) {
  return api === "anthropic-messages" ? messagesBaseUrl(baseUrl) : baseUrl;
}

function unexecutableModel(id, providerId, baseUrl) {
  return {
    id,
    provider: providerId,
    api: REJECTED_MODEL_API,
    baseUrl,
    name: id,
    reasoning: false,
    input: ["text"],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: 1,
    maxTokens: 1,
  };
}

function modelFromRow(row, id, providerId, baseUrl) {
  if (!object(row.ocg) || row.ocg.schemaVersion !== 2) {
    throw new Error("Unsupported OCG model metadata schema version");
  }
  const source = row.ocg;
  const protocol = protocolsOf(source);
  const contextWindow = tokenLimit(source, ["contextWindow", "context_length", "context_window"])
    ?? tokenLimit(row, ["contextWindow", "context_length", "context_window"]);
  const maxOutputTokens = tokenLimit(source, ["maxOutputTokens", "maxTokens", "max_output_tokens"])
    ?? tokenLimit(row, ["maxTokens", "max_output_tokens"]);
  if (contextWindow !== undefined && maxOutputTokens !== undefined && maxOutputTokens > contextWindow) {
    throw new Error("Model output limit exceeds its context window");
  }
  const inputModalities = modalities(source.inputModalities ?? source.input, "input");
  const outputModalities = modalities(source.outputModalities, "output");
  const input = inputModalities?.filter((v) => v === "text" || v === "image") ?? ["text"];
  if (input.length === 0 || (outputModalities !== undefined && !outputModalities.includes("text"))) {
    throw new Error("Model modalities are not supported by the DSH text adapter");
  }
  const thinking = reasoning(source);
  // reasoningEfforts remains the Chat selector->wire map. Messages does not
  // express those wires as a budget or adaptive effort, so its native menu is empty.
  const nativeMap = protocol.api === "anthropic-messages"
    ? Object.fromEntries(THINKING_LEVELS.map((level) => [level, null]))
    : thinking.map;
  const name = typeof source.name === "string" ? source.name : typeof row.name === "string" ? row.name : id;
  if (!name.trim() || name.length > 200 || /[\u0000-\u001f\u007f]/.test(name)) throw new Error("Invalid model display name");
  const metadata = {
    schemaVersion: 2,
    ...(typeof source.status === "string" && source.status.length > 0 && source.status.length <= 64
      && !/[\u0000-\u001f\u007f]/.test(source.status) ? { status: source.status } : {}),
    protocols: { preferred: protocol.preferred, supported: protocol.supported },
    ...(contextWindow === undefined ? {} : { contextWindow }),
    ...(maxOutputTokens === undefined ? {} : { maxOutputTokens }),
    ...(inputModalities === undefined ? {} : { inputModalities }),
    ...(outputModalities === undefined ? {} : { outputModalities }),
    ...(thinking.declared === undefined || thinking.declared === null ? {} : { reasoning: thinking.declared }),
    ...(thinking.hasEfforts ? { reasoningEfforts: Object.fromEntries(Object.entries(thinking.map).filter(([, v]) => v !== null)) } : {}),
    ...(Array.isArray(source.sources) ? { sources: source.sources.filter((value) => ["operator", "upstream", "modelsdev", "unknown"].includes(value)) } : {}),
    ...Object.fromEntries(["toolCalling", "parallelToolCalls"].filter((k) => typeof source[k] === "boolean").map((k) => [k, source[k]])),
    fallbacks: [contextWindow === undefined ? "contextWindow" : null, maxOutputTokens === undefined ? "maxOutputTokens" : null,
      inputModalities === undefined ? "inputModalities" : null].filter(Boolean),
  };
  const model = {
    id,
    provider: providerId,
    api: protocol.api,
    baseUrl: baseUrlFor(protocol.api, baseUrl),
    name,
    reasoning: thinking.enabled,
    thinkingLevelMap: nativeMap,
    input,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: contextWindow ?? Math.max(FALLBACK_CONTEXT, maxOutputTokens ?? 0),
    maxTokens: maxOutputTokens ?? Math.min(FALLBACK_OUTPUT, contextWindow ?? FALLBACK_CONTEXT),
  };
  // Chat wire flags stay on Chat. Responses already stores false and sends the
  // full input. Messages has no thinkingFormat, store, or reasoning_effort field.
  if (protocol.api === "openai-completions") {
    model.compat = {
      thinkingFormat: "openai",
      supportsReasoningEffort: thinking.expressible,
      supportsStore: false,
      supportsDeveloperRole: false,
    };
  }
  return { model, metadata };
}

export function parseModelCatalog(value, { providerId, baseUrl }) {
  if (!object(value) || !Array.isArray(value.data)) throw new Error("Invalid OCG /v1/models payload");
  const models = [];
  const modelErrors = new Map();
  const metadata = new Map();
  for (const row of value.data) {
    const id = typeof row?.id === "string" ? row.id.trim() : "";
    if (!id || id.length > 200 || /[\u0000-\u001f\u007f]/.test(id)) continue;
    if (metadata.has(id)) { modelErrors.set(id, "Duplicate OCG model ID"); continue; }
    try {
      const entry = modelFromRow(row, id, providerId, baseUrl);
      models.push(entry.model);
      metadata.set(id, entry.metadata);
    } catch (error) {
      // One invalid row must not hide unrelated usable models. The placeholder
      // is not a dispatchable API, and profile.modelErrors rejects prepare.
      models.push(unexecutableModel(id, providerId, baseUrl));
      metadata.set(id, { schemaVersion: row?.ocg?.schemaVersion, status: "invalid", fallbacks: ["contextWindow", "maxOutputTokens", "inputModalities"] });
      modelErrors.set(id, error.message);
    }
  }
  return { models, modelErrors, metadata };
}

function explicitEffortMenu(reasoning) {
  return object(reasoning) && Array.isArray(reasoning.efforts) && reasoning.efforts.length > 0;
}

export function describeOcgModel(info, metadata) {
  // DSH's public reasoning object is the effort menu. Empty efforts are invalid
  // metadata there. Omit that object. The pi-ai boolean and ocg.reasoning stay
  // the capability, and a Messages categorical map is not copied into the menu.
  if (metadata === undefined) {
    if (!object(info.reasoning) || explicitEffortMenu(info.reasoning)) return info;
    const result = { ...info };
    delete result.reasoning;
    return result;
  }
  const result = { ...info, ocg: structuredClone(metadata) };
  // DSH permits an absent context, but requires a positive contextWindow when
  // the object is present. Keep pi-ai's numeric fallback internal and report
  // unknown capacity by omitting the entire public context descriptor.
  if (metadata.contextWindow === undefined) {
    delete result.context;
  }
  if (!explicitEffortMenu(result.reasoning)) delete result.reasoning;
  return result;
}

const REPLAY_STOP_REASONS = new Set(["stop", "length", "toolUse", "error", "aborted"]);
const REPLAY_BLOCK_TYPES = new Set(["text", "reasoning", "tool-call"]);

function codedError(code, message) {
  const error = new Error(message);
  error.code = code;
  return error;
}

function signed(value) {
  return typeof value === "string" && value.trim().length > 0;
}

function sameTuple(provider, api, modelId, model) {
  return provider === model?.provider && api === model?.api && modelId === model?.id;
}

// Opaque native replay is a non-empty thinking signature, redacted thinking, or
// a non-empty tool thought signature. Unsigned text and ordinary tools are not.
function piBlockOpaque(block) {
  if (!object(block)) return false;
  if (block.type === "thinking") return block.redacted === true || signed(block.thinkingSignature);
  if (block.type === "toolCall") return signed(block.thoughtSignature);
  return false;
}

function replayBlockOpaque(block) {
  if (block.type === "reasoning") return block.redacted === true || signed(block.thinkingSignature);
  if (block.type === "tool-call") return signed(block.thoughtSignature);
  return false;
}

function replayTupleError(identity, model) {
  throw codedError(
    PROTOCOL_REPLAY_CODE,
    `Open Console Gateway cannot replay assistant history from ${identity.provider}/${identity.api}/${identity.model} on ${model.provider}/${model.api}/${model.id}`,
  );
}

// pi-ai keeps native opaque blocks only when provider, api, and model id all match.
// Any other tuple drops redacted thinking and thought signatures and turns signed
// thinking into plain text. Refuse that send. Unsigned text and tools stay portable.
export function refuseCrossProtocolReplay(model, context) {
  for (const message of context?.messages ?? []) {
    if (message?.role !== "assistant") continue;
    if (sameTuple(message.provider, message.api, message.model, model)) continue;
    if (!Array.isArray(message.content) || !message.content.some(piBlockOpaque)) continue;
    replayTupleError({ provider: message.provider, api: message.api, model: message.model }, model);
  }
}

function rejectReplayMetadata(detail) {
  throw codedError(REPLAY_METADATA_CODE, `Open Console Gateway rejected native replay metadata: ${detail}`);
}

// Finite check of the pi-ai replay envelope this installed adapter understands
// (kind "pi-ai", version 2). The adapter is not exported, and unusable metadata
// is otherwise converted to unsigned text before an API wrapper can see it.
function readReplayEnvelope(value) {
  if (!object(value)) rejectReplayMetadata("expected a replay envelope");
  const response = value.response;
  if (!object(response)) rejectReplayMetadata("expected a response object");
  if (response.kind !== "pi-ai") rejectReplayMetadata("unknown state kind");
  if (response.version !== 2) rejectReplayMetadata(`unsupported version ${String(response.version)}`);
  for (const key of ["api", "provider", "model"]) {
    if (typeof response[key] !== "string" || response[key].length === 0) rejectReplayMetadata(`${key} must be a non-empty string`);
  }
  if (!REPLAY_STOP_REASONS.has(response.stopReason)) rejectReplayMetadata("unknown stopReason");
  for (const key of ["responseModel", "responseId", "providerThinkingLevel"]) {
    if (response[key] !== undefined && typeof response[key] !== "string") rejectReplayMetadata(`${key} must be a string`);
  }
  if (!Array.isArray(value.blocks)) rejectReplayMetadata("blocks must be an array");
  value.blocks.forEach((block, index) => {
    if (!object(block)) rejectReplayMetadata(`block ${index} must be an object`);
    if (!REPLAY_BLOCK_TYPES.has(block.type)) rejectReplayMetadata(`block ${index} has an unknown type`);
    for (const signature of ["textSignature", "thinkingSignature", "thoughtSignature"]) {
      if (block[signature] !== undefined && typeof block[signature] !== "string") {
        rejectReplayMetadata(`block ${index} ${signature} must be a string`);
      }
    }
    if (block.redacted !== undefined && typeof block.redacted !== "boolean") {
      rejectReplayMetadata(`block ${index} redacted must be boolean`);
    }
  });
  return { response, blocks: value.blocks };
}

// Harness messages, before PiAiAdapter turns a bad envelope into plaintext.
// Missing replay metadata stays portable. Present metadata must be version 2,
// aligned with the assistant content, and — when it carries opaque native data —
// the same provider, api, and model as the request.
export function refuseDegradingReplay(model, messages) {
  for (const message of messages ?? []) {
    if (message?.role !== "assistant") continue;
    const source = message.source;
    if (source?.replayState === undefined) continue;
    const state = readReplayEnvelope(source.replayState);
    if (state.response.provider !== source.provider) rejectReplayMetadata("provider does not match assistant source");
    if (state.response.model !== source.model) rejectReplayMetadata("model does not match assistant source");
    const content = message.content;
    if (!Array.isArray(content) || content.length !== state.blocks.length) {
      rejectReplayMetadata("block count does not match assistant content");
    }
    for (let index = 0; index < content.length; index += 1) {
      const block = content[index];
      if (!object(block) || state.blocks[index].type !== block.type) {
        rejectReplayMetadata(`block ${index} does not match assistant content`);
      }
    }
    if (state.blocks.some(replayBlockOpaque)
      && !sameTuple(state.response.provider, state.response.api, state.response.model, model)) {
      replayTupleError(state.response, model);
    }
  }
}

export function refuseUndeclaredMessagesReasoning(model, options) {
  if (model?.api !== "anthropic-messages" || options?.reasoning === undefined) return;
  const error = new Error("Open Console Gateway cannot encode a Messages reasoning level. The catalog does not declare an Anthropic thinking budget or adaptive effort.");
  error.code = MESSAGES_REASONING_CODE;
  throw error;
}

// pi-ai 0.87.1 stores SSE reasoning_details with their stream-only index and
// replays them unchanged. Some Chat endpoints reject OpenRouter's typed text
// details too. Plain unbound text can use reasoning_content; opaque or
// provider-specific details must retain their original replay representation.
function portableReasoningText(detail) {
  return object(detail) && detail.type === "reasoning.text" && typeof detail.text === "string"
    && (detail.format === undefined || detail.format === "unknown")
    && Object.keys(detail).every((key) => ["type", "text", "format", "index"].includes(key));
}

// Normalize outgoing payloads, including old saved history, without editing
// the transcript or opaque provider replay fields.
export function normalizeChatReasoningReplay(payload) {
  if (!object(payload) || !Array.isArray(payload.messages)) return payload;
  let changed = false;
  const messages = payload.messages.map((message) => {
    if (message?.role !== "assistant" || !Array.isArray(message.reasoning_details)) return message;
    const savedDetails = message.reasoning_details;
    if (savedDetails.length > 0 && savedDetails.every(portableReasoningText)) {
      const text = savedDetails.map((detail) => detail.text).join("");
      if (message.reasoning_content === undefined || message.reasoning_content === ""
        || message.reasoning_content === text) {
        const replay = { ...message, reasoning_content: text };
        delete replay.reasoning_details;
        changed = true;
        return replay;
      }
    }
    let messageChanged = false;
    const details = message.reasoning_details.map((detail) => {
      if (!object(detail) || !Object.hasOwn(detail, "index")) return detail;
      const replay = { ...detail };
      delete replay.index;
      messageChanged = true;
      return replay;
    });
    if (!messageChanged) return message;
    changed = true;
    return { ...message, reasoning_details: details };
  });
  return changed ? { ...payload, messages } : payload;
}

export function chatStreamOptions(_model, _context, options) {
  return {
    ...options,
    async onPayload(payload, model) {
      const prepared = await options?.onPayload?.(payload, model);
      return normalizeChatReasoningReplay(prepared === undefined ? payload : prepared);
    },
  };
}
