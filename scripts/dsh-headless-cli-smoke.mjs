#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { access, mkdtemp, mkdir, readFile, rm } from "node:fs/promises";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { once } from "node:events";

const repo = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const executable = join(
  repo,
  "target",
  "debug",
  process.platform === "win32" ? "ocg-manager-cli.exe" : "ocg-manager-cli",
);
const packageName = "@open-console-gateway/dsh-plugin";
const primaryKeyId = "00000000-0000-0000-0000-000000000001";
const expectUnsupported = process.argv.includes("--expect-unsupported");
const useRelativeRoots = process.argv.includes("--relative-roots");

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

function safeDiagnostic(value) {
  return value
    .replace(/^gateway key:.*$/gim, "gateway key: [redacted]")
    .slice(-4_000);
}

function comparablePath(value) {
  return value.replace(/^\\\\\?\\/, "").toLocaleLowerCase();
}

async function waitForReady(child, output) {
  const exited = once(child, "exit").then(([code, signal]) => {
    throw new Error(
      `headless CLI exited before readiness (${code ?? signal}): ${safeDiagnostic(output.text)}`,
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
      () => reject(new Error(`headless CLI readiness timed out: ${safeDiagnostic(output.text)}`)),
      30_000,
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

async function main() {
  await access(executable);
  const root = await mkdtemp(join(tmpdir(), "ocg-dsh-headless-"));
  const data = join(root, "data");
  const home = join(root, "dsh-home");
  await Promise.all([mkdir(data), mkdir(home)]);
  const port = await freePort();
  const output = { text: "" };
  const child = spawn(
    executable,
    [
      "--data-dir",
      useRelativeRoots ? "data" : data,
      "serve",
      "--host",
      "127.0.0.1",
      "--port",
      String(port),
      "--dashboard-dir",
      root,
    ],
    {
      cwd: useRelativeRoots ? root : repo,
      env: { ...process.env, DSH_HOME: useRelativeRoots ? "dsh-home" : home },
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    },
  );

  try {
    await waitForReady(child, output);
    const endpoint = `http://127.0.0.1:${port}/dashboard/api/v4/applications/dsh`;
    const inspectedResponse = await fetch(endpoint);
    assert.equal(inspectedResponse.status, 200);
    const inspected = await inspectedResponse.json();
    if (expectUnsupported) {
      assert.equal(inspected.status, "unsupported_runtime");
      assert.equal(inspected.detected, false);
      assert.equal(inspected.installSupported, false);
      process.stdout.write(`${JSON.stringify({
        status: "pass",
        runtime: "cli-without-local-host-capability",
        dshStatus: inspected.status,
        realUserHomeTouched: false,
      }, null, 2)}\n`);
      return;
    }
    assert.equal(inspected.status, "ready");
    assert.equal(inspected.detected, true);
    assert.equal(inspected.installSupported, true);
    assert.ok(inspected.fingerprint);
    assert.equal(inspected.version, "0.1.5-rc.2");

    const installedResponse = await fetch(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        keyId: primaryKeyId,
        expectedFingerprint: inspected.fingerprint,
        expectedRevision: inspected.revision.revision,
        processGeneration: inspected.revision.processGeneration,
      }),
    });
    if (installedResponse.status !== 200) {
      throw new Error(
        `headless DSH install returned HTTP ${installedResponse.status}: ${await installedResponse.text()}`,
      );
    }
    const installed = await installedResponse.json();
    assert.equal(installed.status, "installed");
    assert.equal(installed.installed, true);
    assert.equal(installed.activationRequired, true);

    const manifest = JSON.parse(
      await readFile(join(home, "profiles", "web", "package.json"), "utf8"),
    );
    assert.ok(manifest.dependencies?.[packageName]);
    assert.ok(manifest.dsh?.profile?.bundles?.includes(packageName));
    assert.ok(
      installed.targetPaths.every((path) =>
        [data, home].some((rootPath) =>
          comparablePath(path).startsWith(comparablePath(rootPath)),
        ),
      ),
    );

    process.stdout.write(`${JSON.stringify({
      status: "pass",
      runtime: "native-headless-cli",
      dshVersion: installed.version,
      installed: true,
      activationRequired: true,
      isolatedDshHome: true,
      relativeRoots: useRelativeRoots,
      realUserHomeTouched: false,
    }, null, 2)}\n`);
  } finally {
    await stopChild(child);
    await rm(root, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? safeDiagnostic(error.stack ?? error.message) : String(error));
  process.exitCode = 1;
});
