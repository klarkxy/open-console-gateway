import assert from "node:assert/strict";
import test from "node:test";

import { devEnvironment } from "./dev.mjs";

test("development uses 19042 unless an explicit Gateway port is provided", () => {
  assert.equal(devEnvironment({}).OCG_GATEWAY_PORT, "19042");
  assert.equal(devEnvironment({ OCG_GATEWAY_PORT: "" }).OCG_GATEWAY_PORT, "19042");
  assert.equal(devEnvironment({ OCG_GATEWAY_PORT: " 19043 " }).OCG_GATEWAY_PORT, "19043");
});
