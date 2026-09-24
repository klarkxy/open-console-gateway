import assert from "node:assert/strict";
import test from "node:test";
import type { Connection } from "../api/connections.ts";
import type { Destination } from "../api/destinations.ts";
import {
  OLLAMA_PROVIDER_ID,
  connectionForDestination,
  destinationForConnection,
  filterDestinations,
  isProvidersRailDestination,
  providersDestinationProjectionState,
  providersPageLoadOutcome,
  providersQueryAction,
  providersRailItemName,
  providersRailItems,
  providersSelectionProjectionReady,
  railKeyForDestination,
  railKeyForDraftConnection,
  resolveProvidersSelection,
} from "./destination-providers.ts";
import { findPlanDefinition, OPENCODE_GO_PLAN } from "./plans.ts";
import { sortProvidersByName } from "./provider-sort.ts";
import { readProviderPageQuery } from "../views/app-navigation.ts";

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

function fulfilled(): PromiseFulfilledResult<unknown> {
  return { status: "fulfilled", value: undefined };
}

function rejected(message: string): PromiseRejectedResult {
  return { status: "rejected", reason: new Error(message) };
}

test("destinationForConnection is the inverse join and skips platform parents", () => {
  const dests = [
    destination("dest-go", { legacy: { kind: "builtin", id: "opencode" } }),
    destination("dest-lab", { legacy: { kind: "dynamic", id: "lab-http" } }),
    destination("dest-site", { legacy: { kind: "platform_parent", id: "site-1" } }),
  ];
  const go = connection({ id: "c-go", legacy: { kind: "builtin_provider", id: "opencode" } });
  const lab = connection({ id: "c-lab", legacy: { kind: "dynamic_provider", id: "lab-http" } });
  assert.equal(destinationForConnection(dests, go)?.id, "dest-go");
  assert.equal(destinationForConnection(dests, lab)?.id, "dest-lab");
  assert.equal(
    destinationForConnection(dests, connection({ id: "c-site", legacy: { kind: "custom_account", id: "site-1" } })),
    undefined,
  );
});

test("explicit provider= wins over a cached destination selection", () => {
  const dests = [
    destination("dest-a", { legacy: { kind: "builtin", id: "opencode" } }),
    destination("dest-b", { legacy: { kind: "dynamic", id: "lab-http" } }),
  ];
  const rows = [
    connection({ id: "c-a", legacy: { kind: "builtin_provider", id: "opencode" } }),
    connection({ id: "c-b", legacy: { kind: "dynamic_provider", id: "lab-http" } }),
  ];
  const resolved = resolveProvidersSelection({
    query: { connection: null, provider: "lab-http", destination: null },
    cached: { destinationId: "dest-a", connectionId: "c-a" },
    destinations: dests,
    connections: rows,
  });
  assert.deepEqual(resolved, {
    destinationId: "dest-b",
    connectionId: "c-b",
    fellBack: false,
  });
});

test("legacy provider and dynamic scope links resolve through the page query", () => {
  const dests = [
    destination("dest-a", { legacy: { kind: "builtin", id: "opencode" } }),
    destination("dest-b", { legacy: { kind: "dynamic", id: "acme" } }),
  ];
  const rows = [
    connection({ id: "c-a", legacy: { kind: "builtin_provider", id: "opencode" } }),
    connection({ id: "c-b", legacy: { kind: "dynamic_provider", id: "acme" } }),
  ];
  const cached = { destinationId: "dest-a", connectionId: "c-a" };
  for (const search of [
    "?view=providers&provider=acme",
    "?view=providers&scope_kind=provider&scope_id=acme",
    "?view=providers&scope_kind=dynamic&scope_id=acme",
  ]) {
    const query = readProviderPageQuery(search);
    const resolved = resolveProvidersSelection({
      query: {
        connection: query.connection,
        provider: query.provider,
        destination: query.destination,
      },
      cached,
      destinations: dests,
      connections: rows,
    });
    assert.equal(resolved.destinationId, "dest-b", search);
    assert.equal(resolved.connectionId, "c-b", search);
    assert.equal(resolved.fellBack, false, search);
  }
});

test("connection and destination query targets still resolve, then cache, then default", () => {
  const dests = [
    destination("dest-a", { legacy: { kind: "builtin", id: "opencode" } }),
    destination("dest-b", { legacy: { kind: "dynamic", id: "lab-http" } }),
  ];
  const rows = [
    connection({ id: "c-a", legacy: { kind: "builtin_provider", id: "opencode" } }),
    connection({ id: "c-b", legacy: { kind: "dynamic_provider", id: "lab-http" } }),
  ];
  assert.deepEqual(
    resolveProvidersSelection({
      query: { connection: "c-b", provider: "opencode", destination: "dest-a" },
      cached: { destinationId: "dest-a", connectionId: "c-a" },
      destinations: dests,
      connections: rows,
    }),
    { destinationId: "dest-b", connectionId: "c-b", fellBack: false },
  );
  assert.deepEqual(
    resolveProvidersSelection({
      query: { connection: null, provider: null, destination: "dest-b" },
      cached: { destinationId: "dest-a", connectionId: "c-a" },
      destinations: dests,
      connections: rows,
    }),
    { destinationId: "dest-b", connectionId: "c-b", fellBack: false },
  );
  assert.deepEqual(
    resolveProvidersSelection({
      query: { connection: null, provider: null, destination: null },
      cached: { destinationId: "dest-b", connectionId: "c-b" },
      destinations: dests,
      connections: rows,
    }),
    { destinationId: "dest-b", connectionId: "c-b", fellBack: false },
  );
  assert.deepEqual(
    resolveProvidersSelection({
      query: { connection: null, provider: null, destination: null },
      prefer: { connectionId: "c-b" },
      cached: { destinationId: "dest-a", connectionId: "c-a" },
      destinations: dests,
      connections: rows,
    }),
    { destinationId: "dest-b", connectionId: "c-b", fellBack: false },
  );
  assert.deepEqual(
    resolveProvidersSelection({
      query: { connection: null, provider: null, destination: null },
      cached: { destinationId: null, connectionId: null },
      destinations: dests,
      connections: rows,
    }),
    { destinationId: "dest-a", connectionId: "c-a", fellBack: false },
  );
});

test("an unmatched draft connection stays selectable without a destination id", () => {
  const draft = connection({
    id: "c-draft",
    lifecycle: "draft",
    legacy: { kind: "dynamic_provider", id: "draft-http" },
  });
  const dests = [destination("dest-a", { legacy: { kind: "builtin", id: "opencode" } })];
  const rows = [
    connection({ id: "c-a", legacy: { kind: "builtin_provider", id: "opencode" } }),
    draft,
  ];
  assert.deepEqual(
    resolveProvidersSelection({
      query: { connection: "c-draft", provider: null, destination: null },
      cached: { destinationId: "dest-a", connectionId: "c-a" },
      destinations: dests,
      connections: rows,
    }),
    { destinationId: null, connectionId: "c-draft", fellBack: false },
  );
  const emptyDefault = resolveProvidersSelection({
    query: { connection: null, provider: null, destination: null },
    cached: { destinationId: null, connectionId: null },
    destinations: [],
    connections: [draft],
  });
  assert.deepEqual(emptyDefault, {
    destinationId: null,
    connectionId: "c-draft",
    fellBack: false,
  });
});

test("missing explicit targets fall back and do not default to a configured connection when destinations are empty", () => {
  const rows = [
    connection({ id: "c-a", lifecycle: "configured", legacy: { kind: "builtin_provider", id: "opencode" } }),
  ];
  const missing = resolveProvidersSelection({
    query: { connection: null, provider: "missing", destination: null },
    cached: { destinationId: null, connectionId: null },
    destinations: [],
    connections: rows,
  });
  assert.deepEqual(missing, {
    destinationId: null,
    connectionId: null,
    fellBack: true,
  });
});

test("rail projection states stay distinct and unmatched drafts stay on the rail", () => {
  assert.equal(providersDestinationProjectionState({
    loaded: false, loadFailed: false, railCount: 0,
  }), "not_loaded");
  assert.equal(providersDestinationProjectionState({
    loaded: false, loadFailed: true, railCount: 0,
  }), "failure");
  assert.equal(providersDestinationProjectionState({
    loaded: true, loadFailed: true, railCount: 2,
  }), "ready");
  assert.equal(providersDestinationProjectionState({
    loaded: true, loadFailed: false, railCount: 0,
  }), "empty");
  const dest = destination("dest-a", { legacy: { kind: "builtin", id: "opencode" } });
  const joined = connection({ id: "c-a", legacy: { kind: "builtin_provider", id: "opencode" } });
  const draft = connection({
    id: "c-draft",
    lifecycle: "draft",
    legacy: { kind: "dynamic_provider", id: "draft-http" },
  });
  const configuredOrphan = connection({
    id: "c-orphan",
    lifecycle: "configured",
    legacy: { kind: "dynamic_provider", id: "orphan" },
  });
  const items = providersRailItems(
    [dest, destination("dest-site", { legacy: { kind: "platform_parent", id: "site-1" } })],
    [joined, draft, configuredOrphan],
  );
  assert.deepEqual(items.map((item) => (
    item.kind === "destination" ? item.destination.id : item.connection.id
  )), ["dest-a", "c-draft"]);
  assert.equal(railKeyForDraftConnection(draft), "c-draft");
  assert.equal(railKeyForDestination(dest), "dest-a");
});

test("partial Destination-only success does not apply selection; a full success does", () => {
  const destFail = providersPageLoadOutcome({
    destinations: rejected("dest down"),
    catalog: fulfilled(),
    connections: fulfilled(),
    contracts: fulfilled(),
    accounts: rejected("accounts down"),
  });
  assert.equal(destFail.ok, false);
  assert.equal(destFail.applySelection, false);
  assert.equal(destFail.failedResource, "destinations");
  const catalogFail = providersPageLoadOutcome({
    destinations: fulfilled(),
    catalog: rejected("catalog down"),
    connections: fulfilled(),
    contracts: fulfilled(),
    accounts: fulfilled(),
  });
  assert.equal(catalogFail.ok, false);
  assert.equal(catalogFail.applySelection, false);
  assert.equal(catalogFail.failedResource, "catalog");
  const connectionsFail = providersPageLoadOutcome({
    destinations: fulfilled(),
    catalog: fulfilled(),
    connections: rejected("connections down"),
    contracts: fulfilled(),
    accounts: fulfilled(),
  });
  assert.equal(connectionsFail.ok, false);
  assert.equal(connectionsFail.applySelection, false);
  assert.equal(connectionsFail.failedResource, "connections");
  const contractsFail = providersPageLoadOutcome({
    destinations: fulfilled(),
    catalog: fulfilled(),
    connections: fulfilled(),
    contracts: rejected("contracts down"),
    accounts: fulfilled(),
  });
  assert.equal(contractsFail.ok, false);
  assert.equal(contractsFail.applySelection, false);
  assert.equal(contractsFail.failedResource, "contracts");
  const allOk = providersPageLoadOutcome({
    destinations: fulfilled(),
    catalog: fulfilled(),
    connections: fulfilled(),
    contracts: fulfilled(),
    accounts: rejected("ignored"),
  });
  assert.equal(allOk.ok, true);
  assert.equal(allOk.applySelection, true);
  assert.equal(allOk.failedResource, null);
});

test("add/preset redirects without waiting; selection waits for a ready projection", () => {
  const idle = { unresolvedExplicitTarget: false, freshLoadSucceeded: false };
  assert.equal(providersQueryAction({ add: true, projectionReady: false, ...idle }), "redirect-add");
  assert.equal(providersQueryAction({ add: true, projectionReady: true, ...idle }), "redirect-add");
  assert.equal(providersQueryAction({ add: false, projectionReady: false, ...idle }), "defer");
  assert.equal(providersQueryAction({ add: false, projectionReady: true, ...idle }), "apply-selection");
  assert.equal(providersSelectionProjectionReady({
    destinationsLoaded: true,
    connectionsLoaded: true,
    catalogLoaded: true,
    contractsLoaded: true,
  }), true);
  assert.equal(providersSelectionProjectionReady({
    destinationsLoaded: true,
    connectionsLoaded: false,
    catalogLoaded: true,
    contractsLoaded: true,
  }), false);
});

test("unresolved explicit provider defers on stale cache until a fresh load succeeds", () => {
  const destA = destination("dest-a", { legacy: { kind: "builtin", id: "opencode" } });
  const destB = destination("dest-b", { legacy: { kind: "dynamic", id: "lab-http" } });
  const connA = connection({ id: "c-a", legacy: { kind: "builtin_provider", id: "opencode" } });
  const connB = connection({ id: "c-b", legacy: { kind: "dynamic_provider", id: "lab-http" } });
  const query = { connection: null, provider: "lab-http", destination: null };
  const cached = { destinationId: "dest-a", connectionId: "c-a" };

  const stale = resolveProvidersSelection({
    query,
    cached,
    destinations: [destA],
    connections: [connA],
  });
  assert.equal(stale.fellBack, true);
  assert.equal(stale.destinationId, "dest-a");
  assert.equal(providersQueryAction({
    add: false,
    projectionReady: true,
    unresolvedExplicitTarget: stale.fellBack,
    freshLoadSucceeded: false,
  }), "defer");

  const stillMissing = resolveProvidersSelection({
    query,
    cached,
    destinations: [destA],
    connections: [connA],
  });
  assert.equal(stillMissing.fellBack, true);
  assert.equal(providersQueryAction({
    add: false,
    projectionReady: true,
    unresolvedExplicitTarget: stillMissing.fellBack,
    freshLoadSucceeded: true,
  }), "apply-selection");

  const fresh = resolveProvidersSelection({
    query,
    cached,
    destinations: [destA, destB],
    connections: [connA, connB],
  });
  assert.equal(fresh.fellBack, false);
  assert.equal(fresh.destinationId, "dest-b");
  assert.equal(providersQueryAction({
    add: false,
    projectionReady: true,
    unresolvedExplicitTarget: fresh.fellBack,
    freshLoadSucceeded: false,
  }), "apply-selection");

  assert.equal(providersQueryAction({
    add: true,
    projectionReady: false,
    unresolvedExplicitTarget: true,
    freshLoadSucceeded: false,
  }), "redirect-add");
});

test("a direct rail pick commits while an unresolved URL target is still pending", () => {
  const destA = destination("dest-a", { legacy: { kind: "builtin", id: "opencode" } });
  const connA = connection({ id: "c-a", legacy: { kind: "builtin_provider", id: "opencode" } });
  const pending = resolveProvidersSelection({
    query: { connection: null, provider: "lab-http", destination: null },
    cached: { destinationId: "dest-a", connectionId: "c-a" },
    destinations: [destA],
    connections: [connA],
  });
  assert.equal(pending.fellBack, true);
  const pendingInput = {
    add: false,
    projectionReady: true,
    unresolvedExplicitTarget: pending.fellBack,
    freshLoadSucceeded: false,
  };
  assert.equal(providersQueryAction(pendingInput), "defer");
  assert.equal(providersQueryAction({ ...pendingInput, userSelection: true }), "apply-selection");
  assert.equal(providersQueryAction({
    add: true,
    projectionReady: true,
    unresolvedExplicitTarget: true,
    freshLoadSucceeded: false,
    userSelection: true,
  }), "redirect-add");
  assert.equal(providersQueryAction({
    add: false,
    projectionReady: false,
    unresolvedExplicitTarget: true,
    freshLoadSucceeded: false,
    userSelection: true,
  }), "defer");
});

test("a locally deleted destination can select the next rail row immediately", () => {
  const remaining = destination("dest-b", { legacy: { kind: "builtin", id: "minimax" } });
  const resolved = resolveProvidersSelection({
    query: { connection: null, provider: null, destination: "deleted-dest" },
    cached: { destinationId: null, connectionId: null },
    destinations: [remaining],
    connections: [],
  });
  assert.deepEqual(resolved, { destinationId: "dest-b", connectionId: null, fellBack: true });
  assert.equal(providersQueryAction({
    add: false,
    projectionReady: true,
    unresolvedExplicitTarget: resolved.fellBack,
    freshLoadSucceeded: false,
    userSelection: true,
  }), "apply-selection");
});

test("mixed destination and draft rail names sort as one A-Z list", () => {
  const dest = destination("dest-zulu", {
    name: "Zulu HTTP",
    legacy: { kind: "builtin", id: "opencode" },
  });
  const draft = connection({
    id: "c-alpha",
    name: "Alpha Draft",
    lifecycle: "draft",
    legacy: { kind: "dynamic_provider", id: "draft-http" },
  });
  const items = providersRailItems([dest], [draft]);
  const ascending = sortProvidersByName(items, providersRailItemName, "name_asc");
  assert.deepEqual(ascending.map(providersRailItemName), ["Alpha Draft", "Zulu HTTP"]);
  const descending = sortProvidersByName(items, providersRailItemName, "name_desc");
  assert.deepEqual(descending.map(providersRailItemName), ["Zulu HTTP", "Alpha Draft"]);
});
