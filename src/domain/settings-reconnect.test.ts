import assert from "node:assert/strict";
import test from "node:test";

type SettingsReconnectKind = "stay" | "manual-recovery";

type SettingsReconnectPlan = {
  kind: SettingsReconnectKind;
  href: string;
};

type SettingsReconnectModule = {
  planSettingsReconnect: (input: {
    href: string;
    previousGatewayPort: number;
    nextGatewayPort: number;
    dev: boolean;
  }) => SettingsReconnectPlan;
  SETTINGS_RECONNECT_KIND_KEYS: Record<SettingsReconnectKind, string>;
};

async function loadReconnect(): Promise<SettingsReconnectModule> {
  return await import("./settings-reconnect.ts") as SettingsReconnectModule;
}

test("settings reconnect kinds have semantic-code copy mappings", async () => {
  const { SETTINGS_RECONNECT_KIND_KEYS } = await loadReconnect();
  for (const kind of ["stay", "manual-recovery"] as const) {
    assert.equal(typeof SETTINGS_RECONNECT_KIND_KEYS[kind], "string");
    assert.ok(SETTINGS_RECONNECT_KIND_KEYS[kind].length > 0);
  }
});

test("DEV origins stay on the current href when the gateway port changes", async () => {
  const { planSettingsReconnect } = await loadReconnect();
  const vite = planSettingsReconnect({
    href: "http://127.0.0.1:5173/dashboard/#/settings",
    previousGatewayPort: 9042,
    nextGatewayPort: 9050,
    dev: true,
  });
  assert.equal(vite.kind, "stay");
  assert.equal(vite.href, "http://127.0.0.1:5173/dashboard/#/settings");

  const coincidentalPort = planSettingsReconnect({
    href: "http://127.0.0.1:9042/dashboard/#/settings",
    previousGatewayPort: 9042,
    nextGatewayPort: 9050,
    dev: true,
  });
  assert.equal(coincidentalPort.kind, "stay");
  assert.equal(coincidentalPort.href, "http://127.0.0.1:9042/dashboard/#/settings");
});

test("reverse-proxy origins stay even when the page port equals the gateway port", async () => {
  const { planSettingsReconnect } = await loadReconnect();
  const frontDoor = planSettingsReconnect({
    href: "https://ocg.example.com/dashboard/#/settings?x=1",
    previousGatewayPort: 9042,
    nextGatewayPort: 9050,
    dev: false,
  });
  assert.equal(frontDoor.kind, "stay");
  assert.equal(frontDoor.href, "https://ocg.example.com/dashboard/#/settings?x=1");

  const matchingPublicPort = planSettingsReconnect({
    href: "https://ocg.example.com:9042/dashboard/ui/#/settings",
    previousGatewayPort: 9042,
    nextGatewayPort: 9050,
    dev: false,
  });
  assert.equal(matchingPublicPort.kind, "stay");
  assert.equal(matchingPublicPort.href, "https://ocg.example.com:9042/dashboard/ui/#/settings");
});

test("loopback matching the old gateway port offers a manual recovery URL and does not auto-navigate", async () => {
  const { planSettingsReconnect } = await loadReconnect();
  const hashed = planSettingsReconnect({
    href: "http://127.0.0.1:9042/dashboard/ui/#/settings?tab=proxy",
    previousGatewayPort: 9042,
    nextGatewayPort: 9050,
    dev: false,
  });
  assert.equal(hashed.kind, "manual-recovery");
  assert.equal(hashed.href, "http://127.0.0.1:9050/dashboard/ui/#/settings?tab=proxy");

  const searched = planSettingsReconnect({
    href: "http://localhost:9042/dashboard?from=save#/settings",
    previousGatewayPort: 9042,
    nextGatewayPort: 19042,
    dev: false,
  });
  assert.equal(searched.kind, "manual-recovery");
  assert.equal(searched.href, "http://localhost:19042/dashboard?from=save#/settings");

  const ipv6 = planSettingsReconnect({
    href: "http://[::1]:9042/dashboard/#/settings",
    previousGatewayPort: 9042,
    nextGatewayPort: 9050,
    dev: false,
  });
  assert.equal(ipv6.kind, "manual-recovery");
  assert.equal(ipv6.href, "http://[::1]:9050/dashboard/#/settings");
});

test("an unchanged gateway port stays on the current href", async () => {
  const { planSettingsReconnect } = await loadReconnect();
  const plan = planSettingsReconnect({
    href: "http://127.0.0.1:9042/dashboard/#/settings",
    previousGatewayPort: 9042,
    nextGatewayPort: 9042,
    dev: false,
  });
  assert.equal(plan.kind, "stay");
  assert.equal(plan.href, "http://127.0.0.1:9042/dashboard/#/settings");
});
