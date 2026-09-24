import assert from "node:assert/strict";
import test from "node:test";
import { LIVE_KEY_ENV, LIVE_MODEL, LIVE_URL_ENV, MARKER, TOOL_MARKER, TOOL_NAME } from "../lib/common.mjs";
import { createLiveClient, readLiveEnv } from "../lib/live.mjs";
import { createLab } from "../lib/lab.mjs";
import { canonicalToChatRequest, chatToCanonical } from "../lib/adapters.mjs";

test("missing live env is a hard configuration miss, not a pass", () => {
  const cfg = readLiveEnv({ [LIVE_URL_ENV]: "", [LIVE_KEY_ENV]: "" });
  assert.equal(cfg.present, false);
});

test("live client sends minimax-m3, enforces budget, and does not retry", async () => {
  const calls = [];
  const fetchImpl = async (url, init) => {
    calls.push({ url, body: JSON.parse(init.body), authorization: init.headers.authorization });
    return new Response(JSON.stringify({
      id: "chatcmpl-x",
      model: LIVE_MODEL,
      choices: [{ index: 0, message: { role: "assistant", content: "ok" }, finish_reason: "stop" }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    }), { status: 200, headers: { "content-type": "application/json" } });
  };
  const client = createLiveClient({
    url: "http://127.0.0.1:9/v1/chat/completions",
    key: "sk-test-not-real",
    maxCalls: 2,
    fetchImpl,
  });
  const first = await client.send({ body: { model: "ignored", messages: [{ role: "user", content: "a" }] }, stream: false });
  assert.equal(first.status, 200);
  assert.equal(calls[0].body.model, LIVE_MODEL);
  assert.equal(calls[0].authorization, "Bearer sk-test-not-real");
  await client.send({ body: { model: "ignored", messages: [] }, stream: false });
  await assert.rejects(() => client.send({ body: { model: "ignored", messages: [] }, stream: false }), /budget/);
  assert.equal(calls.length, 2);
});

test("armed live path rewrites only the protocol model and records a remote call", async () => {
  const fetchImpl = async (_url, init) => {
    const body = JSON.parse(init.body);
    assert.equal(body.model, LIVE_MODEL);
    return new Response(JSON.stringify({
      id: "chatcmpl-x",
      model: LIVE_MODEL,
      choices: [{ index: 0, message: { role: "assistant", content: "from-remote" }, finish_reason: "stop" }],
      usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
    }), { status: 200, headers: { "content-type": "application/json" } });
  };
  const live = createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", fetchImpl });
  const lab = createLab({ live });
  lab.armLive(true);
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const response = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
    });
    const body = await response.json();
    assert.equal(response.status, 200);
    assert.equal(body.model, "upstream-chat");
    assert.equal(body.choices[0].message.content, "from-remote");
    assert.equal(lab.stats().remoteCalls, 1);
  } finally {
    await lab.close();
  }
});

test("remote HTTP error is not converted into a local success", async () => {
  const fetchImpl = async () => new Response(JSON.stringify({ error: { message: "nope" } }), { status: 503 });
  const live = createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", fetchImpl });
  const lab = createLab({ live });
  lab.armLive(true);
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const response = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
    });
    assert.equal(response.status, 503);
    const body = await response.json();
    assert.ok(body.error);
    assert.notEqual(body.choices?.[0]?.message?.content, chat.ok);
  } finally {
    await lab.close();
  }
});

test("unarmed live client stays local with zero remote calls", async () => {
  let called = 0;
  const fetchImpl = async () => {
    called += 1;
    return new Response("{}", { status: 500 });
  };
  const live = createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", fetchImpl });
  const lab = createLab({ live });
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const response = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
    });
    const body = await response.json();
    assert.equal(response.status, 200);
    assert.equal(body.choices[0].message.content, chat.ok);
    assert.equal(called, 0);
    assert.equal(lab.stats().remoteCalls, 0);
  } finally {
    await lab.close();
  }
});

test("canonical chat request never ships the public or upstream lab model live", () => {
  const canonical = chatToCanonical({
    model: "upstream-chat",
    messages: [{ role: "user", content: MARKER }],
  });
  const outbound = canonicalToChatRequest(canonical);
  assert.equal(outbound.model, LIVE_MODEL);
});

function validCompletion() {
  return {
    id: "chatcmpl-x",
    object: "chat.completion",
    model: LIVE_MODEL,
    choices: [{ index: 0, message: { role: "assistant", content: "ok" }, finish_reason: "stop" }],
    usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
  };
}

function validSse() {
  return `data: ${JSON.stringify({
    id: "chatcmpl-x",
    object: "chat.completion.chunk",
    choices: [{ index: 0, delta: { content: "ok" }, finish_reason: null }],
  })}\n\ndata: [DONE]\n\n`;
}

function delayedResponse(ms, build) {
  return new Promise((resolve) => setTimeout(() => resolve(build()), ms));
}

function delayedBody(ms, text, headers = { "content-type": "text/event-stream" }) {
  const encoder = new TextEncoder();
  return new Response(
    new ReadableStream({
      async start(controller) {
        await new Promise((resolve) => setTimeout(resolve, ms));
        controller.enqueue(encoder.encode(text));
        controller.close();
      },
    }),
    { status: 200, headers },
  );
}

test("maxCalls is atomic: 3 concurrent sends with maxCalls=1 perform one call", async () => {
  let calls = 0;
  let maxInFlight = 0;
  let inFlight = 0;
  const fetchImpl = async () => {
    calls += 1;
    inFlight += 1;
    maxInFlight = Math.max(maxInFlight, inFlight);
    await new Promise((resolve) => setTimeout(resolve, 40));
    inFlight -= 1;
    return new Response(JSON.stringify(validCompletion()), { status: 200, headers: { "content-type": "application/json" } });
  };
  const client = createLiveClient({
    url: "http://127.0.0.1:9/v1/chat/completions",
    key: "sk-test-not-real",
    maxCalls: 1,
    fetchImpl,
  });
  const settled = await Promise.allSettled([
    client.send({ body: { messages: [] }, stream: false }),
    client.send({ body: { messages: [] }, stream: false }),
    client.send({ body: { messages: [] }, stream: false }),
  ]);
  assert.equal(calls, 1);
  assert.equal(maxInFlight, 1);
  assert.equal(settled.filter((item) => item.status === "fulfilled").length, 1);
  assert.equal(settled.filter((item) => item.status === "rejected").length, 2);
});

test("timeout covers delayed headers and delayed body", async () => {
  const headerClient = createLiveClient({
    url: "http://127.0.0.1:9/v1/chat/completions",
    key: "sk-test-not-real",
    timeoutMs: 30,
    fetchImpl: (_url, init) =>
      new Promise((resolve, reject) => {
        const timer = setTimeout(() => resolve(new Response(JSON.stringify(validCompletion()), { status: 200 })), 120);
        init.signal.addEventListener(
          "abort",
          () => {
            clearTimeout(timer);
            reject(Object.assign(new Error("aborted"), { name: "AbortError" }));
          },
          { once: true },
        );
      }),
  });
  await assert.rejects(() => headerClient.send({ body: { messages: [] }, stream: false }), /aborted/);

  const bodyClient = createLiveClient({
    url: "http://127.0.0.1:9/v1/chat/completions",
    key: "sk-test-not-real",
    timeoutMs: 30,
    fetchImpl: () => delayedBody(120, JSON.stringify(validCompletion()), { "content-type": "application/json" }),
  });
  await assert.rejects(() => bodyClient.send({ body: { messages: [] }, stream: false }), /aborted/);
});

test("caller abort is observed through delayed stream completion", async () => {
  const ac = new AbortController();
  const client = createLiveClient({
    url: "http://127.0.0.1:9/v1/chat/completions",
    key: "sk-test-not-real",
    timeoutMs: 5000,
    fetchImpl: (_url, init) => {
      assert.equal(init.signal.aborted, false);
      return delayedBody(200, validSse());
    },
  });
  const pending = client.send({ body: { messages: [] }, stream: true, signal: ac.signal });
  await new Promise((resolve) => setTimeout(resolve, 20));
  ac.abort();
  await assert.rejects(() => pending, /aborted/);
  assert.equal(client.stats().aborted, 1);
});

test("lab live 200 empty object is not a synthetic success", async () => {
  const fetchImpl = async () => new Response(JSON.stringify({}), { status: 200, headers: { "content-type": "application/json" } });
  const live = createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", fetchImpl });
  const lab = createLab({ live });
  lab.armLive(true);
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const response = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
    });
    const body = await response.json();
    assert.equal(response.status, 502);
    assert.ok(body.error);
    assert.equal(body.usage, undefined);
    assert.equal(lab.stats().liveErrors, 1);
  } finally {
    await lab.close();
  }
});

test("lab live SSE error/malformed/missing terminal are not empty success", async () => {
  async function run(remoteBody) {
    const fetchImpl = async () => new Response(remoteBody, { status: 200, headers: { "content-type": "text/event-stream" } });
    const live = createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", fetchImpl });
    const lab = createLab({ live });
    lab.armLive(true);
    const started = await lab.start();
    try {
      const chat = started.slots.find((slot) => slot.slot === "chat");
      const response = await fetch(chat.url, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
        body: JSON.stringify({ model: chat.model, stream: true, messages: [{ role: "user", content: MARKER }] }),
      });
      const text = await response.text();
      return { live, lab, text, status: response.status };
    } finally {
      await lab.close();
    }
  }

  const errorCase = await run(`data: ${JSON.stringify({ error: { message: "nope" } })}\n\ndata: [DONE]\n\n`);
  assert.ok(errorCase.lab.stats().liveErrors >= 1);
  assert.equal(errorCase.text.includes("[DONE]"), false);

  const malformed = await run("data: {not-json\n\n");
  assert.ok(malformed.lab.stats().liveErrors >= 1);
  assert.equal(malformed.text.includes("[DONE]"), false);

  const missing = await run(`data: ${JSON.stringify({ id: "x", choices: [{ index: 0, delta: { content: "hi" }, finish_reason: null }] })}\n\n`);
  assert.ok(missing.lab.stats().liveErrors >= 1);
  assert.equal(missing.text.includes("[DONE]"), false);

  const valid = await run(validSse());
  assert.equal(valid.lab.stats().liveErrors, 0);
  assert.equal(valid.text.includes("[DONE]"), true);
  assert.match(valid.text, /ok/);
});

test("live model override is rejected and outbound is always minimax-m3", () => {
  assert.throws(
    () => createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", model: "other-model" }),
    /minimax-m3/,
  );
});

test("converted Responses/Messages wait for remote DONE and do not fabricate usage", async () => {
  function sse(parts) {
    return parts.join("");
  }
  function chunk(finish, usage) {
    const body = { id: "c", object: "chat.completion.chunk", choices: [{ index: 0, delta: { content: "hi" }, finish_reason: finish }] };
    if (usage) body.usage = usage;
    return `data: ${JSON.stringify(body)}\n\n`;
  }
  async function run(protocol, remote) {
    const fetchImpl = async () => new Response(remote, { status: 200, headers: { "content-type": "text/event-stream" } });
    const live = createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", fetchImpl });
    const lab = createLab({ live });
    lab.armLive(true);
    const started = await lab.start();
    try {
      const slot = started.slots.find((item) => item.protocol === protocol);
      const body = protocol === "messages"
        ? { model: slot.model, stream: true, messages: [{ role: "user", content: [{ type: "text", text: MARKER }] }] }
        : protocol === "responses"
          ? { model: slot.model, stream: true, store: false, input: MARKER }
          : { model: slot.model, stream: true, messages: [{ role: "user", content: MARKER }] };
      const headers = protocol === "messages"
        ? { "content-type": "application/json", "x-api-key": slot.secret, "anthropic-version": "2023-06-01" }
        : { "content-type": "application/json", authorization: `Bearer ${slot.secret}` };
      const response = await fetch(slot.url, { method: "POST", headers, body: JSON.stringify(body) });
      const text = await response.text();
      return { text, errors: lab.stats().liveErrors };
    } finally {
      await lab.close();
    }
  }

  const eofChat = await run("chat_completions", chunk("stop"));
  assert.equal(eofChat.text.includes("[DONE]"), false);
  assert.ok(eofChat.errors >= 1);

  const eof = await run("responses", chunk("stop") + chunk("stop"));
  assert.equal(eof.text.includes("response.completed"), false);
  assert.ok(eof.errors >= 1);

  const afterErrChat = await run("chat_completions", chunk("stop") + `data: ${JSON.stringify({ error: { message: "nope" } })}\n\n`);
  assert.equal(afterErrChat.text.includes("[DONE]"), false);
  assert.ok(afterErrChat.errors >= 1);

  const afterErr = await run("messages", chunk("stop") + `data: ${JSON.stringify({ error: { message: "nope" } })}\n\n`);
  assert.equal(afterErr.text.includes("message_stop"), false);
  assert.ok(afterErr.errors >= 1);

  const withUsageChat = await run("chat_completions", chunk(null) + chunk("stop", { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 }) + "data: [DONE]\n\n");
  assert.equal(withUsageChat.text.includes("[DONE]"), true);
  assert.match(withUsageChat.text, /"completion_tokens":4/);
  assert.equal(withUsageChat.errors, 0);

  const withUsage = await run("responses", chunk(null) + chunk("stop", { prompt_tokens: 3, completion_tokens: 4, total_tokens: 7 }) + "data: [DONE]\n\n");
  assert.equal(withUsage.text.includes("response.completed"), true);
  assert.match(withUsage.text, /"output_tokens":4/);
  assert.equal(withUsage.errors, 0);

  const noUsageChat = await run("chat_completions", chunk(null) + chunk("stop") + "data: [DONE]\n\n");
  assert.equal(noUsageChat.text.includes("[DONE]"), true);
  assert.equal(noUsageChat.text.includes('"prompt_tokens":1'), false);
  assert.equal(noUsageChat.errors, 0);

  const noUsage = await run("messages", chunk(null) + chunk("stop") + "data: [DONE]\n\n");
  assert.equal(noUsage.text.includes("message_stop"), true);
  assert.equal(noUsage.text.includes('"output_tokens":1'), false);
});

test("early stream error/malformed/missing terminal cancel the remaining-open remote body", async () => {
  async function run(remoteBytes, { timeoutMs = 5000 } = {}) {
    let cancelled = false;
    let remoteSignal;
    const fetchImpl = async (_url, init) => {
      remoteSignal = init.signal;
      return new Response(
        new ReadableStream({
          start(controller) {
            controller.enqueue(new TextEncoder().encode(remoteBytes));
          },
          cancel() {
            cancelled = true;
          },
        }),
        { status: 200, headers: { "content-type": "text/event-stream" } },
      );
    };
    const live = createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", timeoutMs, fetchImpl });
    const lab = createLab({ live });
    lab.armLive(true);
    const started = await lab.start();
    try {
      const chat = started.slots.find((slot) => slot.slot === "chat");
      await fetch(chat.url, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
        body: JSON.stringify({ model: chat.model, stream: true, messages: [{ role: "user", content: MARKER }] }),
      });
      const deadline = Date.now() + Math.max(250, timeoutMs + 150);
      while (Date.now() < deadline && !(cancelled && remoteSignal?.aborted)) {
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      return { cancelled, aborted: Boolean(remoteSignal?.aborted) };
    } finally {
      await lab.close();
    }
  }

  const errorCase = await run(`data: ${JSON.stringify({ error: { message: "nope" } })}\n\n`);
  assert.equal(errorCase.cancelled, true);
  assert.equal(errorCase.aborted, true);

  const malformed = await run("data: {not-json\n\n");
  assert.equal(malformed.cancelled, true);
  assert.equal(malformed.aborted, true);

  const missing = await run(
    `data: ${JSON.stringify({ id: "x", choices: [{ index: 0, delta: { content: "hi" }, finish_reason: "stop" }] })}\n\n`,
    { timeoutMs: 80 },
  );
  assert.equal(missing.cancelled, true);
  assert.equal(missing.aborted, true);
});

test("aborting the Gateway-facing client aborts the remote fetch signal", async () => {
  let remoteSignal;
  const fetchImpl = async (_url, init) => {
    remoteSignal = init.signal;
    return delayedBody(400, validSse());
  };
  const live = createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", timeoutMs: 5000, fetchImpl });
  const lab = createLab({ live });
  lab.armLive(true);
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const ac = new AbortController();
    const pending = fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, stream: true, messages: [{ role: "user", content: MARKER }] }),
      signal: ac.signal,
    });
    await new Promise((resolve) => setTimeout(resolve, 40));
    ac.abort();
    await pending.catch(() => {});
    await new Promise((resolve) => setTimeout(resolve, 40));
    assert.ok(remoteSignal, "remote fetch was not started");
    assert.equal(remoteSignal.aborted, true);
  } finally {
    await lab.close();
  }
});

test("mock remote tool round-trip sends the model-produced tool result back", async () => {
  const bodies = [];
  const fetchImpl = async (_url, init) => {
    const body = JSON.parse(init.body);
    bodies.push(body);
    if (bodies.length === 1) {
      return new Response(JSON.stringify({
        id: "chatcmpl-tool",
        model: LIVE_MODEL,
        choices: [{
          index: 0,
          message: {
            role: "assistant",
            content: null,
            tool_calls: [{ id: "call_remote_1", type: "function", function: { name: TOOL_NAME, arguments: JSON.stringify({ value: "ping" }) } }],
          },
          finish_reason: "tool_calls",
        }],
        usage: { prompt_tokens: 3, completion_tokens: 2, total_tokens: 5 },
      }), { status: 200, headers: { "content-type": "application/json" } });
    }
    assert.ok(body.messages.some((message) => message.role === "tool" && message.tool_call_id === "call_remote_1"));
    return new Response(JSON.stringify({
      id: "chatcmpl-final",
      model: LIVE_MODEL,
      choices: [{ index: 0, message: { role: "assistant", content: "done-from-result" }, finish_reason: "stop" }],
      usage: { prompt_tokens: 4, completion_tokens: 1, total_tokens: 5 },
    }), { status: 200, headers: { "content-type": "application/json" } });
  };
  const live = createLiveClient({ url: "http://127.0.0.1:9/v1/chat/completions", key: "sk-test-not-real", fetchImpl });
  const lab = createLab({ live });
  lab.armLive(true);
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const first = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({
        model: chat.model,
        messages: [{ role: "user", content: `${MARKER} ${TOOL_MARKER}` }],
        tools: [{ type: "function", function: { name: TOOL_NAME, parameters: { type: "object" } } }],
      }),
    });
    const firstJson = await first.json();
    assert.equal(first.status, 200);
    const call = firstJson.choices[0].message.tool_calls[0];
    assert.equal(call.id, "call_remote_1");
    assert.equal(call.function.name, TOOL_NAME);
    const second = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({
        model: chat.model,
        messages: [
          { role: "user", content: `${MARKER} ${TOOL_MARKER}` },
          { role: "assistant", content: null, tool_calls: [call] },
          { role: "tool", tool_call_id: call.id, content: JSON.stringify({ value: "ping" }) },
        ],
        tools: [{ type: "function", function: { name: TOOL_NAME, parameters: { type: "object" } } }],
      }),
    });
    const secondJson = await second.json();
    assert.equal(second.status, 200);
    assert.equal(secondJson.choices[0].message.content, "done-from-result");
    assert.equal(bodies.length, 2);
  } finally {
    await lab.close();
  }
});
