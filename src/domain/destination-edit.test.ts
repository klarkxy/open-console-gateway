import assert from "node:assert/strict";
import test from "node:test";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import {
  DESTINATION_EDIT_ISSUE_KEYS,
  DestinationEditError,
  buildDestinationPatch,
  destinationEditDraft,
  destinationGrantCandidates,
  destinationPatchOrigins,
  destinationRouteChanged,
  isDestinationDeletable,
  isDestinationEditable,
  withAuthorizedCredentials,
  type DestinationEditDraft,
  type DestinationEditIssue,
} from "./destination-edit.ts";

function destination(overrides: Partial<Destination> = {}): Destination {
  return {
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

function draft(overrides: Partial<DestinationEditDraft> = {}): DestinationEditDraft {
  return {
    name: "Lab HTTP",
    endpoint_url: "https://api.lab.example/v1",
    auth_scheme: "bearer",
    upstream_protocol: "chat_completions",
    models: [{ public_model: "lab-opus", upstream_model: "vendor/opus", upstream_override: null }],
    ...overrides,
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
  assert.equal(result.endpoint_url, "https://api.lab.example/v1");
  assert.equal(result.auth_scheme, "bearer");
  assert.equal(result.upstream_protocol, "chat_completions");
  assert.deepEqual(result.models, [{
    enabled: true,
    public_model: "lab-fast",
    upstream_model: "vendor/fast",
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
