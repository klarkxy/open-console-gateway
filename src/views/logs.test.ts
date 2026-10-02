import assert from "node:assert/strict";
import test from "node:test";
import { DashboardRequestError, dashboardV3 } from "../api/dashboard-v3.ts";
import { dashboardApi } from "../api/dashboard.ts";
import { useConnectionStore } from "../stores/connection.ts";
import {
  installFetchMock,
  setupControlPlane,
  v3AccountDto,
} from "../test-helpers/dashboard-v3-fetch.ts";
import { computeTimeRange, resolveTimeRange } from "./log-time-range.ts";

function v3ForwardLogs(): object {
  return {
    revision: 1,
    processGeneration: 99,
    pricingRevision: null,
    items: [],
    summary: {
      totalRequests: 0,
      promptTokens: 0,
      completionTokens: 0,
      cachedTokens: 0,
      cost: 0,
    },
  };
}

test("forward log API sends remote paging and filter parameters", async () => {
  const requests = installFetchMock(() => v3ForwardLogs());

  await dashboardApi.getForwardLogs({
    limit: 20,
    offset: 40,
    status: "success",
    account_id: "account 117",
    request_id: "ocg-test id",
    provider_id: "opencode",
    route_account_id: "route 1",
    credential_account_id: "cred 2",
    sort_by: "attempt",
    sort_order: "asc",
  });

  const query = new URL(requests[0]!.url, "http://localhost").searchParams;
  assert.equal(query.get("limit"), "20");
  assert.equal(query.get("offset"), "40");
  assert.equal(query.get("status"), "success");
  assert.equal(query.get("accountId"), "account 117");
  assert.equal(query.get("requestId"), "ocg-test id");
  assert.equal(query.get("providerId"), "opencode");
  assert.equal(query.get("routeAccountId"), "route 1");
  assert.equal(query.get("credentialAccountId"), "cred 2");
  assert.equal(query.get("sortBy"), "attempt");
  assert.equal(query.get("sortOrder"), "asc");
});

test("dashboard request errors preserve status for localized handling", async () => {
  installFetchMock(() => new Response(JSON.stringify({ code: "conflict", message: "raw fallback" }), {
    status: 409,
    headers: { "Content-Type": "application/json" },
  }));

  await assert.rejects(
    () => dashboardV3.registerAdmin("admin", "password123", { expectedRevision: 0, processGeneration: 0 }),
    (error) => error instanceof DashboardRequestError
      && error.status === 409
      && error.message === "raw fallback",
  );
});

test("dashboard request errors preserve a non-JSON proxy response body", async () => {
  installFetchMock(() => new Response("<h1>Bad Gateway</h1>", {
    status: 502,
    statusText: "Bad Gateway",
    headers: { "Content-Type": "text/html" },
  }));

  await assert.rejects(
    () => dashboardV3.registerAdmin("admin", "password123", { expectedRevision: 0, processGeneration: 0 }),
    (error) => error instanceof DashboardRequestError
      && error.status === 502
      && error.message === "<h1>Bad Gateway</h1>",
  );
});

test("settings update sends the captured snapshot CAS and does not revalidate inside the write", async () => {
  setupControlPlane(9, 100);
  const requests = installFetchMock(({ url, method }) => {
    if (method === "PUT" && url.endsWith("/settings")) {
      return { revision: 8, processGeneration: 11 };
    }
    throw new Error(`unexpected request ${method} ${url}`);
  });

  const result = await dashboardApi.updateSettings({
    revision: 7,
    process_generation: 11,
    gateway_port: 9042,
    gateway_port_from_env: true,
    proxy_mode: "auto",
    proxy_url: "",
    proxy_list_direction: "whitelist",
    proxy_list_models: [],
    proxy_supported_models: [],
    opencode_invite_url: "https://opencode.ai/go?ref=68XPB6NP8V",
    client_root_url: "",
    client_root_url_from_env: false,
    auto_start: false,
    auto_start_supported: false,
    show_dock_icon: true,
    dock_visibility_supported: false,
    connect_timeout_secs: 30,
    non_stream_timeout_secs: 900,
    stream_idle_timeout_secs: 300,
    routing_mode: "strict-priority",
    conversation_sticky: false,
  });

  assert.equal(requests.length, 1);
  assert.equal(requests[0]?.method, "PUT");
  const body = requests[0]?.body ?? {};
  assert.equal(body.expectedRevision, 7);
  assert.equal(body.processGeneration, 11);
  assert.equal(body.routingMode, "strict-priority");
  assert.equal(body.conversationSticky, false);
  assert.equal(body.opencodeInviteUrl, "https://opencode.ai/go?ref=68XPB6NP8V");
  assert.equal("gatewayPort" in body, false);
  assert.equal("autoStart" in body, false);
  assert.equal("showDockIcon" in body, false);
  assert.equal("pricingRevision" in body, false);
  assert.equal("revision" in body, false);
  assert.deepEqual(result, { revision: 8, processGeneration: 11 });
});

test("primary key regeneration finishes at the POST receipt and reads plaintext back once", async () => {
  setupControlPlane(7);
  let releaseRead!: (response: Response) => void;
  const read = new Promise<Response>((resolve) => { releaseRead = resolve; });
  const requests = installFetchMock(({ url, method }) => {
    if (method === "POST" && url.endsWith("/keys/primary/regenerate")) {
      return { revision: 8, processGeneration: 99 };
    }
    if (method === "GET" && url.endsWith("/connection")) return read;
    throw new Error(`unexpected request ${method} ${url}`);
  });

  const store = useConnectionStore();
  const regenerated = await store.regeneratePrimaryKey();
  assert.equal(regenerated.committed, true);
  assert.equal(regenerated.value, undefined);
  assert.deepEqual(
    requests.filter(({ method }) => method === "POST").map(({ body }) => body),
    [{ expectedRevision: 7, processGeneration: 99 }],
  );
  assert.equal(requests.filter(({ method }) => method === "GET").length, 1);
  releaseRead(new Response(JSON.stringify({
    revision: 8,
    processGeneration: 99,
    gatewayPort: 9042,
    clientRootUrl: "http://127.0.0.1:9042",
    primaryKey: "ocg-new-key",
    subKeys: [],
  }), { headers: { "Content-Type": "application/json" } }));
  assert.equal(await regenerated.revalidation, "loaded");
  assert.equal(regenerated.value, "ocg-new-key");
  assert.equal(store.info?.primary_key, "ocg-new-key");
  assert.equal(requests.filter(({ method }) => method === "POST").length, 1);
});

test("account API sends purchase dates and the complete reorder payload", async () => {
  setupControlPlane(1);
  const requests = installFetchMock(({ url, method }) => {
    if (method === "POST" && url.endsWith("/accounts")) {
      return { account: v3AccountDto("account-2"), revision: 1, processGeneration: 99 };
    }
    if (method === "PUT" && url.endsWith("/accounts/order")) {
      return { accounts: [v3AccountDto("account-2")], revision: 1, processGeneration: 99 };
    }
    throw new Error(`unexpected request ${method} ${url}`);
  });

  const created = await dashboardApi.createAccount({
    name: "Second",
    key: "sk-test",
    purchase_date: "2026-07-15",
  });
  const reordered = await dashboardApi.reorderAccounts(["account-2", "account-1"]);

  assert.equal(created.purchase_date, "2026-07-15");
  assert.equal(created.expires_on, "2026-08-15");
  assert.equal(reordered[0]?.id, "account-2");
  assert.deepEqual(requests, [
    {
      url: "/dashboard/api/v4/accounts",
      method: "POST",
      body: {
        name: "Second",
        key: "sk-test",
        purchaseDate: "2026-07-15",
        expectedRevision: 1,
        processGeneration: 99,
      },
    },
    {
      url: "/dashboard/api/v4/accounts/order",
      method: "PUT",
      body: { accountIds: ["account-2", "account-1"], expectedRevision: 1, processGeneration: 99 },
    },
  ]);
});

test("managed account API uses ordered setup, browser targets, and profile reset routes", async () => {
  setupControlPlane(1);
  const requests = installFetchMock(({ url }) => {
    if (url.endsWith("/browser/capabilities")) {
      return { mode: "remote", reason: null, revision: 1, processGeneration: 99, pricingRevision: null };
    }
    if (url.endsWith("/browser-profile")) {
      return { account: v3AccountDto("managed-1"), revision: 1, processGeneration: 99 };
    }
    if (url.endsWith("/browser")) {
      return { mode: "remote", sessionToken: "session-1", revision: 1, processGeneration: 99, pricingRevision: null };
    }
    return { account: v3AccountDto("managed-1"), revision: 1, processGeneration: 99 };
  });

  await dashboardApi.createManagedAccount({ name: "Managed", username: "note@example.com" });
  await dashboardApi.advanceAccountSetup("managed-1", "opencode_registration");
  await dashboardApi.verifyManagedAccountKey("managed-1", "sk-secret");
  assert.deepEqual(await dashboardApi.getBrowserCapabilities(), { mode: "remote", reason: null });
  assert.deepEqual(await dashboardApi.openAccountBrowser("managed-1", "invite"), {
    mode: "remote",
    session_token: "session-1",
  });
  await dashboardApi.resetAccountBrowserProfile("managed-1");

  assert.deepEqual(requests.map(({ url, method }) => ({
    path: new URL(url, "http://localhost").pathname,
    method,
  })), [
    { path: "/dashboard/api/v4/accounts/managed", method: "POST" },
    { path: "/dashboard/api/v4/accounts/managed-1/setup", method: "PATCH" },
    { path: "/dashboard/api/v4/accounts/managed-1/setup/verify-key", method: "POST" },
    { path: "/dashboard/api/v4/browser/capabilities", method: "GET" },
    { path: "/dashboard/api/v4/accounts/managed-1/browser", method: "POST" },
    { path: "/dashboard/api/v4/accounts/managed-1/browser-profile", method: "DELETE" },
  ]);
  assert.deepEqual(requests[4]?.body, {
    target: "invite",
    expectedRevision: 1,
    processGeneration: 99,
  });
});

test("logs time range helpers cover all presets", async () => {
  const now = new Date(2026, 6, 19, 12, 0, 0, 0);
  assert.deepEqual(computeTimeRange("last24h", now), [
    now.getTime() - 24 * 60 * 60 * 1000,
    now.getTime(),
  ]);
  assert.deepEqual(computeTimeRange("last7d", now), [
    now.getTime() - 7 * 24 * 60 * 60 * 1000,
    now.getTime(),
  ]);
  assert.deepEqual(computeTimeRange("last30d", now), [
    now.getTime() - 30 * 24 * 60 * 60 * 1000,
    now.getTime(),
  ]);
  assert.deepEqual(computeTimeRange("thisMonth", now), [
    new Date(2026, 6, 1).getTime(),
    now.getTime(),
  ]);
  assert.deepEqual(computeTimeRange("lastMonth", now), [
    new Date(2026, 5, 1).getTime(),
    new Date(2026, 5, 30, 23, 59, 59, 999).getTime(),
  ]);
});

test("rolling log presets resolve against the current refresh time", async () => {
  const first = new Date("2026-07-19T00:00:00Z");
  const later = new Date("2026-07-19T03:00:00Z");
  const staleSelection = computeTimeRange("last24h", first);

  assert.deepEqual(resolveTimeRange("last24h", staleSelection, later), computeTimeRange("last24h", later));
  assert.deepEqual(resolveTimeRange("custom", staleSelection, later), staleSelection);
  assert.equal(resolveTimeRange("all", staleSelection, later), null);


});
