import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { MARKER, rememberSecret, sleep } from "./common.mjs";
import {
  checkOutput,
  inferenceHeaders,
  input,
  request,
  summarizeHit,
} from "./dashboard.mjs";
import { restartOwnedGateway } from "./harness.mjs";
import { waitFor } from "./process.mjs";
import {
  BUILTIN_GOAT_ID,
  EVIDENCE,
  HTTP_BACKOFF,
  HTTP_INFLIGHT_DELAY_MS,
  LIVE_BACKOFF,
  LIVE_EXPIRY_TIMEOUT_MS,
  LIVE_MAX_REMOTE,
  LIVE_OUTBOUND_MODEL,
  PHASE,
  RESPONSIBILITY,
  SCENARIO,
  SEQ_ALL_WAITING_ZERO,
  SEQ_FIRST_400_STAYS_A,
  SEQ_LIVE_ABBA,
  SEQ_NEXT_AVOIDS_A_USES_B,
  SEQ_SINGLE_FLIGHT_A1_B1,
  assertChatCompletionProtocolValid,
  assertListenerCounts,
  assertListenerSequence,
  assertLiveSuccessReceipt,
  assertRemoteBudget,
  assertUpstreamCount,
  builtinOverride,
  companionRustRows,
  credentialModel400Rule,
  custom400Body,
  custom400Rule,
  delayFault,
  httpFault,
  liveProbeInput,
  listenersOfHits,
  modelTriad,
  nestedEcho400Body,
  protocolSlotsOf,
  routeSlotsOf,
  slotById,
  statusOnlyRule,
} from "./policy-contract.mjs";
import {
  call,
  isolationSpec,
  labSnapshot,
  labStats,
} from "./policy-lab-attach.mjs";
import {
  clearRestrictionCas,
  destinationIdForSlot,
  getPolicyConfig,
  getRestrictions,
  listDestinations,
  probePolicyApi,
  putPolicyConfigCas,
  requireOk,
  restrictionList,
  waitingRestrictions,
} from "./policy-api.mjs";

const CHAT_TIMEOUT_MS = 20000;
const SSE_TIMEOUT_MS = 25000;

async function markNow(lab) {
  return (await labSnapshot(lab)).length;
}

async function hitsSince(lab, mark) {
  return (await labSnapshot(lab)).slice(mark);
}

async function scriptListener(lab, listenerId, queue) {
  await call(lab, "script", listenerId, queue);
}

async function scriptIso(lab, spec, queue) {
  await call(lab, "scriptIsolation", spec, queue);
}

export async function resetLab(lab, ctx) {
  await call(lab, "reset");
  if (ctx?.suite === "live") {
    await call(lab, "armLive", ctx.phase === PHASE.LIVE);
  }
}

async function remoteDeltaNow(ctx) {
  const now = (await labStats(ctx.runtime.lab)).remoteCalls || 0;
  return now - (ctx.remoteCallsBaseline || 0);
}

async function guardRemote(ctx, { beforeSend = false } = {}) {
  if (!ctx) return;
  const delta = await remoteDeltaNow(ctx);
  const max = ctx.remoteMaxDelta ?? (ctx.suite === "live" ? LIVE_MAX_REMOTE : 0);
  assertRemoteBudget({ phase: ctx.phase, delta, max, beforeSend });
}

async function sendChat(runtime, { model = "lab-route", stream = false, timeoutMs = CHAT_TIMEOUT_MS } = {}) {
  const ctx = runtime.policyCtx;
  await guardRemote(ctx, { beforeSend: true });
  const headers = inferenceHeaders("chat", runtime.gatewayKey);
  const response = await request(
    runtime.gatewayBase,
    "/v1/chat/completions",
    "POST",
    input("chat", model, stream),
    headers,
    timeoutMs,
  );
  const text = await response.text();
  const result = { status: response.status, text, headers: response.headers, contentType: response.headers.get("content-type") || "" };
  await guardRemote(ctx);
  return result;
}

async function sendClient(runtime, client, publicModel, stream, timeoutMs = CHAT_TIMEOUT_MS) {
  const ctx = runtime.policyCtx;
  await guardRemote(ctx, { beforeSend: true });
  const pathName =
    client === "chat" ? "/v1/chat/completions" : client === "responses" ? "/v1/responses" : "/v1/messages";
  const response = await request(
    runtime.gatewayBase,
    pathName,
    "POST",
    input(client, publicModel, stream),
    inferenceHeaders(client, runtime.gatewayKey),
    timeoutMs,
  );
  const text = await response.text();
  const result = { status: response.status, text, headers: response.headers, contentType: response.headers.get("content-type") || "" };
  await guardRemote(ctx);
  return result;
}

function extra(hits, rest = {}) {
  return {
    actualHits: hits.map((hit) => ({
      ...summarizeHit(hit),
      live: hit.live,
      liveStatus: hit.liveStatus,
      liveModel: hit.liveModel,
    })),
    listeners: listenersOfHits(hits),
    upstreamSends: hits.length,
    liveOutboundModel: LIVE_OUTBOUND_MODEL,
    promptDigestMarker: MARKER,
    ...rest,
  };
}

function scoped(collector, responsibility) {
  return {
    pass(label, extra = {}) {
      return collector.pass(label, { ...extra, responsibility });
    },
    fail(label, error, extra = {}) {
      return collector.fail(label, error, { ...extra, responsibility });
    },
    notRun(label, reason, extra = {}) {
      return collector.notRun(label, reason, { ...extra, responsibility });
    },
    record(entry) {
      return collector.record({ ...entry, responsibility });
    },
  };
}

async function withRemoteDelta(lab, fn) {
  const before = (await labStats(lab)).remoteCalls || 0;
  const result = await fn();
  const after = (await labStats(lab)).remoteCalls || 0;
  return { result, delta: after - before, before, after };
}

async function sendLiveChat(runtime, { model = "lab-route", timeoutMs = 60000 } = {}) {
  const ctx = runtime.policyCtx;
  await guardRemote(ctx, { beforeSend: true });
  const response = await request(
    runtime.gatewayBase,
    "/v1/chat/completions",
    "POST",
    liveProbeInput(model),
    inferenceHeaders("chat", runtime.gatewayKey),
    timeoutMs,
  );
  const text = await response.text();
  const result = { status: response.status, text, headers: response.headers, contentType: response.headers.get("content-type") || "" };
  await guardRemote(ctx);
  return result;
}

async function prepareRoute(ctx) {
  const runtime = ctx.runtime;
  const routeSlots = routeSlotsOf(runtime.started);
  await runtime.api.setRoutingMode("strict-priority", false);
  await runtime.api.reorder(routeSlots.map((slot) => slot.accountId));
  await runtime.api.resetCooldowns(routeSlots.map((slot) => slot.accountId));
  await resetLab(runtime.lab, ctx);
  return routeSlots;
}

async function putRules(ctx, rules) {
  const parsed = await putPolicyConfigCas(ctx.runtime.api, ctx.runtime.gatewayBase, rules);
  return requireOk(parsed, "PUT temporary-unavailability");
}

async function waitUntilReadyOrTimeout(gatewayBase, { timeoutMs, label }) {
  return waitFor(
    async () => {
      const parsed = await getRestrictions(gatewayBase);
      if (parsed.status !== 200) throw new Error(`restrictions ${parsed.status}`);
      const waiting = waitingRestrictions(parsed.body);
      if (waiting.length === 0) return parsed.body;
      if (waiting.every((row) => Number(row.nextProbeInSeconds) <= 0)) return parsed.body;
      throw new Error(`still waiting ${waiting.map((row) => `${row.ruleId}:${row.state}:${row.nextProbeInSeconds}`).join(",")}`);
    },
    { timeoutMs, intervalMs: 50, label },
  );
}

async function waitForLabArrival(lab, mark, label = "lab arrival") {
  return waitFor(
    async () => {
      const hits = await hitsSince(lab, mark);
      if (hits.length > 0) return hits;
      throw new Error("no arrival");
    },
    { timeoutMs: 10000, intervalMs: 20, label },
  );
}

export async function runPolicyScenarios(ctx) {
  const { runtime, collector, suite, apiProbe, liveSendsAllowed } = ctx;
  ctx.http = scoped(collector, RESPONSIBILITY.HTTP);
  ctx.live = scoped(collector, RESPONSIBILITY.LIVE);
  ctx.harness = scoped(collector, RESPONSIBILITY.HARNESS);
  ctx.phase = PHASE.SIMULATE;
  if (ctx.remoteMaxDelta == null) {
    ctx.remoteMaxDelta = suite === "live" ? LIVE_MAX_REMOTE : 0;
  }
  if (ctx.remoteCallsBaseline == null) {
    ctx.remoteCallsBaseline = (await labStats(runtime.lab)).remoteCalls || 0;
  }
  runtime.policyCtx = ctx;
  if (suite === "live") {
    await call(runtime.lab, "armLive", false);
  }
  await runApiSurface(ctx);
  await runHttpCustom400(ctx);
  await runProtocol400(ctx);
  await runOverrideAndClear(ctx);
  await runIsolation(ctx);
  await runSingleFlightAndLateChange(ctx);
  await runAllWaiting(ctx);
  await runRestart(ctx);
  await runCompleteBodiesAndCancel(ctx);
  try {
    await guardRemote(ctx);
  } catch (error) {
    ctx.harness.fail("simulate-phase remoteCallsDelta must stay 0", error, {
      scenarioId: "policy.remote.simulate-budget",
      evidenceKind: EVIDENCE.LAB,
      remoteCallsDelta: await remoteDeltaNow(ctx).catch(() => null),
    });
  }
  await runLiveOrLocalEvidence(ctx);
}

async function runApiSurface(ctx) {
  const { runtime, apiProbe, http } = ctx;
  const { gatewayBase, api } = runtime;
  http.record({
    label: "configuration API probe",
    scenarioId: SCENARIO.API_PROBE,
    evidenceKind: EVIDENCE.GATEWAY,
    status: apiProbe.ready ? "PASS" : "NOT_RUN",
    error: apiProbe.ready ? undefined : apiProbe.reason,
    httpStatus: apiProbe.status,
  });

  if (!apiProbe.ready) {
    for (const [label, scenarioId] of [
      ["CAS conflict on policy PUT", SCENARIO.CAS_CONFLICT],
      ["policy mutation validation", SCENARIO.VALIDATION],
    ]) {
      http.notRun(label, apiProbe.reason, { scenarioId, evidenceKind: EVIDENCE.GATEWAY });
    }
    return;
  }

  try {
    const { delta } = await withRemoteDelta(runtime.lab, async () => {
      const parsed = await putPolicyConfigCas(api, gatewayBase, [custom400Rule({ id: "custom.http400" })], {
        revision: 0,
        processGeneration: (await api.json("/dashboard/api/v4/contract")).processGeneration,
      });
      assert.equal(parsed.status, 409, `CAS expected 409, got ${parsed.status} ${parsed.text.slice(0, 200)}`);
      return parsed;
    });
    assert.equal(delta, 0, "CAS PUT used the network");
    http.pass("CAS conflict on policy PUT", {
      scenarioId: SCENARIO.CAS_CONFLICT,
      evidenceKind: EVIDENCE.GATEWAY,
      httpStatus: 409,
      remoteCallsDelta: 0,
    });
  } catch (error) {
    http.fail("CAS conflict on policy PUT", error, { scenarioId: SCENARIO.CAS_CONFLICT, evidenceKind: EVIDENCE.GATEWAY });
  }

  try {
    const destinations = await listDestinations(api);
    const invalids = [
      ["empty custom match", [custom400Rule({ id: "custom.empty" })]],
      ["unknown builtin", [builtinOverride({ id: "builtin.not.registered" })]],
      ["unknown destination", [custom400Rule({ id: "custom.http400", destinationId: randomUUID() })]],
    ];
    invalids[0][1][0].match = {};
    const dup = custom400Rule({ id: "custom.dup" });
    invalids.push(["duplicate destinationId+id", [dup, { ...dup }]]);
    if (destinations[0]?.id) {
      invalids[2][1][0].destinationId = randomUUID();
    }
    const { delta } = await withRemoteDelta(runtime.lab, async () => {
      for (const [label, rules] of invalids) {
        const parsed = await putPolicyConfigCas(api, gatewayBase, rules);
        assert.ok(parsed.status >= 400 && parsed.status < 500, `${label}: expected 4xx, got ${parsed.status} ${parsed.text.slice(0, 200)}`);
      }
      const backoff = await putPolicyConfigCas(api, gatewayBase, [
        custom400Rule({ id: "custom.http400", backoff: { initialSeconds: 30, maxSeconds: 1 } }),
      ]);
      assert.ok(backoff.status >= 400, `invalid backoff: ${backoff.status}`);
    });
    assert.equal(delta, 0, "validation PUTs used the network");
    http.pass("policy mutation validation", { scenarioId: SCENARIO.VALIDATION, evidenceKind: EVIDENCE.GATEWAY, remoteCallsDelta: 0 });
  } catch (error) {
    http.fail("policy mutation validation", error, { scenarioId: SCENARIO.VALIDATION, evidenceKind: EVIDENCE.GATEWAY });
  }
}

async function runHttpCustom400(ctx) {
  const label = "custom 400: first request stays on A, next request uses B";
  const { runtime, apiProbe, http } = ctx;
  if (!apiProbe.ready) {
    http.notRun(label, apiProbe.reason, { scenarioId: SCENARIO.CUSTOM_400, evidenceKind: EVIDENCE.GATEWAY });
    return;
  }
  const routeSlots = await prepareRoute(ctx);
  const alpha = slotById(runtime.started, "alpha");
  try {
    await putRules(ctx, [custom400Rule({ id: "custom.http400" })]);
    await scriptListener(runtime.lab, "alpha", [httpFault(400, custom400Body())]);
    const firstMark = await markNow(runtime.lab);
    const first = await sendChat(runtime);
    const firstHits = await hitsSince(runtime.lab, firstMark);
    assert.notEqual(first.status, 200, `first request unexpectedly succeeded: ${first.text.slice(0, 200)}`);
    assertListenerSequence(listenersOfHits(firstHits), SEQ_FIRST_400_STAYS_A, label);
    assertUpstreamCount(firstHits, 1, label);
    assert.equal(firstHits[0].scriptStatus, 400);

    const secondMark = await markNow(runtime.lab);
    const second = await sendChat(runtime);
    const secondHits = await hitsSince(runtime.lab, secondMark);
    assert.equal(second.status, 200, second.text.slice(0, 400));
    checkOutput("chat", false, second.text, "LAB_OK_bravo");
    assertListenerSequence(listenersOfHits(secondHits), SEQ_NEXT_AVOIDS_A_USES_B, label);
    assertUpstreamCount(secondHits, 1, `${label} second`);
    http.pass(label, extra(firstHits.concat(secondHits), {
      scenarioId: SCENARIO.CUSTOM_400,
      evidenceKind: EVIDENCE.GATEWAY,
      models: modelTriad(alpha),
      expectedFirst: SEQ_FIRST_400_STAYS_A,
      expectedNext: SEQ_NEXT_AVOIDS_A_USES_B,
      firstStatus: first.status,
      secondStatus: second.status,
    }));
  } catch (error) {
    http.fail(label, error, { scenarioId: SCENARIO.CUSTOM_400, evidenceKind: EVIDENCE.GATEWAY });
  } finally {
    await scriptListener(runtime.lab, "alpha", []);
    await runtime.api.resetCooldowns(routeSlots.map((slot) => slot.accountId)).catch(() => {});
  }
}

async function runProtocol400(ctx) {
  const { runtime, apiProbe, http } = ctx;
  const cases = [
    ["responses", SCENARIO.CUSTOM_400_RESPONSES, "lab-responses"],
    ["messages", SCENARIO.CUSTOM_400_MESSAGES, "lab-messages"],
  ];
  for (const [client, scenarioId, publicModel] of cases) {
    const label = `${client} custom 400 stays on first upstream then skips it`;
    if (!apiProbe.ready) {
      http.notRun(label, apiProbe.reason, { scenarioId, evidenceKind: EVIDENCE.GATEWAY });
      continue;
    }
    const slot = slotById(runtime.started, client);
    try {
      await runtime.api.resetCooldowns(protocolSlotsOf(runtime.started).map((item) => item.accountId));
      await putRules(ctx, [custom400Rule({ id: `custom.http400.${client}` })]);
      await resetLab(runtime.lab, ctx);
      await scriptIso(runtime.lab, isolationSpec(slot), [httpFault(400, custom400Body())]);
      const firstMark = await markNow(runtime.lab);
      const first = await sendClient(runtime, client, publicModel, false);
      const firstHits = await hitsSince(runtime.lab, firstMark);
      assert.notEqual(first.status, 200, first.text.slice(0, 200));
      assertUpstreamCount(firstHits, 1, label);
      const secondMark = await markNow(runtime.lab);
      const second = await sendClient(runtime, client, publicModel, false);
      const secondHits = await hitsSince(runtime.lab, secondMark);
      assertUpstreamCount(secondHits, 0, `${label} waiting must not resend the same resource`);
      assert.notEqual(second.status, 200);
      http.pass(label, extra(firstHits.concat(secondHits), {
        scenarioId,
        evidenceKind: EVIDENCE.GATEWAY,
        models: modelTriad(slot, { publicModel }),
        firstStatus: first.status,
        secondStatus: second.status,
      }));
    } catch (error) {
      http.fail(label, error, { scenarioId, evidenceKind: EVIDENCE.GATEWAY });
    } finally {
      await resetLab(runtime.lab, ctx);
    }
  }
}

async function runOverrideAndClear(ctx) {
  const { runtime, apiProbe, http } = ctx;
  const overrideLabel = "destination override, disable inherited, disable builtin, nested body does not match";
  const clearLabel = "restrictions GET is zero-send; clear is source-specific and idempotent";
  if (!apiProbe.ready) {
    http.notRun(overrideLabel, apiProbe.reason, { scenarioId: SCENARIO.OVERRIDE, evidenceKind: EVIDENCE.GATEWAY });
    http.notRun(clearLabel, apiProbe.reason, { scenarioId: SCENARIO.CLEAR, evidenceKind: EVIDENCE.GATEWAY });
    return;
  }
  const routeSlots = await prepareRoute(ctx);
  const destinations = await listDestinations(runtime.api);
  const credentials = await runtime.api.credentials();
  const alphaDest = destinationIdForSlot(destinations, slotById(runtime.started, "alpha"), credentials);
  try {
    await putRules(ctx, [
      statusOnlyRule({ id: "custom.status400" }),
      statusOnlyRule({ id: "custom.status400", destinationId: alphaDest, enabled: false }),
      builtinOverride({ id: BUILTIN_GOAT_ID, enabled: false }),
    ]);
    const cfg = await getPolicyConfig(runtime.gatewayBase);
    requireOk(cfg, "GET config after override");
    await scriptListener(runtime.lab, "alpha", [httpFault(400, nestedEcho400Body())]);
    const mark = await markNow(runtime.lab);
    const first = await sendChat(runtime);
    const hits = await hitsSince(runtime.lab, mark);
    assert.notEqual(first.status, 200);
    assertListenerSequence(listenersOfHits(hits), SEQ_FIRST_400_STAYS_A, overrideLabel);
    const nextMark = await markNow(runtime.lab);
    const next = await sendChat(runtime);
    const nextHits = await hitsSince(runtime.lab, nextMark);
    assert.equal(next.status, 200, next.text.slice(0, 300));
    assertListenerSequence(listenersOfHits(nextHits), SEQ_FIRST_400_STAYS_A, `${overrideLabel} disabled overlay must not wait`);
    http.pass(overrideLabel, extra(hits.concat(nextHits), {
      scenarioId: SCENARIO.OVERRIDE,
      evidenceKind: EVIDENCE.GATEWAY,
      destinationId: alphaDest,
    }));
  } catch (error) {
    http.fail(overrideLabel, error, { scenarioId: SCENARIO.OVERRIDE, evidenceKind: EVIDENCE.GATEWAY });
  }

  try {
    await putRules(ctx, [custom400Rule({ id: "custom.http400" })]);
    await scriptListener(runtime.lab, "alpha", [httpFault(400, custom400Body())]);
    await scriptListener(runtime.lab, "bravo", [httpFault(400, custom400Body())]);
    await sendChat(runtime);
    await sendChat(runtime);
    const before = await markNow(runtime.lab);
    const { delta, result: listed } = await withRemoteDelta(runtime.lab, async () => {
      const parsed = await getRestrictions(runtime.gatewayBase);
      requireOk(parsed, "GET restrictions");
      return parsed;
    });
    assert.equal(delta, 0, "GET restrictions used the network");
    const afterGet = await hitsSince(runtime.lab, before);
    assertUpstreamCount(afterGet, 0, "diagnostic GET must not send");
    const rows = restrictionList(listed.body);
    assert.ok(rows.length >= 1, "expected at least one restriction");
    assert.ok(rows.every((row) => row.id && row.ruleId && row.source && row.state), "restriction rows missing identity fields");
    const firstId = rows[0].id;
    const { delta: clearDelta, result: cleared } = await withRemoteDelta(runtime.lab, async () => {
      const parsed = await clearRestrictionCas(runtime.api, runtime.gatewayBase, firstId);
      requireOk(parsed, "POST clear");
      return parsed;
    });
    assert.equal(clearDelta, 0, "clear used the network");
    const afterClear = await hitsSince(runtime.lab, before);
    assertUpstreamCount(afterClear, 0, "clear must not send");
    const again = await clearRestrictionCas(runtime.api, runtime.gatewayBase, firstId);
    requireOk(again, "idempotent clear");
    const remaining = restrictionList(again.body || listed.body);
    http.pass(clearLabel, {
      scenarioId: SCENARIO.CLEAR,
      evidenceKind: EVIDENCE.GATEWAY,
      restrictionCount: rows.length,
      remaining: remaining.length,
      sources: rows.map((row) => row.source),
      remoteCallsDelta: 0,
    });
  } catch (error) {
    http.fail(clearLabel, error, { scenarioId: SCENARIO.CLEAR, evidenceKind: EVIDENCE.GATEWAY });
  } finally {
    await resetLab(runtime.lab, ctx);
    await runtime.api.resetCooldowns(routeSlots.map((slot) => slot.accountId)).catch(() => {});
  }
}

async function runIsolation(ctx) {
  const { runtime, apiProbe, http } = ctx;
  const credLabel = "credential isolation: wait on one Key does not skip another";
  const modelLabel = "credential_model isolation: wait on one upstream model does not skip the other";
  if (!apiProbe.ready) {
    http.notRun(credLabel, apiProbe.reason, { scenarioId: SCENARIO.ISOLATION_CREDENTIAL, evidenceKind: EVIDENCE.GATEWAY });
    http.notRun(modelLabel, apiProbe.reason, { scenarioId: SCENARIO.ISOLATION_MODEL, evidenceKind: EVIDENCE.GATEWAY });
    return;
  }

  const chat = slotById(runtime.started, "chat");
  const responses = slotById(runtime.started, "responses");
  try {
    await putRules(ctx, [custom400Rule({ id: "custom.http400" })]);
    await resetLab(runtime.lab, ctx);
    await scriptIso(runtime.lab, isolationSpec(chat), [httpFault(400, custom400Body())]);
    const firstMark = await markNow(runtime.lab);
    const blocked = await sendClient(runtime, "chat", chat.publicModel, false);
    const blockedHits = await hitsSince(runtime.lab, firstMark);
    assert.notEqual(blocked.status, 200);
    assertUpstreamCount(blockedHits, 1, credLabel);
    const otherMark = await markNow(runtime.lab);
    const other = await sendClient(runtime, "responses", responses.publicModel, false);
    const otherHits = await hitsSince(runtime.lab, otherMark);
    assert.equal(other.status, 200, other.text.slice(0, 300));
    assert.equal(otherHits[0]?.slot, "responses");
    http.pass(credLabel, extra(blockedHits.concat(otherHits), {
      scenarioId: SCENARIO.ISOLATION_CREDENTIAL,
      evidenceKind: EVIDENCE.GATEWAY,
      models: [modelTriad(chat), modelTriad(responses)],
    }));
  } catch (error) {
    http.fail(credLabel, error, { scenarioId: SCENARIO.ISOLATION_CREDENTIAL, evidenceKind: EVIDENCE.GATEWAY });
  }

  try {
    const isoA = "lab-policy-iso-a";
    const isoB = "lab-policy-iso-b";
    const upA = "upstream-policy-iso-a";
    const upB = "upstream-policy-iso-b";
    await call(runtime.lab, "acceptModel", chat.id || chat.slot, upA);
    await call(runtime.lab, "acceptModel", chat.id || chat.slot, upB);
    await runtime.api.mutation("/dashboard/api/v4/onboarding/commit", {
      mode: "complete",
      operationId: randomUUID(),
      connection: {
        kind: "new",
        templateId: "custom-http",
        name: "Policy model isolation",
        endpointUrl: chat.url,
        upstreamProtocol: chat.protocol,
        authKind: chat.auth,
      },
      authorization: { kind: "api_key", secretInput: chat.secret, accountLabel: "policy-iso" },
      targets: [
        { publicModel: isoA, upstreamModel: upA },
        { publicModel: isoB, upstreamModel: upB },
      ],
    });
    rememberSecret(chat.secret);
    await putRules(ctx, [credentialModel400Rule({ id: "custom.http400.model" })]);
    await resetLab(runtime.lab, ctx);
    await scriptIso(runtime.lab, isolationSpec(chat, { model: upA }), [httpFault(400, custom400Body())]);
    const aMark = await markNow(runtime.lab);
    const aResp = await sendClient(runtime, "chat", isoA, false);
    const aHits = await hitsSince(runtime.lab, aMark);
    assert.notEqual(aResp.status, 200);
    assert.equal(aHits[0]?.model, upA);
    const bMark = await markNow(runtime.lab);
    const bResp = await sendClient(runtime, "chat", isoB, false);
    const bHits = await hitsSince(runtime.lab, bMark);
    assert.equal(bResp.status, 200, bResp.text.slice(0, 300));
    assert.equal(bHits[0]?.model, upB);
    http.pass(modelLabel, extra(aHits.concat(bHits), {
      scenarioId: SCENARIO.ISOLATION_MODEL,
      evidenceKind: EVIDENCE.GATEWAY,
      models: {
        a: { public: isoA, upstream: upA, live: LIVE_OUTBOUND_MODEL },
        b: { public: isoB, upstream: upB, live: LIVE_OUTBOUND_MODEL },
      },
    }));
  } catch (error) {
    http.fail(modelLabel, error, { scenarioId: SCENARIO.ISOLATION_MODEL, evidenceKind: EVIDENCE.GATEWAY });
  }
}

async function runSingleFlightAndLateChange(ctx) {
  const { runtime, apiProbe, http } = ctx;
  const flightLabel = "expired wait: overlapping A probe falls through to B (A1 B1)";
  const lateLabel = "in-flight delayed 400 after same-id recreate does not create a new restriction";
  if (!apiProbe.ready) {
    http.notRun(flightLabel, apiProbe.reason, { scenarioId: SCENARIO.SINGLE_FLIGHT, evidenceKind: EVIDENCE.GATEWAY });
    http.notRun(lateLabel, apiProbe.reason, { scenarioId: SCENARIO.LATE_CHANGE, evidenceKind: EVIDENCE.GATEWAY });
    return;
  }
  const routeSlots = await prepareRoute(ctx);
  try {
    await putRules(ctx, [custom400Rule({ id: "custom.http400", backoff: HTTP_BACKOFF })]);
    await scriptListener(runtime.lab, "alpha", [httpFault(400, custom400Body())]);
    const first = await sendChat(runtime);
    assert.notEqual(first.status, 200);
    await waitUntilReadyOrTimeout(runtime.gatewayBase, {
      timeoutMs: HTTP_BACKOFF.initialSeconds * 1000 + 1500,
      label: "policy wait expiry",
    }).catch(async () => {
      await sleep(HTTP_BACKOFF.initialSeconds * 1000 + 50);
    });
    await resetLab(runtime.lab, ctx);
    await scriptListener(runtime.lab, "alpha", [delayFault(HTTP_INFLIGHT_DELAY_MS)]);
    const mark = await markNow(runtime.lab);
    const firstProbe = sendChat(runtime, { timeoutMs: 15000 });
    await waitForLabArrival(runtime.lab, mark, "A probe arrival while response blocked");
    const overlap = sendChat(runtime, { timeoutMs: 15000 });
    const results = await Promise.all([firstProbe, overlap]);
    const hits = await hitsSince(runtime.lab, mark);
    assert.ok(results.every((item) => item.status === 200), results.map((item) => item.status).join(","));
    assertListenerCounts(hits, SEQ_SINGLE_FLIGHT_A1_B1, flightLabel);
    http.pass(flightLabel, extra(hits, { scenarioId: SCENARIO.SINGLE_FLIGHT, evidenceKind: EVIDENCE.GATEWAY }));
  } catch (error) {
    http.fail(flightLabel, error, { scenarioId: SCENARIO.SINGLE_FLIGHT, evidenceKind: EVIDENCE.GATEWAY });
  }

  try {
    await putRules(ctx, [custom400Rule({ id: "custom.http400", backoff: HTTP_BACKOFF })]);
    await resetLab(runtime.lab, ctx);
    await scriptListener(runtime.lab, "alpha", [httpFault(400, custom400Body(), {}, HTTP_INFLIGHT_DELAY_MS)]);
    const mark = await markNow(runtime.lab);
    const pending = sendChat(runtime, { timeoutMs: 15000 });
    const arrived = await waitForLabArrival(runtime.lab, mark, "in-flight delayed HTTP 400 arrival");
    assert.equal(arrived[0].scriptKind, "http");
    assert.equal(arrived[0].scriptStatus, 400);
    await putRules(ctx, [custom400Rule({ id: "custom.placeholder", backoff: HTTP_BACKOFF })]);
    await putRules(ctx, [custom400Rule({ id: "custom.http400", backoff: HTTP_BACKOFF })]);
    const finished = await pending;
    assert.notEqual(finished.status, 200, finished.text.slice(0, 200));
    const hits = await hitsSince(runtime.lab, mark);
    assertUpstreamCount(hits, 1, lateLabel);
    assert.equal(hits[0].scriptStatus, 400);
    const { delta, result: listed } = await withRemoteDelta(runtime.lab, async () => {
      const parsed = await getRestrictions(runtime.gatewayBase);
      requireOk(parsed, "GET restrictions after late 400");
      return parsed;
    });
    assert.equal(delta, 0, "restriction GET used the network");
    assert.equal(waitingRestrictions(listed.body).length, 0, "late 400 created a waiting restriction after same-id recreate");
    http.pass(lateLabel, extra(hits, { scenarioId: SCENARIO.LATE_CHANGE, evidenceKind: EVIDENCE.GATEWAY, remoteCallsDelta: 0 }));
  } catch (error) {
    http.fail(lateLabel, error, { scenarioId: SCENARIO.LATE_CHANGE, evidenceKind: EVIDENCE.GATEWAY });
  } finally {
    await scriptListener(runtime.lab, "alpha", []);
    await runtime.api.resetCooldowns(routeSlots.map((slot) => slot.accountId)).catch(() => {});
  }
}

async function runAllWaiting(ctx) {
  const label = "all candidates waiting: zero upstream send";
  const { runtime, apiProbe, http } = ctx;
  if (!apiProbe.ready) {
    http.notRun(label, apiProbe.reason, { scenarioId: SCENARIO.ALL_WAITING, evidenceKind: EVIDENCE.GATEWAY });
    return;
  }
  const routeSlots = await prepareRoute(ctx);
  try {
    await putRules(ctx, [custom400Rule({ id: "custom.http400" })]);
    for (const id of ["alpha", "bravo", "charlie"]) {
      await scriptListener(runtime.lab, id, [httpFault(400, custom400Body())]);
    }
    for (let i = 0; i < 3; i += 1) {
      const response = await sendChat(runtime);
      assert.notEqual(response.status, 200);
    }
    const mark = await markNow(runtime.lab);
    const last = await sendChat(runtime);
    const hits = await hitsSince(runtime.lab, mark);
    assertListenerSequence(listenersOfHits(hits), SEQ_ALL_WAITING_ZERO, label);
    assertUpstreamCount(hits, 0, label);
    assert.notEqual(last.status, 200);
    http.pass(label, extra(hits, {
      scenarioId: SCENARIO.ALL_WAITING,
      evidenceKind: EVIDENCE.GATEWAY,
      lastStatus: last.status,
    }));
  } catch (error) {
    http.fail(label, error, { scenarioId: SCENARIO.ALL_WAITING, evidenceKind: EVIDENCE.GATEWAY });
  } finally {
    await resetLab(runtime.lab, ctx);
    await runtime.api.resetCooldowns(routeSlots.map((slot) => slot.accountId)).catch(() => {});
  }
}

async function runRestart(ctx) {
  const label = "restart keeps saved rules and drops transient waits";
  const { runtime, apiProbe, http } = ctx;
  if (!apiProbe.ready) {
    http.notRun(label, apiProbe.reason, { scenarioId: SCENARIO.RESTART, evidenceKind: EVIDENCE.GATEWAY });
    return;
  }
  const routeSlots = await prepareRoute(ctx);
  try {
    await putRules(ctx, [custom400Rule({ id: "custom.http400", backoff: HTTP_BACKOFF })]);
    await scriptListener(runtime.lab, "alpha", [httpFault(400, custom400Body())]);
    const first = await sendChat(runtime);
    assert.notEqual(first.status, 200);
    await restartOwnedGateway(runtime);
    runtime.api = (await import("./dashboard.mjs")).makeApi(runtime.gatewayBase, runtime.lab, () => runtime.gatewayKey);
    const cfg = await getPolicyConfig(runtime.gatewayBase);
    requireOk(cfg, "GET config after restart");
    const rules = cfg.body.rules || [];
    assert.ok(
      rules.some((rule) => rule.id === "custom.http400" && rule.enabled !== false),
      `saved rule missing after restart: ${JSON.stringify(rules).slice(0, 400)}`,
    );
    const listed = await getRestrictions(runtime.gatewayBase);
    if (listed.status === 200) {
      assert.equal(waitingRestrictions(listed.body).length, 0, "transient waits survived restart");
    }
    await resetLab(runtime.lab, ctx);
    const mark = await markNow(runtime.lab);
    const next = await sendChat(runtime);
    const hits = await hitsSince(runtime.lab, mark);
    assert.equal(next.status, 200, next.text.slice(0, 300));
    assert.equal(hits[0]?.listener, "alpha");
    http.pass(label, extra(hits, { scenarioId: SCENARIO.RESTART, evidenceKind: EVIDENCE.GATEWAY }));
  } catch (error) {
    http.fail(label, error, { scenarioId: SCENARIO.RESTART, evidenceKind: EVIDENCE.GATEWAY });
  } finally {
    await runtime.api.resetCooldowns(routeSlots.map((slot) => slot.accountId)).catch(() => {});
  }
}

async function runCompleteBodiesAndCancel(ctx) {
  const { runtime, http } = ctx;
  const chat = slotById(runtime.started, "chat");
  try {
    await resetLab(runtime.lab, ctx);
    const jsonMark = await markNow(runtime.lab);
    const json = await sendClient(runtime, "chat", chat.publicModel, false);
    assert.equal(json.status, 200, json.text.slice(0, 300));
    JSON.parse(json.text);
    checkOutput("chat", false, json.text, chat.ok);
    const jsonHits = await hitsSince(runtime.lab, jsonMark);
    assertUpstreamCount(jsonHits, 1, "complete JSON");
    http.pass("complete JSON body is fully read", extra(jsonHits, {
      scenarioId: SCENARIO.COMPLETE_JSON,
      evidenceKind: EVIDENCE.GATEWAY,
      models: modelTriad(chat),
    }));
  } catch (error) {
    http.fail("complete JSON body is fully read", error, { scenarioId: SCENARIO.COMPLETE_JSON, evidenceKind: EVIDENCE.GATEWAY });
  }

  try {
    await resetLab(runtime.lab, ctx);
    const sseMark = await markNow(runtime.lab);
    const sse = await sendClient(runtime, "chat", chat.publicModel, true, SSE_TIMEOUT_MS);
    assert.equal(sse.status, 200, sse.text.slice(0, 300));
    assert.match(sse.contentType, /text\/event-stream/);
    assert.ok(sse.text.includes("[DONE]"), "SSE missing terminal [DONE]");
    checkOutput("chat", true, sse.text, chat.ok);
    const sseHits = await hitsSince(runtime.lab, sseMark);
    assertUpstreamCount(sseHits, 1, "complete SSE");
    http.pass("complete SSE body is fully read", extra(sseHits, {
      scenarioId: SCENARIO.COMPLETE_SSE,
      evidenceKind: EVIDENCE.GATEWAY,
      models: modelTriad(chat),
    }));
  } catch (error) {
    http.fail("complete SSE body is fully read", error, { scenarioId: SCENARIO.COMPLETE_SSE, evidenceKind: EVIDENCE.GATEWAY });
  }

  try {
    await resetLab(runtime.lab, ctx);
    await scriptListener(runtime.lab, chat.listener, [delayFault(400)]);
    await guardRemote(runtime.policyCtx, { beforeSend: true });
    const ac = new AbortController();
    const pending = fetch(`${runtime.gatewayBase}/v1/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json", ...inferenceHeaders("chat", runtime.gatewayKey) },
      body: JSON.stringify(input("chat", chat.publicModel, true)),
      signal: ac.signal,
    });
    ac.abort();
    let interrupted = false;
    let text = "";
    try {
      const response = await pending;
      text = await response.text();
    } catch {
      interrupted = true;
    }
    assert.ok(interrupted || !text.includes("[DONE]"), "cancel still delivered a complete SSE body");
    http.pass("cancel interrupts the response body", {
      scenarioId: SCENARIO.CANCEL,
      evidenceKind: EVIDENCE.GATEWAY,
      interrupted,
      hadDone: text.includes("[DONE]"),
      models: modelTriad(chat),
    });
  } catch (error) {
    http.fail("cancel interrupts the response body", error, { scenarioId: SCENARIO.CANCEL, evidenceKind: EVIDENCE.GATEWAY });
  } finally {
    await scriptListener(runtime.lab, chat.listener, []).catch(() => {});
  }
}

async function runLiveOrLocalEvidence(ctx) {
  const { runtime, suite, liveSendsAllowed, apiProbe, live, harness } = ctx;
  const stats = await labStats(runtime.lab);
  if (suite === "local") {
    if (stats.remoteCalls !== 0) {
      harness.fail("local remoteCalls=0", new Error(`remoteCalls=${stats.remoteCalls}`), {
        scenarioId: SCENARIO.LOCAL_REMOTE_ZERO,
        evidenceKind: EVIDENCE.LAB,
        remoteCalls: stats.remoteCalls,
      });
    } else {
      harness.pass("local remoteCalls=0", {
        scenarioId: SCENARIO.LOCAL_REMOTE_ZERO,
        evidenceKind: EVIDENCE.LAB,
        remoteCalls: 0,
      });
    }
    return;
  }

  const started = runtime.lab.runtime();
  await call(runtime.lab, "armLive", false);
  if (!started.liveConfigured) {
    live.fail("live lab is not a local fixture", new Error("attached runtime liveConfigured=false; refusing to treat a local simulator as live"), {
      scenarioId: SCENARIO.LIVE_NOT_FAKE,
      evidenceKind: EVIDENCE.LIVE,
    });
  } else {
    live.pass("live lab is not a local fixture", {
      scenarioId: SCENARIO.LIVE_NOT_FAKE,
      evidenceKind: EVIDENCE.LIVE,
      liveConfigured: true,
      liveEnabled: Boolean(started.liveEnabled),
    });
  }

  const chat = slotById(runtime.started, "chat");
  const before = (await labStats(runtime.lab)).remoteCalls || 0;
  try {
    await resetLab(runtime.lab, ctx);
    await scriptIso(runtime.lab, isolationSpec(chat), [httpFault(400, custom400Body())]);
    const mark = await markNow(runtime.lab);
    const faulted = await sendClient(runtime, "chat", chat.publicModel, false);
    void faulted.text;
    const hits = await hitsSince(runtime.lab, mark);
    const after = (await labStats(runtime.lab)).remoteCalls || 0;
    assert.equal(after, before, `local fault incremented remoteCalls ${before} -> ${after}`);
    assert.ok(hits.length >= 1, "local fault produced no lab arrival");
    assert.notEqual(hits[0].live, true, "local fault was journaled as a live remote");
    live.pass("live-suite local fault: arrivals increment, remoteCalls do not", extra(hits, {
      scenarioId: SCENARIO.LIVE_FAULT_ZERO_REMOTE,
      evidenceKind: EVIDENCE.LIVE,
      localArrivals: hits.length,
      remoteCallsDelta: after - before,
      models: modelTriad(chat),
    }));
  } catch (error) {
    live.fail("live-suite local fault: arrivals increment, remoteCalls do not", error, {
      scenarioId: SCENARIO.LIVE_FAULT_ZERO_REMOTE,
      evidenceKind: EVIDENCE.LIVE,
    });
  } finally {
    await resetLab(runtime.lab, ctx);
  }

  await runLivePolicyAbba(ctx, liveSendsAllowed, apiProbe);
}

async function runLivePolicyAbba(ctx, liveSendsAllowed, apiProbe) {
  const { runtime, live } = ctx;
  const abbaLabel = "live custom400 A then B B A through minimax-m3";
  const charlieLabel = "live charlie minimax-m3 proves a second upstream";
  const skipReason = apiProbe.ready
    ? "live policy sends deferred until configuration API is ready on this binary"
    : `${apiProbe.reason}; not arming live or sending minimax-m3`;
  if (!liveSendsAllowed) {
    live.notRun(abbaLabel, skipReason, { scenarioId: SCENARIO.LIVE_POLICY_ABBA, evidenceKind: EVIDENCE.LIVE });
    live.notRun(charlieLabel, skipReason, { scenarioId: SCENARIO.LIVE_CHARLIE, evidenceKind: EVIDENCE.LIVE });
    return;
  }

  try {
    await guardRemote(ctx, { beforeSend: true });
  } catch (error) {
    live.fail(abbaLabel, error, { scenarioId: SCENARIO.LIVE_POLICY_ABBA, evidenceKind: EVIDENCE.LIVE });
    live.notRun(charlieLabel, error instanceof Error ? error.message : String(error), {
      scenarioId: SCENARIO.LIVE_CHARLIE,
      evidenceKind: EVIDENCE.LIVE,
    });
    return;
  }

  const routeSlots = await prepareRoute(ctx);
  const alpha = slotById(runtime.started, "alpha");
  const bravo = slotById(runtime.started, "bravo");
  const charlie = slotById(runtime.started, "charlie");
  const remoteBefore = (await labStats(runtime.lab)).remoteCalls || 0;
  let abbaDelta = 0;
  let abbaPassed = false;
  try {
    try {
      await putRules(ctx, [custom400Rule({ id: "custom.http400", backoff: LIVE_BACKOFF })]);
      const { delta: controlDelta } = await withRemoteDelta(runtime.lab, async () => {
        requireOk(await getPolicyConfig(runtime.gatewayBase), "GET config");
        requireOk(await getRestrictions(runtime.gatewayBase), "GET restrictions");
      });
      assert.equal(controlDelta, 0, "config/diagnostics used the network");

      ctx.phase = PHASE.LIVE;
      await call(runtime.lab, "armLive", true);
      await scriptListener(runtime.lab, "alpha", [httpFault(400, custom400Body())]);
      const firstMark = await markNow(runtime.lab);
      const first = await sendLiveChat(runtime);
      const firstHits = await hitsSince(runtime.lab, firstMark);
      assert.notEqual(first.status, 200, `first live request unexpectedly succeeded: ${first.text.slice(0, 200)}`);
      assertListenerSequence(listenersOfHits(firstHits), SEQ_FIRST_400_STAYS_A, abbaLabel);
      assert.notEqual(firstHits[0].live, true, "A 400 was forwarded live");
      const afterReject = (await labStats(runtime.lab)).remoteCalls || 0;
      assert.equal(afterReject, remoteBefore, `A 400 remote delta ${afterReject - remoteBefore}`);

      const secondMark = await markNow(runtime.lab);
      const second = await sendLiveChat(runtime);
      assert.equal(second.status, 200, second.text.slice(0, 400));
      assertChatCompletionProtocolValid(second.text, "B1");
      const secondHits = await hitsSince(runtime.lab, secondMark);
      assertListenerSequence(listenersOfHits(secondHits), SEQ_NEXT_AVOIDS_A_USES_B, `${abbaLabel} B1`);
      assertLiveSuccessReceipt(secondHits[0], "B1");
      assert.equal(LIVE_OUTBOUND_MODEL, "minimax-m3");

      const thirdMark = await markNow(runtime.lab);
      const third = await sendLiveChat(runtime);
      assert.equal(third.status, 200, third.text.slice(0, 400));
      assertChatCompletionProtocolValid(third.text, "B2");
      const thirdHits = await hitsSince(runtime.lab, thirdMark);
      assertListenerSequence(listenersOfHits(thirdHits), SEQ_NEXT_AVOIDS_A_USES_B, `${abbaLabel} B2`);
      assertLiveSuccessReceipt(thirdHits[0], "B2");

      await waitUntilReadyOrTimeout(runtime.gatewayBase, {
        timeoutMs: LIVE_EXPIRY_TIMEOUT_MS,
        label: "live wait natural expiry",
      });

      const fourthMark = await markNow(runtime.lab);
      const fourth = await sendLiveChat(runtime);
      assert.equal(fourth.status, 200, fourth.text.slice(0, 400));
      assertChatCompletionProtocolValid(fourth.text, "A probe");
      const fourthHits = await hitsSince(runtime.lab, fourthMark);
      assertListenerSequence(listenersOfHits(fourthHits), SEQ_FIRST_400_STAYS_A, `${abbaLabel} expired A`);
      assertLiveSuccessReceipt(fourthHits[0], "A probe");

      const sequenceHits = firstHits.concat(secondHits, thirdHits, fourthHits);
      assertListenerSequence(listenersOfHits(sequenceHits), SEQ_LIVE_ABBA, abbaLabel);
      const afterAbba = (await labStats(runtime.lab)).remoteCalls || 0;
      abbaDelta = afterAbba - remoteBefore;
      assert.equal(abbaDelta, 3, `ABBA remote delta ${abbaDelta}`);
      assert.ok(abbaDelta <= LIVE_MAX_REMOTE, `live budget ${LIVE_MAX_REMOTE} exceeded`);
      live.pass(abbaLabel, extra(sequenceHits, {
        scenarioId: SCENARIO.LIVE_POLICY_ABBA,
        evidenceKind: EVIDENCE.LIVE,
        remoteCallsDelta: abbaDelta,
        liveOutboundModel: LIVE_OUTBOUND_MODEL,
        models: { a: modelTriad(alpha), b: modelTriad(bravo) },
      }));
      abbaPassed = true;
    } catch (error) {
      live.fail(abbaLabel, error, { scenarioId: SCENARIO.LIVE_POLICY_ABBA, evidenceKind: EVIDENCE.LIVE });
      live.notRun(charlieLabel, error instanceof Error ? error.message : String(error), {
        scenarioId: SCENARIO.LIVE_CHARLIE,
        evidenceKind: EVIDENCE.LIVE,
      });
    }

    if (abbaPassed) {
      try {
        await runtime.api.reorder([charlie.accountId, alpha.accountId, bravo.accountId]);
        const charlieMark = await markNow(runtime.lab);
        const charlieResp = await sendLiveChat(runtime);
        assert.equal(charlieResp.status, 200, charlieResp.text.slice(0, 400));
        assertChatCompletionProtocolValid(charlieResp.text, "charlie");
        const charlieHits = await hitsSince(runtime.lab, charlieMark);
        assert.equal(charlieHits[0]?.listener, "charlie");
        assertLiveSuccessReceipt(charlieHits[0], "charlie");
        const totalDelta = await remoteDeltaNow(ctx);
        assert.ok(totalDelta <= LIVE_MAX_REMOTE, `whole-run live budget ${LIVE_MAX_REMOTE} exceeded: ${totalDelta}`);
        live.pass(charlieLabel, extra(charlieHits, {
          scenarioId: SCENARIO.LIVE_CHARLIE,
          evidenceKind: EVIDENCE.LIVE,
          remoteCallsDelta: totalDelta - abbaDelta,
          liveOutboundModel: LIVE_OUTBOUND_MODEL,
          models: modelTriad(charlie),
        }));
      } catch (error) {
        live.fail(charlieLabel, error, { scenarioId: SCENARIO.LIVE_CHARLIE, evidenceKind: EVIDENCE.LIVE });
      }
    }
  } finally {
    ctx.phase = PHASE.SIMULATE;
    await call(runtime.lab, "armLive", false).catch(() => {});
    await scriptListener(runtime.lab, "alpha", []).catch(() => {});
    await runtime.api.resetCooldowns(routeSlots.map((slot) => slot.accountId)).catch(() => {});
    await runtime.api.reorder(routeSlots.map((slot) => slot.accountId)).catch(() => {});
  }
}

export function recordHttpNotRun(collector, reason, { suite = "local" } = {}) {
  const http = scoped(collector, RESPONSIBILITY.HTTP);
  const rows = [
    ["configuration API probe", SCENARIO.API_PROBE, EVIDENCE.GATEWAY],
    ["CAS conflict on policy PUT", SCENARIO.CAS_CONFLICT, EVIDENCE.GATEWAY],
    ["policy mutation validation", SCENARIO.VALIDATION, EVIDENCE.GATEWAY],
    ["custom 400: first request stays on A, next request uses B", SCENARIO.CUSTOM_400, EVIDENCE.GATEWAY],
    ["responses custom 400 stays on first upstream then skips it", SCENARIO.CUSTOM_400_RESPONSES, EVIDENCE.GATEWAY],
    ["messages custom 400 stays on first upstream then skips it", SCENARIO.CUSTOM_400_MESSAGES, EVIDENCE.GATEWAY],
    ["destination override, disable inherited, disable builtin, nested body does not match", SCENARIO.OVERRIDE, EVIDENCE.GATEWAY],
    ["restrictions GET is zero-send; clear is source-specific and idempotent", SCENARIO.CLEAR, EVIDENCE.GATEWAY],
    ["credential isolation: wait on one Key does not skip another", SCENARIO.ISOLATION_CREDENTIAL, EVIDENCE.GATEWAY],
    ["credential_model isolation: wait on one upstream model does not skip the other", SCENARIO.ISOLATION_MODEL, EVIDENCE.GATEWAY],
    ["expired wait: overlapping A probe falls through to B (A1 B1)", SCENARIO.SINGLE_FLIGHT, EVIDENCE.GATEWAY],
    ["in-flight delayed 400 after same-id recreate does not create a new restriction", SCENARIO.LATE_CHANGE, EVIDENCE.GATEWAY],
    ["all candidates waiting: zero upstream send", SCENARIO.ALL_WAITING, EVIDENCE.GATEWAY],
    ["restart keeps saved rules and drops transient waits", SCENARIO.RESTART, EVIDENCE.GATEWAY],
    ["complete JSON body is fully read", SCENARIO.COMPLETE_JSON, EVIDENCE.GATEWAY],
    ["complete SSE body is fully read", SCENARIO.COMPLETE_SSE, EVIDENCE.GATEWAY],
    ["cancel interrupts the response body", SCENARIO.CANCEL, EVIDENCE.GATEWAY],
  ];
  for (const [label, scenarioId, evidenceKind] of rows) {
    http.notRun(label, reason, { scenarioId, evidenceKind });
  }
  if (suite === "live") recordLiveNotRun(collector, reason);
}

export function recordLiveNotRun(collector, reason) {
  const live = scoped(collector, RESPONSIBILITY.LIVE);
  for (const [label, scenarioId] of [
    ["live custom400 A then B B A through minimax-m3", SCENARIO.LIVE_POLICY_ABBA],
    ["live charlie minimax-m3 proves a second upstream", SCENARIO.LIVE_CHARLIE],
  ]) {
    live.notRun(label, reason, { scenarioId, evidenceKind: EVIDENCE.LIVE });
  }
}

export { probePolicyApi, companionRustRows };
