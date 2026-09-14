import assert from "node:assert/strict";
import { createHash, randomUUID } from "node:crypto";
import { execFileSync, spawnSync, spawn } from "node:child_process";
import { createServer } from "node:http";
import net from "node:net";
import { existsSync, openSync, closeSync, readFileSync } from "node:fs";
import { mkdir, rm, writeFile, readFile, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createLab } from "./server.mjs";

const sourceDir = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.resolve(sourceDir, "..", "..");
const here = path.join(repoRoot, ".artifacts", "routing-lab");
const MARKER = "LAB_PROBE";
const TOOL_NAME = "lab_echo";
const SCHEMA = { type: "object", properties: { value: { type: "string" } }, required: ["value"] };
const FORBIDDEN_PORTS = new Set([19143, 19144]);
const REQUEST_TIMEOUT_MS = 20000;

function parseArgs(argv) {
  const args = { shadow: false, cli: null };
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (token === "--shadow") args.shadow = true;
    else if (token === "--cli") args.cli = argv[++i];
    else if (token.startsWith("--cli=")) args.cli = token.slice("--cli=".length);
    else if (!token.startsWith("-") && !args.cli) args.cli = token;
  }
  return args;
}

function sha256Buffer(buf) {
  return createHash("sha256").update(buf).digest("hex");
}

function sha256Text(value) {
  return createHash("sha256").update(value).digest("hex");
}

function authHash(slot) {
  return sha256Text(slot.auth === "bearer" ? `Bearer ${slot.secret}` : slot.secret);
}

async function pickLoopbackPort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      server.close((error) => {
        if (error) reject(error);
        else if (FORBIDDEN_PORTS.has(port)) pickLoopbackPort().then(resolve, reject);
        else resolve(port);
      });
    });
  });
}

const ownedChildren = new Map();
function startHidden(executable, { cwd, stdout, stderr, argsPath, shadow = false }) {
  const args = JSON.parse(readFileSync(argsPath, "utf8"));
  assert.ok(Array.isArray(args) && args.every((arg) => typeof arg === "string"));
  const env = { ...process.env };
  for (const name of ["OCG_MANAGER_ENCRYPTION_KEY", "OCG_GATEWAY_PORT", "OCG_CLIENT_ROOT_URL", "OCG_ADMIN_USERNAME", "OCG_ADMIN_PASSWORD", "OCG_SHADOW_COMPARE"]) delete env[name];
  if (shadow) env.OCG_SHADOW_COMPARE = "1";
  const out = openSync(stdout, "a");
  const err = openSync(stderr, "a");
  try {
    const child = spawn(executable, args, { cwd, env, windowsHide: true, stdio: ["ignore", out, err] });
    child.on("error", (error) => { child.launchError = error; });
    if (!child.pid) throw new Error(`Could not launch ${executable}`);
    ownedChildren.set(child.pid, child);
    return child.pid;
  } finally {
    closeSync(out);
    closeSync(err);
  }
}

function portOpen(port) {
  return new Promise((resolve) => {
    const socket = net.connect({ host: "127.0.0.1", port }, () => {
      socket.destroy();
      resolve(true);
    });
    socket.setTimeout(400, () => {
      socket.destroy();
      resolve(false);
    });
    socket.on("error", () => resolve(false));
  });
}

function processAlive(pid) {
  const child = ownedChildren.get(pid);
  return !!child && !child.launchError && child.exitCode === null && child.signalCode === null;
}

function stopPid(pid) {
  const child = ownedChildren.get(pid);
  if (child && processAlive(pid)) child.kill();
}

async function waitFor(fn, { timeoutMs, intervalMs, label }) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      return await fn();
    } catch (error) {
      lastError = error;
      await new Promise((resolve) => setTimeout(resolve, intervalMs));
    }
  }
  throw new Error(`${label}: ${lastError?.message || lastError}`);
}

async function request(base, pathName, method = "GET", body, headers = {}, timeoutMs = REQUEST_TIMEOUT_MS) {
  return fetch(base + pathName, {
    method,
    headers: { "content-type": "application/json", ...headers },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
}

async function readJsonResponse(response) {
  const text = await response.text();
  try {
    return { status: response.status, body: JSON.parse(text), text, headers: response.headers };
  } catch {
    return { status: response.status, body: null, text, headers: response.headers };
  }
}

function input(client, model, stream) {
  if (client === "chat") {
    return {
      model,
      stream,
      max_tokens: 64,
      messages: [
        { role: "system", content: "Preserve the routing lab message." },
        { role: "user", content: MARKER },
      ],
      tools: [{ type: "function", function: { name: TOOL_NAME, description: "Echo a value", parameters: SCHEMA } }],
    };
  }
  if (client === "responses") {
    return {
      model,
      stream,
      store: false,
      max_output_tokens: 64,
      instructions: "Preserve the routing lab message.",
      input: [{ role: "user", content: [{ type: "input_text", text: MARKER }] }],
      tools: [{ type: "function", name: TOOL_NAME, description: "Echo a value", parameters: SCHEMA }],
    };
  }
  if (client === "gemini") {
    return {
      contents: [{ role: "user", parts: [{ text: MARKER }] }],
    };
  }
  return {
    model,
    stream,
    max_tokens: 64,
    system: "Preserve the routing lab message.",
    messages: [{ role: "user", content: [{ type: "text", text: MARKER }] }],
    tools: [{ name: TOOL_NAME, description: "Echo a value", input_schema: SCHEMA }],
  };
}

function clientPath(client, model, stream) {
  if (client === "chat") return "/v1/chat/completions";
  if (client === "responses") return "/v1/responses";
  if (client === "messages") return "/v1/messages";
  const action = stream ? "streamGenerateContent" : "generateContent";
  return `/v1beta/models/${model}:${action}`;
}

function checkOutput(client, stream, body, marker) {
  if (client === "gemini") {
    if (stream) {
      const data = body
        .split(/\r?\n/)
        .filter((line) => line.startsWith("data: "))
        .map((line) => line.slice(6))
        .filter((line) => line && line !== "[DONE]");
      assert.ok(data.length > 0, "No Gemini SSE objects");
      const objects = data.map((line) => JSON.parse(line));
      const text = objects
        .flatMap((item) => item.candidates ?? [])
        .flatMap((candidate) => candidate.content?.parts ?? [])
        .map((part) => part.text ?? "")
        .join("");
      assert.equal(text, marker, "Gemini SSE text changed/duplicated");
      return;
    }
    const parsed = JSON.parse(body);
    const text = (parsed.candidates ?? [])
      .flatMap((candidate) => candidate.content?.parts ?? [])
      .map((part) => part.text ?? "")
      .join("");
    assert.equal(text, marker, "Gemini JSON text changed/duplicated");
    return;
  }
  if (stream) {
    const data = body.split(/\r?\n/).filter((line) => line.startsWith("data: ")).map((line) => line.slice(6));
    const objects = data.filter((line) => line !== "[DONE]").map((line) => JSON.parse(line));
    assert.ok(objects.length > 0, "No SSE objects");
    let text = "";
    if (client === "chat") {
      text = objects.map((item) => item.choices?.[0]?.delta?.content ?? "").join("");
      assert.ok(data.includes("[DONE]"), "Missing Chat terminal event");
      assert.ok(objects.some((item) => item.object === "chat.completion.chunk"));
    } else if (client === "responses") {
      text = objects.filter((item) => item.type === "response.output_text.delta").map((item) => item.delta).join("");
      assert.ok(objects.some((item) => item.type === "response.completed" && item.response?.status === "completed"));
    } else {
      text = objects.filter((item) => item.type === "content_block_delta").map((item) => item.delta?.text ?? "").join("");
      assert.ok(objects.some((item) => item.type === "message_start"));
      assert.ok(objects.some((item) => item.type === "message_stop"));
    }
    assert.equal(text, marker, "SSE text changed/duplicated");
    return;
  }
  const parsed = JSON.parse(body);
  if (client === "chat") {
    assert.equal(parsed.object, "chat.completion");
    assert.equal(parsed.choices[0].message.role, "assistant");
    assert.equal(parsed.choices[0].message.content, marker);
  } else if (client === "responses") {
    assert.equal(parsed.object, "response");
    assert.equal(parsed.status, "completed");
    assert.equal(
      parsed.output.flatMap((item) => item.content ?? []).filter((part) => part.type === "output_text").map((part) => part.text).join(""),
      marker,
    );
  } else {
    assert.equal(parsed.type, "message");
    assert.equal(parsed.role, "assistant");
    assert.equal(parsed.content.filter((part) => part.type === "text").map((part) => part.text).join(""), marker);
  }
}

function summarizeHit(hit) {
  return {
    seq: hit.sequence,
    listener: hit.listener,
    slot: hit.slot,
    path: hit.path,
    model: hit.model,
    valid: hit.valid,
    errors: hit.errors,
    store: hit.store,
    scriptKind: hit.scriptKind,
    scriptStatus: hit.scriptStatus,
  };
}

function defaultCliPath() {
  const windows = path.join(repoRoot, "target", "debug", "ocg-manager-cli.exe");
  const posix = path.join(repoRoot, "target", "debug", "ocg-manager-cli");
  if (existsSync(windows)) return windows;
  if (existsSync(posix)) return posix;
  return windows;
}

async function inspectBinary(cliPath) {
  const info = await stat(cliPath);
  const buf = await readFile(cliPath);
  const hash = sha256Buffer(buf).toUpperCase();
  const suspects = [
    "crates/ocg-core/src/gateway/forwarder.rs",
    "crates/ocg-core/src/gateway/executor.rs",
    "crates/ocg-core/src/gateway/protocol.rs",
    "crates/ocg-core/src/dashboard_v4/identities.rs",
    "crates/ocg-core/src/dashboard_v4/onboarding.rs",
  ];
  const newer = [];
  for (const relative of suspects) {
    const full = path.join(repoRoot, relative);
    if (!existsSync(full)) continue;
    const source = await stat(full);
    if (source.mtimeMs > info.mtimeMs) newer.push({ path: relative, mtime: source.mtime.toISOString() });
  }
  return {
    path: cliPath,
    bytes: info.size,
    mtime: info.mtime.toISOString(),
    mtimeUtc: info.mtime.toISOString(),
    sha256: hash,
    sourceFilesNewerThanBinary: newer,
    staleRelativeToDirtySource: newer.length > 0,
    evidenceClass: newer.length > 0 ? "old-binary-against-newer-source" : "binary-not-older-than-sampled-source",
  };
}

async function inspectOldLab() {
  const result = { ports: {}, note: "read-only inspect; this lab did not mutate, reconfigure, or stop those processes" };
  for (const port of [19143, 19144]) {
    try {
      const response = await fetch(port === 19143 ? "http://127.0.0.1:19143/dashboard/api/v3/connection" : "http://127.0.0.1:19144/_lab/health", {
        method: "GET",
        signal: AbortSignal.timeout(3000),
      });
      result.ports[port] = { reachable: true, status: response.status };
    } catch (error) {
      result.ports[port] = { reachable: false, error: error.message };
    }
  }
  const netstat = spawnSync("netstat", ["-ano"], { encoding: "utf8" });
  const lines = (netstat.stdout || "").split(/\r?\n/).filter((line) => line.includes(":19143") || line.includes(":19144"));
  result.netstat = lines.map((line) => line.trim()).filter(Boolean);
  return result;
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const cliPath = path.resolve(args.cli || process.env.OCG_ROUTING_LAB_CLI || defaultCliPath());
  const dataDir = path.join(here, "data");
  const logDir = path.join(here, "logs");
  const results = [];
  const owned = { pids: [], listeners: [], dataDir, gatewayPort: null };
  const lab = createLab();
  let gatewayPid = null;
  let binary = null;
  let started = null;
  let gatewayKey = null;
  const startedAt = new Date().toISOString();

  const record = (entry) => {
    results.push(entry);
    const tag = entry.pass ? "PASS" : "FAIL";
    console.log(`${tag} ${entry.label}${entry.error ? `: ${entry.error}` : ""}`);
  };

  const fail = (label, error, extra = {}) => {
    record({ label, pass: false, gap: false, error: error instanceof Error ? error.message : String(error), ...extra });
  };

  const pass = (label, extra = {}) => {
    record({ label, pass: true, gap: false, ...extra });
  };

  async function cleanup() {
    const listenerPorts = (started?.listeners || lab.listeners || []).map((item) => ({ id: item.id, port: item.port }));
    if (gatewayPid) stopPid(gatewayPid);
    await lab.close();
    // Wait on owned-process exit and its listener, rather than one timing sample.
    const deadline = Date.now() + 10000;
    while ((gatewayPid && processAlive(gatewayPid)) || (owned.gatewayPort && await portOpen(owned.gatewayPort))) {
      if (Date.now() >= deadline) break;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
    const gatewayAlive = gatewayPid ? processAlive(gatewayPid) : false;
    const gatewayPortStillOpen = owned.gatewayPort ? await portOpen(owned.gatewayPort) : false;
    const listeners = [];
    for (const item of listenerPorts) {
      listeners.push({ id: item.id, port: item.port, openAfterClose: await portOpen(item.port) });
    }
    const proof = {
      dataDir,
      gateway: {
        pid: gatewayPid,
        aliveAfterStop: gatewayAlive,
        port: owned.gatewayPort,
        portOpenAfterStop: gatewayPortStillOpen,
      },
      listeners,
      gatewayPidExited: !gatewayAlive,
      gatewayPortClosed: !gatewayPortStillOpen,
      listenersClosed: listeners.every((item) => item.openAfterClose === false),
    };
    proof.verified = proof.gatewayPidExited && proof.gatewayPortClosed && proof.listenersClosed;
    await writeFile(path.join(here, "cleanup.json"), JSON.stringify({ generatedAt: new Date().toISOString(), ...proof }, null, 2));
    return proof;
  }

  try {
    if (!existsSync(cliPath)) {
      throw new Error(`CLI binary not found: ${cliPath}. Parent can build with cargo build -p ocg-manager-cli and rerun.`);
    }
    binary = await inspectBinary(cliPath);
    const oldLab = await inspectOldLab();
    await mkdir(logDir, { recursive: true });
    if (existsSync(dataDir)) await rm(dataDir, { recursive: true, force: true });
    await mkdir(dataDir, { recursive: true });

    execFileSync(process.execPath, [path.join(sourceDir, "server.mjs"), "--self-check"], { stdio: "pipe" });

    const helpStdout = path.join(logDir, "help.stdout.log");
    const helpStderr = path.join(logDir, "help.stderr.log");
    const helpArgsPath = path.join(logDir, "help.args.json");
    await writeFile(helpStdout, "");
    await writeFile(helpStderr, "");
    await writeFile(helpArgsPath, JSON.stringify(["--help"]));
    const helpPid = startHidden(cliPath, { cwd: repoRoot, stdout: helpStdout, stderr: helpStderr, argsPath: helpArgsPath, shadow: false });
    await waitFor(
      async () => {
        if (processAlive(helpPid)) throw new Error("help process still running");
        return true;
      },
      { timeoutMs: 8000, intervalMs: 100, label: "cli --help exit" },
    );
    const helpOut = `${await readFile(helpStdout, "utf8")}\n${await readFile(helpStderr, "utf8")}`;
    if (!/Usage:|serve/i.test(helpOut)) {
      throw new Error(`CLI --help smoke failed:\n${helpOut.slice(0, 800)}`);
    }
    pass("cli --help smoke", { pid: helpPid });

    started = await lab.start();
    owned.listeners = started.listeners;
    const gatewayPort = await pickLoopbackPort();
    owned.gatewayPort = gatewayPort;
    const gatewayBase = `http://127.0.0.1:${gatewayPort}`;
    const stdout = path.join(logDir, "gateway.stdout.log");
    const stderr = path.join(logDir, "gateway.stderr.log");
    const argsPath = path.join(logDir, "gateway.args.json");
    await writeFile(stdout, "");
    await writeFile(stderr, "");
    const cliArgs = [
      "--data-dir",
      dataDir,
      "--encryption-key",
      "routing-lab-dummy-not-a-real-secret",
      "serve",
      "--host",
      "127.0.0.1",
      "--port",
      String(gatewayPort),
      "--dashboard-dir",
      path.join(repoRoot, "dist"),
    ];
    await writeFile(argsPath, JSON.stringify(cliArgs));

    gatewayPid = startHidden(cliPath, { cwd: repoRoot, stdout, stderr, argsPath, shadow: args.shadow });
    owned.pids.push(gatewayPid);

    await writeFile(
      path.join(here, "runtime.json"),
      JSON.stringify(
        {
          generatedAt: startedAt,
          binary,
          gateway: { pid: gatewayPid, host: "127.0.0.1", port: gatewayPort, url: gatewayBase },
          listeners: started.listeners,
          dataDir,
          oldLab,
          shadow: args.shadow,
        },
        null,
        2,
      ),
    );

    const connection = await waitFor(
      async () => {
        if (!processAlive(gatewayPid)) {
          const errLog = await readFile(stderr, "utf8").catch(() => "");
          throw new Error(`gateway pid ${gatewayPid} exited before listen\n${errLog.slice(-2000)}`);
        }
        const response = await request(gatewayBase, "/dashboard/api/v3/connection");
        const parsed = await readJsonResponse(response);
        if (parsed.status !== 200 || !parsed.body?.primaryKey) {
          throw new Error(`connection ${parsed.status}: ${parsed.text.slice(0, 300)}`);
        }
        return parsed.body;
      },
      { timeoutMs: 45000, intervalMs: 250, label: "gateway ready" },
    );
    gatewayKey = connection.primaryKey;
    assert.ok(gatewayKey, "isolated primaryKey");

    const api = makeApi(gatewayBase, lab, () => gatewayKey);

    const protocolSlots = started.slots.filter((slot) => ["chat", "responses", "messages"].includes(slot.slot));
    const routeSlots = started.slots.filter((slot) => ["alpha", "bravo", "charlie"].includes(slot.slot));

    for (const slot of [...protocolSlots, ...routeSlots]) {
      const name = `Routing Lab ${slot.slot}`;
      const receipt = await api.mutation("/dashboard/api/v4/onboarding/commit", {
        mode: "complete",
        operationId: randomUUID(),
        connection: {
          kind: "new",
          templateId: "custom-http",
          name,
          endpointUrl: slot.url,
          upstreamProtocol: slot.protocol,
          authKind: slot.auth,
        },
        authorization: { kind: "api_key", secretInput: slot.secret, accountLabel: `${name} Key` },
        targets: [{ publicModel: slot.publicModel, upstreamModel: slot.model }],
      });
      slot.connectionId = receipt.connectionId;
      slot.accountId = receipt.accountId;
      slot.credentialId = receipt.credentialId;
      await api.setEnabled(slot.accountId, true);
    }

    const identities = await api.identities();
    const v3Accounts = await api.v3Accounts();
    for (const slot of [...protocolSlots, ...routeSlots]) {
      const found = findCredential(identities, slot.connectionId);
      assert.ok(found, `missing identity for ${slot.slot}`);
      slot.accountId = found.legacy.id;
      slot.identityId = found.identityId;
      slot.credentialId = found.credential.id;
      slot.binding = found.binding;
    }

    await api.setRoutingMode("strict-priority", false);
    await api.reorder(routeSlots.map((slot) => slot.accountId));

    lab.reset();

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
            pass(label, { status: response.status, upstreamSends: 1, receipt: summarizeHit(hits[0]) });
          } catch (error) {
            fail(label, error, { upstreamSends: lab.snapshot().slice(mark).length, hits: lab.snapshot().slice(mark).map(summarizeHit) });
          }
        }
      }
    }

    for (const stream of [false, true]) {
      const label = `gemini -> chat ${stream ? "SSE" : "JSON"}`;
      const target = protocolSlots.find((slot) => slot.slot === "chat");
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
        pass(label, { status: response.status, upstreamSends: 1 });
      } catch (error) {
        fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
      }
    }

    const alphaUrl = started.listeners.find((item) => item.id === "alpha").url;
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
        pass(label, { status: response.status, control: "direct validator negative control" });
      } catch (error) {
        fail(label, error);
      }
    }

    {
      const label = "unknown model zero send";
      const mark = lab.snapshot().length;
      try {
        const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", "lab-unknown", false), inferenceHeaders("chat", gatewayKey));
        await response.text();
        assert.ok(!response.ok);
        assert.equal(lab.snapshot().length, mark);
        pass(label, { status: response.status, upstreamSends: 0 });
      } catch (error) {
        fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
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
        pass(label, { status: response.status, upstreamSends: 0 });
      } catch (error) {
        fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
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
        pass(label, { status: response.status, order: routeSlots.map((slot) => slot.slot) });
      } catch (error) {
        fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
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
        pass(label, { status: response.status, chronological: lab.snapshot().slice(mark).map((hit) => hit.listener) });
      } catch (error) {
        fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
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
        pass(label, { status: response.status, upstreamSends: 1, bodyExcerpt: response.text.slice(0, 240) });
      } catch (error) {
        fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
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
        pass(label, { status: response.status, upstreamSends: 1, bodyExcerpt: response.text.slice(0, 240) });
      } catch (error) {
        fail(label, error, { hits: lab.snapshot().slice(mark).map(summarizeHit) });
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
        pass(label, { status: response.status, order: ["bravo", "alpha", "charlie"] });
      } catch (error) {
        fail(label, error);
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
        pass(label, { status: response.status });
      } catch (error) {
        fail(label, error);
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
        pass(label, { status: response.status });
      } catch (error) {
        fail(label, error);
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
        pass(label, {
          status: response.status,
          siblingAccountId,
          identityId: alphaCred.identityId,
          sourceCredentialId: alphaCred.credential.id,
          quotaPoolId: sibling.credential.quotaPoolId,
          createBody: { ...createBody, secretInput: "sk-lab-bravo" },
          chronological: hits.map((hit) => hit.listener),
        });
      } catch (error) {
        fail(label, error);
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
        pass(label, { first: "alpha", second: "alpha" });
      } catch (error) {
        fail(label, error);
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
        pass(label, { first: firstHits[0].listener, second: secondHits[0].listener });
      } catch (error) {
        fail(label, error);
      } finally {
        await api.setRoutingMode("strict-priority", false);
      }
    }

    const journal = lab.snapshot();
    await writeFile(path.join(here, "journal.json"), JSON.stringify({ requests: journal }, null, 2));
    const proof = await cleanup();
    if (!proof.verified) {
      fail("cleanup verification", new Error(`listenersClosed=${proof.listenersClosed} gatewayPortClosed=${proof.gatewayPortClosed} gatewayPidExited=${proof.gatewayPidExited}`), proof);
    }
    const failed = results.filter((item) => !item.pass);
    const passed = results.filter((item) => item.pass);
    const report = {
      generatedAt: new Date().toISOString(),
      startedAt,
      evidenceClass: binary.evidenceClass,
      binary,
      replay: replayCommand(cliPath, args.shadow),
      shadow: args.shadow,
      gateway: { pid: gatewayPid, url: gatewayBase, port: gatewayPort, host: "127.0.0.1" },
      listeners: started.listeners,
      dataDir,
      oldLab,
      cleanup: proof,
      passed: passed.length,
      failed: failed.length,
      results,
    };
    await writeFile(path.join(here, "report.json"), JSON.stringify(report, null, 2));
    await writeResultsMarkdown(report);
    if (failed.length) process.exitCode = 1;
  } catch (error) {
    fail("orchestrator", error);
    const report = {
      generatedAt: new Date().toISOString(),
      startedAt,
      evidenceClass: binary?.evidenceClass ?? "unknown",
      binary,
      replay: replayCommand(cliPath, args.shadow),
      shadow: args.shadow,
      owned,
      passed: results.filter((item) => item.pass).length,
      failed: results.filter((item) => !item.pass).length,
      results,
      fatal: error instanceof Error ? error.message : String(error),
    };
    await writeFile(path.join(here, "journal.json"), JSON.stringify({ requests: lab.snapshot() }, null, 2)).catch(() => {});
    report.cleanup = await cleanup().catch((cleanupError) => ({ verified: false, error: String(cleanupError) }));
    await writeFile(path.join(here, "report.json"), JSON.stringify(report, null, 2)).catch(() => {});
    await writeResultsMarkdown(report).catch(() => {});
    process.exitCode = 1;
    console.error(error);
  }
}

function replayCommand(cliPath, shadow = false) {
  const quoted = `"${cliPath}"`;
  return `node scripts/routing-lab/run.mjs --cli ${quoted}${shadow ? " --shadow" : ""}`;
}

function inferenceHeaders(client, gatewayKey) {
  if (client === "messages") return { authorization: `Bearer ${gatewayKey}`, "anthropic-version": "2023-06-01" };
  if (client === "gemini") return { "x-goog-api-key": gatewayKey };
  return { authorization: `Bearer ${gatewayKey}`, "anthropic-version": "2023-06-01" };
}

function findCredential(identities, connectionId) {
  for (const identity of identities) {
    for (const credential of identity.credentials) {
      const binding = credential.bindings.find((item) => item.connectionId === connectionId);
      if (binding) {
        return {
          identityId: identity.identity.id,
          credential: credential.credential,
          binding,
          legacy: credential.legacy,
          raw: credential,
        };
      }
    }
  }
  return null;
}

function makeApi(gatewayBase, lab, gatewayKey) {
  async function json(pathName, method = "GET", body) {
    const response = await request(gatewayBase, pathName, method, body);
    const parsed = await readJsonResponse(response);
    assert.equal(parsed.status, 200, `${method} ${pathName}: ${parsed.status} ${parsed.text.slice(0, 1200)}`);
    return parsed.body;
  }

  async function tokens() {
    return json("/dashboard/api/v4/contract");
  }

  async function mutation(pathName, body, method = "POST") {
    const cas = await tokens();
    return json(pathName, method, { ...body, expectedRevision: cas.revision, processGeneration: cas.processGeneration });
  }

  async function identities() {
    return (await json("/dashboard/api/v4/accounts")).identities;
  }

  async function v3Accounts() {
    return (await json("/dashboard/api/v3/accounts")).accounts;
  }

  async function reorder(preferredIds) {
    const accounts = await v3Accounts();
    const remaining = accounts.map((item) => item.id).filter((id) => !preferredIds.includes(id));
    const accountIds = [...preferredIds, ...remaining];
    const missing = preferredIds.filter((id) => !accounts.some((item) => item.id === id));
    assert.equal(missing.length, 0, `reorder missing ids: ${missing}`);
    return mutation("/dashboard/api/v3/accounts/order", { accountIds }, "PUT");
  }

  async function patchBinding(id, patch) {
    return mutation(`/dashboard/api/v4/bindings/${id}`, patch, "PATCH");
  }

  async function setRoutingMode(routingMode, conversationSticky) {
    return mutation("/dashboard/api/v3/settings", { routingMode, conversationSticky }, "PUT");
  }

  async function resetCooldowns(accountIds) {
    for (const id of accountIds) {
      if (!id) continue;
      await mutation(`/dashboard/api/v3/accounts/${id}/reset-cooldown`, {});
    }
  }

  async function setEnabled(accountId, enabled) {
    const accounts = await v3Accounts();
    const account = accounts.find((item) => item.id === accountId);
    assert.ok(account, `missing account ${accountId}`);
    if (Boolean(account.enabled) !== Boolean(enabled)) {
      await mutation(`/dashboard/api/v3/accounts/${accountId}/toggle`, {});
    }
  }

  async function chatRoute(timeoutMs) {
    const response = await request(gatewayBase, "/v1/chat/completions", "POST", input("chat", "lab-route", false), inferenceHeaders("chat", gatewayKey()), timeoutMs);
    const text = await response.text();
    return { status: response.status, text, headers: response.headers };
  }

  function expectHits(label, mark, expected) {
    const got = lab.snapshot().slice(mark);
    if (got.length !== expected.length) {
      throw new Error(`${label}: expected ${expected.length} upstream hits, got ${got.length}: ${JSON.stringify(got.map(summarizeHit))}`);
    }
    for (let i = 0; i < expected.length; i += 1) {
      const exp = expected[i];
      const hit = got[i];
      if (exp.listener && hit.listener !== exp.listener) throw new Error(`${label}: hit ${i} listener ${hit.listener} != ${exp.listener}`);
      if (exp.slot && hit.slot !== exp.slot) throw new Error(`${label}: hit ${i} slot ${hit.slot} != ${exp.slot}`);
      if (exp.model && hit.model !== exp.model) throw new Error(`${label}: hit ${i} model ${hit.model} != ${exp.model}`);
      if (exp.path && hit.path !== exp.path) throw new Error(`${label}: hit ${i} path ${hit.path} != ${exp.path}`);
      if (exp.valid != null && hit.valid !== exp.valid) throw new Error(`${label}: hit ${i} valid=${hit.valid} errors=${JSON.stringify(hit.errors)}`);
      if (exp.store !== undefined && hit.store !== exp.store) throw new Error(`${label}: hit ${i} store=${JSON.stringify(hit.store)} != ${exp.store}`);
      if (exp.authHash && hit.authHash !== exp.authHash) throw new Error(`${label}: hit ${i} auth hash mismatch`);
    }
    return got;
  }

  return { json, mutation, identities, v3Accounts, reorder, patchBinding, setRoutingMode, resetCooldowns, setEnabled, chatRoute, expectHits };
}

async function writeResultsMarkdown(report) {
  const lines = [];
  lines.push("# Routing lab results");
  lines.push("");
  lines.push(`Generated: ${report.generatedAt}`);
  lines.push("");
  lines.push(`Evidence class: **${report.evidenceClass || "unknown"}**. This run exercised the on-disk CLI binary, not an unbuilt source tree.`);
  lines.push("");
  if (report.binary) {
    lines.push(`- CLI: \`${report.binary.path}\``);
    lines.push(`- SHA-256: \`${report.binary.sha256}\``);
    lines.push(`- mtime: ${report.binary.mtime}`);
    lines.push(`- bytes: ${report.binary.bytes}`);
    lines.push(`- stale relative to sampled dirty source: ${report.binary.staleRelativeToDirtySource}`);
  }
  lines.push("");
  lines.push(`Passed ${report.passed}, failed ${report.failed}. Shadow compare: ${report.shadow ? "on" : "off (pass --shadow to set OCG_SHADOW_COMPARE=1)"}.`);
  lines.push("");
  lines.push("## Replay");
  lines.push("");
  lines.push("```powershell");
  lines.push(report.replay);
  lines.push("```");
  lines.push("");
  if (report.gateway) {
    lines.push(`Gateway: pid ${report.gateway.pid} at ${report.gateway.url}`);
  }
  if (report.listeners) {
    lines.push("Listeners:");
    for (const listener of report.listeners) lines.push(`- ${listener.id} ${listener.url}`);
  }
  lines.push("");
  lines.push("## Scenarios");
  lines.push("");
  for (const item of report.results || []) {
    const tag = item.pass ? "PASS" : "FAIL";
    lines.push(`- ${tag} ${item.label}${item.error ? ` — ${item.error}` : ""}`);
  }
  lines.push("");
  await writeFile(path.join(here, "RESULTS.md"), `${lines.join("\n")}\n`);
}

await main();
