import assert from "node:assert/strict";
import test from "node:test";

import { devEnvironment, tauriDevArgs } from "./dev.mjs";

test("development uses 19042 unless an explicit Gateway port is provided", () => {
  assert.equal(devEnvironment({}).OCG_GATEWAY_PORT, "19042");
  assert.equal(devEnvironment({ OCG_GATEWAY_PORT: "" }).OCG_GATEWAY_PORT, "19042");
  assert.equal(devEnvironment({ OCG_GATEWAY_PORT: " 19043 " }).OCG_GATEWAY_PORT, "19043");
});

test("development enables request capture and debug logging with explicit overrides", () => {
  const defaults = devEnvironment({});
  assert.equal(defaults.OCG_DEBUG_REQUESTS, "1");
  assert.equal(defaults.OCG_LOG_LEVEL, undefined);
  assert.match(defaults.OCG_DEBUG_DIR, /[\\/]\.artifacts[\\/]debug-requests$/);
  const overrides = devEnvironment({ OCG_DEBUG_REQUESTS: "0", OCG_LOG_LEVEL: "trace", OCG_DEBUG_DIR: "D:/captures" });
  assert.equal(overrides.OCG_DEBUG_REQUESTS, "0");
  assert.equal(overrides.OCG_LOG_LEVEL, "trace");
  assert.equal(overrides.OCG_DEBUG_DIR, "D:/captures");
});

test("development forwards extra arguments to the Tauri CLI", () => {
  assert.deepEqual(tauriDevArgs([]), ["dev"]);
  assert.deepEqual(tauriDevArgs(["--no-watch"]), ["dev", "--no-watch"]);
});

test("Windows child environment preserves pnpm's bin path without duplicate casing", () => {
  const env = devEnvironment({ PATH: "system-bin", Path: "local-bin;system-bin" }, "win32");
  assert.deepEqual(Object.keys(env).filter(key => key.toLowerCase() === "path"), ["Path"]);
  assert.equal(env.Path, "local-bin;system-bin");
  assert.equal(devEnvironment({ PATH: "system-bin" }, "win32").Path, "system-bin");
  assert.equal(devEnvironment({ PATH: "unix-bin" }, "linux").PATH, "unix-bin");
});

test("program log filtering has its own development default and override", () => {
  assert.equal(devEnvironment({}).RUST_LOG, "warn,ocg=debug");
  assert.equal(devEnvironment({ RUST_LOG: " error ", OCG_LOG_LEVEL: "trace" }).RUST_LOG, "error");
});
