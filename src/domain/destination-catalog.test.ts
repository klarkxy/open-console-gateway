import assert from "node:assert/strict";
import test from "node:test";
import type { Destination } from "../api/destinations.ts";
import {
  catalogUpdatesFromOverrides,
  destinationConfiguredProtocols,
  destinationConfiguredRoutes,
  destinationProbeIdentity,
  projectDestinationCatalog,
} from "./destination-catalog.ts";
import {
  buildPreferredProtocolOverrides,
  modelAvailableProtocols,
  modelEffectiveOn,
  providerScopeKey,
} from "./provider-contracts.ts";

function destination(overrides: Partial<Destination> = {}): Destination {
  return {
    account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
    adapter: "http",
    auth_scheme: "bearer",
    base_url: "https://api.lab.example/v1",
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
    legacy: { kind: "dynamic", id: "prov-lab" },
    max_credentials: null,
    name: "Lab HTTP",
    observer_credential_id: null,
    plan: null,
    protocols: ["chat_completions"],
    ...overrides,
  };
}

test("HTTP projection uses destination id as a unique custom_endpoint scope", () => {
  const a = projectDestinationCatalog(destination());
  const b = projectDestinationCatalog(destination({ id: "dest-2", name: "Lab HTTP" }));
  assert.equal(a.scope_kind, "custom_endpoint");
  assert.equal(a.scope_id, "dest-1");
  assert.equal(a.key, providerScopeKey("custom_endpoint", "dest-1"));
  assert.notEqual(a.key, b.key);
  assert.equal(a.provider_id, "prov-lab");
  assert.equal(
    projectDestinationCatalog(destination({ legacy: { kind: "custom_account", id: "acct-1" } })).provider_id,
    "custom",
  );
});

test("model identity is the public name and upstream stays visible when it differs", () => {
  const scope = projectDestinationCatalog(destination());
  assert.deepEqual(scope.catalog.models, ["lab-opus"]);
  assert.equal(scope.models[0]?.model_id, "lab-opus");
  assert.equal(scope.models[0]?.alias, "lab-opus");
  assert.equal(scope.models[0]?.secondary, "vendor/opus");
  const same = projectDestinationCatalog(destination({
    catalog: [{
      enabled: true,
      preferred: "chat_completions",
      protocols: ["chat_completions"],
      public_model: "same",
      upstream_model: "same",
      upstream_override: null,
    }],
  }));
  assert.equal(same.models[0]?.secondary, "same");
});

test("available protocols follow explicit routes, then destination.protocols", () => {
  const legacy = destinationConfiguredProtocols(destination());
  assert.deepEqual(legacy, ["chat_completions"]);
  const multi = destination({
    protocols: ["chat_completions"],
    protocol_routes: [
      { protocol: "chat_completions", endpoint_url: "https://api.lab.example/v1", auth_scheme: "bearer" },
      { protocol: "responses", endpoint_url: "https://api.lab.example/v1/responses", auth_scheme: "bearer" },
    ],
  });
  assert.deepEqual(destinationConfiguredProtocols(multi), ["chat_completions", "responses"]);
  const scope = projectDestinationCatalog(multi);
  assert.deepEqual(modelAvailableProtocols(scope.models[0]!), ["chat_completions", "responses"]);
});

test("a model upstream override stays a single available protocol", () => {
  const scope = projectDestinationCatalog(destination({
    protocol_routes: [
      { protocol: "chat_completions", endpoint_url: "https://api.lab.example/v1", auth_scheme: "bearer" },
      { protocol: "messages", endpoint_url: "https://api.lab.example/anthropic", auth_scheme: "x_api_key" },
    ],
    catalog: [{
      enabled: true,
      preferred: "messages",
      protocols: ["messages"],
      public_model: "lab-opus",
      upstream_model: "vendor/opus",
      upstream_override: { protocol: "messages", endpoint_url: "https://fast.lab.example/v1" },
    }],
  }));
  assert.deepEqual(modelAvailableProtocols(scope.models[0]!), ["messages"]);
  assert.equal(scope.models[0]?.protocols.chat_completions?.available, false);
});

test("enabled protocols require the whole model on and membership in model.protocols", () => {
  const listedButOff = projectDestinationCatalog(destination({
    catalog: [{
      enabled: false,
      preferred: "chat_completions",
      protocols: ["chat_completions"],
      public_model: "lab-opus",
      upstream_model: "vendor/opus",
      upstream_override: null,
    }],
  }));
  assert.equal(modelEffectiveOn(listedButOff.models[0]!, listedButOff), false);
  const onWithoutProtocol = projectDestinationCatalog(destination({
    catalog: [{
      enabled: true,
      preferred: "chat_completions",
      protocols: [],
      public_model: "lab-opus",
      upstream_model: "vendor/opus",
      upstream_override: null,
    }],
  }));
  assert.equal(onWithoutProtocol.models[0]?.protocols.chat_completions?.enabled, false);
});

test("configuration evidence is not a probe claim and carries no snapshot date", () => {
  const preset = projectDestinationCatalog(destination(), {
    source: "preset",
    source_url: "https://docs.lab.example/api",
  });
  assert.equal(preset.catalog.source, "preset");
  assert.equal(preset.catalog.refreshed_at, null);
  assert.equal(preset.static_protocol_snapshot_date, null);
  assert.equal(preset.models[0]?.protocols.chat_completions?.source, "preset");
  assert.equal(preset.models[0]?.protocols.chat_completions?.verified_at, null);
  assert.equal(preset.models[0]?.protocols.chat_completions?.last_probe_at, null);
  const staticScope = projectDestinationCatalog(destination());
  assert.equal(staticScope.catalog.source, "static");
  assert.equal(staticScope.models[0]?.protocols.chat_completions?.source, "static");
});

test("legacy empty protocol_routes keep one route per destination protocol", () => {
  const routes = destinationConfiguredRoutes(destination());
  assert.deepEqual(routes, [{
    protocol: "chat_completions",
    endpoint_url: "https://api.lab.example/v1",
    auth_scheme: "bearer",
  }]);
});

test("matrix overrides convert to catalog updates without inventing models", () => {
  const row = destination({
    protocol_routes: [
      { protocol: "chat_completions", endpoint_url: "https://api.lab.example/v1", auth_scheme: "bearer" },
      { protocol: "responses", endpoint_url: "https://api.lab.example/responses", auth_scheme: "bearer" },
    ],
    catalog: [{
      enabled: true,
      preferred: "chat_completions",
      protocols: ["chat_completions", "responses"],
      public_model: "lab-opus",
      upstream_model: "vendor/opus",
      upstream_override: null,
    }],
  });
  const scope = projectDestinationCatalog(row);
  const off = catalogUpdatesFromOverrides(row, [
    { model_id: "lab-opus", protocol: "chat_completions", state: "force_off", preferred: true },
    { model_id: "lab-opus", protocol: "responses", state: "force_off" },
    { model_id: "missing", protocol: "chat_completions", state: "force_on" },
  ]);
  assert.deepEqual(off.updates, [{
    publicModel: "lab-opus",
    enabled: false,
    protocols: [],
    preferred: "chat_completions",
  }]);
  const prefer = catalogUpdatesFromOverrides(
    row,
    buildPreferredProtocolOverrides(scope, "lab-opus", "responses"),
  );
  assert.equal(prefer.updates[0]?.preferred, "responses");
  assert.equal(prefer.updates[0]?.enabled, true);
  assert.ok(prefer.updates[0]?.protocols?.includes("responses"));
});

test("probe identity changes with protocol preference, route, or Key observation", () => {
  const row = destination();
  const key = {
    auth_state: "valid" as const,
    destination_id: "dest-1",
    has_secret: true,
    id: "cred-1",
    last_error: null as string | null,
  };
  const chat = destinationProbeIdentity(row, [key], "chat_completions");
  assert.notEqual(destinationProbeIdentity(row, [key], "responses"), chat);
  assert.notEqual(
    destinationProbeIdentity(
      destination({ base_url: "https://api.other.example/v1" }),
      [key],
      "chat_completions",
    ),
    chat,
  );
  assert.notEqual(
    destinationProbeIdentity(row, [{ ...key, auth_state: "unknown" }], "chat_completions"),
    chat,
  );
});


test("probe identity includes model mapping and scoped grants", () => {
  const row = destination();
  const key = { auth_state: "valid" as const, destination_id: row.id, has_secret: true, id: "key", last_error: null,
    grants: { allowed_endpoint_ids: ["chat"], allowed_origins: ["https://api.lab.example"] } };
  const before = destinationProbeIdentity(row, [key], "chat_completions", "lab-opus");
  assert.notEqual(destinationProbeIdentity({ ...row, catalog: [{ ...row.catalog[0]!, upstream_model: "vendor/new" }] }, [key], "chat_completions", "lab-opus"), before);
  assert.notEqual(destinationProbeIdentity(row, [{ ...key, grants: { ...key.grants, allowed_endpoint_ids: [] } }], "chat_completions", "lab-opus"), before);
});
