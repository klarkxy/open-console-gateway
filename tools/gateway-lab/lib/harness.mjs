import { existsSync } from "node:fs";
import { mkdir, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { rememberSecret, repoRoot } from "./common.mjs";
import { createLab } from "./lab.mjs";
import { makeApi, findCredential, readJsonResponse, request } from "./dashboard.mjs";
import {
  defaultCliPath,
  inspectBinary,
  inspectOldLab,
  pickLoopbackPort,
  processAlive,
  startHidden,
  stopOwnedGateway,
  stopOwnedChildren,
  stopPid,
  waitFor,
} from "./process.mjs";

export async function registerSlots(api, slots) {
  for (const slot of slots) {
    const name = `Gateway Lab ${slot.slot}`;
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
  for (const slot of slots) {
    const found = findCredential(identities, slot.connectionId);
    if (!found) throw new Error(`missing identity for ${slot.slot}`);
    slot.accountId = found.legacy.id;
    slot.identityId = found.identityId;
    slot.credentialId = found.credential.id;
    slot.binding = found.binding;
  }
  return slots;
}

export async function startGatewayCli({
  cliPath,
  dataDir,
  logDir,
  port,
  encryptionKey = "gateway-lab-dummy-not-a-real-secret",
}) {
  rememberSecret(encryptionKey);
  const root = repoRoot();
  const stdout = path.join(logDir, "gateway.stdout.log");
  const stderr = path.join(logDir, "gateway.stderr.log");
  const argsPath = path.join(logDir, "gateway.args.json");
  await writeFile(stdout, "");
  await writeFile(stderr, "");
  const cliArgs = [
    "--data-dir",
    dataDir,
    "--encryption-key",
    encryptionKey,
    "serve",
    "--host",
    "127.0.0.1",
    "--port",
    String(port),
    "--dashboard-dir",
    path.join(root, "dist"),
  ];
  const redacted = [];
  for (let i = 0; i < cliArgs.length; i += 1) {
    if (cliArgs[i] === "--encryption-key") {
      redacted.push(cliArgs[i], "[redacted-dummy-lab-encryption-key]");
      i += 1;
      continue;
    }
    redacted.push(cliArgs[i]);
  }
  await writeFile(
    argsPath,
    `${JSON.stringify(
      {
        note: "spawn argv; dummy lab encryption key is redacted in this artifact and is not a live Key",
        args: redacted,
      },
      null,
      2,
    )}\n`,
  );
  const pid = startHidden(cliPath, { cwd: root, stdout, stderr, args: cliArgs });
  return { pid, stdout, stderr, argsPath };
}

export async function waitForGateway(gatewayBase, pid, stderrPath) {
  const connection = await waitFor(
    async () => {
      if (!processAlive(pid)) {
        const errLog = await readFileSafe(stderrPath);
        throw new Error(`gateway pid ${pid} exited before listen\n${errLog.slice(-2000)}`);
      }
      const response = await request(gatewayBase, "/dashboard/api/v4/connection");
      const parsed = await readJsonResponse(response);
      if (parsed.status !== 200 || !parsed.body?.primaryKey) {
        throw new Error(`connection ${parsed.status}: ${parsed.text.slice(0, 300)}`);
      }
      return parsed.body;
    },
    { timeoutMs: 45000, intervalMs: 250, label: "gateway ready" },
  );
  return connection;
}

async function readFileSafe(file) {
  try {
    const { readFile } = await import("node:fs/promises");
    return await readFile(file, "utf8");
  } catch {
    return "";
  }
}

export async function withRuntime({
  cliPath: cliArg,
  profile,
  artifactDir,
  live = null,
  runId,
  encryptionKey,
  register = true,
  inject = {},
  existingLab = null,
  wipeData = true,
  onCleanup = null,
} = {}) {
  const cliPath = path.resolve(cliArg || process.env.OCG_ROUTING_LAB_CLI || defaultCliPath());
  if (!existsSync(cliPath)) {
    throw new Error(`CLI binary not found: ${cliPath}. Parent can build with cargo build -p ocg-manager-cli and rerun.`);
  }
  const dataDir = path.join(artifactDir, "data");
  const logDir = path.join(artifactDir, "logs");
  await mkdir(logDir, { recursive: true });
  if (wipeData && existsSync(dataDir)) await rm(dataDir, { recursive: true, force: true });
  await mkdir(dataDir, { recursive: true });

  const binary = await inspectBinary(cliPath);
  const oldLab = await inspectOldLab();
  const lab = existingLab || createLab({ profile, live, runId, host: profile?.host });
  let gatewayPid = null;
  let started = existingLab ? existingLab.runtime() : null;
  let gatewayPort = null;
  let gatewayKey = null;
  let cleaned = false;
  const extras = [];
  const gatewayBase = () => `http://127.0.0.1:${gatewayPort}`;

  async function cleanup() {
    if (cleaned) {
      return { verified: true, idempotent: true };
    }
    cleaned = true;
    const extraProofs = [];
    for (const extra of extras) {
      if (typeof extra.cleanup === "function") extraProofs.push(await extra.cleanup().catch((error) => ({ verified: false, error: String(error) })));
    }
    stopOwnedChildren(new Set([gatewayPid].filter(Boolean)));
    const proof = await stopOwnedGateway({
      pid: gatewayPid,
      port: gatewayPort,
      lab: existingLab ? null : lab,
      listeners: existingLab ? [] : started?.listeners || lab.listeners,
    });
    if (existingLab) {
      proof.labPreserved = true;
      proof.listenersClosed = true;
      proof.controlClosed = true;
    }
    proof.extras = extraProofs;
    if (extraProofs.some((item) => item && item.verified === false)) proof.verified = false;
    return proof;
  }

  if (typeof onCleanup === "function") onCleanup(cleanup);

  try {
    if (!existingLab) started = await lab.start();
    if (inject.failAfter === "labStart") throw new Error("injected failure after lab start");
    gatewayPort = await pickLoopbackPort();
    if (inject.failAfter === "port") throw new Error("injected failure after port selection");
    const launched = await startGatewayCli({
      cliPath,
      dataDir,
      logDir,
      port: gatewayPort,
      encryptionKey,
    });
    gatewayPid = launched.pid;
    if (inject.failAfter === "spawn") throw new Error("injected failure after spawn");
    const connection = await waitForGateway(gatewayBase(), gatewayPid, launched.stderr);
    if (inject.failAfter === "gatewayReady") throw new Error("injected failure after gateway ready");
    gatewayKey = connection.primaryKey;
    const api = makeApi(gatewayBase(), lab, () => gatewayKey);
    if (register) await registerSlots(api, started.slots);
    if (inject.failAfter === "register") throw new Error("injected failure after register");

    const runtime = {
      cliPath,
      binary,
      oldLab,
      lab,
      started,
      api,
      get gatewayPid() {
        return gatewayPid;
      },
      set gatewayPid(value) {
        gatewayPid = value;
      },
      get gatewayPort() {
        return gatewayPort;
      },
      gatewayBase: gatewayBase(),
      get gatewayKey() {
        return gatewayKey;
      },
      set gatewayKey(value) {
        gatewayKey = value;
      },
      dataDir,
      logDir,
      extras,
      cleanup,
    };
    return runtime;
  } catch (error) {
    if (!cleaned) {
      error.cleanup = await cleanup().catch((cleanupError) => ({ verified: false, error: String(cleanupError) }));
    }
    throw error;
  }
}

export async function restartOwnedGateway(runtime) {
  const oldPid = runtime.gatewayPid;
  if (oldPid) stopPid(oldPid);
  await waitFor(
    async () => {
      if (oldPid && processAlive(oldPid)) throw new Error("old gateway still alive");
      return true;
    },
    { timeoutMs: 10000, intervalMs: 100, label: "old gateway exit" },
  );
  const launched = await startGatewayCli({
    cliPath: runtime.cliPath,
    dataDir: runtime.dataDir,
    logDir: runtime.logDir,
    port: runtime.gatewayPort,
  });
  runtime.gatewayPid = launched.pid;
  const connection = await waitForGateway(runtime.gatewayBase, launched.pid, launched.stderr);
  runtime.gatewayKey = connection.primaryKey;
  return connection;
}

export async function smokeCliHelp(cliPath, logDir) {
  const helpStdout = path.join(logDir, "help.stdout.log");
  const helpStderr = path.join(logDir, "help.stderr.log");
  const helpArgsPath = path.join(logDir, "help.args.json");
  await writeFile(helpStdout, "");
  await writeFile(helpStderr, "");
  await writeFile(helpArgsPath, JSON.stringify(["--help"]));
  const helpPid = startHidden(cliPath, {
    cwd: repoRoot(),
    stdout: helpStdout,
    stderr: helpStderr,
    argsPath: helpArgsPath,
  });
  await waitFor(
    async () => {
      if (processAlive(helpPid)) throw new Error("help process still running");
      return true;
    },
    { timeoutMs: 8000, intervalMs: 100, label: "cli --help exit" },
  );
  const { readFile } = await import("node:fs/promises");
  const helpOut = `${await readFile(helpStdout, "utf8")}\n${await readFile(helpStderr, "utf8")}`;
  if (!/Usage:|serve/i.test(helpOut)) {
    throw new Error(`CLI --help smoke failed:\n${helpOut.slice(0, 800)}`);
  }
  return helpPid;
}
