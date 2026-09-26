import assert from "node:assert/strict";
import test from "node:test";
import net from "node:net";
import { LIVE_MODEL, MARKER, sha256 } from "../lib/common.mjs";
import { createLab } from "../lib/lab.mjs";
import { isolationKey } from "../lib/faults.mjs";
import { createLiveClient } from "../lib/live.mjs";

function portOpen(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ host: "127.0.0.1", port }, () => {
      socket.destroy();
      resolve(true);
    });
    socket.setTimeout(300, () => {
      socket.destroy();
      resolve(false);
    });
    socket.on("error", () => resolve(false));
  });
}

async function withLab(fn) {
  const lab = createLab();
  const started = await lab.start();
  try {
    return await fn(lab, started);
  } finally {
    await lab.close();
  }
}

test("serve binds loopback inference listeners plus a control port", async () => {
  await withLab(async (lab, started) => {
    assert.equal(started.listeners.length, 3);
    assert.ok(started.control.port > 0);
    const health = await fetch(`${started.control.url}/health`);
    assert.equal(health.status, 200);
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const response = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
    });
    const body = await response.json();
    assert.equal(response.status, 200);
    assert.equal(body.choices[0].message.content, chat.ok);
    assert.ok(body.usage);
    assert.equal(lab.stats().remoteCalls, 0);
    const receipt = lab.snapshot()[0];
    assert.equal(receipt.sequence, 1);
    assert.equal(receipt.valid, true);
  });
});

test("acceptModel allows an extra exact upstream id on the same listener", async () => {
  await withLab(async (lab, started) => {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    lab.acceptModel(chat.id, "exact-raw-unique");
    const response = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: "exact-raw-unique", messages: [{ role: "user", content: MARKER }] }),
    });
    assert.equal(response.status, 200, await response.text());
    assert.equal(lab.snapshot().at(-1).model, "exact-raw-unique");
  });
});

test("each endpoint Key lists its own upstream catalog", async () => {
  await withLab(async (_lab, started) => {
    for (const slot of started.slots.filter((item) => ["chat", "responses", "messages"].includes(item.slot))) {
      const headers =
        slot.auth === "bearer"
          ? { authorization: `Bearer ${slot.secret}` }
          : { "x-api-key": slot.secret };
      const response = await fetch(slot.modelsUrl, { headers });
      const body = await response.json();
      assert.equal(response.status, 200, slot.slot);
      assert.deepEqual(
        body.data.map((item) => item.id),
        slot.catalog,
      );
    }
  });
});

test("faults are isolated by endpoint, Key, and model", async () => {
  await withLab(async (lab, started) => {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const responses = started.slots.find((slot) => slot.slot === "responses");
    lab.scriptIsolation(
      { endpointId: chat.id, keyFingerprint: sha256(chat.secret), model: chat.model, scenario: "" },
      [{ kind: "http", status: 429, body: { error: { message: "isolated", type: "rate_limit_error" } } }],
    );
    const blocked = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
    });
    assert.equal(blocked.status, 429);
    const other = await fetch(responses.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${responses.secret}` },
      body: JSON.stringify({ model: responses.model, store: false, input: MARKER }),
    });
    assert.equal(other.status, 200);
    void isolationKey;
  });
});

test("journal uses a monotonic sequence and marks truncation", async () => {
  const lab = createLab({ maxLog: 2 });
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    for (let i = 0; i < 3; i += 1) {
      await fetch(chat.url, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
        body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
      });
    }
    const rows = lab.snapshot();
    assert.deepEqual(rows.map((row) => row.sequence), [2, 3]);
    assert.equal(lab.truncated, true);
    assert.equal(lab.stats().nextSequence, 3);
  } finally {
    await lab.close();
  }
});

test("close releases owned inference and control ports", async () => {
  const lab = createLab();
  const started = await lab.start();
  const ports = [...started.listeners.map((item) => item.port), started.control.port];
  for (const port of ports) assert.equal(await portOpen(port), true);
  await lab.close();
  for (const port of ports) assert.equal(await portOpen(port), false);
});

test("control reset clears receipts", async () => {
  await withLab(async (lab, started) => {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
    });
    assert.equal(lab.snapshot().length, 1);
    const reset = await fetch(`${started.control.url}/reset`, { method: "POST" });
    assert.equal(reset.status, 200);
    assert.equal(lab.snapshot().length, 0);
  });
});

test("control reset re-arms when a live client exists and the next success is forwarded", async () => {
  const calls = [];
  const live = createLiveClient({
    url: "http://127.0.0.1:9/v1/chat/completions",
    key: "sk-test-not-real",
    fetchImpl: async (_url, init) => {
      calls.push(JSON.parse(init.body));
      return new Response(
        JSON.stringify({
          id: "chatcmpl-reset",
          model: LIVE_MODEL,
          choices: [{ index: 0, message: { role: "assistant", content: "from-remote" }, finish_reason: "stop" }],
          usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    },
  });
  const lab = createLab({ live, runId: "lab-reset-rearm" });
  const started = await lab.start();
  try {
    lab.armLive(false);
    assert.equal(lab.stats().liveEnabled, false);
    const reset = await fetch(`${started.control.url}/reset`, { method: "POST" });
    assert.equal(reset.status, 200);
    assert.equal(lab.stats().liveEnabled, true, "POST /reset must change liveEnabled from false to true");
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const response = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
    });
    const body = await response.json();
    assert.equal(response.status, 200);
    assert.equal(body.choices[0].message.content, "from-remote");
    assert.equal(calls.length, 1);
    assert.equal(lab.stats().remoteCalls, 1);
  } finally {
    await lab.close();
  }
});

test("control reset does not invent liveEnabled when no live client is configured", async () => {
  await withLab(async (lab, started) => {
    lab.armLive(true);
    assert.equal(lab.stats().liveEnabled, false);
    const reset = await fetch(`${started.control.url}/reset`, { method: "POST" });
    assert.equal(reset.status, 200);
    assert.equal(lab.stats().liveEnabled, false);
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const response = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify({ model: chat.model, messages: [{ role: "user", content: MARKER }] }),
    });
    const body = await response.json();
    assert.equal(response.status, 200);
    assert.equal(body.choices[0].message.content, chat.ok);
    assert.equal(lab.stats().remoteCalls, 0);
  });
});
