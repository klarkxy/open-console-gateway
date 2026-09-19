import assert from "node:assert/strict";
import test from "node:test";
import { spawn } from "node:child_process";
import path from "node:path";
import { LIVE_KEY_ENV, LIVE_URL_ENV, repoRoot } from "../lib/common.mjs";

const cliPath = path.join(repoRoot(), "tools", "gateway-lab", "cli.mjs");
const missingCli = path.join(repoRoot(), "tools", "gateway-lab", "definitely-missing-ocg-cli.exe");

function spawnCli(args, envOverrides = {}) {
  const env = { ...process.env, ...envOverrides };
  for (const key of Object.keys(env)) {
    if (key === LIVE_URL_ENV || key === LIVE_KEY_ENV || key.toUpperCase() === LIVE_URL_ENV || key.toUpperCase() === LIVE_KEY_ENV) {
      delete env[key];
    }
  }
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [cliPath, ...args], {
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

test("verify --suite live without remote env prints NOT_RUN and exits 2", async () => {
  const result = await spawnCli(["verify", "--cli", missingCli, "--suite", "live"]);
  assert.match(result.stdout, /NOT_RUN/);
  assert.equal(result.code, 2, `stdout=${result.stdout.slice(0, 400)}\nstderr=${result.stderr.slice(0, 400)}`);
});

test("unknown verify suite is a genuine error and exits 1", async () => {
  const result = await spawnCli(["verify", "--cli", missingCli, "--suite", "not-a-suite"]);
  assert.match(result.stderr, /unknown suite/);
  assert.equal(result.code, 1, `stdout=${result.stdout.slice(0, 400)}\nstderr=${result.stderr.slice(0, 400)}`);
});

test("thrown error after process.exitCode 2 still exits 1", async () => {
  const { runCli } = await import("../cli.mjs");
  const previous = process.exitCode;
  process.exitCode = 2;
  try {
    const code = await runCli(["verify", "--cli", missingCli, "--suite", "not-a-suite"]);
    assert.equal(code, 1);
    assert.equal(process.exitCode, 1);
  } finally {
    process.exitCode = previous;
  }
});
