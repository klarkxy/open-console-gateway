import assert from "node:assert/strict";
import test from "node:test";
import type { Connection } from "../api/connections.ts";
import type { Destination } from "../api/destinations.ts";
import {
  OLLAMA_PROVIDER_ID,
  connectionForDestination,
  filterDestinations,
  isProvidersRailDestination,
  railKeyForDestination,
} from "./destination-providers.ts";
import { findPlanDefinition, OPENCODE_GO_PLAN } from "./plans.ts";

function destination(
  id: string,
  overrides: Partial<Destination> = {},
): Destination {
  return {
    account_controls: { toggleWrite: "account", configurationOwner: "destination", consoleLink: null, browserProfile: false },
    adapter: "http",
    legacy: { kind: "builtin", id },
    auth_scheme: "bearer",
    base_url: null,
    brand_family: null,
    capabilities: {
      billing_tier_required: false,
      discoverable_models: false,
      external_integration: false,
      identity_headers: false,
      managed_signup: false,
      observer: false,
      official_balance_probe: [],
      redirect_policy: "no_follow",
      testable: true,
    },
    catalog: [],
    enabled: true,
    id,
    max_credentials: null,
    name: id,
    observer_credential_id: null,
    plan: null,
    protocols: ["chat_completions"],
    ...overrides,
  };
}

function connection(overrides: Partial<Connection> = {}): Connection {
  return {
    id: "conn-1",
    name: "Lab",
    origin: "custom",
    template_ref: null,
    adapter_kind: "configurable_http",
    lifecycle: "configured",
    authorization: "valid",
    eligibility: { state: "eligible", reason: "none" },
    credential_count: 1,
    enabled_credential_count: 1,
    target_count: 1,
    endpoints: [],
    targets: [],
    legacy: { kind: "builtin_provider", id: "opencode" },
    display_family: "OpenCode",
    offering: "plan",
    ...overrides,
  };
}

const monthly = {
  expiry_cadence: "monthly" as const,
  manual_calibration: false,
  pricing_source: "official" as const,
  usage_source: "official_api" as const,
  windows: [{ kind: "month" as const }],
};

test("connection join follows destination legacy kind, not destination id", () => {
  const rows = [
    connection({ id: "c-go", legacy: { kind: "builtin_provider", id: "opencode" } }),
    connection({ id: "c-lab", legacy: { kind: "dynamic_provider", id: "lab-http" } }),
    connection({ id: "c-acc", legacy: { kind: "custom_account", id: "acc-9" } }),
  ];
  assert.equal(
    connectionForDestination(rows, destination("dest-go", { legacy: { kind: "builtin", id: "opencode" } }))?.id,
    "c-go",
  );
  assert.equal(
    connectionForDestination(rows, destination("dest-lab", { legacy: { kind: "dynamic", id: "lab-http" } }))?.id,
    "c-lab",
  );
  assert.equal(
    connectionForDestination(rows, destination("dest-acc", { legacy: { kind: "custom_account", id: "acc-9" } }))?.id,
    "c-acc",
  );
  assert.equal(
    connectionForDestination(rows, destination("dest-site", { legacy: { kind: "platform_parent", id: "site-1" } })),
    undefined,
  );
});

test("destination filtering matches connection identity", () => {
  const rows = [
    destination("go", { name: "OpenCode Go", plan: monthly, brand_family: "OpenCode" }),
    destination("lab", { name: "lab.example", base_url: "https://lab.example/v1" }),
    destination("kimi", { name: "Kimi", plan: monthly }),
  ];
  assert.deepEqual(filterDestinations(rows, "LAB.EXAMPLE").map((row) => row.id), ["lab"]);
  assert.deepEqual(filterDestinations(rows, "opencode").map((row) => row.id), ["go"]);
});

test("rail key is the destination id even when a connection join exists", () => {
  const dest = destination("dest-go", { legacy: { kind: "builtin", id: "opencode" } });
  const platform = destination("dest-site", { legacy: { kind: "platform_parent", id: "site-1" } });
  assert.equal(railKeyForDestination(dest), "dest-go");
  assert.equal(railKeyForDestination(platform), "dest-site");
  assert.equal(isProvidersRailDestination(dest), true);
  assert.equal(isProvidersRailDestination(platform), false);
});

test("ollama surface exists only when the catalog supplies it", () => {
  assert.equal(findPlanDefinition(OLLAMA_PROVIDER_ID), undefined);
  const plan = findPlanDefinition(OLLAMA_PROVIDER_ID, [{
    ...OPENCODE_GO_PLAN,
    provider_id: OLLAMA_PROVIDER_ID,
    display_name: "Ollama Cloud",
  }]);
  assert.ok(plan);
  assert.equal(plan.provider_id, OLLAMA_PROVIDER_ID);
});
