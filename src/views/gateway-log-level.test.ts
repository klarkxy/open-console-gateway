import assert from "node:assert/strict";
import test from "node:test";
import { dashboardApi } from "../api/dashboard.ts";
import { installFetchMock } from "../test-helpers/dashboard-v3-fetch.ts";
import { gatewayLogLevelTag, parseGatewayLogLevel } from "./gateway-log-level.ts";

test("gateway level selection validates URL values and chooses semantic tag colors", () => {
  assert.equal(parseGatewayLogLevel("warn"), "WARN");
  assert.equal(parseGatewayLogLevel("fatal"), "");
  assert.equal(gatewayLogLevelTag("TRACE"), "default");
  assert.equal(gatewayLogLevelTag("DEBUG"), "primary");
  assert.equal(gatewayLogLevelTag("INFO"), "info");
  assert.equal(gatewayLogLevelTag("WARN"), "warning");
  assert.equal(gatewayLogLevelTag("ERROR"), "error");
});

test("gateway API forwards exact level, category and request ID before the 200-row limit", async () => {
  const requests = installFetchMock(() => ({
    revision: 1, processGeneration: 1, pricingRevision: null, items: [],
  }));
  await dashboardApi.getGatewayLogs({ limit: 200, requestId: "request /1", level: "DEBUG", category: "gateway.http" });
  const url = new URL(requests[0]!.url, "http://localhost");
  assert.equal(url.pathname, "/dashboard/api/v4/logs/gateway");
  assert.deepEqual(Object.fromEntries(url.searchParams), {
    limit: "200", requestId: "request /1", level: "DEBUG", category: "gateway.http",
  });
});
