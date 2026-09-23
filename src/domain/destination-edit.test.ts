import assert from "node:assert/strict";
import test from "node:test";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import {
  DESTINATION_EDIT_ISSUE_KEYS,
  DestinationEditError,
  addDraftProtocolRoute,
  applyPresetProtocolRoutesToDraft,
  buildDestinationPatch,
  destinationEditDraft,
  destinationGrantCandidates,
  destinationPatchOrigins,
  destinationRouteChanged,
  isDestinationDeletable,
  isDestinationEditable,
  isDestinationCatalogRefreshable,
  withAuthorizedCredentials,
  type DestinationEditDraft,
  type DestinationEditIssue,
} from "./destination-edit.ts";

test("HTTP catalog refresh follows capabilities for presets and custom destinations", () => {
  for (const kind of ["dynamic", "custom_account"] as const) {
    const row = destination({ legacy: { kind, id: "arbitrary-provider" } });
    assert.equal(isDestinationCatalogRefreshable(row), true);
    assert.equal(isDestinationCatalogRefreshable({ ...row, adapter: "opencode_go" }), false);
    assert.equal(isDestinationCatalogRefreshable({ ...row, capabilities: { ...row.capabilities, observer: true } }), false);
    assert.equal(isDestinationCatalogRefreshable({ ...row, capabilities: { ...row.capabilities, discoverable_models: false } }), false);
  }
});

test("editing discovered models preserves default-off until explicitly enabled", () => {
  const row = destination();
  row.catalog[0]!.enabled = false;
  const draft = destinationEditDraft(row);
  assert.equal(buildDestinationPatch(row, draft).models[0]?.enabled, false);
  draft.models[0]!.enabled = true;
  assert.equal(buildDestinationPatch(row, draft).models[0]?.enabled, true);
});

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
    legacy: { kind: "custom_account", id: "acct-1" },
    max_credentials: null,
    name: "Lab HTTP",
    observer_credential_id: null,
    plan: null,
    protocols: ["chat_completions"],
    ...overrides,
  };
}

function credential(overrides: Partial<DestinationCredential> = {}): DestinationCredential {
  return {
    auth_state: "unknown",
    cooldowns: {
      five_hour_until: null,
      free_until: null,
      generic_until: null,
      month_until: null,
      week_until: null,
    },
    destination_id: "dest-1",
    enabled: true,
    grants: {
      allowed_endpoint_ids: [],
      allowed_origins: ["https://api.lab.example"],
    },
    has_secret: true,
    id: "cred-1",
    last_error: null,
    legacy_account_id: "acct-1",
    name: "Lab Key",
    notes: null,
    onboarding_task: null,
    purchase_date: null,
    quota_pool_id: null,
    routing_rank: 1,
    scope: { kind: "all" },
    ...overrides,
  };
}

function draft(overrides: Partial<DestinationEditDraft> & {
  endpoint_url?: string;
  auth_scheme?: DestinationEditDraft["protocol_routes"][number]["auth_scheme"];
  upstream_protocol?: DestinationEditDraft["protocol_routes"][number]["protocol"];
} = {}): DestinationEditDraft {
  const { endpoint_url, auth_scheme, upstream_protocol, protocol_routes, ...rest } = overrides;
  const routes = protocol_routes ?? [{
    protocol: "chat_completions" as const,
    endpoint_url: "https://api.lab.example/v1",
    auth_scheme: "bearer" as const,
  }];
  const first = { ...routes[0]! };
  if (endpoint_url !== undefined) first.endpoint_url = endpoint_url;
  if (auth_scheme !== undefined) first.auth_scheme = auth_scheme;
  if (upstream_protocol !== undefined) first.protocol = upstream_protocol;
  return {
    name: "Lab HTTP",
    protocol_routes: [first, ...routes.slice(1)],
    models: [{ public_model: "lab-opus", upstream_model: "vendor/opus", upstream_override: null }],
    ...rest,
  };
}

function issueOf(run: () => unknown): DestinationEditIssue {
  try {
    run();
  } catch (error) {
    assert.ok(error instanceof DestinationEditError);
    return error.issue;
  }
  throw new Error("expected DestinationEditError");
}

test("every issue code has a copy mapping", () => {
  const codes: DestinationEditIssue[] = [
    "immutable_destination",
    "missing_name",
    "missing_endpoint_url",
    "invalid_endpoint_url",
    "endpoint_url_not_http",
    "endpoint_url_with_credentials",
    "missing_protocol",
    "missing_mappings",
    "duplicate_public_model",
    "missing_public_model",
    "missing_upstream_model",
    "missing_override_endpoint",
    "invalid_override_endpoint",
    "override_endpoint_not_http",
    "override_endpoint_with_credentials",
    "missing_route_endpoint",
    "invalid_route_endpoint",
    "route_endpoint_not_http",
    "route_endpoint_with_credentials",
    "duplicate_protocol_route",
    "too_many_protocol_routes",
  ];
  for (const code of codes) assert.ok(DESTINATION_EDIT_ISSUE_KEYS[code]);
});

test("only user-defined http destinations are editable", () => {
  assert.equal(isDestinationEditable(destination()), true);
  assert.equal(isDestinationEditable(destination({ legacy: { kind: "dynamic", id: "dyn-1" } })), true);
  assert.equal(isDestinationEditable(destination({ adapter: "zen" })), false);
  assert.equal(isDestinationEditable(destination({ capabilities: { ...destination().capabilities, observer: true } })), false);
  assert.equal(isDestinationEditable(destination({ adapter: "zen" })), false);
});

test("delete requires an editable destination with no referencing Keys", () => {
  assert.equal(isDestinationDeletable(destination(), []), true);
  assert.equal(isDestinationDeletable(destination(), [credential()]), false);
  assert.equal(
    isDestinationDeletable(destination(), [credential({ destination_id: "dest-2", id: "cred-2" })]),
    true,
  );
  assert.equal(isDestinationDeletable(destination({ adapter: "zen" }), []), false);
});

test("draft round-trips the persisted destination including per-model overrides", () => {
  const value = destination({
    catalog: [{
      enabled: true,
      preferred: "responses",
      protocols: ["responses"],
      public_model: "lab-fast",
      upstream_model: "vendor/fast",
      upstream_override: { protocol: "responses", endpoint_url: "https://fast.lab.example/v1" },
    }],
  });
  const result = destinationEditDraft(value);
  assert.equal(result.name, "Lab HTTP");
  assert.deepEqual(result.protocol_routes, [{
    protocol: "chat_completions",
    endpoint_url: "https://api.lab.example/v1",
    auth_scheme: "bearer",
  }]);
  assert.deepEqual(result.models, [{
    enabled: true,
    public_model: "lab-fast",
    upstream_model: "vendor/fast",
    protocols: ["responses"],
    preferred: "responses",
    upstream_override: { protocol: "responses", endpoint_url: "https://fast.lab.example/v1" },
  }]);
});

test("buildDestinationPatch validates before producing the full replacement", () => {
  assert.equal(issueOf(() => buildDestinationPatch(destination(), draft({ name: "  " }))), "missing_name");
  assert.equal(issueOf(() => buildDestinationPatch(destination(), draft({ endpoint_url: "" }))), "missing_endpoint_url");
  assert.equal(issueOf(() => buildDestinationPatch(destination(), draft({ endpoint_url: "not a url" }))), "invalid_endpoint_url");
  assert.equal(issueOf(() => buildDestinationPatch(destination(), draft({ endpoint_url: "ftp://x" }))), "endpoint_url_not_http");
  assert.equal(issueOf(() => buildDestinationPatch(destination(), draft({ endpoint_url: "https://u:p@x" }))), "endpoint_url_with_credentials");
  assert.equal(issueOf(() => buildDestinationPatch(destination(), draft({ upstream_protocol: "" }))), "missing_protocol");
  assert.equal(issueOf(() => buildDestinationPatch(destination(), draft({ models: [] }))), "missing_mappings");
  assert.equal(
    issueOf(() => buildDestinationPatch(destination(), draft({
      models: [
        { public_model: "A", upstream_model: "a", upstream_override: null },
        { public_model: "a", upstream_model: "b", upstream_override: null },
      ],
    }))),
    "duplicate_public_model",
  );
  assert.equal(
    issueOf(() => buildDestinationPatch(destination(), draft({
      models: [{ public_model: "", upstream_model: "a", upstream_override: null }],
    }))),
    "missing_public_model",
  );
  assert.equal(
    issueOf(() => buildDestinationPatch(destination(), draft({
      models: [{ public_model: "a", upstream_model: "", upstream_override: null }],
    }))),
    "missing_upstream_model",
  );
  assert.equal(
    issueOf(() => buildDestinationPatch(destination(), draft({
      models: [{
        public_model: "a",
        upstream_model: "b",
        upstream_override: { protocol: "messages", endpoint_url: "" },
      }],
    }))),
    "missing_override_endpoint",
  );
  assert.equal(
    issueOf(() => buildDestinationPatch(destination({
      adapter: "zen",
    }), draft())),
    "immutable_destination",
  );
});

test("buildDestinationPatch emits wire-shaped models with overrides", () => {
  const input = buildDestinationPatch(destination(), draft({
    endpoint_url: " https://api.lab.example/v1 ",
    models: [
      { public_model: " a ", upstream_model: " b ", upstream_override: null },
      {
        public_model: "c",
        upstream_model: "d",
        upstream_override: { protocol: "responses", endpoint_url: " https://fast.lab.example/v1 " },
      },
    ],
  }));
  assert.equal(input.endpointUrl, "https://api.lab.example/v1");
  assert.equal(input.authScheme, "bearer");
  assert.equal(input.upstreamProtocol, "chat_completions");
  assert.deepEqual(input.models, [
    { publicModel: "a", upstreamModel: "b", upstreamOverride: null },
    {
      publicModel: "c",
      upstreamModel: "d",
      upstreamOverride: { protocol: "responses", endpointUrl: "https://fast.lab.example/v1" },
    },
  ]);
  assert.equal("authorizeCredentialIds" in input, false);
});

test("legacy single-route saves omit protocolRoutes; explicit routes round-trip", () => {
  const legacy = buildDestinationPatch(destination(), draft());
  assert.equal("protocolRoutes" in legacy, false);
  const explicit = destination({
    protocol_routes: [
      { protocol: "chat_completions", endpoint_url: "https://api.lab.example/v1", auth_scheme: "bearer" },
      { protocol: "messages", endpoint_url: "https://api.lab.example/anthropic", auth_scheme: "x_api_key" },
    ],
  });
  const result = buildDestinationPatch(explicit, destinationEditDraft(explicit));
  assert.deepEqual(result.protocolRoutes, [
    { protocol: "chat_completions", endpointUrl: "https://api.lab.example/v1", authScheme: "bearer" },
    { protocol: "messages", endpointUrl: "https://api.lab.example/anthropic", authScheme: "x_api_key" },
  ]);
  assert.equal(result.models[0]?.enabled, true);
  assert.deepEqual(result.models[0]?.protocols, ["chat_completions"]);
  assert.equal(result.models[0]?.preferred, "chat_completions");
  assert.equal(result.models[0]?.upstreamOverride, null);
});

test("adding a second protocol route is a grant-affecting route change", () => {
  const value = destination();
  const next = draft();
  assert.equal(addDraftProtocolRoute(next), true);
  next.protocol_routes[1]!.endpoint_url = "https://api.lab.example/responses";
  next.protocol_routes[1]!.protocol = "responses";
  const input = buildDestinationPatch(value, next);
  assert.equal(destinationRouteChanged(value, input), true);
  assert.equal(input.protocolRoutes?.length, 2);
});

test("duplicate protocols and more than three routes are rejected", () => {
  assert.equal(
    issueOf(() => buildDestinationPatch(destination(), draft({
      protocol_routes: [
        { protocol: "chat_completions", endpoint_url: "https://api.lab.example/v1", auth_scheme: "bearer" },
        { protocol: "chat_completions", endpoint_url: "https://other.example/v1", auth_scheme: "bearer" },
      ],
    }))),
    "duplicate_protocol_route",
  );
  assert.equal(
    issueOf(() => buildDestinationPatch(destination(), draft({
      protocol_routes: [
        { protocol: "chat_completions", endpoint_url: "https://a.example/v1", auth_scheme: "bearer" },
        { protocol: "responses", endpoint_url: "https://b.example/v1", auth_scheme: "bearer" },
        { protocol: "messages", endpoint_url: "https://c.example/v1", auth_scheme: "x_api_key" },
        { protocol: "chat_completions", endpoint_url: "https://d.example/v1", auth_scheme: "bearer" },
      ],
    }))),
    "too_many_protocol_routes",
  );
});

test("preset protocol routes fill the draft only when applied", () => {
  const next = draft({ endpoint_url: "https://user.example/v1" });
  assert.equal(applyPresetProtocolRoutesToDraft(next, {}), false);
  assert.equal(next.protocol_routes[0]?.endpoint_url, "https://user.example/v1");
  assert.equal(applyPresetProtocolRoutesToDraft(next, {
    protocolRoutes: [
      { protocol: "responses", endpointUrl: "https://api.preset.example/v1", authScheme: "bearer" },
      { protocol: "messages", endpointUrl: "https://api.preset.example/anthropic", authScheme: "x_api_key" },
    ],
  }), true);
  assert.equal(next.protocol_routes[0]?.endpoint_url, "https://api.preset.example/v1");
  assert.equal(next.protocol_routes[0]?.protocol, "responses");
  assert.equal(next.protocol_routes[0]?.auth_scheme, "bearer");
  assert.equal(next.protocol_routes.length, 2);
});

test("route change detection covers origin, protocol, and override edits", () => {
  const value = destination();
  const unchanged = buildDestinationPatch(value, draft());
  assert.equal(destinationRouteChanged(value, unchanged), false);

  const renamed = buildDestinationPatch(value, draft({ name: "Renamed" }));
  assert.equal(destinationRouteChanged(value, renamed), false);

  const moved = buildDestinationPatch(value, draft({ endpoint_url: "https://other.example/v1" }));
  assert.equal(destinationRouteChanged(value, moved), true);

  const repathed = buildDestinationPatch(value, draft({ endpoint_url: "https://api.lab.example/v2" }));
  assert.equal(destinationRouteChanged(value, repathed), true);

  const reprotocoled = buildDestinationPatch(value, draft({ upstream_protocol: "responses" }));
  assert.equal(destinationRouteChanged(value, reprotocoled), true);

  const overridden = buildDestinationPatch(value, draft({
    models: [{
      public_model: "lab-opus",
      upstream_model: "vendor/opus",
      upstream_override: { protocol: "chat_completions", endpoint_url: "https://fast.lab.example/v1" },
    }],
  }));
  assert.equal(destinationRouteChanged(value, overridden), true);
});

test("patch origins collect the base and every override origin", () => {
  const input = buildDestinationPatch(destination(), draft({
    models: [
      { public_model: "a", upstream_model: "b", upstream_override: null },
      { public_model: "c", upstream_model: "d", upstream_override: null },
    ],
  }));
  assert.deepEqual(destinationPatchOrigins(input), ["https://api.lab.example"]);
  const withOverride = buildDestinationPatch(destination(), draft({
    models: [
      { public_model: "a", upstream_model: "b", upstream_override: null },
      {
        public_model: "c",
        upstream_model: "d",
        upstream_override: { protocol: "messages", endpoint_url: "https://m.lab.example/api" },
      },
    ],
  }));
  assert.deepEqual(destinationPatchOrigins(withOverride), [
    "https://api.lab.example",
    "https://m.lab.example",
  ]);
});

test("grant candidates flag Keys whose grants miss a new origin", () => {
  const value = destination();
  const moved = buildDestinationPatch(value, draft({ endpoint_url: "https://other.example/v1" }));
  const candidates = destinationGrantCandidates(value, [
    credential(),
    credential({ id: "cred-2", name: "Second", grants: { allowed_endpoint_ids: [], allowed_origins: [] } }),
    credential({ id: "cred-3", destination_id: "dest-2" }),
  ], moved);
  assert.deepEqual(candidates, [
    { id: "cred-1", name: "Lab Key", enabled: true, covered: false },
    { id: "cred-2", name: "Second", enabled: true, covered: false },
  ]);

  const unchanged = buildDestinationPatch(value, draft());
  const stable = destinationGrantCandidates(value, [credential({
    grants: {
      allowed_endpoint_ids: ["ep-base"],
      allowed_origins: ["https://api.lab.example"],
    },
  })], unchanged, [{
    id: "ep-base",
    connection_id: "conn-1",
    auth_scheme: "bearer",
    locked: false,
    operation: "chat_create",
    url: "https://api.lab.example/v1",
    wire_protocol: "chat_completions",
  }]);
  assert.deepEqual(stable, [{ id: "cred-1", name: "Lab Key", enabled: true, covered: true }]);
});

test("grant consent attaches only the explicit selection", () => {
  const input = buildDestinationPatch(destination(), draft());
  assert.equal("authorizeCredentialIds" in withAuthorizedCredentials(input, []), false);
  assert.deepEqual(
    withAuthorizedCredentials(input, ["cred-1", "cred-2"]).authorizeCredentialIds,
    ["cred-1", "cred-2"],
  );
});


test("route changes reconcile model protocols while metadata edits preserve manual selection", () => {
  const row = destination();
  const draft = destinationEditDraft(row);
  draft.protocol_routes[0]!.protocol = "messages";
  draft.protocol_routes[0]!.endpoint_url = "https://api.lab.example/anthropic/v1/messages";
  const changed = buildDestinationPatch(row, draft).models[0]!;
  assert.deepEqual(changed.protocols, ["messages"]);
  assert.equal(changed.preferred, "messages");
  const override = destinationEditDraft(row);
  override.models[0]!.upstream_override = { protocol: "messages", endpoint_url: "https://alternate.example/messages" };
  assert.deepEqual(buildDestinationPatch(row, override).models[0]!.protocols, ["messages"]);
  const off = destination({ catalog: [{ ...row.catalog[0]!, enabled: false, protocols: [] }] });
  const enable = destinationEditDraft(off);
  enable.models[0]!.enabled = true;
  assert.deepEqual(buildDestinationPatch(off, enable).models[0]!.protocols, ["chat_completions"]);
  const multi = destination({ protocols: ["chat_completions", "messages"], protocol_routes: [
    { protocol: "chat_completions", endpoint_url: row.base_url!, auth_scheme: "bearer" },
    { protocol: "messages", endpoint_url: "https://api.lab.example/anthropic/v1/messages", auth_scheme: "x_api_key" },
  ], catalog: [{ ...row.catalog[0]!, protocols: ["messages"], preferred: "messages" }] });
  const rename = destinationEditDraft(multi);
  rename.name = "Renamed";
  assert.deepEqual(buildDestinationPatch(multi, rename).models[0]!.protocols, ["messages"]);
  assert.equal(buildDestinationPatch(multi, rename).models[0]!.preferred, "messages");
});
