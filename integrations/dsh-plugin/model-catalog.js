// Pure /v1/models -> pi-ai translation. Do not guess capabilities from names.
export const THINKING_LEVELS = Object.freeze(["off", "minimal", "low", "medium", "high", "xhigh", "max"]);
const FALLBACK_CONTEXT = 128_000;
const FALLBACK_OUTPUT = 16_384;
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
  if (raw !== undefined && raw !== null) {
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
    if (declared === false && pairs.length > 0) throw new Error("Non-reasoning model declares reasoning efforts");
  }
  // A bare `true` cannot justify offering arbitrary low/medium/high choices.
  return { enabled: Object.values(map).some((v) => v !== null), map, declared, hasEfforts: raw !== undefined && raw !== null };
}

function modelFromRow(row, id, providerId, baseUrl) {
  if (row.ocg !== undefined && (!object(row.ocg) || row.ocg.schemaVersion !== 1)) {
    throw new Error("Unsupported OCG model metadata schema version");
  }
  const source = row.ocg ?? row;
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
  const name = typeof source.name === "string" ? source.name : typeof row.name === "string" ? row.name : id;
  if (!name.trim() || name.length > 200 || /[\u0000-\u001f\u007f]/.test(name)) throw new Error("Invalid model display name");
  const metadata = {
    schemaVersion: 1,
    status: row.ocg?.status ?? "legacy",
    ...(contextWindow === undefined ? {} : { contextWindow }),
    ...(maxOutputTokens === undefined ? {} : { maxOutputTokens }),
    ...(inputModalities === undefined ? {} : { inputModalities }),
    ...(outputModalities === undefined ? {} : { outputModalities }),
    ...(thinking.declared === undefined || thinking.declared === null ? {} : { reasoning: thinking.declared }),
    ...(thinking.hasEfforts ? { reasoningEfforts: Object.fromEntries(Object.entries(thinking.map).filter(([, v]) => v !== null)) } : {}),
    ...(Array.isArray(source.sources) ? { sources: source.sources.filter((value) => ["operator", "upstream", "unknown"].includes(value)) } : {}),
    // Only whitelisted boolean facts cross into the runtime metadata object.
    ...Object.fromEntries(["toolCalling", "parallelToolCalls"].filter((k) => typeof source[k] === "boolean").map((k) => [k, source[k]])),
    fallbacks: [contextWindow === undefined ? "contextWindow" : null, maxOutputTokens === undefined ? "maxOutputTokens" : null,
      inputModalities === undefined ? "inputModalities" : null].filter(Boolean),
  };
  return {
    model: {
      id, provider: providerId, api: "openai-completions", baseUrl, name,
      reasoning: thinking.enabled, thinkingLevelMap: thinking.map,
      input, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      contextWindow: contextWindow ?? Math.max(FALLBACK_CONTEXT, maxOutputTokens ?? 0),
      maxTokens: maxOutputTokens ?? Math.min(FALLBACK_OUTPUT, contextWindow ?? FALLBACK_CONTEXT),
      // OCG owns dialect conversion. Never infer a vendor dialect from the ID.
      compat: { thinkingFormat: "openai", supportsReasoningEffort: thinking.enabled, supportsStore: false },
    },
    metadata,
  };
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
      // One invalid row must not hide unrelated usable models. Its own calls
      // are rejected by PiAiAdapter through profile.modelErrors.
      const fallback = modelFromRow({ id }, id, providerId, baseUrl);
      models.push(fallback.model);
      metadata.set(id, { ...fallback.metadata, status: "invalid" });
      modelErrors.set(id, error.message);
    }
  }
  return { models, modelErrors, metadata };
}

export function describeOcgModel(info, metadata) {
  if (metadata === undefined) return info;
  const result = { ...info, ocg: structuredClone(metadata) };
  // pi-ai requires numeric internals, but do not present its legacy fallback
  // as an upstream-declared context limit in DSH's public model descriptor.
  if (result.context !== undefined && metadata.contextWindow === undefined) {
    result.context = { ...result.context };
    delete result.context.contextWindow;
  }
  return result;
}
