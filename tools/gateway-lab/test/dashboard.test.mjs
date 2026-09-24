import assert from "node:assert/strict";
import test from "node:test";
import { assertGeminiLiveStream, assertLiveRemoteReceipt, makeApi } from "../lib/dashboard.mjs";
import { createCollector, exitCodeFor, resultsMarkdown, writeReportDir } from "../lib/report.mjs";
import { loadProfile } from "../lib/profile.mjs";
import { parseArgv } from "../lib/common.mjs";
import { applyCoverageChecklist } from "../lib/coverage.mjs";

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

test("dashboard client constructs V4 requests with CAS and never calls V3", async () => {
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

    const order = recording.calls.find((call) => call.method === "PUT" && call.url.endsWith("/dashboard/api/v4/accounts/order"));
    assert.ok(order);
    assert.deepEqual(order.body.accountIds, ["acc-bravo", "acc-alpha", "acc-zen"]);
    assert.equal(order.body.expectedRevision, 7);
    assert.equal(order.body.processGeneration, 99);
  } finally {
    recording.restore();
  }
});

test("default profile has three protocols, independent Keys, and a shared public model", async () => {
  const profile = await loadProfile("default");
  const protocols = new Set(profile.endpoints.map((item) => item.protocol));
  assert.deepEqual([...protocols].sort(), ["chat_completions", "messages", "responses"]);
  const secrets = new Set(profile.endpoints.map((item) => item.secret));
  assert.equal(secrets.size, profile.endpoints.length);
  const shared = profile.endpoints.filter((item) => item.publicModel === "lab-route");
  assert.ok(shared.length >= 2);
  const independents = new Set(
    profile.endpoints.filter((item) => ["chat", "responses", "messages"].includes(item.id)).map((item) => item.publicModel),
  );
  assert.equal(independents.size, 3);
});

test("unsupported and not_run do not count as pass", () => {
  const collector = createCollector();
  collector.unsupported("gemini", "binary");
  collector.notRun("live", "missing env");
  const counts = collector.counts();
  assert.equal(counts.PASS, 0);
  assert.equal(exitCodeFor(counts, { suite: "live", liveBlocked: true }), 2);
});

test("PASS plus UNSUPPORTED or NOT_RUN exits 2; FAIL exits 1", () => {
  const mixed = createCollector();
  mixed.pass("ok");
  mixed.unsupported("gemini", "binary");
  assert.equal(exitCodeFor(mixed.counts()), 2);
  const notRun = createCollector();
  notRun.pass("ok");
  notRun.notRun("gap", "not executed");
  assert.equal(exitCodeFor(notRun.counts()), 2);
  const failed = createCollector();
  failed.fail("broke", new Error("x"));
  failed.unsupported("gemini", "binary");
  assert.equal(exitCodeFor(failed.counts()), 1);
});

test("pass extras may include HTTP status without replacing PASS", () => {
  const collector = createCollector();
  collector.pass("chat JSON", { status: 200, upstreamSends: 1 });
  assert.equal(collector.counts().PASS, 1);
  assert.equal(collector.results[0].status, "PASS");
  assert.equal(collector.results[0].httpStatus, 200);
});

test("redaction strips keys nested in error strings and bundle passwords", async () => {
  const { redactDeep, rememberSecret } = await import("../lib/common.mjs");
  const arbitrary = "lab-token-arbitrary-7f3a9c21";
  rememberSecret(arbitrary);
  const redacted = redactDeep({
    error: { message: "upstream said sk-live-secret-value is bad" },
    nested: { authorization: "Bearer super-secret-token-value" },
    bundlePassword: "export-pass",
    bundle: "AAAA",
    errors: [{ details: { secretInput: "sk-dummy-rotate-key", nested: "Bearer nested-token" } }],
    provider: { message: `provider echoed ${arbitrary} in a remote error` },
  });
  assert.equal(String(redacted.error.message).includes("sk-live-secret-value"), false);
  assert.equal(String(redacted.nested.authorization).includes("super-secret-token-value"), false);
  assert.match(String(redacted.bundlePassword), /redacted/);
  assert.match(String(redacted.bundle), /redacted/);
  assert.equal(JSON.stringify(redacted).includes("sk-dummy-rotate-key"), false);
  assert.equal(JSON.stringify(redacted).includes("nested-token"), false);
  assert.equal(JSON.stringify(redacted).includes(arbitrary), false);
});

test("RESULTS.md and report.json redact arbitrary remembered secrets", async () => {
  const { mkdtemp, readFile } = await import("node:fs/promises");
  const os = await import("node:os");
  const path = await import("node:path");
  const { rememberSecret } = await import("../lib/common.mjs");
  const secret = "lab-token-markdown-leak-9c81ff42";
  rememberSecret(secret);
  const collector = createCollector();
  collector.fail("provider error", new Error(`upstream said ${secret} is invalid`));
  const dir = await mkdtemp(path.join(os.tmpdir(), "gw-lab-redact-"));
  const report = {
    generatedAt: new Date().toISOString(),
    counts: collector.counts(),
    results: collector.results,
    replay: "node tools/gateway-lab/cli.mjs verify",
  };
  await writeReportDir(dir, {
    "report.json": report,
    "RESULTS.md": resultsMarkdown(report),
  });
  const json = await readFile(path.join(dir, "report.json"), "utf8");
  const md = await readFile(path.join(dir, "RESULTS.md"), "utf8");
  assert.equal(json.includes(secret), false);
  assert.equal(md.includes(secret), false);
  assert.match(json, /redacted/);
  assert.match(md, /redacted/);
});

test("gemini live SSE requires finishReason terminal and exact live receipt", () => {
  assert.throws(
    () => assertGeminiLiveStream("data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"hi\"}]}}]}\n\n"),
    /finishReason/,
  );
  const objects = assertGeminiLiveStream(
    "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"hi\"}]},\"finishReason\":\"STOP\"}]}\n\n",
  );
  assert.equal(objects[0].candidates[0].finishReason, "STOP");
  const okLab = { snapshot: () => [{ liveStatus: 200 }, { liveStatus: 200 }] };
  const live = { stats: () => ({ calls: 4 }) };
  assert.throws(() => assertLiveRemoteReceipt(okLab, live, 1, 4), /exactly one remote call/);
  assert.throws(() => assertLiveRemoteReceipt(okLab, { stats: () => ({ calls: 5 }) }, 0, 4), /exactly one receipt/);
  const hits = assertLiveRemoteReceipt({ snapshot: () => [{ liveStatus: 200 }] }, { stats: () => ({ calls: 1 }) }, 0, 0);
  assert.equal(hits[0].liveStatus, 200);
  assert.throws(
    () => assertLiveRemoteReceipt({ snapshot: () => [{ liveStatus: 502 }] }, { stats: () => ({ calls: 1 }) }, 0, 0),
    /liveStatus/,
  );
});

test("profile rejects non-loopback hosts and live model overrides", async () => {
  const { normalizeProfile, DEFAULT_PROFILE } = await import("../lib/profile.mjs");
  assert.throws(() => normalizeProfile({ ...DEFAULT_PROFILE, host: "0.0.0.0" }), /loopback/);
  assert.throws(() => normalizeProfile({ ...DEFAULT_PROFILE, live: { model: "other-model" } }), /minimax-m3/);
});

test("coverage requires scenarioId evidence and rejects lab_fixture for gateway-only items", () => {
  const collector = createCollector();
  collector.pass("lab fixture cannot cover ambiguity", { scenarioId: "gw.ambiguity", evidenceKind: "lab_fixture" });
  collector.pass("duplicate label 4xx", { scenarioId: "random.4xx", evidenceKind: "gateway_black_box" });
  applyCoverageChecklist(collector);
  const gap = collector.results.find((row) => row.requirement === "model-catalog-ambiguity");
  assert.ok(gap);
  assert.equal(gap.status, "NOT_RUN");
});

test("CLI parse accepts verify --cli and --suite", () => {
  const parsed = parseArgv(["verify", "--cli", "ocg.exe", "--suite", "local"]);
  assert.equal(parsed.command, "verify");
  assert.equal(parsed.flags.cli, "ocg.exe");
  assert.equal(parsed.flags.suite, "local");
  const help = parseArgv(["--help"]);
  assert.equal(help.command, "");
  assert.equal(help.flags.help, true);
});
