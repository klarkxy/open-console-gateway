import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import path from "node:path";
import { LIVE_KEY_ENV, LIVE_URL_ENV, MARKER, TOOL_MARKER, UTF8_MARKER, newRunId, promptDigest, repoRoot, sha256 } from "./common.mjs";
import { assertGeminiLiveStream, assertLiveRemoteReceipt, inferenceHeaders, input, request, summarizeHit } from "./dashboard.mjs";
import { loadProfile } from "./profile.mjs";
import { createLiveClient, readLiveEnv } from "./live.mjs";
import { applyCoverageChecklist } from "./coverage.mjs";
import { createCollector, exitCodeFor, resultsMarkdown, writeReportDir } from "./report.mjs";
import { defaultCliPath, inspectBinary, installInterruptCleanup } from "./process.mjs";
import { smokeCliHelp, withRuntime } from "./harness.mjs";
import { isolationKey } from "./faults.mjs";
import {
  runDirectValidatorNegatives,
  runProtocolMatrix,
  runRoutingControlScenarios,
  protocolSlotsOf,
  routeSlotsOf,
} from "./scenarios-routing.mjs";
import { parseClientToolCall, runToolHistoryScenarios, toolResultInput } from "./scenarios-tools.mjs";
import {
  beginCooldownRecovery,
  finishCooldownRecovery,
  runAmbiguityAndAlias,
  runAuthIsolation,
  runCatalogRefreshFailure,
  runExplicitUnsupported,
  runGatewayStreamAndUsage,
  runImportExport,
  runKeyRotation,
  runProxyIsolation,
  runQuotaNoDisable,
  runRestartPersistence,
  runRustPriceAndFree,
  runUnpricedAttribution,
} from "./scenarios-gaps.mjs";

function replayVerify(cliPath, suite) {
  return `node tools/gateway-lab/cli.mjs verify --cli "${cliPath}" --suite ${suite}`;
}

async function expectRemoteZero(lab, collector) {
  const stats = lab.stats();
  if (stats.remoteCalls !== 0) {
    collector.fail("local remoteCalls=0", new Error(`remoteCalls=${stats.remoteCalls}`), stats);
  } else {
    collector.pass("local remoteCalls=0", stats);
  }
}

async function runLocalFaultsAndExtras(runtime, collector) {
  const { lab, started, api, gatewayBase } = runtime;
  const protocolSlots = protocolSlotsOf(started);
  const chatSlot = protocolSlots.find((slot) => slot.slot === "chat");
  const responsesSlot = protocolSlots.find((slot) => slot.slot === "responses");
  if (chatSlot) {
    const direct = [
      ["fault delay", { kind: "delay", ms: 50 }],
      ["fault malformed_json", { kind: "malformed_json" }],
      ["fault missing usage", { kind: "missing_usage" }],
    ];
    for (const [label, scripted] of direct) {
      lab.script(chatSlot.listener, [scripted]);
      try {
        const response = await request(chatSlot.listenerUrl, chatSlot.path, "POST", input("chat", chatSlot.model, false), {
          authorization: `Bearer ${chatSlot.secret}`,
        });
        const text = await response.text();
        if (scripted.kind === "malformed_json") {
          assert.equal(text.startsWith("{not-json"), true, text.slice(0, 80));
        } else if (scripted.kind === "missing_usage") {
          const parsed = JSON.parse(text);
          assert.equal(parsed.usage, undefined);
        } else {
          assert.equal(response.status, 200);
        }
        collector.pass(label, { status: response.status });
      } catch (error) {
        collector.fail(label, error);
      } finally {
        lab.script(chatSlot.listener, []);
      }
    }

    lab.script(chatSlot.listener, [{ kind: "sse_split" }]);
    try {
      const response = await request(chatSlot.listenerUrl, chatSlot.path, "POST", input("chat", chatSlot.model, true), {
        authorization: `Bearer ${chatSlot.secret}`,
      });
      const text = await response.text();
      assert.match(response.headers.get("content-type") || "", /text\/event-stream/);
      assert.match(text, /\[DONE\]/);
      collector.pass("fault sse_split", { bytes: text.length });
    } catch (error) {
      collector.fail("fault sse_split", error);
    } finally {
      lab.script(chatSlot.listener, []);
    }

    lab.script(chatSlot.listener, [{ kind: "missing_end" }]);
    try {
      const response = await request(chatSlot.listenerUrl, chatSlot.path, "POST", input("chat", chatSlot.model, true), {
        authorization: `Bearer ${chatSlot.secret}`,
      });
      const text = await response.text();
      assert.equal(text.includes("[DONE]"), false);
      collector.pass("fault missing end", {});
    } catch (error) {
      collector.fail("fault missing end", error);
    } finally {
      lab.script(chatSlot.listener, []);
    }

    lab.script(chatSlot.listener, [{ kind: "stream_interrupt" }]);
    try {
      let interrupted = false;
      let text = "";
      try {
        const response = await request(chatSlot.listenerUrl, chatSlot.path, "POST", input("chat", chatSlot.model, true), {
          authorization: `Bearer ${chatSlot.secret}`,
        });
        text = await response.text();
      } catch {
        interrupted = true;
      }
      assert.ok(interrupted || !text.includes("[DONE]"), "stream interrupt still completed");
      collector.pass("fault stream_interrupt", {});
    } catch (error) {
      collector.fail("fault stream_interrupt", error);
    } finally {
      lab.script(chatSlot.listener, []);
    }

    lab.script(chatSlot.listener, [{ kind: "drop" }]);
    try {
      let dropped = false;
      try {
        await request(chatSlot.listenerUrl, chatSlot.path, "POST", input("chat", chatSlot.model, false), {
          authorization: `Bearer ${chatSlot.secret}`,
        });
      } catch {
        dropped = true;
      }
      assert.equal(dropped, true);
      collector.pass("fault drop", {});
    } catch (error) {
      collector.fail("fault drop", error);
    } finally {
      lab.script(chatSlot.listener, []);
    }

    lab.applyScenario("fail_then_success", { listener: chatSlot.listener });
    try {
      const first = await request(chatSlot.listenerUrl, chatSlot.path, "POST", input("chat", chatSlot.model, false), {
        authorization: `Bearer ${chatSlot.secret}`,
      });
      assert.equal(first.status, 500);
      const second = await request(chatSlot.listenerUrl, chatSlot.path, "POST", input("chat", chatSlot.model, false), {
        authorization: `Bearer ${chatSlot.secret}`,
      });
      assert.equal(second.status, 200);
      collector.pass("fault fail_then_success", { first: first.status, second: second.status });
    } catch (error) {
      collector.fail("fault fail_then_success", error);
    } finally {
      lab.script(chatSlot.listener, []);
    }

    try {
      const body = {
        model: chatSlot.model,
        stream: true,
        messages: [{ role: "user", content: `${MARKER} ${UTF8_MARKER}` }],
      };
      const response = await request(chatSlot.listenerUrl, chatSlot.path, "POST", body, {
        authorization: `Bearer ${chatSlot.secret}`,
      });
      const text = await response.text();
      assert.ok(text.includes("\u2603"), "utf-8 snowman missing from SSE");
      collector.pass("stream utf-8", {});
    } catch (error) {
      collector.fail("stream utf-8", error);
    }

    lab.script(chatSlot.listener, [{ kind: "delay", ms: 2000 }]);
    try {
      let timedOut = false;
      try {
        await fetch(`${chatSlot.listenerUrl}${chatSlot.path}`, {
          method: "POST",
          headers: { "content-type": "application/json", authorization: `Bearer ${chatSlot.secret}` },
          body: JSON.stringify(input("chat", chatSlot.model, false)),
          signal: AbortSignal.timeout(80),
        });
      } catch {
        timedOut = true;
      }
      assert.equal(timedOut, true);
      collector.pass("local timeout", {});
    } catch (error) {
      collector.fail("local timeout", error);
    } finally {
      lab.script(chatSlot.listener, []);
    }

    try {
      const ac = new AbortController();
      const pending = fetch(`${chatSlot.listenerUrl}${chatSlot.path}`, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${chatSlot.secret}` },
        body: JSON.stringify(input("chat", chatSlot.model, true)),
        signal: ac.signal,
      });
      ac.abort();
      let cancelled = false;
      try {
        await pending;
      } catch {
        cancelled = true;
      }
      assert.equal(cancelled, true);
      collector.pass("local client cancel", {});
    } catch (error) {
      collector.fail("local client cancel", error);
    }
  }

  if (chatSlot && responsesSlot) {
    const label = "fault isolation chat vs responses";
    lab.scriptIsolation(
      { endpointId: chatSlot.id || chatSlot.slot, keyFingerprint: sha256(chatSlot.secret), model: chatSlot.model, scenario: "" },
      [{ kind: "http", status: 429, body: { error: { message: "isolated", type: "rate_limit_error" } } }],
    );
    try {
      const blocked = await request(chatSlot.listenerUrl, chatSlot.path, "POST", input("chat", chatSlot.model, false), {
        authorization: `Bearer ${chatSlot.secret}`,
      });
      assert.equal(blocked.status, 429);
      const other = await request(responsesSlot.listenerUrl, responsesSlot.path, "POST", input("responses", responsesSlot.model, false), {
        authorization: `Bearer ${responsesSlot.secret}`,
      });
      assert.equal(other.status, 200);
      collector.pass(label, {
        chat: 429,
        responses: 200,
        scenarioId: "lab.auth.isolation",
        evidenceKind: "lab_fixture",
        isolation: isolationKey({ endpointId: chatSlot.slot, keyFingerprint: sha256(chatSlot.secret), model: chatSlot.model }),
      });
    } catch (error) {
      collector.fail(label, error);
    } finally {
      lab.reset();
    }
  }

  try {
    const cas = await api.json("/dashboard/api/v4/contract");
    const response = await request(gatewayBase, "/dashboard/api/v4/settings", "PUT", {
      routingMode: "strict-priority",
      conversationSticky: false,
      expectedRevision: 0,
      processGeneration: cas.processGeneration,
    });
    const text = await response.text();
    assert.equal(response.status, 409, text.slice(0, 300));
    collector.pass("CAS conflict", { status: 409, scenarioId: "gw.cas.conflict", evidenceKind: "gateway_black_box" });
  } catch (error) {
    collector.fail("CAS conflict", error);
  }

  try {
    const chat = protocolSlotsOf(started).find((slot) => slot.slot === "chat");
    const operationId = randomUUID();
    const payload = {
      mode: "complete",
      operationId,
      connection: {
        kind: "new",
        templateId: "custom-http",
        name: `Gateway Lab idempotency ${operationId.slice(0, 8)}`,
        endpointUrl: chat.url,
        upstreamProtocol: chat.protocol,
        authKind: chat.auth,
      },
      authorization: { kind: "api_key", secretInput: chat.secret, accountLabel: "idempotent" },
      targets: [{ publicModel: `lab-idem-${operationId.slice(0, 8)}`, upstreamModel: chat.model }],
    };
    const first = await api.mutation("/dashboard/api/v4/onboarding/commit", payload);
    const second = await api.mutation("/dashboard/api/v4/onboarding/commit", payload);
    assert.equal(second.replayed, true);
    assert.equal(second.connectionId, first.connectionId);
    collector.pass("onboarding idempotency", { operationId, scenarioId: "gw.cas.idempotency", evidenceKind: "gateway_black_box" });
  } catch (error) {
    collector.fail("onboarding idempotency", error);
  }
}

async function runLocalSuite(runtime, collector) {
  const { lab, started, api, cliPath, logDir } = runtime;
  await smokeCliHelp(cliPath, logDir).then((pid) => collector.pass("cli --help smoke", { pid }));

  try {
    const health = await fetch(`${lab.runtime().control.url}/health`, { signal: AbortSignal.timeout(3000) });
    const body = await health.json();
    assert.equal(health.status, 200);
    assert.equal(body.ok, true);
    collector.pass("control plane health", { control: lab.runtime().control.url });
  } catch (error) {
    collector.fail("control plane health", error);
  }

  const protocolSlots = protocolSlotsOf(started);
  const routeSlots = routeSlotsOf(started);

  for (const slot of protocolSlots) {
    const label = `models catalog ${slot.slot}`;
    try {
      const headers = slot.auth === "bearer" ? { authorization: `Bearer ${slot.secret}` } : { "x-api-key": slot.secret, "anthropic-version": "2023-06-01" };
      const response = await request(slot.listenerUrl, slot.modelsPath, "GET", undefined, headers);
      const parsed = await response.json();
      assert.equal(response.status, 200, `${label} ${response.status}`);
      const ids = (parsed.data || []).map((item) => item.id);
      assert.ok(ids.includes(slot.model), `${label} missing ${slot.model}: ${ids}`);
      collector.pass(label, { ids, scenarioId: `lab.models.${slot.slot}`, evidenceKind: "lab_fixture" });
    } catch (error) {
      collector.fail(label, error);
    }
  }

  {
    const slot = protocolSlots[0];
    const label = "models reject wrong Key";
    try {
      const response = await request(slot.listenerUrl, slot.modelsPath, "GET", undefined, { authorization: "Bearer wrong" });
      await response.text();
      assert.equal(response.status, 401);
      collector.pass(label, { status: 401, scenarioId: "lab.models.reject-wrong-key", evidenceKind: "lab_fixture" });
    } catch (error) {
      collector.fail(label, error);
    }
  }

  await api.setRoutingMode("strict-priority", false);
  if (routeSlots.length) await api.reorder(routeSlots.map((slot) => slot.accountId));
  lab.reset();

  await runProtocolMatrix(runtime, collector, { includeGeminiUpstreams: true });
  await runGatewayStreamAndUsage(runtime, collector);
  await runDirectValidatorNegatives(runtime, collector);
  await runRoutingControlScenarios(runtime, collector);
  const cooldown = await beginCooldownRecovery(runtime);
  await runToolHistoryScenarios(runtime, collector);
  await runLocalFaultsAndExtras(runtime, collector);
  await runAmbiguityAndAlias(runtime, collector);
  await runKeyRotation(runtime, collector);
  await runCatalogRefreshFailure(runtime, collector);
  await runUnpricedAttribution(runtime, collector);
  await runQuotaNoDisable(runtime, collector);
  await runProxyIsolation(runtime, collector);
  await runExplicitUnsupported(runtime, collector);
  await runAuthIsolation(runtime, collector);
  await runImportExport(runtime, collector);
  await runRestartPersistence(runtime, collector);
  await runRustPriceAndFree(collector);
  await finishCooldownRecovery(runtime, collector, cooldown);
  await expectRemoteZero(lab, collector);
  applyCoverageChecklist(collector);
}

async function liveToolRoundTrip(runtime, collector, live, client, target) {
  const { gatewayBase, gatewayKey, lab } = runtime;
  const label = `live tool round-trip ${client} -> ${target.slot}`;
  const before = live.stats().calls;
  const mark = lab.snapshot().length;
  try {
    const body =
      client === "chat"
        ? {
            model: target.publicModel,
            stream: false,
            max_tokens: 64,
            messages: [{ role: "user", content: `${MARKER} ${TOOL_MARKER}` }],
            tools: [{ type: "function", function: { name: "lab_echo", description: "Echo a value", parameters: { type: "object", properties: { value: { type: "string" } }, required: ["value"] } } }],
          }
        : client === "responses"
          ? {
              model: target.publicModel,
              stream: false,
              store: false,
              max_output_tokens: 64,
              input: [{ role: "user", content: [{ type: "input_text", text: `${MARKER} ${TOOL_MARKER}` }] }],
              tools: [{ type: "function", name: "lab_echo", description: "Echo a value", parameters: { type: "object", properties: { value: { type: "string" } } } }],
            }
          : {
              model: target.publicModel,
              stream: false,
              max_tokens: 64,
              messages: [{ role: "user", content: [{ type: "text", text: `${MARKER} ${TOOL_MARKER}` }] }],
              tools: [{ name: "lab_echo", description: "Echo a value", input_schema: { type: "object", properties: { value: { type: "string" } } } }],
            };
    const pathName = client === "chat" ? "/v1/chat/completions" : client === "responses" ? "/v1/responses" : "/v1/messages";
    const response = await request(gatewayBase, pathName, "POST", body, inferenceHeaders(client, gatewayKey), 60000);
    const text = await response.text();
    if (!response.ok) throw new Error(`${response.status} ${text.slice(0, 400)}`);
    const parsed = JSON.parse(text);
    const tool =
      parsed.choices?.[0]?.message?.tool_calls?.length ||
      (parsed.output || []).some((item) => item.type === "function_call") ||
      (parsed.content || []).some((part) => part.type === "tool_use");
    if (!tool) {
      collector.unsupported(label, "remote did not emit a tool call for the fixed sample");
      return;
    }
    const call = parseClientToolCall(client, false, text);
    const second = await request(gatewayBase, pathName, "POST", toolResultInput(client, target.publicModel, call), inferenceHeaders(client, gatewayKey), 60000);
    const secondText = await second.text();
    if (!second.ok) throw new Error(`tool result ${second.status} ${secondText.slice(0, 400)}`);
    JSON.parse(secondText);
    const hits = lab.snapshot().slice(mark);
    assert.ok(hits.length >= 2, `${label}: expected a result round-trip, hits=${hits.length}`);
    assert.equal(hits[1].hasToolResult, true);
    collector.pass(label, {
      remoteCalls: live.stats().calls - before,
      actualHits: hits.map(summarizeHit),
      callId: call.id,
      promptDigest: promptDigest(MARKER),
    });
  } catch (error) {
    collector.fail(label, error);
  }
}

async function runLiveSuite(runtime, collector, live) {
  const { lab, started, gatewayBase, gatewayKey } = runtime;
  const protocolSlots = protocolSlotsOf(started);
  lab.reset();

  const cases = [
    ["live chat JSON", "chat", "chat", false],
    ["live chat SSE", "chat", "chat", true],
    ["live responses JSON", "responses", "responses", false],
    ["live responses SSE", "responses", "responses", true],
    ["live messages JSON", "messages", "messages", false],
    ["live messages SSE", "messages", "messages", true],
  ];
  for (const [label, client, slotName, stream] of cases) {
    const target = protocolSlots.find((slot) => slot.slot === slotName);
    const mark = lab.snapshot().length;
    const before = live.stats().calls;
    try {
      const response = await request(
        gatewayBase,
        client === "chat" ? "/v1/chat/completions" : client === "responses" ? "/v1/responses" : "/v1/messages",
        "POST",
        input(client, target.publicModel, stream),
        inferenceHeaders(client, gatewayKey),
        60000,
      );
      const body = await response.text();
      if (!response.ok) throw new Error(`${label}: ${response.status} ${body.slice(0, 400)}`);
      if (stream) {
        assert.match(response.headers.get("content-type") || "", /text\/event-stream/);
        if (client === "chat") assert.ok(body.includes("[DONE]"), "live Chat SSE missing [DONE]");
        if (client === "responses") {
          assert.ok(body.includes("response.completed") || body.includes("response.incomplete"), "live Responses SSE missing terminal");
        }
        if (client === "messages") assert.ok(body.includes("message_stop"), "live Messages SSE missing message_stop");
      } else {
        JSON.parse(body);
      }
      const after = live.stats().calls;
      assert.equal(after - before, 1, `${label}: expected exactly one remote call, got ${after - before}`);
      const hits = lab.snapshot().slice(mark);
      assert.equal(hits.length, 1, `${label}: expected exactly one receipt, got ${hits.length}`);
      assert.equal(hits[0].liveStatus, 200, `${label}: liveStatus=${hits[0].liveStatus}`);
      collector.pass(label, {
        status: response.status,
        remoteCalls: after - before,
        expectedHits: 1,
        actualHits: hits.map(summarizeHit),
        liveStatus: hits[0].liveStatus,
        promptDigest: promptDigest(MARKER),
      });
    } catch (error) {
      collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit), remoteCalls: live.stats().calls - before });
    }
  }

  for (const slotName of ["chat", "responses", "messages"]) {
    const target = protocolSlots.find((slot) => slot.slot === slotName);
    await liveToolRoundTrip(runtime, collector, live, slotName === "messages" ? "messages" : slotName, target);
  }

  {
    const label = "live gemini -> chat JSON";
    const target = protocolSlots.find((slot) => slot.slot === "chat");
    const before = live.stats().calls;
    try {
      const response = await request(
        gatewayBase,
        `/v1beta/models/${target.publicModel}:generateContent`,
        "POST",
        input("gemini", target.publicModel, false),
        { "x-goog-api-key": gatewayKey },
        60000,
      );
      const body = await response.text();
      if (response.status === 404 || /not (found|supported)/i.test(body)) {
        collector.unsupported(label, "gemini client path not accepted by this binary");
      } else if (!response.ok) {
        throw new Error(`${response.status} ${body.slice(0, 400)}`);
      } else {
        JSON.parse(body);
        assert.ok(live.stats().calls > before, "gemini live made no remote call");
        collector.pass(label, { remoteCalls: live.stats().calls - before, promptDigest: promptDigest(MARKER) });
      }
    } catch (error) {
      collector.fail(label, error);
    }
  }

  {
    const label = "live gemini -> chat SSE terminal";
    const target = protocolSlots.find((slot) => slot.slot === "chat");
    const mark = lab.snapshot().length;
    const before = live.stats().calls;
    try {
      const response = await request(
        gatewayBase,
        `/v1beta/models/${target.publicModel}:streamGenerateContent`,
        "POST",
        input("gemini", target.publicModel, true),
        { "x-goog-api-key": gatewayKey },
        60000,
      );
      const body = await response.text();
      if (response.status === 404 || /not (found|supported)/i.test(body)) {
        collector.unsupported(label, "gemini stream path not accepted by this binary");
      } else if (!response.ok) {
        throw new Error(`${response.status} ${body.slice(0, 400)}`);
      } else {
        assert.match(response.headers.get("content-type") || "", /text\/event-stream|application\/json/);
        assertGeminiLiveStream(body);
        const hits = assertLiveRemoteReceipt(lab, live, mark, before);
        collector.pass(label, {
          status: response.status,
          remoteCalls: 1,
          expectedHits: 1,
          actualHits: hits.map(summarizeHit),
          liveStatus: hits[0].liveStatus,
          promptDigest: promptDigest(MARKER),
        });
      }
    } catch (error) {
      collector.fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit), remoteCalls: live.stats().calls - before });
    }
  }

  {
    const label = "live multi-turn context";
    const target = protocolSlots.find((slot) => slot.slot === "chat");
    const before = live.stats().calls;
    try {
      const first = await request(
        gatewayBase,
        "/v1/chat/completions",
        "POST",
        input("chat", target.publicModel, false),
        inferenceHeaders("chat", gatewayKey),
        60000,
      );
      const firstText = await first.text();
      if (!first.ok) throw new Error(`turn1 ${first.status} ${firstText.slice(0, 300)}`);
      const firstJson = JSON.parse(firstText);
      const second = await request(
        gatewayBase,
        "/v1/chat/completions",
        "POST",
        {
          model: target.publicModel,
          stream: false,
          max_tokens: 64,
          messages: [
            { role: "user", content: MARKER },
            firstJson.choices?.[0]?.message || { role: "assistant", content: "ok" },
            { role: "user", content: MARKER },
          ],
        },
        inferenceHeaders("chat", gatewayKey),
        60000,
      );
      const secondText = await second.text();
      if (!second.ok) throw new Error(`turn2 ${second.status} ${secondText.slice(0, 300)}`);
      JSON.parse(secondText);
      assert.ok(live.stats().calls - before >= 2, "multi-turn did not make two remote calls");
      collector.pass(label, { remoteCalls: live.stats().calls - before, promptDigest: promptDigest(MARKER) });
    } catch (error) {
      collector.fail(label, error);
    }
  }

  {
    const label = "live client cancel aborts remote";
    const target = protocolSlots.find((slot) => slot.slot === "chat");
    const beforeAbort = live.stats().aborted;
    try {
      const ac = new AbortController();
      const pending = fetch(`${gatewayBase}/v1/chat/completions`, {
        method: "POST",
        headers: { "content-type": "application/json", ...inferenceHeaders("chat", gatewayKey) },
        body: JSON.stringify(input("chat", target.publicModel, true)),
        signal: ac.signal,
      });
      await new Promise((resolve) => setTimeout(resolve, 200));
      ac.abort();
      let aborted = false;
      try {
        await pending;
      } catch {
        aborted = true;
      }
      assert.equal(aborted, true);
      await new Promise((resolve) => setTimeout(resolve, 200));
      const afterAbort = live.stats().aborted;
      if (afterAbort > beforeAbort) collector.pass(label, { aborted: afterAbort - beforeAbort });
      else collector.unsupported(label, "remote abort was not observed before the request finished");
    } catch (error) {
      collector.fail(label, error);
    }
  }
}

export async function runVerify({ cli, suite = "local", profile = "default", shadow = false } = {}) {
  const suiteName = suite || "local";
  if (!["local", "live", "all"].includes(suiteName)) throw new Error(`unknown suite ${suiteName}`);
  const cliPath = path.resolve(cli || process.env.OCG_GATEWAY_LAB_CLI || defaultCliPath());
  const runId = newRunId();
  const artifactDir = path.join(repoRoot(), ".artifacts", "gateway-lab", runId);
  const collector = createCollector();
  const startedAt = new Date().toISOString();
  const loaded = await loadProfile(profile);
  let liveBlocked = false;
  let liveClient = null;
  let runtime = null;
  let cleanupFn = async () => {};
  const uninstallInterrupt = installInterruptCleanup(() => cleanupFn());
  let binary = existsSync(cliPath) ? await inspectBinary(cliPath) : null;

  if (suiteName === "live" || suiteName === "all") {
    const env = readLiveEnv();
    if (!env.present) {
      liveBlocked = true;
      collector.notRun("live environment", `missing ${[!env.urlPresent ? LIVE_URL_ENV : null, !env.keyPresent ? LIVE_KEY_ENV : null].filter(Boolean).join(" and ")}`);
      if (suiteName === "live") {
        const counts = collector.counts();
        const report = {
          generatedAt: new Date().toISOString(),
          startedAt,
          runId,
          suite: suiteName,
          counts,
          results: collector.results,
          replay: replayVerify(cliPath, suiteName),
          liveBlocked: true,
        };
        const code = exitCodeFor(counts, { liveBlocked: true });
        process.exitCode = code;
        try {
          await writeReportDir(artifactDir, {
            "report.json": report,
            "RESULTS.md": resultsMarkdown(report),
          });
        } finally {
          uninstallInterrupt();
        }
        return report;
      }
    } else {
      liveClient = createLiveClient({
        url: env.url,
        key: env.key,
        model: loaded.live.model,
        maxTokens: loaded.live.maxTokens,
        timeoutMs: loaded.live.timeoutMs,
        maxCalls: loaded.live.maxCallsPerVerify,
      });
    }
  }

  try {
    runtime = await withRuntime({
      cliPath,
      profile: loaded,
      artifactDir,
      shadow,
      live: suiteName === "local" ? null : liveClient,
      runId,
      onCleanup(fn) {
        cleanupFn = fn;
      },
    });
    binary = runtime.binary;
    cleanupFn = runtime.cleanup;
    let localRemoteCalls = null;
    if (suiteName === "local" || suiteName === "all") {
      await runLocalSuite(runtime, collector);
      localRemoteCalls = runtime.lab.stats().remoteCalls;
    }
    if ((suiteName === "live" || suiteName === "all") && liveClient && !liveBlocked) {
      runtime.lab.armLive(true);
      runtime.lab.reset();
      await runLiveSuite(runtime, collector, liveClient);
    }
    const proof = await runtime.cleanup();
    if (!proof.verified) collector.fail("cleanup verification", new Error(JSON.stringify(proof)), proof);
    else collector.pass("cleanup verification", proof);

    const counts = collector.counts();
    const stats = runtime.lab.stats();
    const report = {
      generatedAt: new Date().toISOString(),
      startedAt,
      runId,
      suite: suiteName,
      evidenceClass: binary?.evidenceClass,
      binary,
      replay: replayVerify(cliPath, suiteName),
      shadow,
      gateway: { pid: runtime.gatewayPid, url: runtime.gatewayBase, port: runtime.gatewayPort, host: "127.0.0.1" },
      control: runtime.lab.runtime().control,
      listeners: runtime.started.listeners,
      dataDir: runtime.dataDir,
      remoteCalls: stats.remoteCalls,
      localRemoteCalls,
      truncated: runtime.lab.truncated,
      cleanup: proof,
      counts,
      passed: counts.PASS,
      failed: counts.FAIL,
      results: collector.results,
      liveBlocked,
    };
    await writeReportDir(artifactDir, {
      "runtime.json": { ...runtime.lab.runtime(), binary, gateway: report.gateway },
      "journal.json": { requests: runtime.lab.snapshot(), truncated: runtime.lab.truncated, nextSequence: stats.nextSequence },
      "cleanup.json": { generatedAt: new Date().toISOString(), ...proof },
      "report.json": report,
      "RESULTS.md": resultsMarkdown(report),
    });
    uninstallInterrupt();
    process.exitCode = exitCodeFor(counts, { liveBlocked });
    console.log(`report: ${path.join(artifactDir, "report.json")}`);
    return report;
  } catch (error) {
    collector.fail("orchestrator", error);
    const counts = collector.counts();
    const report = {
      generatedAt: new Date().toISOString(),
      startedAt,
      runId,
      suite: suiteName,
      evidenceClass: binary?.evidenceClass ?? "unknown",
      binary,
      replay: replayVerify(cliPath, suiteName),
      counts,
      results: collector.results,
      fatal: error instanceof Error ? error.message : String(error),
      liveBlocked,
      cleanup: error.cleanup,
    };
    if (runtime) {
      report.cleanup = await runtime.cleanup().catch((cleanupError) => ({ verified: false, error: String(cleanupError) }));
      report.remoteCalls = runtime.lab.stats().remoteCalls;
    }
    await writeReportDir(artifactDir, {
      "report.json": report,
      "RESULTS.md": resultsMarkdown(report),
    }).catch(() => {});
    uninstallInterrupt();
    process.exitCode = 1;
    console.error(error);
    return report;
  }
}
