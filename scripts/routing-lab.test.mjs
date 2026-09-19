import assert from "node:assert/strict";
import test from "node:test";
import { makeApi } from "./routing-lab/run.mjs";

function installRecordingFetch() {
  const calls = [];
  const previous = globalThis.fetch;
  globalThis.fetch = async (input, init = {}) => {
    const url = String(input);
    const method = init.method ?? "GET";
    const body = init.body ? JSON.parse(String(init.body)) : null;
    calls.push({ url, method, body });
    let payload = { revision: 7, processGeneration: 99 };
    if (url.endsWith("/dashboard/api/v4/accounts") && method === "GET") {
      payload = { identities: [], revision: { revision: 7, processGeneration: 99 } };
    } else if (url.endsWith("/dashboard/api/v4/credentials") && method === "GET") {
      payload = {
        credentials: [
          { legacyAccountId: "acc-alpha", enabled: false, id: "cred-alpha" },
          { legacyAccountId: "acc-bravo", enabled: true, id: "cred-bravo" },
          { legacyAccountId: "acc-zen", enabled: true, id: "cred-zen" },
        ],
        revision: { revision: 7, processGeneration: 99 },
      };
    } else if (url.endsWith("/dashboard/api/v4/contract") && method === "GET") {
      payload = { revision: 7, processGeneration: 99, pricingRevision: "p1" };
    }
    return new Response(JSON.stringify(payload), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };
  return {
    calls,
    restore() {
      globalThis.fetch = previous;
    },
  };
}

test("routing lab dashboard client constructs V4 requests with CAS and never calls V3", async () => {
  const recording = installRecordingFetch();
  try {
    const api = makeApi("http://127.0.0.1:19042", { snapshot: () => [] }, () => "sk-lab");
    const identities = await api.identities();
    assert.deepEqual(identities, []);
    await api.reorder(["acc-bravo", "acc-alpha"]);
    await api.setRoutingMode("strict-priority", false);
    await api.resetCooldowns(["acc-alpha"]);
    await api.setEnabled("acc-alpha", true);
    await api.patchBinding("bind-1", { enabled: false });

    assert.ok(recording.calls.length > 0, "client issued dashboard requests");
    for (const call of recording.calls) {
      assert.equal(call.url.includes("/dashboard/api/v3"), false, call.url);
      assert.match(call.url, /\/dashboard\/api\/v4\//);
    }

    const identityGet = recording.calls.find((call) => (
      call.method === "GET" && call.url.endsWith("/dashboard/api/v4/accounts")
    ));
    assert.ok(identityGet, "identities listing uses GET /dashboard/api/v4/accounts");

    const credentialGets = recording.calls.filter((call) => (
      call.method === "GET" && call.url.endsWith("/dashboard/api/v4/credentials")
    ));
    assert.ok(credentialGets.length >= 2, "reorder and enablement read V4 credentials");

    const order = recording.calls.find((call) => (
      call.method === "PUT" && call.url.endsWith("/dashboard/api/v4/accounts/order")
    ));
    assert.ok(order, "reorder uses remounted V4 accounts/order");
    assert.deepEqual(order.body.accountIds, ["acc-bravo", "acc-alpha", "acc-zen"]);
    assert.equal(order.body.expectedRevision, 7);
    assert.equal(order.body.processGeneration, 99);

    const settings = recording.calls.find((call) => (
      call.method === "PUT" && call.url.endsWith("/dashboard/api/v4/settings")
    ));
    assert.ok(settings);
    assert.equal(settings.body.routingMode, "strict-priority");
    assert.equal(settings.body.expectedRevision, 7);

    const cooldown = recording.calls.find((call) => (
      call.method === "POST" && call.url.endsWith("/dashboard/api/v4/accounts/acc-alpha/reset-cooldown")
    ));
    assert.ok(cooldown);
    assert.equal(cooldown.body.expectedRevision, 7);

    const toggle = recording.calls.find((call) => (
      call.method === "POST" && call.url.endsWith("/dashboard/api/v4/accounts/acc-alpha/toggle")
    ));
    assert.ok(toggle);
    assert.equal(toggle.body.processGeneration, 99);

    const binding = recording.calls.find((call) => (
      call.method === "PATCH" && call.url.endsWith("/dashboard/api/v4/bindings/bind-1")
    ));
    assert.ok(binding);
    assert.equal(binding.body.enabled, false);
    assert.equal(binding.body.expectedRevision, 7);
  } finally {
    recording.restore();
  }
});
