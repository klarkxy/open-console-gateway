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
  assert.equal(defaults.OCG_LOG_LEVEL, "debug");
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
