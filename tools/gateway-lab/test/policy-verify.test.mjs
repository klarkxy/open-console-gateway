import assert from "node:assert/strict";
import test from "node:test";
import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { LIVE_KEY_ENV, LIVE_URL_ENV, repoRoot } from "../lib/common.mjs";
import { POLICY_HELP, resolvePolicyVerifyInput } from "../lib/policy-verify-run.mjs";
import { classifyConfigProbe } from "../lib/policy-api.mjs";
import { RESPONSIBILITY } from "../lib/policy-contract.mjs";

const verifyPath = path.join(repoRoot(), "tools", "gateway-lab", "policy-verify.mjs");
const missingCli = path.join(repoRoot(), "tools", "gateway-lab", "definitely-missing-ocg-cli.exe");

function spawnVerify(args, envOverrides = {}) {
  const env = { ...process.env, ...envOverrides };
  for (const key of Object.keys(env)) {
    if (key === LIVE_URL_ENV || key === LIVE_KEY_ENV || key.toUpperCase() === LIVE_URL_ENV || key.toUpperCase() === LIVE_KEY_ENV) {
      delete env[key];
    }
  }
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [verifyPath, ...args], {
      cwd: repoRoot(),
      env,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });
    child.on("close", (code) => {
      resolve({ code, stdout, stderr });
    });
  });
}

test("help prints usage and exits 0", async () => {
  const result = await spawnVerify(["--help"]);
  assert.equal(result.code, 0, result.stderr.slice(0, 400));
  assert.match(result.stdout, /--suite/);
  assert.match(result.stdout, /--lab-runtime/);
  assert.equal(result.stdout.includes(POLICY_HELP.slice(0, 40)), true);
});

test("unknown suite is a genuine error and exits 1", async () => {
  const result = await spawnVerify(["--cli", missingCli, "--suite", "not-a-suite", "--artifacts", "tmp-policy"]);
  assert.match(result.stderr, /unknown suite/);
  assert.equal(result.code, 1, `stdout=${result.stdout.slice(0, 300)}\nstderr=${result.stderr.slice(0, 300)}`);
});

test("live without lab-runtime refuses a local fixture and exits 1", async () => {
  const result = await spawnVerify(["--cli", missingCli, "--suite", "live", "--artifacts", "tmp-policy"]);
  assert.match(result.stderr, /lab-runtime/);
  assert.doesNotMatch(result.stderr, /OCG_LAB_REMOTE/);
  assert.equal(result.code, 1);
});

test("missing --artifacts is a genuine error", async () => {
  const result = await spawnVerify(["--cli", missingCli, "--suite", "local"]);
  assert.match(result.stderr, /--artifacts/);
  assert.equal(result.code, 1);
});

test("local plus lab-runtime is rejected", () => {
  assert.throws(
    () =>
      resolvePolicyVerifyInput([
        "--cli",
        missingCli,
        "--suite",
        "local",
        "--artifacts",
        "tmp",
        "--lab-runtime",
        "runtime.json",
      ]),
    /own simulator/,
  );
});

test("config probe 404 is missing, not a pass", () => {
  const classified = classifyConfigProbe(404);
  assert.equal(classified.ready, false);
  assert.equal(classified.missing, true);
});

test("config probe 200 is ready", () => {
  const classified = classifyConfigProbe(200);
  assert.equal(classified.ready, true);
  assert.equal(classified.missing, false);
});

test("local suite with missing CLI creates a simulator, keeps remoteCalls=0, and exits 2", async () => {
  const artifacts = path.join(repoRoot(), ".artifacts", "temporary-policy-20260926", "verify-local-missing-cli");
  const result = await spawnVerify(["--cli", missingCli, "--suite", "local", "--artifacts", artifacts]);
  assert.match(result.stdout, /NOT_RUN/);
  assert.match(result.stdout, /PASS local remoteCalls=0/);
  assert.doesNotMatch(result.stdout, /GOAT A,B then B/);
  assert.equal(result.code, 2, `stdout=${result.stdout.slice(0, 800)}\nstderr=${result.stderr.slice(0, 400)}`);
  const report = JSON.parse(await readFile(path.join(artifacts, "report.json"), "utf8"));
  assert.equal(report.exitCode, 2);
  assert.equal(report.remoteCallsDelta, 0);
  assert.ok(Array.isArray(report.companionRust) && report.companionRust.length >= 1);
  assert.ok(report.companionRust.every((row) => row.status === "NOT_RUN"));
  assert.ok(!(report.results || []).some((row) => row.responsibility === RESPONSIBILITY.RUST));
});

test("live suite with missing CLI attaches coordinator runtime and does not close it", async () => {
  const runtimePath = path.join(repoRoot(), ".artifacts", "temporary-policy-20260926", "live-lab", "runtime.json");
  let controlUrl = "";
  try {
    const { loadLabRuntimeFile } = await import("../lib/policy-lab-attach.mjs");
    const runtime = await loadLabRuntimeFile(runtimePath);
    controlUrl = runtime.control.url;
    const health = await fetch(`${controlUrl}/health`, { signal: AbortSignal.timeout(1500) });
    if (!health.ok) return;
  } catch {
    return;
  }
  const artifacts = path.join(repoRoot(), ".artifacts", "temporary-policy-20260926", "verify-live-missing-cli");
  const result = await spawnVerify([
    "--cli",
    missingCli,
    "--suite",
    "live",
    "--lab-runtime",
    runtimePath,
    "--artifacts",
    artifacts,
  ]);
  assert.match(result.stdout, /NOT_RUN/);
  assert.match(result.stdout, /PASS attached coordinator live-lab runtime/);
  assert.match(result.stdout, /PASS cleanup preserves external lab/);
  assert.doesNotMatch(result.stdout, /GOAT A,B then B/);
  assert.equal(result.code, 2, `stdout=${result.stdout.slice(0, 800)}\nstderr=${result.stderr.slice(0, 400)}`);
  const still = await fetch(`${controlUrl}/health`, { signal: AbortSignal.timeout(1500) });
  assert.equal(still.status, 200);
  const report = JSON.parse(await readFile(path.join(artifacts, "report.json"), "utf8"));
  assert.equal(report.remoteCallsDelta, 0);
  assert.equal(report.exitCode, 2);
  assert.ok(!(report.results || []).some((row) => row.responsibility === RESPONSIBILITY.RUST));
});

