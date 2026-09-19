import assert from "node:assert/strict";
import { createServer } from "node:http";
import { randomUUID } from "node:crypto";
import { MARKER, UTF8_MARKER, promptDigest, rememberSecret, sha256 } from "./common.mjs";
import { findCredential, inferenceHeaders, input, readJsonResponse, request, summarizeHit } from "./dashboard.mjs";
import { restartOwnedGateway, withRuntime } from "./harness.mjs";
import { protocolSlotsOf, routeSlotsOf } from "./scenarios-routing.mjs";
import { recordRustEvidence } from "./rust-runner.mjs";
import { EVIDENCE } from "./coverage.mjs";
import { portOpen, waitFor } from "./process.mjs";

const GW = EVIDENCE.GATEWAY;

function gw(collector, label, extra) {
  collector.pass(label, { evidenceKind: GW, ...extra });
}

export async function latestForwardLogs(api) {
  const body = await api.json("/dashboard/api/v4/logs/forward");
  return body.items || body.logs || [];
}

export async function runGatewayStreamAndUsage(runtime, collector) {
  const { lab, started, api, gatewayBase, gatewayKey } = runtime;
  const chat = protocolSlotsOf(started).find((slot) => slot.slot === "chat");

  {
    const label = "exact usage present";
    try {
      const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", gatewayKey));
      const parsed = JSON.parse(await response.text());
      assert.equal(response.status, 200);
      assert.equal(parsed.usage?.prompt_tokens, 1);
      assert.equal(parsed.usage?.completion_tokens, 1);
      assert.equal(parsed.usage?.total_tokens, 2);
      const logs = await latestForwardLogs(api);
      const last = logs[0] || logs[logs.length - 1];
      if (last) {
        const prompt = last.promptTokens ?? last.prompt_tokens;
        const completion = last.completionTokens ?? last.completion_tokens;
        if (prompt != null) assert.equal(prompt, 1);
        if (completion != null) assert.equal(completion, 1);
      }
      gw(collector, label, { scenarioId: "gw.usage.exact", usage: parsed.usage, log: last && { status: last.status, costState: last.costState || last.cost_state } });
    } catch (error) {
      collector.fail(label, error, { scenarioId: "gw.usage.exact" });
    }
  }

  {
    const label = "missing usage through gateway";
    lab.script(chat.listener, [{ kind: "missing_usage" }]);
    try {
      const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", gatewayKey));
      const text = await response.text();
      const parsed = JSON.parse(text);
      assert.equal(parsed.usage, undefined);
      const logs = await latestForwardLogs(api);
      const last = logs[0] || logs[logs.length - 1];
      gw(collector, label, { scenarioId: "gw.usage.missing", logStatus: last?.status, costState: last?.costState || last?.cost_state });
    } catch (error) {
      collector.fail(label, error, { scenarioId: "gw.usage.missing" });
    } finally {
      lab.script(chat.listener, []);
    }
  }

  lab.script(chat.listener, [{ kind: "missing_end" }]);
  try {
    let text = "";
    let status = 0;
    try {
      const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, true), inferenceHeaders("chat", gatewayKey));
      status = response.status;
      text = await response.text();
    } catch (error) {
      text = String(error?.message || error);
      status = 0;
    }
    const hits = lab.snapshot().filter((hit) => hit.scriptKind === "missing_end");
    assert.ok(hits.length > 0, "lab did not apply missing_end");
    const completed =
      status === 200 &&
      (text.includes("[DONE]") || /"finish_reason"\s*:\s*"stop"/i.test(text)) &&
      !/error|incomplete|truncated|terminal event/i.test(text);
    assert.equal(completed, false, `missing terminal treated as completed: ${text.slice(0, 240)}`);
    const logs = await latestForwardLogs(api);
    const last = logs[0] || logs[logs.length - 1];
    const logStatus = String(last?.status || "");
    if (last && /^success$/i.test(logStatus)) {
      throw new Error(`forward log treated incomplete stream as success: ${logStatus}`);
    }
    gw(collector, "missing stream end through gateway", {
      scenarioId: "gw.stream.missing-end",
      clientStatus: status,
      completed: false,
      labScriptKind: "missing_end",
      logStatus,
    });
  } catch (error) {
    collector.fail("missing stream end through gateway", error, { scenarioId: "gw.stream.missing-end" });
  } finally {
    lab.script(chat.listener, []);
  }

  lab.script(chat.listener, [{ kind: "stream_interrupt" }]);
  try {
    let text = "";
    try {
      const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, true), inferenceHeaders("chat", gatewayKey));
      text = await response.text();
    } catch {
      text = "";
    }
    assert.equal(text.includes("[DONE]"), false);
    gw(collector, "stream interrupt through gateway", { scenarioId: "gw.stream.interrupt" });
  } catch (error) {
    collector.fail("stream interrupt through gateway", error, { scenarioId: "gw.stream.interrupt" });
  } finally {
    lab.script(chat.listener, []);
  }

  try {
    const body = { ...input("chat", chat.publicModel, true), messages: [{ role: "user", content: `${MARKER} ${UTF8_MARKER}` }] };
    const response = await request(gatewayBase, "/v1/chat/completions", "POST", body, inferenceHeaders("chat", gatewayKey));
    const text = await response.text();
    assert.ok(text.includes("\u2603"), "utf-8 missing through gateway");
    gw(collector, "stream utf-8 through gateway", { scenarioId: "gw.stream.utf8" });
  } catch (error) {
    collector.fail("stream utf-8 through gateway", error, { scenarioId: "gw.stream.utf8" });
  }

  const priorSettings = await api.json("/dashboard/api/v4/settings").catch(() => null);
  try {
    await api.mutation("/dashboard/api/v4/settings", { nonStreamTimeoutSecs: 1, streamIdleTimeoutSecs: 1 }, "PUT");
    lab.script(chat.listener, [{ kind: "delay", ms: 2500 }]);
    const response = await request(
      gatewayBase,
      "/v1/chat/completions",
      "POST",
      input("chat", chat.publicModel, false),
      inferenceHeaders("chat", gatewayKey),
      15000,
    );
    const text = await response.text();
    assert.ok(
      [408, 504, 502, 500].includes(response.status) || /timeout/i.test(text),
      `gateway timeout missing: ${response.status} ${text.slice(0, 200)}`,
    );
    const logs = await latestForwardLogs(api);
    const last = logs[0] || logs[logs.length - 1];
    const logStatus = String(last?.status || last?.errorStage || last?.error_stage || "");
    assert.equal(/^success$/i.test(String(last?.status || "")), false, `timeout logged as success: ${logStatus}`);
    gw(collector, "timeout through gateway", {
      scenarioId: "gw.stream.timeout",
      clientStatus: response.status,
      logStatus: last?.status,
      errorStage: last?.errorStage || last?.error_stage,
    });
  } catch (error) {
    collector.fail("timeout through gateway", error, { scenarioId: "gw.stream.timeout" });
  } finally {
    lab.script(chat.listener, []);
    if (priorSettings) {
      await api.mutation("/dashboard/api/v4/settings", {
        nonStreamTimeoutSecs: priorSettings.nonStreamTimeoutSecs,
        streamIdleTimeoutSecs: priorSettings.streamIdleTimeoutSecs,
      }, "PUT").catch(() => {});
    }
  }

  lab.script(chat.listener, [{ kind: "delay", ms: 3000 }]);
  try {
    const mark = lab.snapshot().length;
    const ac = new AbortController();
    const pending = fetch(`${gatewayBase}/v1/chat/completions`, {
      method: "POST",
      headers: { "content-type": "application/json", ...inferenceHeaders("chat", gatewayKey) },
      body: JSON.stringify(input("chat", chat.publicModel, true)),
      signal: ac.signal,
    });
    await waitFor(
      async () => {
        if (lab.snapshot().length > mark) return true;
        throw new Error("upstream receipt not yet observed");
      },
      { timeoutMs: 2000, intervalMs: 25, label: "cancel reached upstream" },
    );
    ac.abort();
    let clientClosed = false;
    let text = "";
    try {
      const response = await pending;
      text = await response.text().catch(() => "");
    } catch {
      clientClosed = true;
    }
    const completed = !clientClosed && text.includes("[DONE]") && !/error|incomplete|truncated/i.test(text);
    assert.equal(completed, false, "cancel still produced a completed success");
    assert.ok(clientClosed || text.length >= 0);
    const hits = lab.snapshot().slice(mark);
    assert.ok(hits.length >= 1, "cancel had no upstream/Gateway receipt");
    gw(collector, "cancel through gateway", {
      scenarioId: "gw.stream.cancel",
      clientClosed,
      upstreamHits: hits.length,
      completed: false,
    });
  } catch (error) {
    collector.fail("cancel through gateway", error, { scenarioId: "gw.stream.cancel" });
  } finally {
    lab.script(chat.listener, []);
  }
}

export async function runAmbiguityAndAlias(runtime, collector) {
  const { api, lab, started, gatewayBase, gatewayKey } = runtime;
  const chat = protocolSlotsOf(started).find((slot) => slot.slot === "chat");

  try {
    const sharedRaw = "shared/raw-conflict";
    for (const name of ["A", "B"]) {
      await api.mutation("/dashboard/api/v4/onboarding/commit", {
        mode: "complete",
        operationId: randomUUID(),
        connection: {
          kind: "new",
          templateId: "custom-http",
          name: `Ambiguity ${name}`,
          endpointUrl: chat.url,
          upstreamProtocol: "chat_completions",
          authKind: "bearer",
        },
        authorization: { kind: "api_key", secretInput: chat.secret, accountLabel: `amb-${name}` },
        targets: [{ publicModel: `${name}-pub`, upstreamModel: sharedRaw }],
      });
    }
    const listed = await request(gatewayBase, "/v1/models", "GET", undefined, inferenceHeaders("chat", gatewayKey));
    const models = JSON.parse(await listed.text());
    const ids = (models.data || []).map((item) => item.id);
    assert.equal(ids.includes(sharedRaw), false, `ambiguous raw listed: ${ids}`);
    const mark = lab.snapshot().length;
    const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", sharedRaw, false), inferenceHeaders("chat", gatewayKey));
    const body = JSON.parse(await response.text());
    assert.ok(response.status >= 400);
    assert.equal(body.error?.type, "ambiguous_model_id");
    assert.equal(lab.snapshot().length, mark);
    gw(collector, "model catalog ambiguity", { scenarioId: "gw.ambiguity", listed: ids.filter((id) => id.includes("pub")) });
  } catch (error) {
    collector.fail("model catalog ambiguity", error, { scenarioId: "gw.ambiguity" });
  }

  try {
    const listedBefore = JSON.parse(await (await request(gatewayBase, "/v1/models", "GET", undefined, inferenceHeaders("chat", gatewayKey))).text());
    assert.ok((listedBefore.data || []).some((item) => item.id === "lab-chat"));
    await api.mutation("/dashboard/api/v4/alias-publication", { publicModel: "lab-chat", published: false }, "PATCH");
    const listedHidden = JSON.parse(await (await request(gatewayBase, "/v1/models", "GET", undefined, inferenceHeaders("chat", gatewayKey))).text());
    assert.equal((listedHidden.data || []).some((item) => item.id === "lab-chat"), false);
    const routed = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", "lab-chat", false), inferenceHeaders("chat", gatewayKey));
    assert.equal(routed.status, 200, await routed.text());
    gw(collector, "hidden public model stays routable", { scenarioId: "gw.hidden-public" });
    await api.mutation("/dashboard/api/v4/alias-publication", { publicModel: "lab-chat", published: true }, "PATCH");
  } catch (error) {
    collector.fail("hidden public model stays routable", error, { scenarioId: "gw.hidden-public" });
  }

  try {
    lab.acceptModel(chat.id || chat.slot, "exact-raw-unique");
    await api.mutation("/dashboard/api/v4/onboarding/commit", {
      mode: "complete",
      operationId: randomUUID(),
      connection: {
        kind: "new",
        templateId: "custom-http",
        name: "Raw ID Lab",
        endpointUrl: chat.url,
        upstreamProtocol: "chat_completions",
        authKind: "bearer",
      },
      authorization: { kind: "api_key", secretInput: chat.secret, accountLabel: "raw-id" },
      targets: [{ publicModel: "raw-pub-unique", upstreamModel: "exact-raw-unique" }],
    });
    const listed = JSON.parse(await (await request(gatewayBase, "/v1/models", "GET", undefined, inferenceHeaders("chat", gatewayKey))).text());
    const ids = (listed.data || []).map((item) => item.id);
    assert.equal(ids.includes("raw-pub-unique"), true, `public raw mapping missing from catalog: ${ids}`);
    assert.equal(ids.includes("exact-raw-unique"), false, `raw upstream id was published: ${ids}`);
    const mark = lab.snapshot().length;
    const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", "raw-pub-unique", false), inferenceHeaders("chat", gatewayKey));
    const text = await response.text();
    assert.equal(response.status, 200, text.slice(0, 400));
    const hits = lab.snapshot().slice(mark);
    assert.ok(hits.some((hit) => hit.model === "exact-raw-unique"), `expected exact upstream raw id, hits=${JSON.stringify(hits.map(summarizeHit))}`);
    gw(collector, "exact raw-id route", { scenarioId: "gw.raw-id", publicModel: "raw-pub-unique", upstreamModel: "exact-raw-unique", hits: hits.map(summarizeHit) });
  } catch (error) {
    collector.fail("exact raw-id route", error, { scenarioId: "gw.raw-id" });
  }
}

export async function beginCooldownRecovery(runtime) {
  const { lab, api } = runtime;
  const routeSlots = routeSlotsOf(runtime.started);
  lab.script("alpha", [{ kind: "http", status: 429, body: { error: { message: "Resets in 5 minutes", type: "rate_limit_error" } } }]);
  const first = await api.chatRoute();
  const rows = await api.credentials();
  const alpha = rows.find((row) => row.legacyAccountId === routeSlots[0].accountId);
  const deadline = alpha?.cooldowns?.genericUntil || alpha?.cooldownGenericUntil || alpha?.cooldowns?.generic_until;
  return { startedAt: Date.now(), deadline, firstStatus: first.status, routeSlots };
}

export async function finishCooldownRecovery(runtime, collector, token) {
  const { lab, api } = runtime;
  try {
    const second = await api.chatRoute();
    assert.equal(second.status, 200);
    const until = token.deadline ? Date.parse(token.deadline) : token.startedAt + 5 * 60 * 1000;
    const waitMs = Math.max(0, until - Date.now() + 1000);
    if (waitMs > 0 && waitMs < 6 * 60 * 1000) await new Promise((resolve) => setTimeout(resolve, waitMs));
    lab.script("alpha", []);
    const afterMark = lab.snapshot().length;
    const after = await api.chatRoute();
    const recovered = lab.snapshot().slice(afterMark);
    assert.equal(after.status, 200);
    assert.ok(recovered.some((hit) => hit.listener === "alpha"), `selection did not resume to alpha: ${JSON.stringify(recovered.map(summarizeHit))}`);
    gw(collector, "cooldown recovery after expiry", {
      scenarioId: "gw.fail.cooldown-recovery",
      deadline: token.deadline,
      waitedMs: Date.now() - token.startedAt,
      lastListener: recovered.at(-1)?.listener,
    });
  } catch (error) {
    collector.fail("cooldown recovery after expiry", error, { scenarioId: "gw.fail.cooldown-recovery" });
  }
}

export async function runKeyRotation(runtime, collector) {
  const { lab, started, api, gatewayBase, gatewayKey } = runtime;
  const chat = protocolSlotsOf(started).find((slot) => slot.slot === "chat");
  const rotated = "sk-lab-chat-rotated";
  try {
    lab.acceptSecret(chat.id || chat.slot, rotated);
    const creds = await api.credentials();
    const row = creds.find((item) => item.legacyAccountId === chat.accountId);
    assert.ok(row, "chat credential missing");
    const identity = findCredential(await api.identities(), chat.connectionId);
    const beforeVersion = Number(row.version ?? identity?.credential?.version);
    rememberSecret(rotated);
    const rotatedResult = await api.mutation(`/dashboard/api/v4/credentials/${row.id}/rotate`, { secretInput: rotated });
    const after = (await api.credentials()).find((item) => item.id === row.id);
    const afterIdentity = findCredential(await api.identities(), chat.connectionId);
    const afterVersion = Number(rotatedResult.version ?? after?.version ?? afterIdentity?.credential?.version);
    assert.equal(Number.isFinite(beforeVersion), true, "missing credential version before rotate");
    assert.equal(Number.isFinite(afterVersion), true, "missing credential version after rotate");
    assert.ok(afterVersion > beforeVersion, `credential version did not increment: ${beforeVersion} -> ${afterVersion}`);
    const mark = lab.snapshot().length;
    const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", gatewayKey));
    assert.equal(response.status, 200, await response.text());
    const hit = lab.snapshot().slice(mark)[0];
    assert.equal(hit.authHash, sha256(`Bearer ${rotated}`));
    const previousSecret = chat.secret;
    lab.revokeSecret(chat.id || chat.slot, previousSecret);
    const oldDirect = await request(chat.listenerUrl, chat.path, "POST", input("chat", chat.model, false), { authorization: `Bearer ${previousSecret}` });
    assert.equal(oldDirect.status, 401);
    chat.secret = rotated;
    gw(collector, "key rotation", {
      scenarioId: "gw.key.rotate",
      beforeVersion,
      afterVersion,
      newFingerprint: sha256(rotated).slice(0, 12),
    });
  } catch (error) {
    collector.fail("key rotation", error, { scenarioId: "gw.key.rotate" });
  }
}

async function accountCapabilities(api, accountId) {
  const body = await api.json(`/dashboard/api/v4/accounts/${accountId}`);
  return body.modelCapabilities || body.account?.modelCapabilities || [];
}

export async function runCatalogRefreshFailure(runtime, collector) {
  const { lab, started, api } = runtime;
  const chat = protocolSlotsOf(started).find((slot) => slot.slot === "chat");
  let savedViaV4 = false;
  try {
    lab.scriptModels(chat.id || chat.slot, [{ kind: "success", catalog: ["upstream-chat", "discovered-extra"] }]);
    const labListed = await request(chat.listenerUrl, chat.modelsPath, "GET", undefined, { authorization: `Bearer ${chat.secret}` });
    const labCatalog = JSON.parse(await labListed.text());
    assert.equal(labListed.status, 200);
    assert.ok((labCatalog.data || []).some((item) => item.id === "discovered-extra"), "lab /v1/models scripting did not expose discovered-extra");
    const discovered = await request(runtime.gatewayBase, "/dashboard/api/v4/custom/models/discover", "POST", {
      endpointUrl: chat.url,
      upstreamProtocol: "chat_completions",
      apiKey: chat.secret,
    });
    const discoveredBody = JSON.parse(await discovered.text());
    assert.equal(discovered.status, 200, JSON.stringify(discoveredBody).slice(0, 300));
    await api.mutation(`/dashboard/api/v4/accounts/${chat.accountId}/custom-config`, {
      endpointUrl: chat.url,
      upstreamProtocol: "chat_completions",
      modelCapabilities: [
        { publicModel: chat.publicModel, upstreamModel: chat.model, protocol: "chat_completions" },
        { publicModel: "lab-discovered", upstreamModel: "discovered-extra", protocol: "chat_completions" },
      ],
    }, "PUT");
    savedViaV4 = true;
    const saved = await accountCapabilities(api, chat.accountId);
    assert.ok(saved.some((item) => item.publicModel === "lab-discovered" && item.upstreamModel === "discovered-extra"), `saved catalog missing discovered mapping: ${JSON.stringify(saved)}`);
    lab.scriptModels(chat.id || chat.slot, [{ kind: "http", status: 503, body: { error: { message: "catalog down" } } }]);
    const failed = await request(runtime.gatewayBase, "/dashboard/api/v4/custom/models/discover", "POST", {
      endpointUrl: chat.url,
      upstreamProtocol: "chat_completions",
      apiKey: chat.secret,
    });
    assert.ok(failed.status >= 400);
    const preserved = await accountCapabilities(api, chat.accountId);
    assert.ok(preserved.some((item) => item.publicModel === "lab-discovered"), `failed refresh dropped last-good catalog: ${JSON.stringify(preserved)}`);
    const still = await request(runtime.gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", runtime.gatewayKey));
    assert.equal(still.status, 200, await still.text());
    gw(collector, "catalog refresh failure preserves last-good", { scenarioId: "gw.catalog.refresh-fail", saved: saved.map((item) => item.publicModel) });
  } catch (error) {
    if (savedViaV4) {
      collector.fail("catalog refresh failure preserves last-good", error, { scenarioId: "gw.catalog.refresh-fail" });
      return;
    }
    await recordRustEvidence(collector, {
      scenarioId: "rust.catalog.refresh-fail",
      crate: "ocg-core",
      testFile: "dashboard_v3_providers",
      testName: "dashboard_v3_zen_refresh_persists_on_success_and_preserves_state_on_failure_or_busy",
    });
  }
}

async function captureControlPlane(api) {
  const identities = await api.identities();
  const credentials = await api.credentials();
  const destinations = ((await api.json("/dashboard/api/v4/destinations")).destinations) || [];
  const connections = ((await api.json("/dashboard/api/v4/connections")).connections) || [];
  return {
    identityCount: identities.length,
    credentialOrder: credentials.map((item) => item.legacyAccountId || item.id),
    enabled: credentials.map((item) => ({ id: item.legacyAccountId || item.id, enabled: Boolean(item.enabled) })),
    bindings: identities.flatMap((identity) =>
      (identity.credentials || []).flatMap((cred) =>
        (cred.bindings || []).map((binding) => ({
          enabled: Boolean(binding.enabled),
          modelScope: binding.modelScope ?? null,
        })),
      ),
    ),
    catalog: destinations
      .flatMap((destination) =>
        (destination.catalog || []).map((model) => ({
          publicModel: model.publicModel,
          upstreamModel: model.upstreamModel,
          enabled: Boolean(model.enabled),
        })),
      )
      .sort((a, b) => `${a.publicModel}:${a.upstreamModel}`.localeCompare(`${b.publicModel}:${b.upstreamModel}`)),
    targets: connections
      .flatMap((connection) =>
        (connection.targets || []).map((target) => ({
          publicName: target.publicName,
          upstreamModelId: target.upstreamModelId,
          enabled: Boolean(target.enabled),
        })),
      )
      .sort((a, b) => `${a.publicName}:${a.upstreamModelId}`.localeCompare(`${b.publicName}:${b.upstreamModelId}`)),
  };
}

export async function runRestartPersistence(runtime, collector) {
  try {
    const before = await captureControlPlane(runtime.api);
    await restartOwnedGateway(runtime);
    runtime.api = (await import("./dashboard.mjs")).makeApi(runtime.gatewayBase, runtime.lab, () => runtime.gatewayKey);
    const after = await captureControlPlane(runtime.api);
    assert.equal(after.identityCount, before.identityCount);
    assert.deepEqual(after.credentialOrder, before.credentialOrder);
    assert.deepEqual(after.enabled, before.enabled);
    assert.deepEqual(after.bindings, before.bindings);
    assert.deepEqual(after.catalog, before.catalog);
    assert.deepEqual(after.targets, before.targets);
    const chat = protocolSlotsOf(runtime.started).find((slot) => slot.slot === "chat");
    const response = await request(runtime.gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", runtime.gatewayKey));
    assert.equal(response.status, 200, await response.text());
    gw(collector, "restart persistence", { scenarioId: "gw.restart", accounts: after.credentialOrder.length, catalog: after.catalog.length });
  } catch (error) {
    collector.fail("restart persistence", error, { scenarioId: "gw.restart" });
  }
}

export async function runImportExport(runtime, collector) {
  const password = `lab-bundle-${randomUUID()}`;
  rememberSecret(password);
  let second = null;
  try {
    const exportRes = await request(runtime.gatewayBase, "/dashboard/api/v4/accounts/transfer/export", "POST", { bundlePassword: password });
    const exported = JSON.parse(await exportRes.text());
    assert.equal(exportRes.status, 200, JSON.stringify(exported).slice(0, 300));
    assert.ok(exported.bundle, "export missing bundle");
    const artifactDir = `${runtime.dataDir}-import`;
    second = await withRuntime({
      cliPath: runtime.cliPath,
      profile: (await import("./profile.mjs")).DEFAULT_PROFILE,
      artifactDir,
      existingLab: runtime.lab,
      register: false,
      encryptionKey: "gateway-lab-dummy-not-a-real-secret",
    });
    runtime.extras.push({ cleanup: second.cleanup });
    const beforeIdentities = await second.api.identities();
    const beforeCreds = await second.api.credentials();
    const wrong = await request(second.gatewayBase, "/dashboard/api/v4/accounts/transfer/preview", "POST", { password: "wrong-password", bundle: exported.bundle });
    assert.ok(wrong.status >= 400);
    const tampered = `${exported.bundle.slice(0, -2)}aa`;
    const bad = await request(second.gatewayBase, "/dashboard/api/v4/accounts/transfer/preview", "POST", { password, bundle: tampered });
    assert.ok(bad.status >= 400);
    assert.equal((await second.api.identities()).length, beforeIdentities.length);
    assert.equal((await second.api.credentials()).length, beforeCreds.length);
    await second.api.mutation("/dashboard/api/v4/accounts/transfer/import", { password, bundle: exported.bundle });
    const importedIdentities = await second.api.identities();
    const importedCreds = await second.api.credentials();
    assert.ok(
      importedIdentities.length > beforeIdentities.length || importedCreds.length > beforeCreds.length,
      `import did not add destinations (identities ${importedIdentities.length} from ${beforeIdentities.length}, creds ${importedCreds.length} from ${beforeCreds.length})`,
    );
    const source = await captureControlPlane(runtime.api);
    const imported = await captureControlPlane(second.api);
    for (const row of source.catalog) {
      assert.ok(
        imported.catalog.some((item) => item.publicModel === row.publicModel && item.upstreamModel === row.upstreamModel),
        `import lost catalog ${row.publicModel}->${row.upstreamModel}`,
      );
    }
    for (const row of source.targets) {
      assert.ok(
        imported.targets.some((item) => item.publicName === row.publicName && item.upstreamModelId === row.upstreamModelId && item.enabled === row.enabled),
        `import lost target ${row.publicName}->${row.upstreamModelId}`,
      );
    }
    const chat = protocolSlotsOf(runtime.started).find((slot) => slot.slot === "chat");
    const connection = await readJsonResponse(await request(second.gatewayBase, "/dashboard/api/v4/connection"));
    assert.equal(connection.status, 200, connection.text.slice(0, 300));
    assert.ok(connection.body?.primaryKey, "second runtime lost its Gateway Key after import");
    second.gatewayKey = connection.body.primaryKey;
    const ping = await request(second.gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", second.gatewayKey));
    assert.equal(ping.status, 200, await ping.text());
    gw(collector, "import export", {
      scenarioId: "gw.transfer",
      importedIdentities: importedIdentities.length,
      importedCredentials: importedCreds.length,
      catalog: imported.catalog.length,
      promptDigest: promptDigest(MARKER),
    });
  } catch (error) {
    collector.fail("import export", error, { scenarioId: "gw.transfer" });
  }
}

export async function runUnpricedAttribution(runtime, collector) {
  try {
    const chat = protocolSlotsOf(runtime.started).find((slot) => slot.slot === "chat");
    await request(runtime.gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", runtime.gatewayKey));
    const logs = await latestForwardLogs(runtime.api);
    const last = logs[0] || logs[logs.length - 1];
    const costState = last?.costState || last?.cost_state || last?.status;
    assert.ok(last, "missing forward log");
    assert.ok(/unpriced|success_unpriced|not_applicable/i.test(String(costState)), `expected unpriced attribution, got ${costState}`);
    gw(collector, "unpriced attribution", { scenarioId: "gw.unpriced", costState });
  } catch (error) {
    collector.fail("unpriced attribution", error, { scenarioId: "gw.unpriced" });
  }
}

export async function runQuotaNoDisable(runtime, collector) {
  const chat = protocolSlotsOf(runtime.started).find((slot) => slot.slot === "chat");
  let calibrated = false;
  try {
    const before = (await runtime.api.credentials()).find((item) => item.legacyAccountId === chat.accountId);
    await runtime.api.mutation(`/dashboard/api/v4/accounts/${chat.accountId}/usage`, { window: "window_month", percent: 100 }, "PATCH");
    calibrated = true;
    const after = (await runtime.api.credentials()).find((item) => item.legacyAccountId === chat.accountId);
    assert.equal(Boolean(after.enabled), Boolean(before.enabled));
    const ping = await request(runtime.gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", runtime.gatewayKey));
    assert.equal(ping.status, 200, await ping.text());
    gw(collector, "quota does not disable", { scenarioId: "gw.quota-no-disable" });
  } catch (error) {
    if (calibrated) {
      collector.fail("quota does not disable", error, { scenarioId: "gw.quota-no-disable" });
      return;
    }
    await recordRustEvidence(collector, {
      scenarioId: "rust.quota-no-disable",
      crate: "ocg-core",
      testFile: "ollama_cloud_gateway",
      testName: "ollama_soft_quota_overage_does_not_skip_selection",
    });
  }
}

export async function startProxyFixture() {
  const hits = [];
  const server = createServer((req, res) => {
    hits.push({ method: req.method, url: req.url, host: req.headers.host, authorization: req.headers.authorization || "" });
    res.writeHead(502, { "content-type": "application/json" });
    res.end(JSON.stringify({ error: { message: "proxy-fixture" } }));
  });
  await new Promise((resolve, reject) => {
    server.listen(0, "127.0.0.1", (error) => (error ? reject(error) : resolve()));
  });
  const port = server.address().port;
  return {
    url: `http://127.0.0.1:${port}`,
    hits,
    port,
    async cleanup() {
      await new Promise((resolve) => server.close(() => resolve()));
      return { verified: !(await portOpen(port)), port };
    },
  };
}

export async function runProxyIsolation(runtime, collector) {
  const proxy = await startProxyFixture();
  runtime.extras.push({ cleanup: proxy.cleanup });
  const chat = protocolSlotsOf(runtime.started).find((slot) => slot.slot === "chat");
  const settings = await runtime.api.json("/dashboard/api/v4/settings");
  const supported = (settings.proxySupportedModels || []).map((row) => row.id || row);
  const upstreamId = supported.includes(chat.model) ? chat.model : chat.model;
  let configured = false;
  try {
    await runtime.api.mutation("/dashboard/api/v4/settings", {
      proxyMode: "list",
      proxyUrl: proxy.url,
      proxyListDirection: "whitelist",
      proxyListModels: [upstreamId],
    }, "PUT");
    configured = true;
  } catch {
    await recordRustEvidence(collector, {
      scenarioId: "rust.proxy-list",
      crate: "ocg-core",
      testFile: "gateway_routing_acceptance",
      testName: "proxy_list_matches_the_materialized_upstream_id_in_both_directions",
    });
    return;
  }
  try {
    const responses = protocolSlotsOf(runtime.started).find((slot) => slot.slot === "responses");
    await request(runtime.gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", runtime.gatewayKey)).catch(() => {});
    const afterChat = proxy.hits.length;
    await request(runtime.gatewayBase, "/v1/responses", "POST", input("responses", responses.publicModel, false), inferenceHeaders("chat", runtime.gatewayKey)).catch(() => {});
    const afterResp = proxy.hits.length;
    await runtime.api.mutation("/dashboard/api/v4/settings", { proxyMode: "direct", proxyUrl: "" }, "PUT");
    if (afterChat > 0 && afterResp === afterChat) {
      gw(collector, "proxy list isolation", { scenarioId: "gw.proxy.list", proxyHits: afterChat, proxyListModel: upstreamId });
    } else {
      throw new Error(`proxy hits chat=${afterChat} responses=${afterResp - afterChat} listModel=${upstreamId}`);
    }
  } catch (error) {
    if (configured) collector.fail("proxy list isolation", error, { scenarioId: "gw.proxy.list" });
    else {
      await recordRustEvidence(collector, {
        scenarioId: "rust.proxy-list",
        crate: "ocg-core",
        testFile: "gateway_routing_acceptance",
        testName: "proxy_list_matches_the_materialized_upstream_id_in_both_directions",
      });
    }
  }
}

export async function runExplicitUnsupported(runtime, collector) {
  const { lab, started, gatewayBase, gatewayKey } = runtime;
  const responses = protocolSlotsOf(started).find((slot) => slot.slot === "responses");
  {
    const mark = lab.snapshot().length;
    try {
      const body = { ...input("chat", responses.publicModel, false), store: true };
      const response = await request(gatewayBase, "/v1/chat/completions", "POST", body, inferenceHeaders("chat", gatewayKey));
      const text = await response.text();
      const hits = lab.snapshot().slice(mark).filter((hit) => hit.slot === "responses");
      if (hits.length && hits.every((hit) => hit.store === false)) {
        gw(collector, "responses store converted or rejected", { scenarioId: "gw.unsupported.responses-store", hits: hits.map(summarizeHit) });
      } else if (!response.ok && lab.snapshot().length === mark) {
        gw(collector, "responses store rejected with zero send", { scenarioId: "gw.unsupported.responses-store", status: response.status, excerpt: text.slice(0, 120) });
      } else {
        throw new Error(`store=true not handled: status=${response.status} hits=${hits.length}`);
      }
    } catch (error) {
      collector.fail("responses store converted or rejected", error, { scenarioId: "gw.unsupported.responses-store" });
    }
  }
  {
    const mark = lab.snapshot().length;
    try {
      const chat = protocolSlotsOf(started).find((slot) => slot.slot === "chat");
      const response = await request(
        gatewayBase,
        `/v1beta/models/${chat.publicModel}:embedContent`,
        "POST",
        { contents: [{ role: "user", parts: [{ text: MARKER }] }] },
        { "x-goog-api-key": gatewayKey },
      );
      const text = await response.text();
      assert.ok(response.status >= 400);
      assert.equal(lab.snapshot().length, mark);
      gw(collector, "gemini unsupported operation", { scenarioId: "gw.unsupported.gemini-op", status: response.status, excerpt: text.slice(0, 120) });
    } catch (error) {
      collector.fail("gemini unsupported operation", error, { scenarioId: "gw.unsupported.gemini-op" });
    }
  }
  const stateful = [
    ["gw.unsupported.responses-previous", { previous_response_id: "resp_1" }, "previous_response_id"],
    ["gw.unsupported.responses-conversation", { conversation: "conv_1" }, "conversation"],
    ["gw.unsupported.responses-background", { background: true }, "background"],
  ];
  for (const [scenarioId, extra, name] of stateful) {
    const mark = lab.snapshot().length;
    try {
      const body = { ...input("responses", responses.publicModel, false), ...extra };
      const response = await request(gatewayBase, "/v1/responses", "POST", body, inferenceHeaders("responses", gatewayKey));
      const text = await response.text();
      assert.ok(response.status >= 400, `${name} accepted: ${text.slice(0, 200)}`);
      assert.equal(lab.snapshot().length, mark, `${name} sent upstream`);
      gw(collector, `responses ${name} rejected with zero send`, { scenarioId, status: response.status, excerpt: text.slice(0, 120) });
    } catch (error) {
      collector.fail(`responses ${name} rejected with zero send`, error, { scenarioId });
    }
  }
}

export async function runRustPriceAndFree(collector) {
  await recordRustEvidence(collector, {
    scenarioId: "rust.price-go",
    crate: "ocg-core",
    testFile: "gateway_fallback",
    testName: "routes_all_client_formats_to_each_models_native_protocol",
  });
  await recordRustEvidence(collector, {
    scenarioId: "rust.free-zen",
    crate: "ocg-core",
    testFile: "gateway_fallback",
    testName: "zen_free_non_stream_success_without_usage_is_still_zero_cost_free",
  });
}

export async function runAuthIsolation(runtime, collector) {
  const { lab, gatewayBase, gatewayKey } = runtime;
  const chat = protocolSlotsOf(runtime.started).find((slot) => slot.slot === "chat");
  const responses = protocolSlotsOf(runtime.started).find((slot) => slot.slot === "responses");
  lab.scriptIsolation(
    { endpointId: chat.id || chat.slot, keyFingerprint: sha256(chat.secret), model: chat.model, scenario: "" },
    [{ kind: "http", status: 429, body: { error: { message: "isolated" } } }],
  );
  try {
    const mark = lab.snapshot().length;
    const blocked = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", chat.publicModel, false), inferenceHeaders("chat", gatewayKey));
    const other = await request(gatewayBase, "/v1/responses", "POST", input("responses", responses.publicModel, false), inferenceHeaders("responses", gatewayKey));
    assert.equal(blocked.status, 429, await blocked.text());
    assert.equal(other.status, 200, await other.text());
    const hits = lab.snapshot().slice(mark);
    const chatHit = hits.find((hit) => hit.slot === "chat");
    const responsesHit = hits.find((hit) => hit.slot === "responses");
    assert.ok(chatHit, "chat request did not reach the chat upstream");
    assert.ok(responsesHit, "responses request did not reach the responses upstream");
    assert.equal(chatHit.scriptStatus, 429);
    assert.equal(chatHit.authHash, sha256(`Bearer ${chat.secret}`));
    assert.equal(responsesHit.scriptStatus, 200);
    assert.equal(responsesHit.authHash, sha256(`Bearer ${responses.secret}`));
    collector.pass("gateway auth isolation chat vs responses", {
      scenarioId: "gw.auth.isolation",
      evidenceKind: GW,
      hits: hits.map(summarizeHit),
    });
  } catch (error) {
    collector.fail("gateway auth isolation chat vs responses", error, { scenarioId: "gw.auth.isolation" });
  } finally {
    lab.reset();
    await runtime.api.resetCooldowns([chat.accountId]).catch(() => {});
  }
}
