import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";

import { assertPortFree, cliBinaryRelativePath, splitEnvironment, VITE_DEV_PORT } from "./dev-split.mjs";

test("split development reuses the dev gateway port and defaults to an isolated data dir", () => {
  const defaults = splitEnvironment({});
  assert.equal(defaults.OCG_GATEWAY_PORT, "19042");
  assert.match(defaults.OCG_DEV_DATA_DIR, /[\\/]tmp[\\/]dev-data$/);
  assert.equal(defaults.OCG_DEBUG_REQUESTS, "1");

  const overrides = splitEnvironment({ OCG_GATEWAY_PORT: " 19043 ", OCG_DEV_DATA_DIR: " D:/ocg-dev " });
  assert.equal(overrides.OCG_GATEWAY_PORT, "19043");
  assert.equal(overrides.OCG_DEV_DATA_DIR, "D:/ocg-dev");
});

test("CLI binary path matches the workspace target layout per platform", () => {
  assert.match(cliBinaryRelativePath("win32"), /target[\\/]debug[\\/]ocg-manager-cli\.exe$/);
  assert.match(cliBinaryRelativePath("linux"), /target[\\/]debug[\\/]ocg-manager-cli$/);
});

test("port preflight rejects an occupied port and accepts a free one", async () => {
  const blocker = net.createServer();
  await new Promise((resolve) => blocker.listen(0, "127.0.0.1", resolve));
  const occupied = blocker.address().port;
  try {
    await assert.rejects(assertPortFree(occupied, "Test"), /already in use/);
  } finally {
    await new Promise((resolve) => blocker.close(resolve));
  }
  await assertPortFree(occupied, "Test");
});

test("Vite dev server port stays aligned with vite.config.ts", () => {
  assert.equal(Number(VITE_DEV_PORT), 30001);
});
