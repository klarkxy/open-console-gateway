import assert from "node:assert/strict";
import test from "node:test";
import type { AccountOperationDetail } from "../api/pages.ts";
import type { Account } from "../api/dashboard.ts";
import type { ConnectionEndpoint } from "../api/connections.ts";
import type { Identity, IdentityCredential } from "../api/identities.ts";
import { DashboardConflictError, DashboardRequestError } from "../api/dashboard-v3.ts";
import {
  accountCredentialMenuOptions,
  bindingDestinationOptions,
  bindingDraftFrom,
  buildBindingPayload,
  buildCreatePayload,
  buildRotatePayload,
  createPayloadSignature,
  credentialWriteSupport,
  CredentialEditorError,
  emptyCreateDraft,
  emptyRotateDraft,
  isUncertainCreateFailure,
  nextCreateOperationId,
  endpointMatchesSavedGrant,
  normalizeOrigin,
  unionOriginsForEndpoints,
  type CredentialBindingDraft,
} from "./account-credential.ts";

function account(overrides: Partial<Account> = {}): Account {
  return {
    id: "acc-1",
    name: "Go",
    username: "",
    password: "",
    key: "",
    enabled: true,
    account_type: "key",
    setup_step: "ready",
    provider_id: "opencode",
    credential_kind: "api_key",
    quota_scope: "key",
    purchase_date: "2026-08-01",
    expires_on: "2026-09-01",
    cooldown_until: null,
    cooldown_generic_until: null,
    cooldown_5h_until: null,
    cooldown_week_until: null,
    cooldown_month_until: null,
    cooldown_free_until: null,
    last_error: null,
    auth_error: null,
    notes: "",
    usage_sync_last_success_at: null,
    usage_sync_next_allowed_at: null,
    created_at: "2026-08-01T00:00:00Z",
    updated_at: "2026-08-01T00:00:00Z",
    verification_status: "verified",
    connection_verified_at: "2026-08-01T00:00:00Z",
    verification_error: null,
    plan_routable: true,
    model_capabilities: [],
    ...overrides,
  };
}

function credential(overrides: Partial<IdentityCredential> = {}): IdentityCredential {
  return {
    bindings: [{
      id: "bind-1",
      connection_id: "conn-1",
      allowed_endpoint_ids: [],
      allowed_origins: [],
      model_scope: { kind: "all" },
      enabled: true,
      routing_rank: 0,
    }],
    credential: {
      id: "cred-1",
      purpose: "inference",
      material_kind: "api_key",
      has_material: true,
      version: 1,
      enabled: true,
      auth_state: "unknown",
      auth_state_version: 1,
      expires_at: null,
    },
    last_error: null,
    legacy: { kind: "account", id: "acc-1" },
    onboarding_task: null,
    quota_windows: [],
    subject: "account_credential",
    subscription: null,
    quota_pool_id: null,
    ...overrides,
  };
}

function identity(overrides: Partial<Identity> = {}): Identity {
  return {
    credentials: [credential()],
    declared_relations: [],
    identity: {
      id: "ident-1",
      label: "Go",
      authority_ref: null,
      identity_confidence: "opaque",
      enabled: true,
      notes: null,
    },
    legacy: { kind: "account", id: "acc-1" },
    ...overrides,
  };
}

test("rotate payload trims the Key and rejects a blank secret without consuming the draft", () => {
  const draft = { secret: "  sk-new  " };
  assert.deepEqual(buildRotatePayload(draft), { secretInput: "sk-new" });
  assert.equal(draft.secret, "  sk-new  ");

  const empty = emptyRotateDraft();
  empty.secret = "   ";
  assert.throws(
    () => buildRotatePayload(empty),
    (error: unknown) => error instanceof CredentialEditorError && error.issue === "missing_secret",
  );
  assert.equal(empty.secret, "   ");
});

function untouchedBinding(
  overrides: Partial<CredentialBindingDraft> & Pick<CredentialBindingDraft, "enabled" | "scopeKind" | "models">,
): CredentialBindingDraft {
  return { selectedEndpointIds: [], destinationsTouched: false, ...overrides };
}

test("binding payload keeps exact model names and does not rewrite suffixes", () => {
  const all = buildBindingPayload(untouchedBinding({
    enabled: false,
    scopeKind: "all",
    models: ["ignored-model"],
  }));
  assert.deepEqual(all, { enabled: false, modelScope: { kind: "all" } });

  const only = buildBindingPayload(untouchedBinding({
    enabled: true,
    scopeKind: "only",
    models: ["  gpt-4-turbo  ", "claude-3-5-sonnet-20241022", ""],
  }));
  assert.deepEqual(only, {
    enabled: true,
    modelScope: {
      kind: "only",
      models: ["gpt-4-turbo", "claude-3-5-sonnet-20241022"],
    },
  });
});

test("binding only-scope rejects empty, duplicate, oversized, and control-character names", () => {
  assert.throws(
    () => buildBindingPayload(untouchedBinding({ enabled: true, scopeKind: "only", models: ["  ", ""] })),
    (error: unknown) => error instanceof CredentialEditorError && error.issue === "missing_models",
  );
  assert.throws(
    () => buildBindingPayload(untouchedBinding({ enabled: true, scopeKind: "only", models: ["same", "same"] })),
    (error: unknown) => error instanceof CredentialEditorError && error.issue === "duplicate_model",
  );
  assert.throws(
    () => buildBindingPayload(untouchedBinding({
      enabled: true,
      scopeKind: "only",
      models: ["x".repeat(201)],
    })),
    (error: unknown) => error instanceof CredentialEditorError && error.issue === "model_too_long",
  );
  assert.throws(
    () => buildBindingPayload(untouchedBinding({ enabled: true, scopeKind: "only", models: ["bad\nname"] })),
    (error: unknown) => error instanceof CredentialEditorError && error.issue === "model_has_control_character",
  );
});

test("binding draft hydrates from the selected credential binding without inventing models", () => {
  assert.deepEqual(bindingDraftFrom(null), {
    enabled: true,
    scopeKind: "all",
    models: [""],
    selectedEndpointIds: [],
    destinationsTouched: false,
  });
  assert.deepEqual(bindingDraftFrom(credential().bindings[0] ?? null), {
    enabled: true,
    scopeKind: "all",
    models: [""],
    selectedEndpointIds: [],
    destinationsTouched: false,
  });
  const only = credential({
    bindings: [{
      id: "bind-2",
      connection_id: "conn-1",
      allowed_endpoint_ids: [],
      allowed_origins: [],
      model_scope: { kind: "only", models: ["exact-a", "exact-b"] },
      enabled: false,
      routing_rank: 1,
    }],
  });
  assert.deepEqual(bindingDraftFrom(only.bindings[0] ?? null), {
    enabled: false,
    scopeKind: "only",
    models: ["exact-a", "exact-b"],
    selectedEndpointIds: [],
    destinationsTouched: false,
  });
});

function operations(overrides: Partial<AccountOperationDetail> = {}): AccountOperationDetail {
  return { rotate: true, binding: true, create: true, credentialId: "cred-1", bindingId: "bind-1", identityId: "ident-1",
    unsupportedReason: null, allowedConnections: [], shareTargets: [], testModels: [], grantedEndpointIds: [], staleEndpointIds: [], staleOrigins: [], ...overrides };
}

test("write support follows server decisions and selects exact ids among sibling credentials", () => {
  const sibling = credential({ credential: { ...credential().credential, id: "cred-sibling" }, bindings: [{ ...credential().bindings[0]!, id: "bind-sibling" }] });
  const shared = identity({ credentials: [sibling, credential()] });
  const support = credentialWriteSupport(operations(), shared);
  assert.equal(support.credential?.credential.id, "cred-1");
  assert.equal(support.bindingRecord?.id, "bind-1");
  assert.deepEqual(accountCredentialMenuOptions(account(), operations(), shared).map(row => row.key), ["rotate-key", "add-key", "edit-binding"]);
  assert.equal(credentialWriteSupport(operations({ rotate: false, binding: false, create: false }), shared).rotate, false);
  assert.equal(credentialWriteSupport(null, shared).create, false);
  assert.equal(credentialWriteSupport(operations({ credentialId: "missing", bindingId: "missing" }), shared).credential, null);
  assert.equal(credentialWriteSupport(operations({ bindingId: "bind-sibling" }), shared).bindingRecord, null);
  assert.equal(credentialWriteSupport(operations({ create: false, unsupportedReason: "dedicated_account_flow" }), shared).create, false);
});

function endpoint(overrides: Partial<ConnectionEndpoint> = {}): ConnectionEndpoint {
  return {
    id: "ep-1",
    connection_id: "conn-1",
    auth_scheme: "bearer",
    locked: false,
    operation: "chat_create",
    url: "https://lab.example/v1/chat/completions",
    wire_protocol: "chat_completions",
    ...overrides,
  };
}

test("create payload defaults to independent quota and only shares when the user picks a same-identity Key", () => {
  const draft = emptyCreateDraft("conn-1");
  draft.secret = "  sk-new  ";
  draft.accountLabel = " Key B ";
  assert.deepEqual(buildCreatePayload(draft, ["cred-1", "cred-2"]), {
    connectionId: "conn-1",
    secretInput: "sk-new",
    accountLabel: "Key B",
    quotaSharing: { kind: "independent" },
  });

  const shared = emptyCreateDraft("conn-1");
  shared.secret = "sk-new";
  shared.sharingKind = "shared";
  shared.shareCredentialId = "cred-2";
  assert.deepEqual(buildCreatePayload(shared, ["cred-1", "cred-2"]), {
    connectionId: "conn-1",
    secretInput: "sk-new",
    quotaSharing: { kind: "shared", credentialId: "cred-2" },
  });

  shared.shareCredentialId = "foreign";
  assert.throws(
    () => buildCreatePayload(shared, ["cred-1", "cred-2"]),
    (error: unknown) => error instanceof CredentialEditorError && error.issue === "missing_share_target",
  );
  assert.equal(emptyCreateDraft("conn-1").sharingKind, "independent");

});

test("binding grants are omitted until destination selection changes, then both lists are sent", () => {
  const endpoints = [
    endpoint(),
    endpoint({
      id: "ep-2",
      url: "HTTPS://Other.Example/v1",
      wire_protocol: "messages",
      operation: "message_create",
    }),
    endpoint({
      id: "ep-sealed",
      url: null,
      locked: true,
      wire_protocol: "responses",
      operation: "response_create",
    }),
  ];
  const saved = credential({
    bindings: [{
      id: "bind-1",
      connection_id: "conn-1",
      allowed_endpoint_ids: ["ep-1", "gone-ep"],
      allowed_origins: ["https://lab.example", "https://stale.example"],
      model_scope: { kind: "all" },
      enabled: true,
      routing_rank: 0,
    }],
  }).bindings[0] ?? null;

  const draft = bindingDraftFrom(saved, ["ep-1"]);
  assert.deepEqual(draft.selectedEndpointIds, ["ep-1"]);
  assert.equal(draft.destinationsTouched, false);
  const unchanged = buildBindingPayload(draft, endpoints);
  assert.equal("allowedEndpointIds" in unchanged, false);
  assert.equal("allowedOrigins" in unchanged, false);
  draft.destinationsTouched = true;
  draft.selectedEndpointIds = [];
  const revoked = buildBindingPayload(draft, endpoints);
  assert.deepEqual(revoked.allowedEndpointIds, []);
  assert.deepEqual(revoked.allowedOrigins, []);

  draft.selectedEndpointIds = ["ep-1", "ep-2", "ep-sealed"];
  const granted = buildBindingPayload(draft, endpoints);
  assert.deepEqual(granted.allowedEndpointIds, ["ep-1", "ep-2", "ep-sealed"]);
  assert.deepEqual(granted.allowedOrigins, ["https://lab.example", "https://other.example"]);
  assert.deepEqual(unionOriginsForEndpoints(endpoints, ["ep-sealed"]), []);
  assert.equal(bindingDestinationOptions(endpoints)[2]?.url, null);
  assert.equal(bindingDestinationOptions(endpoints)[2]?.locked, true);
});

test("normalizeOrigin lowercases scheme and host and rejects non-http", () => {
  assert.equal(normalizeOrigin("HTTPS://Lab.Example/v1/chat/completions"), "https://lab.example");
  assert.equal(normalizeOrigin("https://lab.example:8443"), "https://lab.example:8443");
  assert.equal(normalizeOrigin("ftp://lab.example"), null);
  assert.equal(normalizeOrigin("not-a-url"), null);
  assert.equal(normalizeOrigin("https://service.example:443/v1/messages"), "https://service.example");
  assert.equal(
    normalizeOrigin("http://[2001:DB8:0:0:0:0:0:1]:8080"),
    "http://[2001:db8::1]:8080",
  );
  assert.equal(
    normalizeOrigin("http://[2001:db8::1]:8080/v1/messages"),
    normalizeOrigin("http://[2001:DB8:0:0:0:0:0:1]:8080"),
  );
  assert.notEqual(normalizeOrigin("http://service.example"), normalizeOrigin("https://service.example"));
  assert.notEqual(normalizeOrigin("https://service.example:8443"), normalizeOrigin("https://service.example"));
});

test("saved origin grants match canonical endpoint origins", () => {
  const binding = credential().bindings[0]!;
  binding.allowed_endpoint_ids = ["ep-1"];
  binding.allowed_origins = ["https://service.example:443"];
  assert.equal(endpointMatchesSavedGrant(endpoint({
    id: "ep-1",
    url: "https://service.example/v1/messages",
  }), binding), true);
  binding.allowed_origins = ["http://[2001:DB8:0:0:0:0:0:1]:8080"];
  assert.equal(endpointMatchesSavedGrant(endpoint({
    id: "ep-1",
    url: "http://[2001:db8::1]:8080/v1/messages",
  }), binding), true);
  binding.allowed_origins = ["http://service.example"];
  assert.equal(endpointMatchesSavedGrant(endpoint({
    id: "ep-1",
    url: "https://service.example/v1/messages",
  }), binding), false);
});

test("same endpoint ID with a moved Origin stays unchecked and is not granted by another checkbox", () => {
  const moved = endpoint({ url: "https://new.example/v1/chat/completions" });
  const other = endpoint({
    id: "ep-2",
    url: "https://other.example/v1",
    wire_protocol: "messages",
    operation: "message_create",
  });
  const saved = {
    id: "bind-1",
    connection_id: "conn-1",
    allowed_endpoint_ids: ["ep-1"],
    allowed_origins: ["https://lab.example"],
    model_scope: { kind: "all" as const },
    enabled: true,
    routing_rank: 0,
  };
  const draft = bindingDraftFrom(saved, []);
  assert.deepEqual(draft.selectedEndpointIds, []);
  assert.equal(draft.destinationsTouched, false);
  assert.equal("allowedEndpointIds" in buildBindingPayload(draft, [moved, other]), false);

  draft.destinationsTouched = true;
  draft.selectedEndpointIds = ["ep-2"];
  const granted = buildBindingPayload(draft, [moved, other]);
  assert.deepEqual(granted.allowedEndpointIds, ["ep-2"]);
  assert.deepEqual(granted.allowedOrigins, ["https://other.example"]);
  assert.equal(granted.allowedOrigins?.includes("https://new.example"), false);
  assert.equal(granted.allowedEndpointIds?.includes("ep-1"), false);

  const sealedDraft = bindingDraftFrom({
    ...saved,
    allowed_endpoint_ids: ["ep-sealed"],
    allowed_origins: [],
  }, ["ep-sealed"]);
  assert.deepEqual(sealedDraft.selectedEndpointIds, ["ep-sealed"]);
});

test("create operation id stays stable for an uncertain same payload and rejects a changed payload", () => {
  const first = nextCreateOperationId({
    previousId: null,
    previousSignature: null,
    nextSignature: "a",
    lastFailure: "none",
  });
  assert.match(first, /^[0-9a-f-]{36}$/iu);
  assert.equal(nextCreateOperationId({
    previousId: first,
    previousSignature: "a",
    nextSignature: "a",
    lastFailure: "uncertain",
  }), first);
  assert.throws(
    () => nextCreateOperationId({
      previousId: first,
      previousSignature: "a",
      nextSignature: "b",
      lastFailure: "uncertain",
    }),
    (error: unknown) => error instanceof CredentialEditorError && error.issue === "uncertain_payload_locked",
  );
  const afterDefinitive = nextCreateOperationId({
    previousId: first,
    previousSignature: "a",
    nextSignature: "a",
    lastFailure: "definitive",
  });
  assert.notEqual(afterDefinitive, first);
  assert.equal(isUncertainCreateFailure(new TypeError("network")), true);
  assert.equal(isUncertainCreateFailure(new SyntaxError("bad json")), true);
  assert.equal(isUncertainCreateFailure(new Error("generic")), true);
  assert.equal(isUncertainCreateFailure(new DashboardRequestError("bad", 400, "invalid")), false);
  assert.equal(isUncertainCreateFailure(new DashboardConflictError("stale", 5, 11)), false);
  assert.equal(isUncertainCreateFailure(new DashboardRequestError("down", 503, "")), true);
  const payload = buildCreatePayload({
    secret: "sk",
    accountLabel: "",
    connectionId: "conn-1",
    sharingKind: "independent",
    shareCredentialId: "cred-1",
  }, ["cred-1"]);
  assert.equal(createPayloadSignature(payload), createPayloadSignature({
    connectionId: "conn-1",
    secretInput: "sk",
    quotaSharing: { kind: "independent" },
  }));
});
