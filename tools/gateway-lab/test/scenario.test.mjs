import assert from "node:assert/strict";
import test from "node:test";
import { MARKER, sha256 } from "../lib/common.mjs";
import { createLab } from "../lib/lab.mjs";
import { input } from "../lib/dashboard.mjs";

test("selector-free scenario is rejected instead of empty isolation", async () => {
  const lab = createLab();
  const started = await lab.start();
  try {
    const response = await fetch(`${started.control.url}/scenario`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: "http_429" }),
    });
    assert.equal(response.status, 400);
    const body = await response.json();
    assert.match(body.error.message, /endpoint|listener/);
  } finally {
    await lab.close();
  }
});

test("non-loopback lab host is rejected", async () => {
  assert.throws(() => createLab({ host: "0.0.0.0" }), /loopback/);
});

test("scripted HTTP failures deliver Retry-After to the client", async () => {
  const lab = createLab();
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    lab.script(chat.listener, [{
      kind: "http", status: 429, headers: { "Retry-After": "3" },
      body: { error: { message: "temporary" } },
    }]);
    const response = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify(input("chat", chat.model, false)),
    });
    assert.equal(response.status, 429);
    assert.equal(response.headers.get("retry-after"), "3");
    assert.deepEqual(await response.json(), { error: { message: "temporary" } });
  } finally {
    await lab.close();
  }
});

test("scoped http_429 matches endpoint+key+model+scenario and does not hit siblings", async () => {
  const lab = createLab();
  const started = await lab.start();
  try {
    const chat = started.slots.find((slot) => slot.slot === "chat");
    const alpha = started.slots.find((slot) => slot.slot === "alpha");
    const responses = started.slots.find((slot) => slot.slot === "responses");
    const applied = await fetch(`${started.control.url}/scenario`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        name: "http_429",
        endpointId: chat.id,
        model: chat.model,
        key: chat.secret,
      }),
    });
    assert.equal(applied.status, 200, await applied.text());
    const blocked = await fetch(chat.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${chat.secret}` },
      body: JSON.stringify(input("chat", chat.model, false)),
    });
    assert.equal(blocked.status, 429);
    const chatHit = lab.snapshot().at(-1);
    assert.equal(chatHit.scriptKind, "http");
    assert.equal(chatHit.scriptStatus, 429);

    const siblingKey = await fetch(alpha.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${alpha.secret}` },
      body: JSON.stringify(input("chat", alpha.model, false)),
    });
    assert.equal(siblingKey.status, 200, await siblingKey.text());

    const siblingEndpoint = await fetch(responses.url, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${responses.secret}` },
      body: JSON.stringify(input("responses", responses.model, false)),
    });
    assert.equal(siblingEndpoint.status, 200);
    void sha256;
  } finally {
    await lab.close();
  }
});
