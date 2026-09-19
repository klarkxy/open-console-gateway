import http from "node:http";
import {
  BODY_LIMIT,
  HOST,
  MAX_LOG,
  assertLoopbackHost,
  headerValue,
  isJsonContentType,
  pathnameOf,
  readBody,
  rememberSecret,
  sha256,
  sleep,
  writeJson,
  writeSse,
  writeSseHeaders,
} from "./common.mjs";
import { DEFAULT_PROFILE, LISTENER_IDS, SLOT_DEFS, loadProfile, normalizeProfile, slotsOf } from "./profile.mjs";
import {
  errorPayload,
  inspectAuth,
  jsonSuccess,
  modelsCatalog,
  pushError,
  splitChunks,
  statusFor,
  streamBody,
  validateBody,
} from "./protocol.mjs";
import {
  canonicalToChatRequest,
  canonicalToClientJson,
  chatResponseToCanonical,
  createLiveStreamTranslator,
  createSseParser,
  liveErrorPayload,
  toCanonical,
} from "./adapters.mjs";
import { isolationKey, normalizeScript, scenarioQueue } from "./faults.mjs";
import { LiveProtocolError, assertChatCompletionJson, classifyChatSseData } from "./live.mjs";

const LAB_PATHS = new Set(["/_lab/health", "/_lab/requests", "/_lab/reset", "/_lab/script", "/_lab/journal"]);

function allowedSecrets(slot, getSecrets) {
  if (typeof getSecrets === "function") return getSecrets(slot);
  return new Set([slot.secret]);
}

function resolveSlot(slots, listenerId, path, headers, getSecrets) {
  const onListener = slots.filter((slot) => slot.listener === listenerId && slot.path === path);
  if (onListener.length === 0) return null;
  const authorization = headerValue(headers.authorization);
  const xApiKey = headerValue(headers["x-api-key"]);
  const bearer = /^Bearer[ \t]+(\S+)$/i.exec(authorization)?.[1] ?? "";
  const bySecret = onListener.find((slot) => {
    const allowed = allowedSecrets(slot, getSecrets);
    return allowed.has(bearer) || allowed.has(xApiKey);
  });
  return bySecret ?? onListener[0];
}

function resolveModelsSlot(slots, listenerId, path, headers) {
  const onListener = slots.filter((slot) => slot.listener === listenerId);
  if (onListener.length === 0) return null;
  const authorization = headerValue(headers.authorization);
  const xApiKey = headerValue(headers["x-api-key"]);
  const bearer = /^Bearer[ \t]+(\S+)$/i.exec(authorization)?.[1] ?? "";
  const secret = bearer || xApiKey;
  const bySecret = onListener.find((slot) => allowedSecrets(slot).has(secret));
  if (!bySecret) return onListener[0];
  const modelsPaths = new Set([bySecret.modelsPath, "/v1/models", "/models"]);
  if (path === "/v1/models" || path === "/models" || path === bySecret.modelsPath || modelsPaths.has(path)) {
    return bySecret;
  }
  const matchingPath = onListener.find((slot) => slot.secret === secret && (slot.modelsPath === path || path === "/v1/models" || path === "/models"));
  return matchingPath ?? bySecret;
}

function isModelsPath(slots, listenerId, path) {
  if (path === "/v1/models" || path === "/models") return true;
  return slots.some((slot) => slot.listener === listenerId && slot.modelsPath === path);
}

async function writeChunked(res, body, { size = 12, delayMs = 5, fragment = false } = {}) {
  writeSseHeaders(res);
  const payload = fragment && body.length > 7 ? `${body.slice(0, 7)}` : body;
  if (fragment) {
    res.write(payload);
    await sleep(delayMs);
    res.write(body.slice(7));
    res.end();
    return;
  }
  for (const chunk of splitChunks(body, size)) {
    res.write(chunk);
    await sleep(delayMs);
  }
  res.end();
}

export function createLab({ host = HOST, profile, live = null, runId = "in-process", maxLog = MAX_LOG } = {}) {
  assertLoopbackHost(host, "lab host");
  const resolvedProfile = profile || normalizeProfile(DEFAULT_PROFILE);
  const slots = slotsOf(resolvedProfile);
  for (const slot of slots) rememberSecret(slot.secret);
  const listenerIds = [...new Set(slots.map((slot) => slot.listener))];
  const journal = [];
  const queues = new Map(listenerIds.map((id) => [id, []]));
  const isolationQueues = new Map();
  const activeScenarios = new Map();
  const acceptedSecrets = new Map();
  const modelQueues = new Map();
  const servers = [];
  const listeners = [];
  let nextSequence = 0;
  let truncated = false;
  let controlServer = null;
  let control = { host, port: 0, url: "" };
  let closed = false;
  let liveArmed = false;
  const liveStats = { calls: 0, aborted: 0, errors: 0 };

  function armLive(value) {
    liveArmed = Boolean(value);
    return liveArmed;
  }

  function remember(row) {
    nextSequence += 1;
    const stored = { ...row, sequence: nextSequence };
    journal.push(stored);
    if (journal.length > maxLog) {
      journal.shift();
      truncated = true;
    }
    return stored;
  }

  function reset() {
    journal.length = 0;
    truncated = false;
    for (const id of listenerIds) queues.set(id, []);
    isolationQueues.clear();
    activeScenarios.clear();
    modelQueues.clear();
  }

  function acceptSecret(endpointId, secret) {
    if (!endpointId || !secret) throw new Error("acceptSecret requires endpointId and secret");
    const set = acceptedSecrets.get(endpointId) || new Set([slots.find((item) => (item.id || item.slot) === endpointId)?.secret].filter(Boolean));
    set.add(secret);
    acceptedSecrets.set(endpointId, set);
    rememberSecret(secret);
    return [...set].map((value) => sha256(value).slice(0, 12));
  }

  function revokeSecret(endpointId, secret) {
    const set = acceptedSecrets.get(endpointId);
    if (set) set.delete(secret);
  }

  function secretsFor(slot) {
    const id = slot.id || slot.slot;
    const extra = acceptedSecrets.get(id);
    if (extra && extra.size) return extra;
    return new Set([slot.secret]);
  }

  function scriptModels(endpointId, queue) {
    modelQueues.set(endpointId, (Array.isArray(queue) ? queue : [queue]).map(normalizeScript));
  }

  function acceptModel(endpointId, model) {
    if (!endpointId || !model) throw new Error("acceptModel requires endpointId and model");
    const slot = slots.find((item) => (item.id || item.slot) === endpointId);
    if (!slot) throw new Error(`unknown endpoint ${endpointId}`);
    if (!Array.isArray(slot.catalog)) slot.catalog = [slot.model];
    if (!slot.catalog.includes(model)) slot.catalog.push(model);
    return [...slot.catalog];
  }

  function fingerprintFromSpec(spec = {}) {
    const raw = spec.keyFingerprint || spec.key || "";
    if (!raw) return "";
    if (/^[a-f0-9]{64}$/i.test(raw)) return raw;
    return sha256(raw);
  }

  function isoSpecFrom(spec = {}, scenarioName = "") {
    return {
      endpointId: spec.endpointId || spec.endpoint || "",
      keyFingerprint: fingerprintFromSpec(spec),
      model: spec.model || "",
      scenario: scenarioName || spec.scenario || "",
    };
  }

  function script(listenerId, queue) {
    if (!listenerIds.includes(listenerId)) throw new Error(`unknown listener ${listenerId}`);
    queues.set(listenerId, (Array.isArray(queue) ? queue : [queue]).map(normalizeScript));
  }

  function scriptIsolation(spec, queue) {
    const key = isolationKey(spec);
    isolationQueues.set(key, (Array.isArray(queue) ? queue : [queue]).map(normalizeScript));
    return key;
  }

  function applyScenario(name, spec = {}) {
    const queue = scenarioQueue(name);
    if (spec.listener) {
      script(spec.listener, queue);
      return { kind: "listener", listener: spec.listener, name };
    }
    if (!(spec.endpointId || spec.endpoint)) {
      const error = new Error("scenario requires --endpoint (or --listener) plus --model or --key; refusing unmatched empty isolation");
      error.code = "scenario_selector_required";
      throw error;
    }
    if (!(spec.model || spec.key || spec.keyFingerprint)) {
      const error = new Error("scenario requires --model or --key together with --endpoint");
      error.code = "scenario_selector_required";
      throw error;
    }
    const iso = isoSpecFrom(spec, name);
    const key = scriptIsolation(iso, queue);
    const activeKey = isolationKey({ ...iso, scenario: "" });
    activeScenarios.set(activeKey, name);
    return { kind: "isolation", key, name, spec: iso, activeKey };
  }

  function snapshot() {
    return journal.map((row) => ({
      ...row,
      errors: [...(row.errors || [])],
      bodyKeys: [...(row.bodyKeys || [])],
      roles: [...(row.roles || [])],
      toolNames: [...(row.toolNames || [])],
    }));
  }

  function takeScript(listenerId, iso) {
    if (iso) {
      const parts = splitIso(iso);
      const activeName = activeScenarios.get(iso) || activeScenarios.get(isolationKey({ ...parts, model: "", scenario: "" })) || "";
      if (activeName) {
        const scoped = isolationKey({ ...parts, scenario: activeName });
        const scopedQueue = isolationQueues.get(scoped);
        if (scopedQueue && scopedQueue.length) {
          const item = scopedQueue.shift();
          if (scopedQueue.length === 0) activeScenarios.delete(iso);
          return item;
        }
        const scopedWild = isolationQueues.get(isolationKey({ ...parts, model: "", scenario: activeName }));
        if (scopedWild && scopedWild.length) return scopedWild.shift();
      }
      const exact = isolationQueues.get(iso);
      if (exact && exact.length) return exact.shift();
      const wildcard = isolationQueues.get(isolationKey({ ...parts, model: "", scenario: "" }));
      if (wildcard && wildcard.length) return wildcard.shift();
    }
    const queue = queues.get(listenerId) ?? [];
    if (queue.length === 0) return { kind: "success" };
    return queue.shift();
  }

  function splitIso(iso) {
    const [endpointId, keyFingerprint, model, scenario] = String(iso).split("|");
    return { endpointId, keyFingerprint, model, scenario };
  }

  async function applyFault(req, res, slot, scripted, { stream }) {
    if (scripted.kind === "delay") await sleep(scripted.ms || 250);
    if (scripted.kind === "drop") {
      req.socket.destroy();
      return true;
    }
    if (scripted.kind === "http") {
      writeJson(res, scripted.status, scripted.body);
      return true;
    }
    if (scripted.kind === "malformed_json") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end("{not-json");
      return true;
    }
    if (scripted.kind === "stream_interrupt") {
      writeSseHeaders(res);
      res.write(streamBody(slot).slice(0, 40));
      req.socket.destroy();
      return true;
    }
    const omitUsage = scripted.kind === "missing_usage";
    const omitEnd = scripted.kind === "missing_end";
    if (stream || scripted.kind === "sse_split" || scripted.kind === "sse_fragment" || scripted.kind === "missing_end") {
      const body = streamBody(slot, { omitUsage, omitEnd });
      if (scripted.kind === "sse_split") {
        await writeChunked(res, body, { size: 8, delayMs: 5 });
        return true;
      }
      if (scripted.kind === "sse_fragment") {
        await writeChunked(res, body, { fragment: true, delayMs: 5 });
        return true;
      }
      writeSse(res, body);
      return true;
    }
    if (omitUsage) {
      writeJson(res, 200, jsonSuccess(slot, { omitUsage: true }));
      return true;
    }
    return false;
  }

  function bindDownstreamAbort(req, res, abort) {
    const trip = () => {
      if (!abort.signal.aborted) abort.abort();
    };
    const onResClose = () => {
      if (!res.writableFinished) trip();
    };
    req.on("aborted", trip);
    res.on("close", onResClose);
    req.socket?.on("close", onResClose);
    return () => {
      req.off("aborted", trip);
      res.off("close", onResClose);
      req.socket?.off("close", onResClose);
    };
  }

  async function forwardLive(req, res, slot, parsed) {
    if (!live) {
      const error = new Error("live is not enabled");
      error.code = "live_disabled";
      throw error;
    }
    const canonical = toCanonical(slot.protocol, parsed);
    const outbound = canonicalToChatRequest(canonical, { liveModel: live.model, maxTokens: live.maxTokens });
    const abort = new AbortController();
    const unbind = bindDownstreamAbort(req, res, abort);
    liveStats.calls += 1;
    try {
      return await live.send({
        body: outbound,
        stream: canonical.stream,
        signal: abort.signal,
        consume: async (response, { signal: remoteSignal, abort: abortRemote } = {}) => {
          const isAborted = () => abort.signal.aborted || remoteSignal?.aborted;
          const abortOwned = () => {
            if (!abort.signal.aborted) abort.abort();
            if (typeof abortRemote === "function") abortRemote();
          };
          if (!response.ok) {
            liveStats.errors += 1;
            const text = await response.text();
            abortOwned();
            writeJson(res, response.status >= 400 ? response.status : 502, liveErrorPayload(slot.protocol, response.status, `live upstream ${response.status}`));
            return { status: response.status, excerpt: text.slice(0, 120), aborted: false };
          }
          if (canonical.stream) {
            writeSseHeaders(res);
            let ended = false;
            let streamError = null;
            let sawDone = false;
            const translator = createLiveStreamTranslator({
              protocol: slot.protocol,
              slot,
              onWrite: (chunk) => {
                if (!ended && !res.writableEnded) res.write(chunk);
              },
              onEnd: () => {
                if (!ended && !res.writableEnded) {
                  ended = true;
                  res.end();
                }
              },
            });
            const parser = createSseParser((event) => {
              if (streamError) return;
              const classified = classifyChatSseData(event.data);
              if (classified.kind === "malformed" || classified.kind === "invalid") {
                streamError = new LiveProtocolError("live SSE event is not a Chat Completions chunk", "live_sse_invalid");
                return;
              }
              if (classified.kind === "error") {
                streamError = new LiveProtocolError(classified.error?.message || "live SSE error", "live_sse_error");
                return;
              }
              if (classified.kind === "done") sawDone = true;
              translator.onChatPayload(event.data);
            });
            const reader = response.body.getReader();
            const decoder = new TextDecoder();
            const cancelRemote = async () => {
              abortOwned();
              try {
                await reader.cancel();
              } catch {
                /* ignore */
              }
            };
            if (remoteSignal) {
              if (remoteSignal.aborted) {
                liveStats.aborted += 1;
                await cancelRemote();
                if (!res.writableEnded) res.end();
                return { status: 499, aborted: true, remoteCancelled: true };
              }
              remoteSignal.addEventListener(
                "abort",
                () => {
                  reader.cancel().catch(() => {});
                },
                { once: true },
              );
            }
            try {
              while (true) {
                if (isAborted()) {
                  liveStats.aborted += 1;
                  await cancelRemote();
                  if (!res.writableEnded) res.end();
                  return { status: 499, aborted: true, remoteCancelled: true };
                }
                const { done, value } = await reader.read();
                if (done) break;
                parser.push(decoder.decode(value, { stream: true }));
                if (streamError) break;
              }
              parser.flush();
              if (isAborted()) {
                liveStats.aborted += 1;
                await cancelRemote();
                if (!res.writableEnded) res.end();
                return { status: 499, aborted: true, remoteCancelled: true };
              }
              if (streamError) {
                liveStats.errors += 1;
                await cancelRemote();
                if (!res.writableEnded) res.end();
                return { status: 502, aborted: false, error: streamError.message, remoteCancelled: true };
              }
              if (!sawDone) {
                liveStats.errors += 1;
                await cancelRemote();
                if (!res.writableEnded) res.end();
                return { status: 502, aborted: false, error: "live SSE missing terminal [DONE]", remoteCancelled: true };
              }
              return { status: 200, aborted: false };
            } catch (error) {
              if (isAborted()) {
                liveStats.aborted += 1;
                await cancelRemote();
                if (!res.writableEnded) res.end();
                return { status: 499, aborted: true, remoteCancelled: true };
              }
              liveStats.errors += 1;
              await cancelRemote();
              if (!res.writableEnded) res.end();
              return { status: 502, aborted: false, error: error instanceof Error ? error.message : String(error), remoteCancelled: true };
            }
          }
          const remoteJson = await response.json();
          try {
            assertChatCompletionJson(remoteJson);
          } catch (error) {
            liveStats.errors += 1;
            writeJson(res, 502, liveErrorPayload(slot.protocol, 502, error.message));
            return { status: 502, aborted: false, error: error.message };
          }
          const result = chatResponseToCanonical(remoteJson);
          writeJson(res, 200, canonicalToClientJson(slot.protocol, result, slot, { synthesizeUsage: false }));
          return { status: 200, aborted: false };
        },
      });
    } catch (error) {
      if (error?.code === "live_aborted" || abort.signal.aborted) {
        liveStats.aborted += 1;
        if (!res.headersSent) writeJson(res, 499, liveErrorPayload(slot.protocol, 499, "client cancelled"));
        else if (!res.writableEnded) res.end();
        return { status: 499, aborted: true };
      }
      liveStats.errors += 1;
      if (!res.headersSent) writeJson(res, 502, liveErrorPayload(slot.protocol, 502, error instanceof Error ? error.message : "live failed"));
      else if (!res.writableEnded) res.end();
      return { status: 502, aborted: false, error: error instanceof Error ? error.message : String(error) };
    } finally {
      unbind();
    }
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
    if (!secretsFor(slot).has(auth.secret)) {
      if (!auth.errors.includes("wrong_secret")) auth.errors.push("wrong_secret");
    } else {
      auth.errors = auth.errors.filter((code) => code !== "wrong_secret");
    }
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

    const keyFingerprint = sha256(auth.secret || "");
    const iso = isolationKey({
      endpointId: slot.id || slot.slot,
      keyFingerprint,
      model: bodyInfo.model || "",
      scenario: "",
    });
    const scripted = takeScript(listener.id, iso);
    const row = remember({
      t: new Date().toISOString(),
      listener: listener.id,
      port: listener.port,
      slot: slot.slot,
      endpointId: slot.id || slot.slot,
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
      toolMarkerPresent: bodyInfo.toolMarkerPresent || false,
      hasToolResult: bodyInfo.hasToolResult || false,
      toolCallIds: bodyInfo.toolCallIds || [],
      argumentDigest: bodyInfo.argumentDigest || null,
      resultDigest: bodyInfo.resultDigest || null,
      scriptKind: scripted.kind,
      scriptStatus: scripted.kind === "http" ? scripted.status : scripted.kind === "success" || scripted.kind === "delay" || scripted.kind.startsWith("sse") || scripted.kind === "missing_usage" || scripted.kind === "missing_end" || scripted.kind === "malformed_json" ? 200 : scripted.kind === "drop" || scripted.kind === "stream_interrupt" ? null : 200,
      isolation: iso,
      live: Boolean(live) && liveArmed && errors.length === 0 && scripted.kind === "success",
    });

    if (scripted.kind !== "success") {
      const handled = await applyFault(req, res, slot, scripted, { stream: bodyInfo.stream });
      if (handled) return;
    }
    if (errors.length) {
      const status = statusFor(errors);
      writeJson(res, status, errorPayload(slot, errors, status));
      return;
    }
    if (live && liveArmed) {
      try {
        const liveResult = await forwardLive(req, res, slot, parsed);
        row.liveStatus = liveResult.status;
        row.liveAborted = liveResult.aborted;
        return;
      } catch (error) {
        row.liveError = error instanceof Error ? error.message : String(error);
        if (!res.headersSent) writeJson(res, 502, liveErrorPayload(slot.protocol, 502, "live failed"));
        return;
      }
    }
    const toolCall = Boolean(bodyInfo.toolMarkerPresent) && !bodyInfo.hasToolResult;
    const text = bodyInfo.utf8MarkerPresent ? `${slot.ok} \u2603` : undefined;
    if (bodyInfo.stream) writeSse(res, streamBody(slot, { toolCall, text }));
    else writeJson(res, 200, jsonSuccess(slot, { toolCall, text }));
  }

  async function handleModels(req, res, listener, path) {
    if (req.method !== "GET") await readBody(req, BODY_LIMIT);
    if (req.method !== "GET") {
      writeJson(res, 400, { error: { message: "method_not_allowed" } });
      return;
    }
    const slot = resolveModelsSlot(slots, listener.id, path, req.headers);
    const dummy = slot || slots.find((item) => item.listener === listener.id);
    const auth = inspectAuth(req.headers, dummy);
    if (auth.errors.includes("wrong_secret") && dummy && secretsFor(dummy).has(auth.secret)) {
      auth.errors = auth.errors.filter((code) => code !== "wrong_secret");
    }
    const fatal = auth.errors.filter((code) => code !== "missing_anthropic_version");
    if (fatal.length) {
      writeJson(res, 401, { error: { message: "unauthorized", type: "authentication_error" } });
      return;
    }
    const endpointId = slot.id || slot.slot;
    const queue = modelQueues.get(endpointId) || [];
    const scripted = queue.length ? queue.shift() : { kind: "success" };
    remember({
      t: new Date().toISOString(),
      listener: listener.id,
      port: listener.port,
      slot: slot.slot,
      endpointId,
      protocol: "models",
      path,
      model: null,
      stream: false,
      valid: true,
      errors: [],
      bodyKeys: [],
      roles: [],
      textMarkerPresent: false,
      authHeader: auth.authHeader,
      authHash: auth.authHash,
      bodyHash: sha256(""),
      toolNames: [],
      scriptKind: scripted.kind,
      scriptStatus: scripted.kind === "http" ? scripted.status : 200,
    });
    if (scripted.kind === "http") {
      writeJson(res, scripted.status, scripted.body || { error: { message: "catalog_failed", type: "api_error" } });
      return;
    }
    const catalog = Array.isArray(scripted.catalog) ? { ...slot, catalog: scripted.catalog } : slot;
    writeJson(res, 200, modelsCatalog(catalog));
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
      writeJson(res, 200, { requests: snapshot(), truncated, nextSequence });
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
      const slot = resolveSlot(slots, listener.id, path, req.headers, secretsFor);
      const run = slot
        ? handleInference(req, res, listener, slot, path)
        : isModelsPath(slots, listener.id, path)
          ? handleModels(req, res, listener, path)
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

  function handleControl(req, res) {
    const path = pathnameOf(req);
    const run = (async () => {
      if (path === "/health" && req.method === "GET") {
        writeJson(res, 200, { ok: true, runId, listeners: listeners.map((item) => ({ ...item })), control });
        return;
      }
      if (path === "/runtime" && req.method === "GET") {
        writeJson(res, 200, runtime());
        return;
      }
      if ((path === "/journal" || path === "/requests") && req.method === "GET") {
        writeJson(res, 200, { requests: snapshot(), truncated, nextSequence });
        return;
      }
      if (path === "/stats" && req.method === "GET") {
        writeJson(res, 200, stats());
        return;
      }
      if (path === "/reset" && req.method === "POST") {
        await readBody(req, BODY_LIMIT);
        reset();
        writeJson(res, 200, { ok: true });
        return;
      }
      const raw = await readBody(req, BODY_LIMIT);
      let body = {};
      if (raw.buf.length) {
        try {
          body = JSON.parse(raw.buf.toString("utf8"));
        } catch {
          writeJson(res, 400, { error: { message: "invalid_json" } });
          return;
        }
      }
      if (path === "/script" && req.method === "POST") {
        if (body.listener) script(body.listener, body.queue);
        else scriptIsolation(body.isolation || body, body.queue);
        writeJson(res, 200, { ok: true });
        return;
      }
      if (path === "/scenario" && req.method === "POST") {
        try {
          const applied = applyScenario(body.name, body);
          writeJson(res, 200, { ok: true, ...applied });
        } catch (error) {
          writeJson(res, error.code === "scenario_selector_required" ? 400 : 400, { error: { message: error.message, code: error.code } });
        }
        return;
      }
      if (path === "/accept-key" && req.method === "POST") {
        const fingerprints = acceptSecret(body.endpointId || body.endpoint, body.secret);
        writeJson(res, 200, { ok: true, fingerprints });
        return;
      }
      if (path === "/revoke-key" && req.method === "POST") {
        revokeSecret(body.endpointId || body.endpoint, body.secret);
        writeJson(res, 200, { ok: true });
        return;
      }
      if (path === "/script-models" && req.method === "POST") {
        scriptModels(body.endpointId || body.endpoint, body.queue);
        writeJson(res, 200, { ok: true });
        return;
      }
      if (path === "/accept-model" && req.method === "POST") {
        const catalog = acceptModel(body.endpointId || body.endpoint, body.model);
        writeJson(res, 200, { ok: true, catalog });
        return;
      }
      if (path === "/live" && req.method === "POST") {
        writeJson(res, 200, { ok: true, armed: armLive(body.armed !== false && body.armed !== 0) });
        return;
      }
      writeJson(res, 404, { error: { message: "unknown_route" } });
    })();
    Promise.resolve(run).catch(() => {
      if (!res.headersSent) writeJson(res, 400, { error: { message: "invalid_request" } });
    });
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

  function listenControl() {
    return new Promise((resolve, reject) => {
      controlServer = http.createServer(handleControl);
      controlServer.on("error", reject);
      controlServer.listen(0, host, () => {
        const address = controlServer.address();
        control.port = address.port;
        control.url = `http://${host}:${address.port}`;
        resolve(control);
      });
    });
  }

  function runtime() {
    return {
      runId,
      host,
      control: { ...control },
      listeners: listeners.map((item) => ({ ...item })),
      slots: slots.map((slot) => {
        const listener = listeners.find((item) => item.id === slot.listener);
        return {
          ...slot,
          url: listener ? `${listener.url}${slot.path}` : slot.path,
          modelsUrl: listener ? `${listener.url}${slot.modelsPath}` : slot.modelsPath,
          listenerUrl: listener?.url,
          port: listener?.port,
        };
      }),
      liveEnabled: Boolean(live) && liveArmed,
      liveConfigured: Boolean(live),
      truncated,
      nextSequence,
    };
  }

  function stats() {
    return {
      remoteCalls: live ? live.stats().calls : liveStats.calls,
      liveAborted: live ? live.stats().aborted : liveStats.aborted,
      liveErrors: liveStats.errors,
      receipts: journal.length,
      truncated,
      nextSequence,
      liveEnabled: Boolean(live) && liveArmed,
      liveConfigured: Boolean(live),
    };
  }

  async function start() {
    for (const id of listenerIds) await listenOne(id);
    await listenControl();
    return runtime();
  }

  function close() {
    if (closed) return Promise.resolve();
    closed = true;
    const all = [...servers, controlServer].filter(Boolean);
    return Promise.all(
      all.map(
        (server) =>
          new Promise((resolve) => {
            if (typeof server.closeAllConnections === "function") server.closeAllConnections();
            server.close(() => resolve());
          }),
      ),
    );
  }

  return {
    start,
    close,
    reset,
    script,
    scriptIsolation,
    applyScenario,
    acceptSecret,
    revokeSecret,
    acceptModel,
    scriptModels,
    armLive,
    snapshot,
    stats,
    runtime,
    listeners,
    journal,
    control,
    get truncated() {
      return truncated;
    },
  };
}

export function selfCheck() {
  const chat = SLOT_DEFS.find((slot) => slot.slot === "chat");
  const responses = SLOT_DEFS.find((slot) => slot.slot === "responses");
  const ok = validateBody(chat, {
    model: chat.model,
    messages: [{ role: "user", content: [{ type: "text", text: `ping LAB_PROBE` }] }],
    tools: [{ type: "function", function: { name: "lookup", parameters: { type: "object" } } }],
  });
  if (ok.errors.length !== 0) throw new Error(`chat native should pass: ${ok.errors}`);
  const responsesOk = validateBody(responses, {
    model: responses.model,
    store: false,
    input: "LAB_PROBE",
  });
  if (responsesOk.errors.length !== 0) throw new Error(`responses store=false should pass: ${responsesOk.errors}`);
  const responsesStore = validateBody(responses, { model: responses.model, input: "LAB_PROBE" });
  if (!responsesStore.errors.includes("store_not_false")) throw new Error("responses without store=false must fail");
  const bearer = inspectAuth({ authorization: `Bearer ${chat.secret}` }, chat);
  if (bearer.errors.length !== 0) throw new Error("chat bearer");
}

export { loadProfile, SLOT_DEFS, LISTENER_IDS };
