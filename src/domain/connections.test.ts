import assert from "node:assert/strict";
import test from "node:test";
import type { Connection } from "../api/connections.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import {
  CONNECTION_STATUS_LABELS,
  catalogEntryForConnection,
  connectionBrandFamily,
  connectionForLegacyProvider,
  connectionStatus,
  isOnboardingDraftConnection,
  filterConnections,
  selectedConnectionIdFromQuery,
} from "./connections.ts";

function connection(overrides: Partial<Connection> = {}): Connection {
  return {
    id: "conn-lab",
    name: "Lab HTTP",
    origin: "custom",
    template_ref: { id: "custom-http", version: 1 },
    adapter_kind: "configurable_http",
    lifecycle: "configured",
    authorization: "missing",
    eligibility: { state: "ineligible", reason: "missing_credential" },
    credential_count: 0,
    enabled_credential_count: 0,
    target_count: 1,
    endpoints: [],
    targets: [],
    legacy: { kind: "dynamic_provider", id: "lab-http" },
    display_family: "Lab",
    offering: "api",
    ...overrides,
  };
}

function catalogEntry(provider_id: string, extra: Partial<ProviderCatalogEntry> = {}): ProviderCatalogEntry {
  return {
    provider_id,
    origin: "builtin",
    editable: false,
    deletable: false,
    offering: "api",
    display_name: provider_id,
    display_family: provider_id,
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    verification_policy: "required",
    verification_runtime_availability: "unavailable",
    routable: false,
    managed_registration: false,
    pricing_availability: "unavailable",
    usage_availability: "unavailable",
    manual_usage_calibration: false,
    quota_unit: "credits",
    model_source: "test",
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
    ...extra,
  };
}

test("connection filter matches name, legacy id, and display family case-insensitively", () => {
  const rows = [
    connection({ name: "OpenCode Go", display_family: "OpenCode", legacy: { kind: "builtin_provider", id: "opencode" } }),
    connection({ id: "kimi", name: "Kimi Code CN", display_family: "Kimi", legacy: { kind: "builtin_provider", id: "kimi" } }),
  ];
  assert.deepEqual(filterConnections(rows, "  ").map((row) => row.legacy.id), ["opencode", "kimi"]);
  assert.deepEqual(filterConnections(rows, "OPENCODE").map((row) => row.legacy.id), ["opencode"]);
  assert.deepEqual(filterConnections(rows, "code cn").map((row) => row.legacy.id), ["kimi"]);
  assert.deepEqual(filterConnections(rows, "kimi").map((row) => row.legacy.id), ["kimi"]);
  assert.deepEqual(filterConnections(rows, "nope"), []);
});

test("connectionStatus uses only authorization, lifecycle, and eligibility", () => {
  assert.equal(connectionStatus(connection({
    lifecycle: "draft",
    authorization: "missing",
    eligibility: { state: "ineligible", reason: "connection_disabled" },
  })).kind, "draft");
  assert.equal(connectionStatus(connection({
    lifecycle: "draft",
    authorization: "valid",
    eligibility: { state: "ineligible", reason: "missing_credential" },
  })).label, CONNECTION_STATUS_LABELS.draft);
  assert.equal(isOnboardingDraftConnection(connection({ lifecycle: "draft" })), true);
  assert.equal(isOnboardingDraftConnection(connection({ lifecycle: "configured" })), false);
  assert.equal(connectionStatus(connection({
    lifecycle: "disabled",
    authorization: "missing",
    eligibility: { state: "ineligible", reason: "connection_disabled" },
  })).kind, "disabled");
  assert.equal(connectionStatus(connection({
    lifecycle: "configured",
    authorization: "valid",
    eligibility: { state: "ineligible", reason: "all_credentials_disabled" },
  })).kind, "disabled");
  assert.equal(connectionStatus(connection({
    lifecycle: "configured",
    authorization: "valid",
    eligibility: { state: "cooling", reason: "cooling" },
  })).kind, "cooling");
  assert.equal(connectionStatus(connection({
    lifecycle: "configured",
    authorization: "invalid",
    eligibility: { state: "ineligible", reason: "all_credentials_invalid" },
  })).kind, "invalid");
  assert.equal(connectionStatus(connection({
    lifecycle: "configured",
    authorization: "missing",
    eligibility: { state: "ineligible", reason: "missing_credential" },
  })).kind, "missing_credential");
  assert.equal(connectionStatus(connection({
    lifecycle: "configured",
    authorization: "valid",
    eligibility: { state: "ineligible", reason: "no_enabled_target" },
  })).kind, "no_target");
  const unknown = connectionStatus(connection({
    lifecycle: "configured",
    authorization: "unknown",
    eligibility: { state: "eligible", reason: "none" },
  }));
  assert.equal(unknown.kind, "ok");
  assert.equal(unknown.label, null);
  assert.equal(connectionStatus(connection({
    lifecycle: "configured",
    authorization: "not_required",
    eligibility: { state: "eligible", reason: "none" },
  })).kind, "ok");
  assert.equal(connectionStatus(connection({
    lifecycle: "disabled",
    authorization: "missing",
    eligibility: { state: "ineligible", reason: "missing_credential" },
  })).kind, "disabled");
});

test("legacy provider id maps onto builtin or dynamic connections only", () => {
  const rows = [
    connection({ id: "c-open", legacy: { kind: "builtin_provider", id: "opencode" } }),
    connection({ id: "c-lab", legacy: { kind: "dynamic_provider", id: "lab-http" } }),
    connection({ id: "c-acc", name: "Custom acc", legacy: { kind: "custom_account", id: "acc-9" } }),
  ];
  assert.equal(connectionForLegacyProvider(rows, "OPENCODE")?.id, "c-open");
  assert.equal(connectionForLegacyProvider(rows, "lab-http")?.id, "c-lab");
  assert.equal(connectionForLegacyProvider(rows, "acc-9"), undefined);
  assert.equal(connectionForLegacyProvider(rows, "missing"), undefined);
});

test("catalogEntryForConnection matches builtin/dynamic legacy ids and skips custom accounts", () => {
  const catalog = [
    catalogEntry("opencode", { display_name: "OpenCode Go" }),
    catalogEntry("lab-http", { origin: "custom", display_name: "Lab HTTP" }),
  ];
  assert.equal(
    catalogEntryForConnection(connection({ legacy: { kind: "builtin_provider", id: "opencode" } }), catalog)?.provider_id,
    "opencode",
  );
  assert.equal(
    catalogEntryForConnection(connection({ legacy: { kind: "dynamic_provider", id: "LAB-HTTP" } }), catalog)?.provider_id,
    "lab-http",
  );
  assert.equal(
    catalogEntryForConnection(connection({ legacy: { kind: "custom_account", id: "acc-9" } }), catalog),
    null,
  );
});

test("custom_account brand is a neutral Custom family; builtin/dynamic use the catalog brand", () => {
  const catalog = [catalogEntry("minimax", { display_family: "MiniMax", display_name: "MiniMax CN" })];
  const custom = connectionBrandFamily(
    connection({
      name: "My Custom",
      display_family: "Custom",
      legacy: { kind: "custom_account", id: "acc-9" },
    }),
    catalog,
  );
  assert.equal(custom.label, "Custom");
  assert.equal(custom.id, "custom");
  const minimax = connectionBrandFamily(
    connection({ legacy: { kind: "builtin_provider", id: "minimax" } }),
    catalog,
  );
  assert.equal(minimax.id, "minimax");
});

test("URL selection prefers connection= and maps a legacy provider= bookmark", () => {
  const rows = [
    connection({ id: "uuid-open", legacy: { kind: "builtin_provider", id: "opencode" } }),
    connection({ id: "uuid-lab", legacy: { kind: "dynamic_provider", id: "lab-http" } }),
  ];
  assert.equal(
    selectedConnectionIdFromQuery({ connection: "uuid-lab", provider: "opencode" }, rows),
    "uuid-lab",
  );
  assert.equal(
    selectedConnectionIdFromQuery({ connection: null, provider: "OPENCODE" }, rows),
    "uuid-open",
  );
  assert.equal(
    selectedConnectionIdFromQuery({ connection: "missing", provider: "lab-http" }, rows),
    "uuid-lab",
  );
  assert.equal(
    selectedConnectionIdFromQuery({ connection: null, provider: null }, rows),
    null,
  );
});
