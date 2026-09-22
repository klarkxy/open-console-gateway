import assert from "node:assert/strict";
import test from "node:test";
import type { Destination, DestinationCredential } from "../api/destinations.ts";
import { planDestinationSave } from "./destination-edit-save.ts";
import type { DestinationEditDraft } from "./destination-edit.ts";

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
    legacy: { kind: "dynamic", id: "prov-1" },
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
    routing_rank: 0,
    scope: { kind: "all" },
    ...overrides,
  };
}

function draftFor(value: Destination): DestinationEditDraft {
  return {
    name: value.name,
    endpoint_url: value.base_url ?? "",
    auth_scheme: value.auth_scheme,
    upstream_protocol: value.protocols[0] ?? "",
    protocol_routes: [{
      protocol: value.protocols[0] ?? "",
      endpoint_url: value.base_url ?? "",
      auth_scheme: value.auth_scheme,
    }],
    models: value.catalog.map((model) => ({
      public_model: model.public_model,
      upstream_model: model.upstream_model,
      upstream_override: model.upstream_override
        ? { protocol: model.upstream_override.protocol, endpoint_url: model.upstream_override.endpoint_url }
        : null,
    })),
  };
}

test("unchanged draft plans a direct patch", () => {
  const dest = destination();
  const plan = planDestinationSave(dest, [credential()], draftFor(dest));
  assert.equal(plan.status, "patch");
  if (plan.status === "patch") {
    assert.equal(plan.input.endpointUrl, "https://api.lab.example/v1");
    assert.equal(plan.input.authorizeCredentialIds, undefined);
  }
});

test("route change with no keys on the destination plans a direct patch", () => {
  const dest = destination();
  const draft = draftFor(dest);
  draft.endpoint_url = "https://api.other.example/v1";
  const plan = planDestinationSave(dest, [], draft);
  assert.equal(plan.status, "patch");
});

test("route change with keys plans explicit grant consent with coverage flags", () => {
  const dest = destination();
  const covered = credential({
    id: "cred-1",
    name: "Covered Key",
    grants: { allowed_endpoint_ids: [], allowed_origins: ["https://api.other.example"] },
  });
  const uncovered = credential({
    id: "cred-2",
    name: "Uncovered Key",
    grants: { allowed_endpoint_ids: [], allowed_origins: ["https://api.lab.example"] },
    destination_id: "dest-1",
  });
  const draft = draftFor(dest);
  draft.endpoint_url = "https://api.other.example/v1";
  const plan = planDestinationSave(dest, [covered, uncovered], draft);
  assert.equal(plan.status, "grant_consent");
  if (plan.status === "grant_consent") {
    assert.equal(plan.candidates.length, 2);
    const byId = new Map(plan.candidates.map((candidate) => [candidate.id, candidate]));
    assert.equal(byId.get("cred-1")?.covered, false);
    assert.equal(byId.get("cred-2")?.covered, false);
    assert.equal(byId.get("cred-1")?.enabled, true);
    assert.equal(byId.get("cred-1")?.name, "Covered Key");
  }
});

test("same-origin override still requires endpoint grant consent", () => {
  const dest = destination();
  const draft = draftFor(dest);
  draft.models[0]!.upstream_override = {
    protocol: "messages",
    endpoint_url: "https://api.lab.example/v1/messages",
  };
  const plan = planDestinationSave(dest, [credential()], draft, [{
    id: "ep-base",
    connection_id: "conn-1",
    auth_scheme: "bearer",
    locked: false,
    operation: "chat_create",
    url: "https://api.lab.example/v1",
    wire_protocol: "chat_completions",
  }]);
  assert.equal(plan.status, "grant_consent");
  if (plan.status === "grant_consent") {
    assert.equal(plan.candidates[0]?.covered, false);
  }
});

test("keys on other destinations never enter the consent list", () => {
  const dest = destination();
  const elsewhere = credential({ id: "cred-9", destination_id: "dest-2" });
  const draft = draftFor(dest);
  draft.endpoint_url = "https://api.other.example/v1";
  const plan = planDestinationSave(dest, [elsewhere], draft);
  assert.equal(plan.status, "patch");
});

test("invalid drafts surface the semantic issue", () => {
  const dest = destination();
  const draft = draftFor(dest);
  draft.models = [];
  const plan = planDestinationSave(dest, [], draft);
  assert.deepEqual(plan, { status: "invalid", issue: "missing_mappings" });
});

test("auth scheme change without origin change plans a direct patch", () => {
  const dest = destination();
  const draft = draftFor(dest);
  draft.auth_scheme = "x_api_key";
  const plan = planDestinationSave(dest, [credential()], draft);
  assert.equal(plan.status, "patch");
});

test("disabled keys are still listed for consent", () => {
  const dest = destination();
  const draft = draftFor(dest);
  draft.endpoint_url = "https://api.other.example/v1";
  const plan = planDestinationSave(dest, [credential({ enabled: false })], draft);
  assert.equal(plan.status, "grant_consent");
  if (plan.status === "grant_consent") {
    assert.equal(plan.candidates[0]?.enabled, false);
  }
});
