#!/usr/bin/env node
// Manual acceptance against an isolated, installed DSH Web or Desktop Host.
import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { createServer } from "node:http";
import { createHash, createHmac, randomUUID } from "node:crypto";
import { createRequire } from "node:module";
import { cp, mkdir, mkdtemp, readFile, writeFile, access } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

const desktop = process.argv.includes("--desktop");
const ui = process.argv.includes("--ui");
const throughOcg = ui || process.argv.includes("--ocg");
const root = await mkdtemp(join(tmpdir(), "ocg-dsh-http-"));
const home = join(root, "home");
const profile = join(home, "profiles", desktop ? "desktop" : "web");
const plugin = join(root, "plugin");
const handoff = join(root, "credential-handoff");
const bin = process.env.DSH_SMOKE_BIN ?? join(process.env.APPDATA ?? "", "npm/node_modules/@deepseek-ai/dsh/lib/bin.js");
const require = createRequire(bin);
const yaml = require("js-yaml");
const packageName = "@open-console-gateway/dsh-plugin";
const secret = "ocg-isolated-http-smoke";
await mkdir(home, { recursive: true });
await cp(new URL("../integrations/dsh-plugin/", import.meta.url), plugin, { recursive: true });
const models = createServer((request, response) => {
  if (request.url === "/v1/models" && request.headers.authorization === `Bearer ${secret}`) {
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ object: "list", data: [{ id: "http-smoke-model" }] }));
  } else response.writeHead(404).end();
});
await new Promise((done) => models.listen(0, "127.0.0.1", done));
const gateway = `http://127.0.0.1:${models.address().port}/v1`;
await writeFile(join(plugin, "index.js"), (await readFile(join(plugin, "index.js"), "utf8"))
  .replaceAll("__OCG_GATEWAY_V1_URL__", gateway)
  .replaceAll("__OCG_CREDENTIAL_BOOTSTRAP_PATH_JSON__", JSON.stringify(handoff)));
await writeFile(handoff, secret, { mode: 0o600 });
const env = { ...process.env, DSH_HOME: home };
let child;
let output = "";
let url;
let exited = false;
let ocg;

async function stop(child, ipc = false) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (ipc && child.connected) child.send({ type: "shutdown" });
  else child.kill();
  for (let count = 0; count < 50 && child.exitCode === null && child.signalCode === null; count++) await delay(100);
  if (child.exitCode === null && child.signalCode === null) {
    if (process.platform === "win32") await new Promise((done) => execFile("taskkill", ["/PID", String(child.pid), "/T", "/F"], { windowsHide: true }, () => done()));
    else child.kill("SIGKILL");
  }
}

try {
  if (desktop) {
    const installation = process.env.DSH_DESKTOP_SMOKE_ROOT
      ?? join(process.env.LOCALAPPDATA ?? "", "Programs/DeepSeek Harness");
    const resources = join(installation, "resources");
    const runtime = join(resources, "app.asar/dsh");
    const executable = join(installation, "DeepSeek Harness.exe");
    const boot = await import(pathToFileURL(require.resolve("@deepseek-ai/dsh-app-boot")).href);
    boot.initProfile(profile, boot.PROFILE_TEMPLATES.web.bundles);
    // Override the bound port only in this newly-created test profile.
    await writeFile(join(profile, "cordis.patch.yml"), "- id: webserver\n  config:\n    host: 127.0.0.1\n    port: 0\n");
    child = spawn(executable, ["--expose-internals",
      join(runtime, "node_modules/@deepseek-ai/dsh-desktop-host/lib/index.js"),
      runtime, profile, join(resources, "runtime/primary-runtime"),
      join(resources, "runtime/pnpm/bin/pnpm.cjs"), join(resources, "runtime/bin"),
    ], { cwd: profile, env: { ...env, ELECTRON_RUN_AS_NODE: "1" },
      windowsHide: true, stdio: ["ignore", "pipe", "pipe", "ipc"] });
    child.on("message", (message) => { if (message?.type === "ready") url = message.url; });
  } else {
    child = spawn(process.execPath, [bin, "web", "--no-open", "--port", "0"],
      { env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
  }
  child.on("exit", () => { exited = true; });
  child.on("error", (error) => { output += error.message; exited = true; });
  for (const stream of [child.stdout, child.stderr]) stream.on("data", (data) => {
    output = (output + String(data)).slice(-64 * 1024);
    if (!desktop) url ??= output.match(/dsh web: (http:\/\/127\.0\.0\.1:\d+\/\?token=[A-Za-z0-9_-]+)/)?.[1];
  });
  const started = Date.now();
  while (!url && !exited && Date.now() - started < 90_000) await delay(100);
  assert.ok(url, `isolated startup failed: ${output.replace(/token=[^\s)]+/g, "token=[redacted]").slice(-4000)}`);
  const origin = new URL(url).origin;
  const authority = new URL(origin).host;
  // This reads ONLY the credential store just created under the isolated home.
  const grant = yaml.load(await readFile(join(home, ".credentials.yaml"), "utf8"))
    .records["client-connection/browser-session"];
  assert.equal(grant.kind, "grant");
  assert.equal(grant.payload.version, 1);
  async function rpc(method, args = {}) {
    const now = Date.now();
    const body = Buffer.from(JSON.stringify({ version: 1, authority, issuedAt: now, expiresAt: now + 300_000 })).toString("base64url");
    const signature = createHmac("sha256", Buffer.from(grant.payload.secret, "base64url")).update(body).digest("base64url");
    const cookie = `dsh-auth-${createHash("sha256").update(authority).digest("base64url")}=v1.${body}.${signature}`;
    const rpcId = randomUUID();
    const response = await fetch(`${origin}/api/${method}`, { method: "POST", redirect: "error",
      signal: AbortSignal.timeout(120_000), headers: { "content-type": "application/json", cookie },
      body: JSON.stringify({ type: "client-request", rpcId, method, payload: { args } }),
    });
    assert.equal(response.status, 200, `HTTP ${response.status} from ${method}`);
    const envelope = await response.json();
    assert.equal(envelope.rpcId, rpcId);
    assert.equal(envelope.result.ok, true, `${method} Remote failure`);
    return envelope.result.value;
  }
  const before = await rpc("pluginManager/listBundles");
  const spec = `file:${plugin.replaceAll("\\", "/")}`;
  let mutateOcg;
  if (throughOcg) {
    const reservation = createServer();
    await new Promise((done) => reservation.listen(0, "127.0.0.1", done));
    const port = reservation.address().port;
    await new Promise((done) => reservation.close(done));
    const executable = resolve(dirname(fileURLToPath(import.meta.url)), "../target/debug", process.platform === "win32" ? "ocg-manager-cli.exe" : "ocg-manager-cli");
    const dashboard = ui ? resolve(dirname(fileURLToPath(import.meta.url)), "../dist") : root;
    ocg = spawn(executable, ["--data-dir", join(root, "ocg"), "serve", "--host", "127.0.0.1", "--port", String(port), "--dashboard-dir", dashboard],
      { env, windowsHide: true, stdio: ["ignore", "pipe", "pipe"] });
    let ready = false;
    for (const stream of [ocg.stdout, ocg.stderr]) stream.on("data", (data) => { if (String(data).includes("gateway started on")) ready = true; });
    const start = Date.now();
    while (!ready && ocg.exitCode === null && Date.now() - start < 30_000) await delay(100);
    assert.ok(ready, "isolated OCG did not start");
    const endpoint = `http://127.0.0.1:${port}/dashboard/api/v4/applications/dsh`;
    const target = { profilePath: profile, runtimeUrl: origin };
    mutateOcg = async (method) => {
      const inspectedResponse = await fetch(`${endpoint}?${new URLSearchParams(target)}`);
      assert.equal(inspectedResponse.status, 200);
      const inspected = await inspectedResponse.json();
      assert.equal(inspected.runtimeUrl, origin);
      const response = await fetch(endpoint, { method, headers: { "content-type": "application/json" },
        signal: AbortSignal.timeout(120_000), body: JSON.stringify({ ...target,
          ...(method === "POST" ? { keyId: "00000000-0000-0000-0000-000000000001" } : {}),
          expectedFingerprint: inspected.fingerprint, expectedRevision: inspected.revision.revision,
          processGeneration: inspected.revision.processGeneration,
        }),
      });
      const result = await response.json();
      assert.equal(response.status, 200, JSON.stringify(result));
      return result;
    };
    if (ui) {
      const done = join(root, "ui-complete");
      console.log(JSON.stringify({ surface: desktop ? "official-desktop-host" : "web", root,
        dashboard: `http://127.0.0.1:${port}/dashboard/#/applications`, runtimeUrl: origin, profile, done }));
      const deadline = Date.now() + 600_000;
      let completed = false;
      while (Date.now() < deadline && !exited) {
        completed = await access(done).then(() => true, () => false);
        if (completed) break;
        await delay(500);
      }
      assert.ok(completed, "UI acceptance was not completed within ten minutes");
      const final = await rpc("pluginManager/listBundles");
      assert.deepEqual(final.map((entry) => entry.name).sort(), before.map((entry) => entry.name).sort());
      const credentials = yaml.load(await readFile(join(home, ".credentials.yaml"), "utf8"));
      assert.ok(credentials.refs.OCG_GATEWAY_KEY?.length > 0, "UI installation must have imported its Key");
      console.log(JSON.stringify({ success: true, ui: true, root, baselineBundlesPreserved: true }));
      process.exitCode = 0;
    }
  }
  if (!ui) {
  const inspected = await rpc("pluginManager/inspect", { spec });
  assert.equal(inspected.status, "accepted");
  const installed = throughOcg ? await mutateOcg("POST")
    : await rpc("pluginManager/installBundle", { spec, options: { enabled: true, requestId: randomUUID() } });
  assert.equal(installed.application, "applied", JSON.stringify(installed));
  if (throughOcg) {
    assert.equal(installed.installed, true);
    assert.equal(installed.enabled, true);
    assert.equal(installed.version, null, "OCG plugin version is not a DSH runtime version");
  }
  const after = await rpc("pluginManager/listBundles");
  assert.ok(after.some((entry) => entry.name === packageName && entry.installed && entry.enabled));
  const live = await rpc("pluginManager/listPlugins");
  assert.ok(live.some((entry) => entry.moduleName === packageName && entry.fiberPhase === "active"));
  const credentials = yaml.load(await readFile(join(home, ".credentials.yaml"), "utf8"));
  if (throughOcg) assert.ok(credentials.refs.OCG_GATEWAY_KEY?.length > 0);
  else assert.equal(credentials.refs.OCG_GATEWAY_KEY, secret);
  if (throughOcg) {
    const replaced = await mutateOcg("POST");
    assert.equal(replaced.application, "restart-required", JSON.stringify(replaced));
    assert.equal(replaced.installed, true);
  }
  const removed = throughOcg ? await mutateOcg("DELETE") : await rpc("pluginManager/removeBundle", { name: packageName });
  assert.equal(removed.application, "applied", JSON.stringify(removed));
  if (throughOcg) assert.equal(removed.installed, false);
  const final = await rpc("pluginManager/listBundles");
  assert.equal(final.some((entry) => entry.name === packageName), false);
  assert.deepEqual(final.map((entry) => entry.name).sort(), before.map((entry) => entry.name).sort());
  console.log(JSON.stringify({ success: true, surface: desktop ? "official-desktop-host" : "web",
    throughOcg, root, origin, installed: installed.application, removed: removed.application, baselineBundlesPreserved: true }));
  }
} finally {
  await stop(ocg);
  await stop(child, desktop);
  models.closeAllConnections();
  await new Promise((done) => models.close(done));
}
