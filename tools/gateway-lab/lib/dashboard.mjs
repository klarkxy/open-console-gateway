import assert from "node:assert/strict";
import { MARKER, REQUEST_TIMEOUT_MS, TOOL_NAME, TOOL_SCHEMA, sha256 } from "./common.mjs";

export async function request(base, pathName, method = "GET", body, headers = {}, timeoutMs = REQUEST_TIMEOUT_MS) {
  return fetch(base + pathName, {
    method,
    headers: { "content-type": "application/json", ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
}

export async function readJsonResponse(response) {
  const text = await response.text();
  try {
    return { status: response.status, body: JSON.parse(text), text, headers: response.headers };
  } catch {
    return { status: response.status, body: null, text, headers: response.headers };
  }
}

export function input(client, model, stream) {
  if (client === "chat") {
    return {
      model,
      stream,
      max_tokens: 64,
      messages: [
        { role: "system", content: "Preserve the routing lab message." },
        { role: "user", content: MARKER },
      ],
      tools: [{ type: "function", function: { name: TOOL_NAME, description: "Echo a value", parameters: TOOL_SCHEMA } }],
    };
  }
  if (client === "responses") {
    return {
      model,
      stream,
      store: false,
      max_output_tokens: 64,
      instructions: "Preserve the routing lab message.",
      input: [{ role: "user", content: [{ type: "input_text", text: MARKER }] }],
      tools: [{ type: "function", name: TOOL_NAME, description: "Echo a value", parameters: TOOL_SCHEMA }],
    };
  }
  if (client === "gemini") {
    return {
      contents: [{ role: "user", parts: [{ text: MARKER }] }],
    };
  }
  return {
    model,
    stream,
    max_tokens: 64,
    system: "Preserve the routing lab message.",
    messages: [{ role: "user", content: [{ type: "text", text: MARKER }] }],
    tools: [{ name: TOOL_NAME, description: "Echo a value", input_schema: TOOL_SCHEMA }],
  };
}

export function clientPath(client, model, stream) {
  if (client === "chat") return "/v1/chat/completions";
  if (client === "responses") return "/v1/responses";
  if (client === "messages") return "/v1/messages";
  const action = stream ? "streamGenerateContent" : "generateContent";
  return `/v1beta/models/${model}:${action}`;
}

export function checkOutput(client, stream, body, marker) {
  if (client === "gemini") {
    if (stream) {
      const data = body
        .split(/\r?\n/)
        .filter((line) => line.startsWith("data: "))
        .map((line) => line.slice(6))
        .filter((line) => line && line !== "[DONE]");
      assert.ok(data.length > 0, "No Gemini SSE objects");
      const objects = data.map((line) => JSON.parse(line));
      const text = objects
        .flatMap((item) => item.candidates ?? [])
        .flatMap((candidate) => candidate.content?.parts ?? [])
        .map((part) => part.text ?? "")
        .join("");
      assert.equal(text, marker, "Gemini SSE text changed/duplicated");
      return;
    }
    const parsed = JSON.parse(body);
    const text = (parsed.candidates ?? [])
      .flatMap((candidate) => candidate.content?.parts ?? [])
      .map((part) => part.text ?? "")
      .join("");
    assert.equal(text, marker, "Gemini JSON text changed/duplicated");
    return;
  }
  if (stream) {
    const data = body.split(/\r?\n/).filter((line) => line.startsWith("data: ")).map((line) => line.slice(6));
    const objects = data.filter((line) => line !== "[DONE]").map((line) => JSON.parse(line));
    assert.ok(objects.length > 0, "No SSE objects");
    let text = "";
    if (client === "chat") {
      text = objects.map((item) => item.choices?.[0]?.delta?.content ?? "").join("");
      assert.ok(data.includes("[DONE]"), "Missing Chat terminal event");
      assert.ok(objects.some((item) => item.object === "chat.completion.chunk"));
    } else if (client === "responses") {
      text = objects.filter((item) => item.type === "response.output_text.delta").map((item) => item.delta).join("");
      assert.ok(objects.some((item) => item.type === "response.completed" && item.response?.status === "completed"));
    } else {
      text = objects.filter((item) => item.type === "content_block_delta").map((item) => item.delta?.text ?? "").join("");
      assert.ok(objects.some((item) => item.type === "message_start"));
      assert.ok(objects.some((item) => item.type === "message_stop"));
    }
    assert.equal(text, marker, "SSE text changed/duplicated");
    return;
  }
  const parsed = JSON.parse(body);
  if (client === "chat") {
    assert.equal(parsed.object, "chat.completion");
    assert.equal(parsed.choices[0].message.role, "assistant");
    assert.equal(parsed.choices[0].message.content, marker);
  } else if (client === "responses") {
    assert.equal(parsed.object, "response");
    assert.equal(parsed.status, "completed");
    assert.equal(
      parsed.output.flatMap((item) => item.content ?? []).filter((part) => part.type === "output_text").map((part) => part.text).join(""),
      marker,
    );
  } else {
    assert.equal(parsed.type, "message");
    assert.equal(parsed.role, "assistant");
    assert.equal(parsed.content.filter((part) => part.type === "text").map((part) => part.text).join(""), marker);
  }
}

export function parseGeminiSseObjects(body) {
  return String(body || "")
    .split(/\r?\n/)
    .filter((line) => line.startsWith("data: "))
    .map((line) => line.slice(6))
    .filter((line) => line && line !== "[DONE]")
    .map((line) => JSON.parse(line));
}

export function assertGeminiLiveStream(body) {
  const objects = parseGeminiSseObjects(body);
  assert.ok(objects.length > 0, "gemini SSE had no data objects");
  const finished = objects.some((item) =>
    (item.candidates || []).some((candidate) => typeof candidate.finishReason === "string" && candidate.finishReason.length > 0),
  );
  assert.ok(finished, "gemini SSE missing finishReason terminal");
  return objects;
}

export function assertLiveRemoteReceipt(lab, live, mark, beforeCalls) {
  const after = live.stats().calls;
  assert.equal(after - beforeCalls, 1, `expected exactly one remote call, got ${after - beforeCalls}`);
  const hits = lab.snapshot().slice(mark);
  assert.equal(hits.length, 1, `expected exactly one receipt, got ${hits.length}`);
  assert.equal(hits[0].liveStatus, 200, `liveStatus=${hits[0].liveStatus}`);
  return hits;
}

export function inferenceHeaders(client, gatewayKey) {
  if (client === "messages") return { authorization: `Bearer ${gatewayKey}`, "anthropic-version": "2023-06-01" };
  if (client === "gemini") return { "x-goog-api-key": gatewayKey };
  return { authorization: `Bearer ${gatewayKey}`, "anthropic-version": "2023-06-01" };
}

export function authHash(slot) {
  return sha256(slot.auth === "bearer" ? `Bearer ${slot.secret}` : slot.secret);
}

export function findCredential(identities, connectionId) {
  for (const identity of identities) {
    for (const credential of identity.credentials) {
      const binding = credential.bindings.find((item) => item.connectionId === connectionId);
      if (binding) {
        return {
          identityId: identity.identity.id,
          credential: credential.credential,
          binding,
          legacy: credential.legacy,
          raw: credential,
        };
      }
    }
  }
  return null;
}

export function summarizeHit(hit) {
  return {
    seq: hit.sequence,
    listener: hit.listener,
    slot: hit.slot,
    path: hit.path,
    model: hit.model,
    valid: hit.valid,
    errors: hit.errors,
    store: hit.store,
    scriptKind: hit.scriptKind,
    scriptStatus: hit.scriptStatus,
  };
}

export function makeApi(gatewayBase, lab, gatewayKey) {
  async function json(pathName, method = "GET", body) {
    const response = await request(gatewayBase, pathName, method, body);
    const parsed = await readJsonResponse(response);
    assert.equal(parsed.status, 200, `${method} ${pathName}: ${parsed.status} ${parsed.text.slice(0, 1200)}`);
    return parsed.body;
  }

  async function tokens() {
    return json("/dashboard/api/v4/contract");
  }

  async function mutation(pathName, body, method = "POST") {
    const cas = await tokens();
    return json(pathName, method, { ...body, expectedRevision: cas.revision, processGeneration: cas.processGeneration });
  }

  async function identities() {
    return (await json("/dashboard/api/v4/accounts")).identities;
  }

  async function credentials() {
    return (await json("/dashboard/api/v4/credentials")).credentials;
  }

  async function reorder(preferredIds) {
    const rows = await credentials();
    const currentIds = rows.map((item) => item.legacyAccountId);
    const remaining = currentIds.filter((id) => !preferredIds.includes(id));
    const accountIds = [...preferredIds, ...remaining];
    const missing = preferredIds.filter((id) => !currentIds.includes(id));
    assert.equal(missing.length, 0, `reorder missing ids: ${missing}`);
    return mutation("/dashboard/api/v4/accounts/order", { accountIds }, "PUT");
  }

  async function patchBinding(id, patch) {
    return mutation(`/dashboard/api/v4/bindings/${id}`, patch, "PATCH");
  }

  async function setRoutingMode(routingMode, conversationSticky) {
    return mutation("/dashboard/api/v4/settings", { routingMode, conversationSticky }, "PUT");
  }

  async function resetCooldowns(accountIds) {
    for (const id of accountIds) {
      if (!id) continue;
      await mutation(`/dashboard/api/v4/accounts/${id}/reset-cooldown`, {});
    }
  }

  async function setEnabled(accountId, enabled) {
    const rows = await credentials();
    const credential = rows.find((item) => item.legacyAccountId === accountId);
    assert.ok(credential, `missing credential ${accountId}`);
    if (Boolean(credential.enabled) !== Boolean(enabled)) {
      await mutation(`/dashboard/api/v4/accounts/${accountId}/toggle`, {});
    }
  }

  async function chatRoute(timeoutMs) {
    const key = typeof gatewayKey === "function" ? gatewayKey() : gatewayKey;
    const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", "lab-route", false), inferenceHeaders("chat", key), timeoutMs);
    const text = await response.text();
    return { status: response.status, text, headers: response.headers };
  }

  function expectHits(label, mark, expected) {
    const got = lab.snapshot().slice(mark);
    if (got.length !== expected.length) {
      throw new Error(`${label}: expected ${expected.length} upstream hits, got ${got.length}: ${JSON.stringify(got.map(summarizeHit))}`);
    }
    for (let i = 0; i < expected.length; i += 1) {
      const exp = expected[i];
      const hit = got[i];
      if (exp.listener && hit.listener !== exp.listener) throw new Error(`${label}: hit ${i} listener ${hit.listener} != ${exp.listener}`);
      if (exp.slot && hit.slot !== exp.slot) throw new Error(`${label}: hit ${i} slot ${hit.slot} != ${exp.slot}`);
      if (exp.model && hit.model !== exp.model) throw new Error(`${label}: hit ${i} model ${hit.model} != ${exp.model}`);
      if (exp.path && hit.path !== exp.path) throw new Error(`${label}: hit ${i} path ${hit.path} != ${exp.path}`);
      if (exp.valid != null && hit.valid !== exp.valid) throw new Error(`${label}: hit ${i} valid=${hit.valid} errors=${JSON.stringify(hit.errors)}`);
      if (exp.store !== undefined && hit.store !== exp.store) throw new Error(`${label}: hit ${i} store=${JSON.stringify(hit.store)} != ${exp.store}`);
      if (exp.authHash && hit.authHash !== exp.authHash) throw new Error(`${label}: hit ${i} auth hash mismatch`);
    }
    return got;
  }

  return { json, mutation, identities, credentials, reorder, patchBinding, setRoutingMode, resetCooldowns, setEnabled, chatRoute, expectHits };
}
