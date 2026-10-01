import assert from "node:assert/strict";
import test from "node:test";
import type { ForwardLog as V3ForwardLog, Settings } from "./generated/dashboard-v3.ts";
import type { AppConfig } from "./dashboard-presenters.ts";
import { presentForwardLog, presentSettings, settingsUpdateInput } from "./dashboard-presenters.ts";

function appConfig(overrides: Partial<AppConfig> = {}): AppConfig {
  return {
    revision: 1,
    process_generation: 11,
    gateway_port: 9042,
    gateway_port_from_env: false,
    proxy_mode: "auto",
    proxy_url: "",
    proxy_list_direction: "whitelist",
    proxy_list_models: [],
    proxy_supported_models: [],
    opencode_invite_url: "https://invite.example.test",
    client_root_url: "https://client.example.test",
    client_root_url_from_env: false,
    auto_start: false,
    auto_start_supported: false,
    show_dock_icon: false,
    dock_visibility_supported: false,
    connect_timeout_secs: 10,
    non_stream_timeout_secs: 60,
    stream_idle_timeout_secs: 300,
    routing_mode: "strict-priority",
    conversation_sticky: true,
    ...overrides,
  };
}

function assertAlwaysSentFields(input: ReturnType<typeof settingsUpdateInput>, value: AppConfig): void {
  assert.equal(input.clientRootUrl, value.client_root_url);
  assert.equal(input.connectTimeoutSecs, value.connect_timeout_secs);
  assert.equal(input.conversationSticky, value.conversation_sticky);
  assert.equal(input.nonStreamTimeoutSecs, value.non_stream_timeout_secs);
  assert.equal(input.opencodeInviteUrl, value.opencode_invite_url);
  assert.equal(input.proxyListDirection, value.proxy_list_direction);
  assert.equal(input.proxyListModels, value.proxy_list_models);
  assert.equal(input.proxyMode, value.proxy_mode);
  assert.equal(input.proxyUrl, value.proxy_url);
  assert.equal(input.routingMode, value.routing_mode);
  assert.equal(input.streamIdleTimeoutSecs, value.stream_idle_timeout_secs);
}

test("settingsUpdateInput sends supported capabilities including false and omits unsupported ones", () => {
  const cases: Array<[Partial<AppConfig>, { autoStart?: boolean; showDockIcon?: boolean }]> = [
    [{ auto_start: true, auto_start_supported: true, dock_visibility_supported: false }, { autoStart: true }],
    [{ auto_start: false, auto_start_supported: true, dock_visibility_supported: false }, { autoStart: false }],
    [{ auto_start: true, auto_start_supported: false, show_dock_icon: true, dock_visibility_supported: false }, {}],
    [{ auto_start: true, auto_start_supported: true, show_dock_icon: true, dock_visibility_supported: true }, { autoStart: true, showDockIcon: true }],
    [{ auto_start: false, auto_start_supported: true, show_dock_icon: false, dock_visibility_supported: true }, { autoStart: false, showDockIcon: false }],
    [{ auto_start_supported: false, show_dock_icon: true, dock_visibility_supported: true }, { showDockIcon: true }],
    [{ auto_start_supported: false, show_dock_icon: false, dock_visibility_supported: true }, { showDockIcon: false }],
  ];
  for (const [config, expected] of cases) {
    const value = appConfig(config);
    const input = settingsUpdateInput(value);
    for (const field of ["autoStart", "showDockIcon"] as const) {
      const present = Object.hasOwn(expected, field);
      assert.equal(Object.hasOwn(input, field), present, `${field} ${JSON.stringify(config)}`);
      if (present) assert.equal(input[field], expected[field], `${field} ${JSON.stringify(config)}`);
    }
    assertAlwaysSentFields(input, value);
  }
});

test("settingsUpdateInput omits gatewayPort when gateway port comes from the environment", () => {
  const value = appConfig({ gateway_port_from_env: true });
  const input = settingsUpdateInput(value);
  assert.equal("gatewayPort" in input, false);
  assertAlwaysSentFields(input, value);
});

test("settingsUpdateInput sends the exact gatewayPort when it is not from the environment", () => {
  const value = appConfig({ gateway_port_from_env: false, gateway_port: 19042 });
  const input = settingsUpdateInput(value);
  assert.equal("gatewayPort" in input, true);
  assert.equal(input.gatewayPort, 19042);
});

function v3Settings(processGeneration: number): Settings {
  return {
    autoStart: false,
    autoStartSupported: true,
    clientRootUrl: "https://client.example.test",
    clientRootUrlFromEnv: false,
    connectTimeoutSecs: 10,
    conversationSticky: true,
    dockVisibilitySupported: false,
    gatewayPort: 9042,
    gatewayPortFromEnv: false,
    nonStreamTimeoutSecs: 60,
    opencodeInviteUrl: "https://invite.example.test",
    processGeneration,
    proxyListDirection: "whitelist",
    proxyListModels: [],
    proxyMode: "auto",
    proxySupportedModels: [],
    proxyUrl: "",
    revision: 7,
    routingMode: "strict-priority",
    showDockIcon: null,
    streamIdleTimeoutSecs: 300,
  };
}

test("presentSettings keeps the snapshot process generation beside its revision", () => {
  const presented = presentSettings(v3Settings(44));
  assert.equal(presented.revision, 7);
  assert.equal(presented.process_generation, 44);
  assert.equal(presented.routing_mode, "strict-priority");
  assert.equal(presented.conversation_sticky, true);
});

function historicalForwardLog(): V3ForwardLog {
  return {
    id: 17,
    timestamp: "2026-07-01T00:00:00Z",
    model: "grok-4.5",
    requestedModel: "grok",
    resolvedAlias: null,
    upstreamModel: "grok-4.5",
    accountId: "acct-hist",
    accountName: "historical",
    clientKeyId: null,
    clientKeyName: null,
    routeAccountId: null,
    providerId: "custom",
    credentialAccountId: null,
    rawCostUsd: null,
    quotaDebit: null,
    effectivePaidCostUsd: null,
    nativeCostValue: 0.02,
    nativeCostUnit: "USD",
    nativeCostCurrency: "USD",
    status: "success",
    httpStatus: 200,
    route: "primary",
    promptTokens: 11,
    completionTokens: 7,
    cachedTokens: 3,
    cacheCreationTokens: 0,
    cost: null,
    costState: "unknown",
    pricingRevisionId: "hist-2026-07-01",
    quotaMultiplier: 1.5,
    localAdjustmentMultiplier: null,
    serviceTier: null,
    errorMessage: null,
    requestId: "req-hist",
    attempt: 1,
    errorSource: null,
    errorStage: null,
    durationMs: 40,
    diagnostic: null,
  };
}

test("stored forward-log cost and pricing fields decode without becoming a zero charge", () => {
  const row = presentForwardLog(historicalForwardLog());
  assert.equal(row.cost, null);
  assert.equal(row.cost_state, "unknown");
  assert.equal(row.pricing_revision_id, "hist-2026-07-01");
  assert.equal(row.quota_multiplier, 1.5);
  assert.equal(row.native_cost_value, 0.02);
  assert.equal(row.native_cost_currency, "USD");
  assert.equal(row.prompt_tokens, 11);
  assert.equal(row.completion_tokens, 7);
  assert.equal(row.cached_tokens, 3);
  assert.equal(row.raw_cost_usd, null);
  assert.equal(row.quota_debit, null);
});
