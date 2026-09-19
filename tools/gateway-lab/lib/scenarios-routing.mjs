import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { MARKER, TOOL_NAME } from "./common.mjs";
import {
  authHash,
  checkOutput,
  clientPath,
  findCredential,
  inferenceHeaders,
  input,
  request,
  summarizeHit,
} from "./dashboard.mjs";

export function protocolSlotsOf(started) {
  return started.slots.filter((slot) => ["chat", "responses", "messages"].includes(slot.slot));
}

export function routeSlotsOf(started) {
  return started.slots.filter((slot) => ["alpha", "bravo", "charlie"].includes(slot.slot));
}

export async function runProtocolMatrix(runtime, collector, { includeGeminiUpstreams = false } = {}) {
  const { lab, started, api, gatewayBase, gatewayKey } = runtime;
  const protocolSlots = protocolSlotsOf(started);
  const clients = ["chat", "responses", "messages"];
  for (const target of protocolSlots) {
    for (const client of clients) {
      for (const stream of [false, true]) {
        const label = `${client} -> ${target.slot} ${stream ? "SSE" : "JSON"}`;
        const mark = lab.snapshot().length;
        try {
          const pathName = clientPath(client, target.publicModel, stream);
          const headers = inferenceHeaders(client, gatewayKey);
          const response = await request(gatewayBase, pathName, "POST", input(client, target.publicModel, stream), headers);
          const body = await response.text();
          assert.equal(response.status, 200, `${label}: ${body.slice(0, 500)}`);
          assert.match(response.headers.get("content-type") || "", stream ? /text\/event-stream/ : /application\/json/);
          checkOutput(client, stream, body, target.ok);
          const hits = api.expectHits(label, mark, [
            {
              listener: target.listener,
              slot: target.slot,
              model: target.model,
              path: target.path,
              valid: true,
              authHash: authHash(target),
              store: target.protocol === "responses" ? false : undefined,
            },
          ]);
          if (target.protocol === "responses") {
            assert.equal(hits[0].store, false, `${label}: Responses conversion lost store=false`);
          }
          if (!hits[0].toolNames.includes(TOOL_NAME)) throw new Error(`${label}: lost function declaration`);
          collector.pass(label, {
            status: response.status,
            upstreamSends: 1,
            receipt: summarizeHit(hits[0]),
            scenarioId: `gw.matrix.${client}.${target.slot}.${stream ? "sse" : "json"}`,
            evidenceKind: "gateway_black_box",
          });
        } catch (error) {
          collector.fail(label, error, { upstreamSends: lab.snapshot().slice(mark).length, hits: lab.snapshot().slice(mark).map(summarizeHit) });
        }
      }
    }
  }

  const geminiTargets = includeGeminiUpstreams ? protocolSlots : protocolSlots.filter((slot) => slot.slot === "chat");
  for (const target of geminiTargets) {
    for (const stream of [false, true]) {
      const label = `gemini -> ${target.slot} ${stream ? "SSE" : "JSON"}`;
      const mark = lab.snapshot().length;
      try {
        const response = await request(
          gatewayBase,
          clientPath("gemini", target.publicModel, stream),
          "POST",
          input("gemini", target.publicModel, stream),
          { "x-goog-api-key": gatewayKey },
        );
        const body = await response.text();
        if (response.status === 404 || /not (found|supported)|unsupported/i.test(body)) {
          collector.unsupported(label, `gemini client path not accepted for ${target.slot} (${response.status})`);
          continue;
        }
        assert.equal(response.status, 200, `${label}: ${body.slice(0, 500)}`);
        checkOutput("gemini", stream, body, target.ok);
        api.expectHits(label, mark, [
          {
            listener: target.listener,
            slot: target.slot,
            model: target.model,
            path: target.path,
            valid: true,
            authHash: authHash(target),
          },
        ]);
        collector.pass(label, {
          status: response.status,
          upstreamSends: 1,
          receipt: summarizeHit(lab.snapshot().slice(mark)[0]),
          scenarioId: `gw.gemini.${target.slot}.${stream ? "sse" : "json"}`,
          evidenceKind: "gateway_black_box",
        });
      } catch (error) {
        collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
      }
    }
  }
}

export async function runDirectValidatorNegatives(runtime, collector) {
  const alphaUrl = runtime.started.listeners.find((item) => item.id === "alpha").url;
  for (const [label, pathName, body, headers] of [
    ["reject wrong Key", "/chat/v1/chat/completions", input("chat", "upstream-chat", false), { authorization: "Bearer wrong" }],
    ["reject wrong model", "/chat/v1/chat/completions", input("chat", "upstream-messages", false), { authorization: "Bearer sk-lab-chat" }],
    ["reject wrong object family", "/chat/v1/chat/completions", input("responses", "upstream-chat", false), { authorization: "Bearer sk-lab-chat" }],
    ["reject wrong provider destination", "/messages/v1/chat/completions", input("chat", "upstream-chat", false), { authorization: "Bearer sk-lab-chat" }],
    ["reject null object", "/chat/v1/chat/completions", null, { authorization: "Bearer sk-lab-chat" }],
    [
      "reject malformed text object",
      "/chat/v1/chat/completions",
      { model: "upstream-chat", messages: [{ role: "user", content: MARKER }, { role: "user", content: [{ type: "text", text: 42 }] }] },
      { authorization: "Bearer sk-lab-chat" },
    ],
    ["reject unexpected query", "/chat/v1/chat/completions?extra=1", input("chat", "upstream-chat", false), { authorization: "Bearer sk-lab-chat" }],
  ]) {
    try {
      const response = await request(alphaUrl, pathName, "POST", body, headers);
      await response.text();
      assert.ok(response.status >= 400 && response.status < 500, `${label}: validator accepted invalid input (${response.status})`);
      collector.pass(label, { status: response.status, control: "direct validator negative control", scenarioId: `gw.validator.${label.replace(/\s+/g, "-")}`, evidenceKind: "lab_fixture" });
    } catch (error) {
      collector.fail(label, error);
    }
  }
}

export async function runRoutingControlScenarios(runtime, collector) {
  const { lab, started, api, gatewayBase, gatewayKey } = runtime;
  const protocolSlots = protocolSlotsOf(started);
  const routeSlots = routeSlotsOf(started);

  {
    const label = "unknown model zero send";
    const mark = lab.snapshot().length;
    try {
      const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", "lab-unknown", false), inferenceHeaders("chat", gatewayKey));
      await response.text();
      assert.ok(!response.ok);
      assert.equal(lab.snapshot().length, mark);
      collector.pass(label, { status: response.status, upstreamSends: 0, scenarioId: "gw.unknown-model", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
    }
  }

  for (const slot of protocolSlots) {
    const label = `revoked ${slot.slot} grant zero send`;
    const mark = lab.snapshot().length;
    const saved = { allowedEndpointIds: slot.binding.allowedEndpointIds, allowedOrigins: slot.binding.allowedOrigins };
    try {
      await api.patchBinding(slot.binding.id, { allowedEndpointIds: [], allowedOrigins: [] });
      const response = await request(gatewayBase, clientPath(slot.slot === "chat" ? "chat" : slot.slot, slot.publicModel, false), "POST", input(slot.slot === "messages" ? "messages" : slot.slot, slot.publicModel, false), inferenceHeaders(slot.slot === "messages" ? "messages" : "chat", gatewayKey));
      await response.text();
      assert.ok(!response.ok);
      assert.equal(lab.snapshot().length, mark, `${label} still sent`);
      collector.pass(label, { status: response.status, upstreamSends: 0, scenarioId: `gw.key.revoke.${slot.slot}`, evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit), scenarioId: `gw.key.revoke.${slot.slot}` });
    } finally {
      await api.patchBinding(slot.binding.id, saved);
    }
  }

  await api.setRoutingMode("strict-priority", false);
  await api.reorder(routeSlots.map((slot) => slot.accountId));
  await api.resetCooldowns(routeSlots.map((slot) => slot.accountId));

  {
    const label = "strict priority stop on success";
    const mark = lab.snapshot().length;
    try {
      const response = await api.chatRoute();
      assert.equal(response.status, 200, response.text.slice(0, 500));
      checkOutput("chat", false, response.text, "LAB_OK_alpha");
      api.expectHits(label, mark, [{ listener: "alpha", slot: "alpha", model: "upstream-alpha", valid: true, authHash: authHash(routeSlots[0]) }]);
      collector.pass(label, { status: response.status, order: routeSlots.map((slot) => slot.slot), scenarioId: "gw.routing.strict", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
    }
  }

  {
    const label = "strict priority 429 fallthrough then stop";
    lab.script("alpha", [{ kind: "http", status: 429, body: { error: { message: "Resets in 5 minutes", type: "rate_limit_error" } } }]);
    const mark = lab.snapshot().length;
    try {
      const response = await api.chatRoute();
      assert.equal(response.status, 200, response.text.slice(0, 500));
      checkOutput("chat", false, response.text, "LAB_OK_bravo");
      api.expectHits(label, mark, [
        { listener: "alpha", slot: "alpha", model: "upstream-alpha" },
        { listener: "bravo", slot: "bravo", model: "upstream-bravo", valid: true, authHash: authHash(routeSlots[1]) },
      ]);
      collector.pass(label, { status: response.status, chronological: lab.snapshot().slice(mark).map((hit) => hit.listener), scenarioId: "gw.fail.429", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
    } finally {
      lab.script("alpha", []);
      await api.resetCooldowns(routeSlots.map((slot) => slot.accountId));
    }
  }

  {
    const label = "5xx must not replay";
    lab.script("alpha", [{ kind: "http", status: 503, body: { error: { message: "unavailable", type: "api_error" } } }]);
    const mark = lab.snapshot().length;
    try {
      const response = await api.chatRoute();
      const hits = lab.snapshot().slice(mark);
      assert.equal(hits.length, 1, `503 replayed: ${JSON.stringify(hits.map(summarizeHit))}`);
      assert.equal(hits[0].listener, "alpha");
      assert.equal(hits[0].scriptStatus, 503);
      assert.notEqual(response.status, 200);
      assert.ok(!hits.some((hit) => hit.listener === "bravo" || hit.listener === "charlie"), "503 replayed to later account");
      collector.pass(label, { status: response.status, upstreamSends: 1, bodyExcerpt: response.text.slice(0, 240), scenarioId: "gw.fail.503", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
    } finally {
      lab.script("alpha", []);
      await api.resetCooldowns(routeSlots.map((slot) => slot.accountId));
    }
  }

  {
    const label = "post-connect uncertain failure must not replay";
    lab.script("alpha", [{ kind: "drop" }]);
    const mark = lab.snapshot().length;
    try {
      const response = await api.chatRoute(25000);
      const hits = lab.snapshot().slice(mark);
      assert.equal(hits.length, 1, `uncertain failure replayed: ${JSON.stringify(hits.map(summarizeHit))}`);
      assert.equal(hits[0].listener, "alpha");
      assert.equal(hits[0].scriptKind, "drop");
      assert.notEqual(response.status, 200);
      assert.ok(!hits.some((hit) => hit.listener !== "alpha"), "post-connect failure replayed");
      collector.pass(label, { status: response.status, upstreamSends: 1, bodyExcerpt: response.text.slice(0, 240), scenarioId: "gw.fail.drop", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
    } finally {
      lab.script("alpha", []);
      await api.resetCooldowns(routeSlots.map((slot) => slot.accountId));
    }
  }

  {
    const label = "reordered accounts change destination";
    try {
      await api.reorder([routeSlots[1].accountId, routeSlots[0].accountId, routeSlots[2].accountId]);
      const mark = lab.snapshot().length;
      const response = await api.chatRoute();
      assert.equal(response.status, 200, response.text.slice(0, 500));
      checkOutput("chat", false, response.text, "LAB_OK_bravo");
      api.expectHits(label, mark, [{ listener: "bravo", slot: "bravo", model: "upstream-bravo", valid: true }]);
      collector.pass(label, { status: response.status, order: ["bravo", "alpha", "charlie"], scenarioId: "gw.routing.reorder", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error);
    } finally {
      await api.reorder(routeSlots.map((slot) => slot.accountId));
    }
  }

  {
    const label = "disabled account skip";
    try {
      await api.setEnabled(routeSlots[0].accountId, false);
      const mark = lab.snapshot().length;
      const response = await api.chatRoute();
      assert.equal(response.status, 200, response.text.slice(0, 500));
      checkOutput("chat", false, response.text, "LAB_OK_bravo");
      api.expectHits(label, mark, [{ listener: "bravo", slot: "bravo", valid: true }]);
      collector.pass(label, { status: response.status, scenarioId: "gw.routing.disable", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error);
    } finally {
      await api.setEnabled(routeSlots[0].accountId, true);
      await api.reorder(routeSlots.map((slot) => slot.accountId));
    }
  }

  {
    const label = "binding modelScope skip";
    const bindingId = routeSlots[0].binding.id;
    try {
      await api.patchBinding(bindingId, { modelScope: { kind: "only", models: ["lab-unrelated"] } });
      const mark = lab.snapshot().length;
      const response = await api.chatRoute();
      assert.equal(response.status, 200, response.text.slice(0, 500));
      checkOutput("chat", false, response.text, "LAB_OK_bravo");
      api.expectHits(label, mark, [{ listener: "bravo", slot: "bravo", valid: true }]);
      collector.pass(label, { status: response.status, scenarioId: "gw.routing.scope", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error);
    } finally {
      await api.patchBinding(bindingId, { modelScope: { kind: "all" } });
    }
  }

  {
    const label = "shared quota sibling skip";
    let siblingAccountId = null;
    try {
      const beforeIdentities = await api.identities();
      const alphaCred = findCredential(beforeIdentities, routeSlots[0].connectionId);
      const bravoCred = findCredential(beforeIdentities, routeSlots[1].connectionId);
      assert.ok(alphaCred && bravoCred, "missing alpha/bravo credentials for shared quota");
      const createBody = {
        operationId: randomUUID(),
        connectionId: routeSlots[1].connectionId,
        secretInput: routeSlots[1].secret,
        accountLabel: "Routing Lab alpha-shared-bravo",
        quotaSharing: { kind: "shared", credentialId: alphaCred.credential.id },
      };
      const created = await api.mutation(`/dashboard/api/v4/identities/${alphaCred.identityId}/credentials`, createBody);
      siblingAccountId = created.accountId;
      assert.ok(siblingAccountId, `create credential returned no accountId: ${JSON.stringify(created)}`);
      const afterIdentities = await api.identities();
      const alphaAfter = findCredential(afterIdentities, routeSlots[0].connectionId);
      const sibling = afterIdentities
        .flatMap((identity) => identity.credentials.map((credential) => ({ identity, credential })))
        .find((row) => row.credential.legacy.id === siblingAccountId);
      assert.ok(sibling, `created sibling account ${siblingAccountId} missing from GET /accounts`);
      assert.equal(sibling.identity.identity.id, alphaCred.identityId, "sibling did not reuse alpha identity");
      assert.equal(
        sibling.credential.quotaPoolId,
        alphaAfter.raw.quotaPoolId,
        `quotaPoolId not shared: sibling=${sibling.credential.quotaPoolId} alpha=${alphaAfter.raw.quotaPoolId}`,
      );
      const siblingBinding = sibling.credential.bindings.find((item) => item.connectionId === routeSlots[1].connectionId);
      assert.ok(siblingBinding, "sibling missing bravo connection binding");
      if (!siblingBinding.allowedEndpointIds?.length || !siblingBinding.allowedOrigins?.length) {
        await api.patchBinding(siblingBinding.id, {
          allowedEndpointIds: bravoCred.binding.allowedEndpointIds,
          allowedOrigins: bravoCred.binding.allowedOrigins,
        });
      }
      await api.setEnabled(siblingAccountId, true);
      await api.setEnabled(routeSlots[1].accountId, false);
      await api.reorder([routeSlots[0].accountId, siblingAccountId, routeSlots[2].accountId]);
      lab.script("alpha", [{ kind: "http", status: 429, body: { error: { message: "Resets in 5 minutes", type: "rate_limit_error" } } }]);
      const mark = lab.snapshot().length;
      const response = await api.chatRoute();
      assert.equal(response.status, 200, response.text.slice(0, 500));
      checkOutput("chat", false, response.text, "LAB_OK_charlie");
      const hits = lab.snapshot().slice(mark);
      assert.ok(!hits.some((hit) => hit.listener === "bravo"), "shared quota sibling was not skipped");
      api.expectHits(label, mark, [
        { listener: "alpha", slot: "alpha" },
        { listener: "charlie", slot: "charlie", valid: true },
      ]);
      collector.pass(label, {
        status: response.status,
        siblingAccountId,
        identityId: alphaCred.identityId,
        sourceCredentialId: alphaCred.credential.id,
        quotaPoolId: sibling.credential.quotaPoolId,
        createBody: { ...createBody, secretInput: "[redacted]" },
        chronological: hits.map((hit) => hit.listener),
        scenarioId: "gw.routing.shared-quota",
        evidenceKind: "gateway_black_box",
      });
    } catch (error) {
      collector.fail(label, error);
    } finally {
      lab.script("alpha", []);
      if (routeSlots[1].accountId) await api.setEnabled(routeSlots[1].accountId, true).catch(() => {});
      await api.resetCooldowns([routeSlots[0].accountId, routeSlots[1].accountId, routeSlots[2].accountId, siblingAccountId].filter(Boolean));
      await api.reorder(routeSlots.map((slot) => slot.accountId)).catch(() => {});
    }
  }

  {
    const label = "sticky-global stays on first success";
    try {
      await api.setRoutingMode("sticky-global", false);
      await api.reorder(routeSlots.map((slot) => slot.accountId));
      const firstMark = lab.snapshot().length;
      const first = await api.chatRoute();
      assert.equal(first.status, 200, first.text.slice(0, 500));
      api.expectHits(`${label} #1`, firstMark, [{ listener: "alpha", valid: true }]);
      const secondMark = lab.snapshot().length;
      const second = await api.chatRoute();
      assert.equal(second.status, 200, second.text.slice(0, 500));
      api.expectHits(`${label} #2`, secondMark, [{ listener: "alpha", valid: true }]);
      collector.pass(label, { first: "alpha", second: "alpha", scenarioId: "gw.routing.sticky", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error);
    } finally {
      await api.setRoutingMode("strict-priority", false);
    }
  }

  {
    const label = "round-robin advances between successes";
    try {
      await api.setRoutingMode("round-robin", false);
      await api.reorder(routeSlots.map((slot) => slot.accountId));
      const firstMark = lab.snapshot().length;
      const first = await api.chatRoute();
      assert.equal(first.status, 200, first.text.slice(0, 500));
      const firstHits = lab.snapshot().slice(firstMark);
      assert.equal(firstHits.length, 1, `RR #1 extra hits: ${JSON.stringify(firstHits.map(summarizeHit))}`);
      const secondMark = lab.snapshot().length;
      const second = await api.chatRoute();
      assert.equal(second.status, 200, second.text.slice(0, 500));
      const secondHits = lab.snapshot().slice(secondMark);
      assert.equal(secondHits.length, 1, `RR #2 extra hits: ${JSON.stringify(secondHits.map(summarizeHit))}`);
      assert.notEqual(secondHits[0].listener, firstHits[0].listener, "round-robin stayed on the same account");
      collector.pass(label, { first: firstHits[0].listener, second: secondHits[0].listener, scenarioId: "gw.routing.rr", evidenceKind: "gateway_black_box" });
    } catch (error) {
      collector.fail(label, error);
    } finally {
      await api.setRoutingMode("strict-priority", false);
    }
  }
}

export async function runCompatibilityScenarios(runtime, collector) {
  await apiPrep(runtime);
  await runProtocolMatrix(runtime, collector, { includeGeminiUpstreams: false });
  await runDirectValidatorNegatives(runtime, collector);
  await runRoutingControlScenarios(runtime, collector);
}

async function apiPrep(runtime) {
  const routeSlots = routeSlotsOf(runtime.started);
  await runtime.api.setRoutingMode("strict-priority", false);
  if (routeSlots.length) await runtime.api.reorder(routeSlots.map((slot) => slot.accountId));
  runtime.lab.reset();
}
