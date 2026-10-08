#!/usr/bin/env node
// Isolated native CLI smoke for BYOK inspect/configure/reconfigure/remove.
// Uses a temporary data dir, home, and synthetic gateway key. It never reads
// the operator's client configs or sends an inference request.

import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { access, mkdtemp, mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { once } from "node:events";
import { promisify } from "node:util";
import { exerciseByokUpdates } from "./lib/byok-update-smoke.mjs";

const execFileAsync = promisify(execFile);
const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function argumentValue(name) {
  const prefix = `${name}=`;
  const args = process.argv.slice(2);
  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    if (arg === name) {
      const value = args[index + 1];
      if (!value || value.startsWith("-")) {
        throw new Error(`${name} requires a path`);
      }
      return value;
    }
    if (arg.startsWith(prefix)) {
      const value = arg.slice(prefix.length);
      if (!value) throw new Error(`${name} requires a path`);
      return value;
    }
  }
  return undefined;
}

const defaultExecutable = join(
  repo,
  "target",
  "debug",
  process.platform === "win32" ? "ocg-manager-cli.exe" : "ocg-manager-cli",
);
const executable = resolve(argumentValue("--cli") ?? defaultExecutable);
const exactModel = "vendor/model.name";
const publicModels = [exactModel, "模".repeat(100), ...Array.from({ length: 248 }, (_, index) => `vendor/model.${index}`)];
const keyNames = { codex: "codex", kimi: "kimi-code", minimax: "minimax-code", zcode: "zcode", copilot: "copilot" };
const upstreamKey = "sk-ocg-byok-smoke-upstream";
const encryptionKey = "ocg-byok-smoke-cipher";
const expectUnsupported = process.argv.includes("--expect-unsupported");
const clients = [
  ["codex", "config.toml", true],
  ["kimi", "config.toml", true],
  ["minimax", "config.yaml", false],
  ["zcode", "provider_config.json", false],
  ["copilot", "chatLanguageModels.json", true],
];

function comparablePath(value) {
  return value.replace(/^\\\\\?\\/, "").replaceAll("/", sep).toLocaleLowerCase();
}

function inside(child, parent) {
  const root = comparablePath(resolve(parent));
  const path = comparablePath(resolve(child));
  return path === root || path.startsWith(root.endsWith(sep) ? root : `${root}${sep}`);
}

function safeDiagnostic(value, secrets) {
  let text = String(value);
  for (const secret of secrets) {
    if (secret) text = text.split(secret).join("[redacted]");
  }
  return text
    .replace(/^gateway key:.*$/gim, "gateway key: [redacted]")
    .slice(-4_000);
}

async function freePort() {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert.equal(typeof address, "object");
  const port = address.port;
  server.close();
  await once(server, "close");
  return port;
}

async function waitForReady(child, output, secrets) {
  const exited = once(child, "exit").then(([code, signal]) => {
    throw new Error(
      `CLI exited before readiness (${code ?? signal}): ${safeDiagnostic(output.text, secrets)}`,
    );
  });
  const ready = new Promise((resolveReady) => {
    const inspect = (chunk) => {
      output.text = `${output.text}${chunk}`.slice(-16_000);
      if (output.text.includes("gateway started on")) resolveReady();
    };
    child.stdout.on("data", inspect);
    child.stderr.on("data", inspect);
  });
  const timeout = new Promise((_, reject) => {
    setTimeout(
      () => reject(new Error(`CLI readiness timed out: ${safeDiagnostic(output.text, secrets)}`)),
      45_000,
    ).unref();
  });
  await Promise.race([ready, exited, timeout]);
}

async function stopChild(child) {
  if (child.exitCode !== null || child.signalCode !== null) return;
  child.kill();
  await Promise.race([
    once(child, "exit"),
    new Promise((resolveTimeout) => setTimeout(resolveTimeout, 5_000)),
  ]);
  if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
}

function isolatedEnv(root, homes) {
  const env = {
    PATH: process.env.PATH ?? process.env.Path ?? "",
    SYSTEMROOT: process.env.SYSTEMROOT ?? process.env.SystemRoot ?? "",
    WINDIR: process.env.WINDIR ?? "",
    PATHEXT: process.env.PATHEXT ?? "",
    COMSPEC: process.env.COMSPEC ?? process.env.ComSpec ?? "",
    TEMP: join(root, "tmp"),
    TMP: join(root, "tmp"),
    HOME: homes.user,
    USERPROFILE: homes.user,
    APPDATA: join(root, "appdata"),
    LOCALAPPDATA: join(root, "localappdata"),
    CODEX_HOME: homes.codex,
    KIMI_CODE_HOME: homes.kimi,
    MINIMAX_DATA_DIR: homes.minimax,
    MAVIS_DATA_DIR: homes.minimax,
    ZCODE_DATA_BASE_DIR: homes.user,
    ZCODE_PERSONAL_PROVIDER_CONFIG_FILE: homes.zcodeFile,
  };
  return Object.fromEntries(Object.entries(env).filter(([, value]) => value !== ""));
}

async function readJson(response, secrets) {
  const text = await response.text();
  let body;
  try {
    body = text ? JSON.parse(text) : {};
  } catch {
    throw new Error(
      `HTTP ${response.status} was not JSON: ${safeDiagnostic(text, secrets)}`,
    );
  }
  if (!response.ok) {
    throw new Error(
      `HTTP ${response.status} ${body.code ?? ""}: ${safeDiagnostic(body.message ?? text, secrets)}`,
    );
  }
  return body;
}

function assertNoSecrets(value, secrets) {
  const encoded = JSON.stringify(value);
  for (const secret of secrets) {
    assert.equal(encoded.includes(secret), false, "dashboard JSON contained a secret");
  }
}

function assertPathsInside(body, root) {
  for (const path of [body.configPath, ...(body.targetPaths ?? []), body.backupPath]
    .filter((path) => typeof path === "string" && path.length > 0)) {
    assert.equal(inside(path, root), true, `BYOK path escaped the temp root: ${path}`);
  }
}

async function treeContains(directory, needle) {
  const pending = [directory];
  while (pending.length > 0) {
    const current = pending.pop();
    let entries;
    try {
      entries = await readdir(current, { withFileTypes: true });
    } catch (error) {
      if (error.code === "ENOENT") continue;
      throw error;
    }
    for (const entry of entries) {
      const path = join(current, entry.name);
      if (entry.isSymbolicLink()) continue;
      if (entry.isDirectory()) {
        pending.push(path);
        continue;
      }
      if (!entry.isFile()) continue;
      const bytes = await readFile(path);
      if (bytes.includes(Buffer.from(needle))) return true;
    }
  }
  return false;
}

async function publishModel(endpoint, secrets) {
  const contract = await readJson(await fetch(`${endpoint}/contract`), secrets);
  const created = await fetch(`${endpoint}/accounts`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      expectedRevision: contract.revision,
      processGeneration: contract.processGeneration,
      providerId: "custom",
      name: "BYOK smoke",
      key: upstreamKey,
      customConfig: {
        endpointUrl: "https://example.test/v1/messages",
        upstreamProtocol: "messages",
      },
      modelCapabilities: publicModels.map((id) => ({
        publicModel: id,
        upstreamModel: id,
        protocol: "messages",
      })),
    }),
  });
  const body = await readJson(created, secrets);
  assertNoSecrets(body, secrets);
  return body.account.id;
}

async function main() {
  await access(executable);
  const root = await mkdtemp(join(tmpdir(), "ocg-byok-smoke-"));
  const data = join(root, "data");
  const homes = {
    user: join(root, "user-home"),
    codex: join(root, "clients", "codex"),
    kimi: join(root, "clients", "kimi"),
    minimax: join(root, "clients", "minimax"),
    zcodeFile: join(root, "clients", "zcode", "provider_config.json"),
    copilotFile: join(root, "clients", "copilot", "chatLanguageModels.json"),
  };
  await Promise.all([
    mkdir(data, { recursive: true }),
    mkdir(join(root, "tmp"), { recursive: true }),
    mkdir(homes.user, { recursive: true }),
    mkdir(homes.codex, { recursive: true }),
    mkdir(homes.kimi, { recursive: true }),
    mkdir(homes.minimax, { recursive: true }),
    mkdir(dirname(homes.zcodeFile), { recursive: true }),
    mkdir(dirname(homes.copilotFile), { recursive: true }),
    mkdir(join(root, "appdata"), { recursive: true }),
    mkdir(join(root, "localappdata"), { recursive: true }),
  ]);
  const targets = {
    codex: join(homes.codex, "config.toml"),
    kimi: join(homes.kimi, "config.toml"),
    minimax: join(homes.minimax, "config.yaml"),
    zcode: homes.zcodeFile,
    copilot: homes.copilotFile,
  };
  for (const [client] of clients) {
    await writeFile(join(dirname(targets[client]), "notes.txt"), "user-note\n");
  }
  const env = isolatedEnv(root, homes);
  const secrets = [upstreamKey, encryptionKey];
  let gatewayKey = "";
  const output = { text: "" };
  let child;

  try {
    const status = await execFileAsync(executable, [
      "--data-dir", data,
      "--encryption-key", encryptionKey,
      "status",
      "--show-key",
    ], { cwd: root, env, windowsHide: true, timeout: 60_000 });
    const match = status.stdout.match(/^gateway key: (.+)$/m);
    assert.ok(match?.[1] && !match[1].includes("[hidden]"), "isolated gateway key was not issued");
    gatewayKey = match[1].trim();
    secrets.push(gatewayKey);

    const port = await freePort();
    child = spawn(executable, [
      "--data-dir", data,
      "--encryption-key", encryptionKey,
      "serve",
      "--host", "127.0.0.1",
      "--port", String(port),
      "--dashboard-dir", root,
    ], {
      cwd: root,
      env,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    await waitForReady(child, output, secrets);
    const endpoint = `http://127.0.0.1:${port}/dashboard/api/v4`;
    const results = [];
    let publishedAccountId;

    for (const [client, , closed] of clients) {
      const inspectStarted = performance.now();
      const inspectedResponse = await fetch(
        `${endpoint}/applications/byok/${client}?targetPath=${encodeURIComponent(targets[client])}`,
      );
      const inspected = await readJson(inspectedResponse, secrets);
      const coldInspectMs = Math.round(performance.now() - inspectStarted);
      assert.equal(inspected.client, client);
      assert.equal(inspected.requiresClosedClient, closed);
      assertPathsInside(inspected, root);
      assertNoSecrets(inspected, secrets);
      if (expectUnsupported) {
        assert.equal(inspected.status, "unsupported_runtime");
        assert.equal(inspected.configureSupported, false);
        assert.equal(inspected.removeSupported, false);
        results.push({ client, status: inspected.status, lifecycle: "unsupported" });
        continue;
      }
      if (inspected.status === "unsupported_runtime") {
        throw new Error(`native CLI returned unsupported_runtime for ${client}`);
      }
      assert.equal(inspected.configureSupported, true, inspected.detail ?? inspected.status);
      if (!results.some((item) => item.modelPublished)) {
        publishedAccountId = await publishModel(endpoint, secrets);
        results.push({ client: "custom", modelPublished: true });
      }
      const refreshed = await readJson(await fetch(
        `${endpoint}/applications/byok/${client}?targetPath=${encodeURIComponent(targets[client])}`,
      ), secrets);
      assert.equal("models" in refreshed, false);
      const published = await readJson(await fetch(`http://127.0.0.1:${port}/v1/models`, {
        headers: { authorization: `Bearer ${gatewayKey}` },
      }), secrets);
      const publishedIds = published.data.map((model) => model.id).sort();
      assert.deepEqual(publishedIds, [...publicModels].sort());
      for (const model of published.data) {
        const protocols = model.ocg?.protocols;
        assert.equal(model.ocg?.schemaVersion, 2, `${model.id} schemaVersion`);
        assert.ok(protocols, `${model.id} omitted ocg.protocols`);
        assert.equal(protocols.preferred, "messages", `${model.id} preferred protocol`);
        assert.equal(
          protocols.supported.includes("messages"),
          true,
          `${model.id} supported protocols`,
        );
        assert.equal(
          protocols.supported.includes("chat_completions"),
          false,
          `${model.id} published a fixed chat protocol`,
        );
      }
      const configured = await readJson(await fetch(`${endpoint}/applications/byok/${client}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          expectedRevision: refreshed.revision.revision,
          processGeneration: refreshed.revision.processGeneration,
          targetPath: targets[client],
          expectedFingerprint: refreshed.fingerprint ?? "",
          clientClosed: true,
        }),
      }), secrets);
      assert.equal(configured.status, "configured", configured.detail ?? configured.status);
      assert.equal(configured.activationRequired, true);
      if (client === "copilot") assert.equal(configured.defaultModelId, null);
      else assert.ok(publishedIds.includes(configured.defaultModelId));
      assert.deepEqual([...configured.configuredModelIds].sort(), publishedIds);
      assert.equal(typeof configured.fingerprint, "string");
      assert.ok(configured.fingerprint.length > 0);
      assertPathsInside(configured, root);
      assertNoSecrets(configured, secrets);
      const clientDir = dirname(targets[client]);
      assert.equal(await treeContains(clientDir, exactModel), true, `${client} files omitted the exact model id`);
      if (client !== "codex" && client !== "copilot") {
        assert.equal(
          await treeContains(clientDir, "ocg-messages"),
          true,
          `${client} files omitted the messages provider group`,
        );
        assert.equal(
          await treeContains(clientDir, "ocg-chat"),
          false,
          `${client} files kept a fixed chat provider group`,
        );
      }
      if (client === "copilot") {
        const providers = JSON.parse(await readFile(targets.copilot, "utf8"));
        assert.equal(providers.length, 1);
        assert.equal(providers[0].vendor, "customendpoint");
        assert.equal("url" in providers[0], false);
        assert.equal("apiKey" in providers[0], false);
        assert.deepEqual(providers[0].models.map((model) => model.id).sort(), publishedIds);
        for (const model of providers[0].models) {
          assert.equal(model.apiType, "messages");
          assert.equal(model.url, `http://127.0.0.1:${port}/v1/messages`);
          assert.equal(model.maxInputTokens, 100000);
          assert.equal(model.maxOutputTokens, 8192);
          assert.equal(model.toolCalling, false);
          assert.equal(model.vision, false);
          assert.equal(typeof model.requestHeaders.Authorization, "string");
        }
      }
      const connection = await readJson(await fetch(`${endpoint}/connection`), secrets);
      const matching = connection.subKeys.filter((key) => key.name === keyNames[client] && key.enabled);
      assert.equal(matching.length, 1);
      const applicationKey = matching[0].value;
      secrets.push(applicationKey);
      assert.equal(await treeContains(clientDir, applicationKey), true, `${client} files omitted its named key`);
      const keyModels = await readJson(await fetch(`http://127.0.0.1:${port}/v1/models`, {
        headers: { authorization: `Bearer ${applicationKey}` },
      }), secrets);
      assert.deepEqual(keyModels.data.map((model) => model.id).sort(), publishedIds);

      const reconfigured = await readJson(await fetch(`${endpoint}/applications/byok/${client}`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          expectedRevision: configured.revision.revision,
          processGeneration: configured.revision.processGeneration,
          targetPath: targets[client],
          expectedFingerprint: configured.fingerprint,
          clientClosed: true,
        }),
      }), secrets);
      assert.equal(reconfigured.defaultModelId, configured.defaultModelId);
      assert.deepEqual([...reconfigured.configuredModelIds].sort(), publishedIds);
      assertNoSecrets(reconfigured, secrets);
      const nextConnection = await readJson(await fetch(`${endpoint}/connection`), secrets);
      assert.deepEqual(nextConnection.subKeys, connection.subKeys, "reconfigure must reuse ordinary Keys");
      const updateEvidence = await exerciseByokUpdates({ endpoint, client, targetPath: targets[client], data, secrets, assertNoSecrets, accountId: publishedAccountId, publicModels });
      const finalView = updateEvidence.view;
      const removed = await readJson(await fetch(`${endpoint}/applications/byok/${client}`, {
        method: "DELETE",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          expectedRevision: finalView.revision.revision,
          processGeneration: finalView.revision.processGeneration,
          targetPath: targets[client],
          expectedFingerprint: finalView.fingerprint,
          clientClosed: true,
        }),
      }), secrets);
      assert.equal(removed.configuredModelIds?.includes(exactModel), false);
      assertNoSecrets(removed, secrets);
      assertPathsInside(removed, root);
      assert.equal(await treeContains(clientDir, applicationKey), false, `${client} files kept the gateway key after remove`);
      assert.equal(
        await readFile(join(dirname(targets[client]), "notes.txt"), "utf8"),
        "user-note\n",
      );
      results.push({ client, status: removed.status, lifecycle: "configure-update-takeover-undo-remove", updateChecks: updateEvidence.checks, coldInspectMs, exportedModels: publishedIds.length });
    }

    if (!expectUnsupported && results.every((item) => item.lifecycle === "unsupported")) {
      throw new Error("native CLI returned unsupported_runtime for every BYOK client");
    }
    process.stdout.write(`${JSON.stringify({
      status: "pass",
      runtime: expectUnsupported ? "cli-without-local-host-capability" : "native-headless-cli",
      clients: results,
      realUserHomeTouched: false,
      liveInference: false,
    }, null, 2)}\n`);
  } finally {
    if (child) await stopChild(child);
    await rm(root, { recursive: true, force: true });
  }
}

main().catch((error) => {
  const secrets = [upstreamKey, encryptionKey];
  console.error(error instanceof Error ? safeDiagnostic(error.stack ?? error.message, secrets) : String(error));
  process.exitCode = 1;
});
