import {
  LIVE_CONCURRENCY,
  LIVE_KEY_ENV,
  LIVE_MAX_CALLS,
  LIVE_MAX_TOKENS,
  LIVE_MODEL,
  LIVE_TIMEOUT_MS,
  LIVE_URL_ENV,
  createMutex,
  isObject,
  rememberSecret,
  sha256,
} from "./common.mjs";

export class LiveConfigError extends Error {
  constructor(message, code = "live_config_missing") {
    super(message);
    this.name = "LiveConfigError";
    this.code = code;
  }
}

export class LiveProtocolError extends Error {
  constructor(message, code = "live_invalid_response") {
    super(message);
    this.name = "LiveProtocolError";
    this.code = code;
  }
}

export function readLiveEnv(env = process.env) {
  const url = String(env[LIVE_URL_ENV] || "").trim();
  const key = String(env[LIVE_KEY_ENV] || "").trim();
  return { url, key, present: Boolean(url && key), urlPresent: Boolean(url), keyPresent: Boolean(key) };
}

export function assertLiveConfig(env = process.env) {
  const cfg = readLiveEnv(env);
  if (!cfg.present) {
    const missing = [!cfg.urlPresent ? LIVE_URL_ENV : null, !cfg.keyPresent ? LIVE_KEY_ENV : null].filter(Boolean);
    throw new LiveConfigError(
      `live suite requires ${missing.join(" and ")} (OpenAI Chat Completions compatible). Do not pass keys on the CLI.`,
    );
  }
  return cfg;
}

export function assertChatCompletionJson(payload) {
  if (!isObject(payload)) {
    throw new LiveProtocolError("live JSON is not an object", "live_invalid_json");
  }
  if (payload.error) {
    const message = payload.error.message || payload.error.type || "live error object";
    throw new LiveProtocolError(message, "live_remote_error");
  }
  if (!Array.isArray(payload.choices) || payload.choices.length === 0) {
    throw new LiveProtocolError("live JSON missing choices", "live_missing_choices");
  }
  const choice = payload.choices[0];
  if (!isObject(choice) || !isObject(choice.message)) {
    throw new LiveProtocolError("live JSON missing choices[0].message", "live_missing_message");
  }
  return payload;
}

export function classifyChatSseData(data) {
  if (data === "[DONE]") return { kind: "done" };
  let parsed;
  try {
    parsed = JSON.parse(data);
  } catch {
    return { kind: "malformed", raw: data };
  }
  if (!isObject(parsed)) return { kind: "invalid", payload: parsed };
  if (parsed.error) return { kind: "error", error: parsed.error, payload: parsed };
  if (!Array.isArray(parsed.choices)) return { kind: "invalid", payload: parsed };
  return { kind: "chunk", payload: parsed };
}

async function defaultConsume(response, { signal } = {}) {
  if (!response.body || typeof response.body.getReader !== "function") {
    const buf = Buffer.from(await response.arrayBuffer());
    return new Response(buf, { status: response.status, statusText: response.statusText, headers: response.headers });
  }
  const reader = response.body.getReader();
  const chunks = [];
  const onAbort = () => {
    reader.cancel().catch(() => {});
  };
  if (signal) {
    if (signal.aborted) {
      await reader.cancel().catch(() => {});
      const error = new Error("live remote aborted");
      error.code = "live_aborted";
      throw error;
    }
    signal.addEventListener("abort", onAbort, { once: true });
  }
  try {
    while (true) {
      if (signal?.aborted) {
        const error = new Error("live remote aborted");
        error.code = "live_aborted";
        throw error;
      }
      let chunk;
      try {
        chunk = await reader.read();
      } catch (error) {
        if (signal?.aborted) {
          const aborted = new Error("live remote aborted");
          aborted.code = "live_aborted";
          aborted.cause = error;
          throw aborted;
        }
        throw error;
      }
      if (chunk.done) {
        if (signal?.aborted) {
          const error = new Error("live remote aborted");
          error.code = "live_aborted";
          throw error;
        }
        break;
      }
      chunks.push(chunk.value);
    }
  } finally {
    if (signal) signal.removeEventListener("abort", onAbort);
    if (signal?.aborted) {
      await reader.cancel().catch(() => {});
    }
  }
  const buf = Buffer.concat(chunks.map((chunk) => Buffer.from(chunk)));
  return new Response(buf, { status: response.status, statusText: response.statusText, headers: response.headers });
}

export function createLiveClient({
  url,
  key,
  model = LIVE_MODEL,
  maxTokens = LIVE_MAX_TOKENS,
  timeoutMs = LIVE_TIMEOUT_MS,
  maxCalls = LIVE_MAX_CALLS,
  concurrency = LIVE_CONCURRENCY,
  fetchImpl = globalThis.fetch,
} = {}) {
  if (!url || !key) {
    throw new LiveConfigError(`live client requires ${LIVE_URL_ENV} and ${LIVE_KEY_ENV}`);
  }
  if (model && model !== LIVE_MODEL) {
    throw new LiveConfigError(`live outbound model is fixed to ${LIVE_MODEL} and cannot be overridden`, "live_model_locked");
  }
  model = LIVE_MODEL;
  rememberSecret(key);
  let calls = 0;
  let inFlight = 0;
  let aborted = 0;
  const lock = createMutex();
  const keyFingerprint = sha256(key).slice(0, 12);

  async function runSend({ body, stream, signal, consume }) {
    if (calls >= maxCalls) {
      const error = new Error(`live call budget exceeded (${maxCalls})`);
      error.code = "live_budget_exceeded";
      throw error;
    }
    calls += 1;
    inFlight += 1;
    const outbound = { ...body, model, max_tokens: body.max_tokens || maxTokens, stream: stream === true };
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), timeoutMs);
    const onAbort = () => ac.abort();
    if (signal) {
      if (signal.aborted) ac.abort();
      else signal.addEventListener("abort", onAbort, { once: true });
    }
    const consumeBody = consume || defaultConsume;
    try {
      const response = await fetchImpl(url, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          authorization: `Bearer ${key}`,
        },
        body: JSON.stringify(outbound),
        signal: ac.signal,
      });
      if (ac.signal.aborted) {
        const error = new Error("live remote aborted");
        error.code = "live_aborted";
        throw error;
      }
      return await consumeBody(response, { signal: ac.signal, abort: () => ac.abort() });
    } catch (error) {
      if (ac.signal.aborted || error?.code === "live_aborted") {
        aborted += 1;
        const abortError = new Error("live remote aborted");
        abortError.code = "live_aborted";
        abortError.cause = error;
        throw abortError;
      }
      const wrapped = new Error(`live remote failed: ${error instanceof Error ? error.message : String(error)}`);
      wrapped.code = error?.code || "live_network";
      wrapped.cause = error;
      throw wrapped;
    } finally {
      clearTimeout(timer);
      if (signal) signal.removeEventListener("abort", onAbort);
      inFlight -= 1;
    }
  }

  async function send(opts) {
    if (concurrency === 1) return lock(() => runSend(opts));
    return runSend(opts);
  }

  return {
    model,
    keyFingerprint,
    send,
    stats() {
      return { calls, inFlight, aborted, maxCalls, model, keyFingerprint };
    },
  };
}
