import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:http";
import net from "node:net";
import { existsSync, openSync, closeSync, readFileSync } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { FORBIDDEN_PORTS, repoRoot, sha256Buffer } from "./common.mjs";

const ownedChildren = new Map();

export function startHidden(executable, { cwd, stdout, stderr, args, argsPath, shadow = false }) {
  const resolved = Array.isArray(args)
    ? args
    : JSON.parse(readFileSync(argsPath, "utf8"));
  const launchArgs = Array.isArray(resolved) ? resolved : resolved.args;
  assert.ok(Array.isArray(launchArgs) && launchArgs.every((arg) => typeof arg === "string"));
  const argsForSpawn = launchArgs;
  const env = { ...process.env };
  for (const name of ["OCG_MANAGER_ENCRYPTION_KEY", "OCG_GATEWAY_PORT", "OCG_CLIENT_ROOT_URL", "OCG_ADMIN_USERNAME", "OCG_ADMIN_PASSWORD", "OCG_SHADOW_COMPARE"]) delete env[name];
  if (shadow) env.OCG_SHADOW_COMPARE = "1";
  const out = openSync(stdout, "a");
  const err = openSync(stderr, "a");
  try {
    const child = spawn(executable, argsForSpawn, { cwd, env, windowsHide: true, stdio: ["ignore", out, err] });
    child.on("error", (error) => {
      child.launchError = error;
    });
    if (!child.pid) throw new Error(`Could not launch ${executable}`);
    registerOwnedChild(child);
    return child.pid;
  } finally {
    closeSync(out);
    closeSync(err);
  }
}

export function portOpen(port) {
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

export function processAlive(pid) {
  const child = ownedChildren.get(pid);
  return !!child && !child.launchError && child.exitCode === null && child.signalCode === null;
}

export function stopPid(pid) {
  const child = ownedChildren.get(pid);
  if (child && processAlive(pid)) child.kill();
}

export function ownedPids() {
  return [...ownedChildren.keys()];
}

export function registerOwnedChild(child) {
  if (child?.pid) ownedChildren.set(child.pid, child);
  return child?.pid ?? null;
}

export function stopOwnedChildren(except = new Set()) {
  for (const pid of [...ownedChildren.keys()]) {
    if (except.has(pid)) continue;
    stopPid(pid);
  }
}

export function installInterruptCleanup(cleanup) {
  let ran = false;
  const run = async () => {
    if (ran) return;
    ran = true;
    try {
      await cleanup();
    } catch {
      /* owned cleanup only */
    }
  };
  const onInt = () => {
    run().finally(() => process.exit(130));
  };
  const onTerm = () => {
    run().finally(() => process.exit(143));
  };
  process.on("SIGINT", onInt);
  process.on("SIGTERM", onTerm);
  return () => {
    process.off("SIGINT", onInt);
    process.off("SIGTERM", onTerm);
  };
}

export async function waitFor(fn, { timeoutMs, intervalMs, label }) {
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

export async function pickLoopbackPort() {
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

export function defaultCliPath() {
  const root = repoRoot();
  const windows = path.join(root, "target", "debug", "ocg-manager-cli.exe");
  const posix = path.join(root, "target", "debug", "ocg-manager-cli");
  if (existsSync(windows)) return windows;
  if (existsSync(posix)) return posix;
  return windows;
}

export async function inspectBinary(cliPath) {
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
    const full = path.join(repoRoot(), relative);
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

export async function inspectOldLab() {
  const result = { ports: {}, note: "read-only inspect; this lab did not mutate, reconfigure, or stop those processes" };
  for (const port of [19143, 19144]) {
    try {
      const response = await fetch(port === 19143 ? "http://127.0.0.1:19143/dashboard/api/v4/connection" : "http://127.0.0.1:19144/_lab/health", {
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

export async function stopOwnedGateway({ pid, port, lab, listeners }) {
  if (pid) stopPid(pid);
  if (lab) await lab.close();
  const deadline = Date.now() + 10000;
  while ((pid && processAlive(pid)) || (port && (await portOpen(port)))) {
    if (Date.now() >= deadline) break;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  const gatewayAlive = pid ? processAlive(pid) : false;
  const gatewayPortStillOpen = port ? await portOpen(port) : false;
  const listenerProof = [];
  for (const item of listeners || []) {
    listenerProof.push({ id: item.id, port: item.port, openAfterClose: await portOpen(item.port) });
  }
  const controlOpen = lab?.control?.port ? await portOpen(lab.control.port) : false;
  const proof = {
    gateway: {
      pid,
      aliveAfterStop: gatewayAlive,
      port,
      portOpenAfterStop: gatewayPortStillOpen,
    },
    listeners: listenerProof,
    control: { port: lab?.control?.port ?? null, openAfterClose: controlOpen },
    gatewayPidExited: !gatewayAlive,
    gatewayPortClosed: !gatewayPortStillOpen,
    listenersClosed: listenerProof.every((item) => item.openAfterClose === false),
    controlClosed: !controlOpen,
  };
  proof.verified = proof.gatewayPidExited && proof.gatewayPortClosed && proof.listenersClosed && proof.controlClosed;
  return proof;
}
