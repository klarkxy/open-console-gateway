import assert from "node:assert/strict";
import test from "node:test";
import type { Destination } from "../api/destinations.ts";
import { planPresetProtocolMigration } from "./destination-protocol-migration.ts";
import { PROVIDER_PRESETS, type ProviderPreset } from "./provider-presets.ts";

function destination(overrides: Partial<Destination> = {}): Destination {
  return {
    account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
    adapter: "http",
    auth_scheme: "bearer",
    base_url: "https://api.lab.example/v1/chat/completions",
    brand_family: null,
    capabilities: {
      billing_tier_required: false,
      discoverable_models: true,
      external_integration: false,
      identity_headers: false,
      managed_signup: false,
      observer: false,
      official_balance_probe: [],
      redirect_policy: "no_follow",
      testable: true,
    },
    catalog: [{
      enabled: true,
      preferred: "chat_completions",
      protocols: ["chat_completions"],
      public_model: "lab-opus",
      upstream_model: "vendor/opus",
      upstream_override: null,
    }],
    enabled: true,
    id: "dest-1",
    legacy: { kind: "custom_account", id: "acct-1" },
    max_credentials: null,
    name: "Lab HTTP",
    observer_credential_id: null,
    plan: null,
    protocols: ["chat_completions"],
    ...overrides,
  };
}

function preset(overrides: Partial<ProviderPreset> = {}): ProviderPreset {
  return {
    id: "lab-preset",
    name: "Lab Preset",
    category: "official",
    endpointUrl: "https://api.lab.example/v1/chat/completions",
    protocol: "chat_completions",
    authKind: "bearer",
    docsUrl: "https://docs.lab.example",
    websiteUrl: "https://lab.example",
    note: { en: "Lab", zh: "Lab" },
    protocolRoutes: [
      { protocol: "chat_completions", endpointUrl: "https://api.lab.example/v1/chat/completions", authScheme: "bearer" },
      { protocol: "responses", endpointUrl: "https://api.lab.example/v1/responses", authScheme: "bearer" },
      { protocol: "messages", endpointUrl: "https://api.lab.example/anthropic/v1/messages", authScheme: "x_api_key" },
    ],
    ...overrides,
  };
}

test("a legacy single-route destination gains every missing preset route", () => {
  const row = destination();
  const plan = planPresetProtocolMigration(row, preset());
  assert.ok(plan);
  assert.deepEqual(plan.added.map((route) => route.protocol), ["responses", "messages"]);
  assert.equal(plan.dropped.length, 0);
  assert.equal(plan.routes.length, 3);
  // The legacy first route keeps its exact URL and auth.
  assert.deepEqual(plan.routes[0], {
    protocol: "chat_completions",
    endpoint_url: "https://api.lab.example/v1/chat/completions",
    auth_scheme: "bearer",
  });
  assert.equal(plan.routes[1]?.endpoint_url, "https://api.lab.example/v1/responses");
  assert.equal(plan.routes[2]?.auth_scheme, "x_api_key");
});

test("explicit destination routes win over the legacy derivation and only the gap is appended", () => {
  const row = destination({
    protocols: ["chat_completions", "responses"],
    protocol_routes: [
      { protocol: "chat_completions", endpoint_url: "https://relay.example/custom/chat", auth_scheme: "bearer" },
      { protocol: "responses", endpoint_url: "https://relay.example/custom/responses", auth_scheme: "bearer" },
    ],
  });
  const plan = planPresetProtocolMigration(row, preset());
  assert.ok(plan);
  assert.deepEqual(plan.added.map((route) => route.protocol), ["messages"]);
  assert.equal(plan.routes.length, 3);
  // Custom URLs survive untouched; the preset route is appended after them.
  assert.equal(plan.routes[0]?.endpoint_url, "https://relay.example/custom/chat");
  assert.equal(plan.routes[1]?.endpoint_url, "https://relay.example/custom/responses");
  assert.equal(plan.routes[2]?.endpoint_url, "https://api.lab.example/anthropic/v1/messages");
});

test("a destination that already covers the preset plans nothing", () => {
  const row = destination({
    protocols: ["chat_completions", "responses", "messages"],
    protocol_routes: [
      { protocol: "chat_completions", endpoint_url: "https://a.example/1", auth_scheme: "bearer" },
      { protocol: "responses", endpoint_url: "https://a.example/2", auth_scheme: "bearer" },
      { protocol: "messages", endpoint_url: "https://a.example/3", auth_scheme: "api_key" },
    ],
  });
  assert.equal(planPresetProtocolMigration(row, preset()), null);
});

test("the route cap appends only what fits and reports the rest as dropped", () => {
  // Corrupt-but-persistable data: two stored routes share one protocol, so
  // one distinct protocol occupies two slots and only one slot is free.
  const row = destination({
    protocols: ["chat_completions"],
    protocol_routes: [
      { protocol: "chat_completions", endpoint_url: "https://a.example/1", auth_scheme: "bearer" },
      { protocol: "chat_completions", endpoint_url: "https://a.example/2", auth_scheme: "bearer" },
    ],
  });
  const plan = planPresetProtocolMigration(row, preset());
  assert.ok(plan);
  assert.equal(plan.routes.length, 3);
  assert.deepEqual(plan.added.map((route) => route.protocol), ["responses"]);
  assert.deepEqual(plan.dropped.map((route) => route.protocol), ["messages"]);
  // Every existing route, even the duplicate, is preserved verbatim.
  assert.deepEqual(plan.routes.slice(0, 2).map((route) => route.endpoint_url), [
    "https://a.example/1",
    "https://a.example/2",
  ]);
});

test("a full route set plans nothing instead of overflowing", () => {
  const row = destination({
    protocols: ["chat_completions"],
    protocol_routes: [
      { protocol: "chat_completions", endpoint_url: "https://a.example/1", auth_scheme: "bearer" },
      { protocol: "chat_completions", endpoint_url: "https://a.example/2", auth_scheme: "bearer" },
      { protocol: "chat_completions", endpoint_url: "https://a.example/3", auth_scheme: "bearer" },
    ],
  });
  assert.equal(planPresetProtocolMigration(row, preset()), null);
});

test("blank-endpoint presets derive sibling routes from the saved endpoint", () => {
  const azure = PROVIDER_PRESETS.find((entry) => entry.id === "azure-openai");
  assert.ok(azure);
  const row = destination({
    base_url: "https://lab.openai.azure.com/openai/v1/responses",
    protocols: ["responses"],
  });
  const plan = planPresetProtocolMigration(row, azure);
  assert.ok(plan);
  assert.deepEqual(plan.added.map((route) => route.protocol), ["chat_completions"]);
  assert.equal(
    plan.routes[1]?.endpoint_url,
    "https://lab.openai.azure.com/openai/v1/chat/completions",
  );
});

test("a blank-endpoint preset with an unrecognized endpoint plans nothing", () => {
  const azure = PROVIDER_PRESETS.find((entry) => entry.id === "azure-openai");
  assert.ok(azure);
  const row = destination({ base_url: "https://unrelated.example/v1" });
  assert.equal(planPresetProtocolMigration(row, azure), null);
});

test("a preset without declared routes plans nothing", () => {
  assert.equal(planPresetProtocolMigration(destination(), preset({ protocolRoutes: undefined })), null);
});
