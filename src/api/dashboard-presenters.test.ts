import assert from "node:assert/strict";
import test from "node:test";
import type { AppConfig } from "./dashboard-presenters.ts";
import { settingsUpdateInput } from "./dashboard-presenters.ts";

function appConfig(overrides: Partial<AppConfig> = {}): AppConfig {
  return {
    revision: 1,
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
