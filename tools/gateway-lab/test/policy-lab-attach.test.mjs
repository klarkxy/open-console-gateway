import assert from "node:assert/strict";
import test from "node:test";
import net from "node:net";
import { LIVE_MODEL, MARKER, sha256 } from "../lib/common.mjs";
import { createLab } from "../lib/lab.mjs";
import { createLiveClient } from "../lib/live.mjs";
import { input } from "../lib/dashboard.mjs";
import { attachLabFromRuntime, call, isolationSpec, labSnapshot, labStats, snapshotLiveArmed } from "../lib/policy-lab-attach.mjs";
import { PHASE, delayFault, httpFault, custom400Body } from "../lib/policy-contract.mjs";
import { resetLab } from "../lib/policy-scenarios.mjs";
import { waitFor } from "../lib/process.mjs";

function stubLive(calls) {
  return createLiveClient({
    url: "http://127.0.0.1:9/v1/chat/completions",
    key: "sk-test-not-real",
    fetchImpl: async (_url, init) => {
      calls.push(JSON.parse(init.body));
      return new Response(
        JSON.stringify({
          id: "chatcmpl-attach",
          model: LIVE_MODEL,
          choices: [{ index: 0, message: { role: "assistant", content: "from-remote" }, finish_reason: "stop" }],
          usage: { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 },
        }),
        { status: 200, headers: { "content-type": "application/json" } },
      );
    },
  });
}

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

async function postChat(slot, model = slot.model) {
  const response = await fetch(slot.url, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${slot.secret}` },
    body: JSON.stringify(input("chat", model, false)),
  });
  const text = await response.text();
  return { status: response.status, text };
}

test("non-loopback runtime is rejected", () => {
  assert.throws(
    () =>
      attachLabFromRuntime({
        host: "8.8.8.8",
        control: { url: "http://8.8.8.8:9" },
        listeners: [],
        slots: [],
      }),
    /loopback/,
  );
});

test("attach talks over control HTTP and close leaves the lab listening", async () => {
  const lab = createLab({ runId: "policy-attach" });
  const started = await lab.start();
  try {
    const attached = attachLabFromRuntime({ ...started, liveConfigured: true, liveEnabled: false });
    const health = await attached.health();
    assert.equal(health.ok, true);
    const chat = started.slots.find((slot) => slot.slot === "chat");
    await call(attached, "script", chat.listener, [httpFault(400, custom400Body())]);
    const first = await postChat(chat);
    assert.equal(first.status, 400);
    const hits = await labSnapshot(attached);
    assert.equal(hits.length, 1);
    assert.equal(hits[0].scriptStatus, 400);
    const stats = await labStats(attached);
    assert.equal(stats.remoteCalls, 0);
    await attached.close();
    assert.equal(await portOpen(started.control.port), true, "external lab control closed by attach.close");
    assert.equal(await portOpen(started.listeners[0].port), true, "external listener closed by attach.close");
    const still = await fetch(`${started.control.url}/health`);
    assert.equal(still.status, 200);
  } finally {
    await lab.close();
  }
});

test("fault queues isolate by endpoint and model", async () => {
  const lab = createLab({ runId: "policy-iso" });
  const started = await lab.start();
  try {
    const attached = attachLabFromRuntime(started);
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const responses = started.slots.find((slot) => slot.slot === "responses");
    await call(attached, "scriptIsolation", isolationSpec(chat), [httpFault(400, custom400Body())]);
    const blocked = await postChat(chat);
    assert.equal(blocked.status, 400);
    const other = await fetch(responses.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${responses.secret}` },
      body: JSON.stringify(input("responses", responses.model, false)),
    });
    const otherText = await other.text();
    assert.equal(other.status, 200, otherText.slice(0, 200));
    await call(attached, "acceptModel", chat.id, "upstream-extra");
    await call(attached, "scriptIsolation", isolationSpec(chat, { model: "upstream-extra" }), [
      httpFault(503, { error: { message: "isolated-model" } }),
    ]);
    const extra = await postChat(chat, "upstream-extra");
    assert.equal(extra.status, 503);
    const original = await postChat(chat);
    assert.equal(original.status, 200);
    assert.equal((await labStats(attached)).remoteCalls, 0);
  } finally {
    await lab.close();
  }
});

test("delay fault plus concurrent barrier yields two arrivals without fuzzy sleep", async () => {
  const lab = createLab({ runId: "policy-delay" });
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    lab.script(chat.listener, [delayFault(120), delayFault(120)]);
    const mark = lab.snapshot().length;
    const launched = [postChat(chat), postChat(chat)];
    const results = await Promise.all(launched);
    assert.ok(results.every((item) => item.status === 200));
    const hits = lab.snapshot().slice(mark);
    assert.equal(hits.length, 2);
    assert.ok(hits.every((hit) => hit.scriptKind === "delay"));
    assert.equal(lab.stats().remoteCalls, 0);
  } finally {
    await lab.close();
  }
});

test("responses are fully read and cancel does not require a complete body", async () => {
  const lab = createLab({ runId: "policy-body" });
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const json = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify(input("chat", chat.model, false)),
    });
    const jsonText = await json.text();
    JSON.parse(jsonText);
    assert.equal(json.status, 200);
    const sse = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify(input("chat", chat.model, true)),
    });
    const sseText = await sse.text();
    assert.ok(sseText.includes("[DONE]"));
    const ac = new AbortController();
    const pending = fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify(input("chat", chat.model, true)),
      signal: ac.signal,
    });
    ac.abort();
    let interrupted = false;
    let cancelledText = "";
    try {
      const response = await pending;
      cancelledText = await response.text();
    } catch {
      interrupted = true;
    }
    assert.ok(interrupted || !cancelledText.includes("[DONE]"));
    assert.equal(lab.stats().remoteCalls, 0);
    assert.equal(sha256(chat.secret).length, 64);
    assert.match(MARKER, /LAB_PROBE/);
  } finally {
    await lab.close();
  }
});

test("delayed HTTP 400 is the same in-flight request, not a later one", async () => {
  const lab = createLab({ runId: "policy-http-delay" });
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    lab.script(chat.listener, [httpFault(400, custom400Body(), {}, 400)]);
    const mark = lab.snapshot().length;
    let settled = false;
    const pending = postChat(chat).then((result) => {
      settled = true;
      return result;
    });
    const arrived = await waitFor(
      async () => {
        const hits = lab.snapshot().slice(mark);
        if (hits.length === 0) throw new Error("no arrival");
        return hits;
      },
      { timeoutMs: 2000, intervalMs: 15, label: "delayed http 400 arrival" },
    );
    assert.equal(settled, false, "response was released before delay finished");
    assert.equal(arrived.length, 1);
    assert.equal(arrived[0].scriptKind, "http");
    assert.equal(arrived[0].scriptStatus, 400);
    const first = await pending;
    assert.equal(first.status, 400);
    assert.equal(lab.snapshot().slice(mark).length, 1);
    const second = await postChat(chat);
    assert.equal(second.status, 200);
    const hits = lab.snapshot().slice(mark);
    assert.equal(hits.length, 2);
    assert.notEqual(hits[1].scriptStatus, 400);
    assert.equal(lab.stats().remoteCalls, 0);
  } finally {
    await lab.close();
  }
});

test("live-arm snapshot is restored without stopping the lab", async () => {
  const lab = createLab({ runId: "policy-arm-restore" });
  const started = await lab.start();
  try {
    const attached = attachLabFromRuntime(started);
    const saved = await snapshotLiveArmed(attached);
    await call(attached, "armLive", true);
    await call(attached, "armLive", saved);
    assert.equal(await snapshotLiveArmed(attached), saved);
    await attached.close();
    assert.equal(await portOpen(started.control.port), true);
    assert.equal((await labStats(attached)).remoteCalls, 0);
  } finally {
    await lab.close();
  }
});

test("raw control reset re-arms; attach.reset restores the previous armed flag", async () => {
  const calls = [];
  const lab = createLab({ live: stubLive(calls), runId: "attach-reset-restore" });
  const started = await lab.start();
  try {
    const attached = attachLabFromRuntime({ ...started, liveConfigured: true });
    const chat = started.slots.find((slot) => slot.slot === "chat");

    await call(attached, "armLive", false);
    assert.equal(await snapshotLiveArmed(attached), false);
    const raw = await fetch(`${started.control.url}/reset`, { method: "POST" });
    assert.equal(raw.status, 200);
    assert.equal(await snapshotLiveArmed(attached), true, "POST /reset must change liveEnabled from false to true");

    await call(attached, "armLive", false);
    await attached.reset();
    assert.equal(await snapshotLiveArmed(attached), false);

    const local = await postChat(chat);
    assert.equal(local.status, 200);
    assert.match(local.text, /LAB_OK_chat/);
    assert.doesNotMatch(local.text, /from-remote/);
    assert.equal(calls.length, 0);
    assert.equal((await labStats(attached)).remoteCalls, 0);

    await call(attached, "armLive", true);
    await attached.reset();
    assert.equal(await snapshotLiveArmed(attached), true);
  } finally {
    await lab.close();
  }
});

test("resetLab simulate phase stays disarmed; live phase is the only re-arm", async () => {
  const calls = [];
  const lab = createLab({ live: stubLive(calls), runId: "resetlab-phase" });
  const started = await lab.start();
  try {
    const attached = attachLabFromRuntime({ ...started, liveConfigured: true });
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const ctxSimulate = { suite: "live", phase: PHASE.SIMULATE };
    const ctxLive = { suite: "live", phase: PHASE.LIVE };

    await call(attached, "armLive", false);
    await resetLab(attached, ctxSimulate);
    assert.equal(await snapshotLiveArmed(attached), false);
    const local = await postChat(chat);
    assert.equal(local.status, 200);
    assert.match(local.text, /LAB_OK_chat/);
    assert.equal(calls.length, 0);
    assert.equal((await labStats(attached)).remoteCalls, 0);

    await resetLab(attached, ctxLive);
    assert.equal(await snapshotLiveArmed(attached), true);
    const remote = await postChat(chat);
    assert.equal(remote.status, 200);
    assert.match(remote.text, /from-remote/);
    assert.equal(calls.length, 1);
    assert.equal((await labStats(attached)).remoteCalls, 1);

    await resetLab(attached, ctxSimulate);
    assert.equal(await snapshotLiveArmed(attached), false);
    const after = await postChat(chat);
    assert.equal(after.status, 200);
    assert.match(after.text, /LAB_OK_chat/);
    assert.equal(calls.length, 1);
  } finally {
    await lab.close();
  }
});
