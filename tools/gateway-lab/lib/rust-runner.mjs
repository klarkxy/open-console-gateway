import { spawn } from "node:child_process";
import { repoRoot } from "./common.mjs";
import { registerOwnedChild, stopPid } from "./process.mjs";

function boundText(text, limit = 8000) {
  const value = String(text || "");
  if (value.length <= limit) return value;
  return `${value.slice(0, limit)}\n...[truncated ${value.length - limit} bytes]`;
}

export function runRustTest({ crate, testFile, testName, timeoutMs = 360000 }) {
  const args = ["test", "-p", crate, "--test", testFile, testName, "--", "--exact", "--nocapture"];
  const started = Date.now();
  return new Promise((resolve) => {
    const child = spawn("cargo", args, {
      cwd: repoRoot(),
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    registerOwnedChild(child);
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => {
      if (child.pid) stopPid(child.pid);
    }, timeoutMs);
    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString("utf8");
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString("utf8");
    });
    child.on("error", (error) => {
      clearTimeout(timer);
      resolve({
        ok: false,
        exitCode: null,
        command: ["cargo", ...args],
        testName,
        testFile,
        crate,
        durationMs: Date.now() - started,
        stdout: boundText(stdout),
        stderr: boundText(`${stderr}\n${error.message}`),
        missing: /ENOENT/i.test(error.message),
      });
    });
    child.on("close", (code, signal) => {
      clearTimeout(timer);
      resolve({
        ok: code === 0,
        exitCode: code,
        signal,
        command: ["cargo", ...args],
        testName,
        testFile,
        crate,
        durationMs: Date.now() - started,
        stdout: boundText(stdout),
        stderr: boundText(stderr),
        skipped: /ignored|no tests to run/i.test(stdout + stderr) && code === 0 && !new RegExp(`test ${testName} \\.\\.\\. ok`).test(stdout),
      });
    });
  });
}

export async function recordRustEvidence(collector, { scenarioId, crate, testFile, testName, timeoutMs }) {
  const result = await runRustTest({ crate, testFile, testName, timeoutMs });
  const extra = {
    scenarioId,
    evidenceKind: "rust_integration",
    command: result.command,
    exitCode: result.exitCode,
    durationMs: result.durationMs,
    testName,
    testFile,
  };
  if (result.missing || result.skipped) {
    collector.notRun(scenarioId, result.missing ? "cargo missing" : "rust test skipped", extra);
    return result;
  }
  if (result.ok && /test result: ok/.test(result.stdout) && new RegExp(`${testName}`).test(result.stdout)) {
    collector.pass(scenarioId, extra);
  } else {
    collector.fail(scenarioId, new Error(`rust test exit ${result.exitCode}`), { ...extra, stderr: result.stderr.slice(-1500) });
  }
  return result;
}
