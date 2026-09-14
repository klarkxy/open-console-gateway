import assert from "node:assert/strict";
import test from "node:test";
import type { Account } from "../api/dashboard.ts";
import type { Identity, IdentityCredential } from "../api/identities.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import {
  accountCredentialCountLabel,
  accountExpiryDisplay,
  accountShowsDeclaredRelation,
  credentialForAccount,
  inferenceAuthState,
  inferenceCredentials,
  inferenceLastError,
  presentedAccountStatusLabel,
  presentedAccountStatusTagType,
  selectedBindingDisabled,
  selectedModelRestrictionLabel,
  selectedQuotaShareLabel,
  sharedQuotaSiblings,
  v3AccountShowsExpiry,
} from "./account-identity.ts";

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
    bindings: [],
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

function dynamicCatalog(providerId: string): ProviderCatalogEntry {
  return {
    provider_id: providerId,
    origin: "custom",
    editable: true,
    deletable: true,
    offering: "api",
    display_name: "Lab",
    display_family: "Lab",
    credential_kind: "api_key",
    quota_scope: "key",
    singleton: false,
    creation_availability: "available",
    verification_policy: "not_required",
    verification_runtime_availability: "not_applicable",
    routable: true,
    managed_registration: false,
    pricing_availability: "unpriced",
    usage_availability: "unavailable",
    manual_usage_calibration: false,
    quota_unit: "",
    model_source: "dynamic_provider",
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
  };
}

test("a multi-key identity is found by credential.legacy and never the first sibling", () => {
  const sibling = credential({
    credential: {
      ...credential().credential,
      id: "cred-2",
      auth_state: "invalid",
    },
    last_error: "401-sibling",
    legacy: { kind: "account", id: "acc-2" },
    subscription: { expires_on: "2026-12-01", purchase_date: "2026-11-01", source: "legacy_manual" },
  });
  const row = identity({
    legacy: { kind: "account", id: "acc-1" },
    credentials: [sibling, credential({
      credential: { ...credential().credential, auth_state: "valid" },
    })],
  });

  assert.equal(credentialForAccount(row, "acc-2")?.credential.id, "cred-2");
  assert.equal(credentialForAccount(row, "acc-1")?.credential.id, "cred-1");
  assert.equal(credentialForAccount(row, "missing"), null);

  assert.equal(inferenceAuthState(row, "acc-1"), "valid");
  assert.equal(inferenceAuthState(row, "acc-2"), "invalid");
  assert.equal(inferenceLastError(row, "acc-1"), null);
  assert.equal(inferenceLastError(row, "acc-2"), "401-sibling");
  assert.equal(presentedAccountStatusLabel(account({ id: "acc-1" }), row), "已启用");
  assert.equal(presentedAccountStatusLabel(account({ id: "acc-2" }), row), "不可用");
  assert.equal(accountExpiryDisplay(account({ id: "acc-1" }), row, null), "v3");
});

test("selected binding disabled and model restriction stay on this card, not a sibling", () => {
  const sibling = credential({
    credential: {
      ...credential().credential,
      id: "cred-2",
      auth_state: "invalid",
    },
    last_error: "401-sibling",
    legacy: { kind: "account", id: "acc-2" },
    bindings: [{
      id: "bind-sib",
      connection_id: "conn-1",
      allowed_endpoint_ids: [],
      allowed_origins: [],
      model_scope: { kind: "all" },
      enabled: true,
      routing_rank: 0,
    }],
  });
  const selected = credential({
    credential: { ...credential().credential, auth_state: "unknown" },
    bindings: [{
      id: "bind-1",
      connection_id: "conn-1",
      allowed_endpoint_ids: [],
      allowed_origins: [],
      model_scope: { kind: "only", models: ["model-x"] },
      enabled: false,
      routing_rank: 0,
    }],
  });
  const row = identity({ credentials: [sibling, selected] });

  assert.equal(selectedBindingDisabled(row, "acc-1"), true);
  assert.equal(selectedBindingDisabled(row, "acc-2"), false);
  assert.equal(selectedModelRestrictionLabel(row, "acc-1"), "仅 model-x");
  assert.equal(selectedModelRestrictionLabel(row, "acc-2"), null);
  assert.equal(presentedAccountStatusLabel(account({ id: "acc-1" }), row), "已启用");
  assert.equal(presentedAccountStatusLabel(account({ id: "acc-2" }), row), "不可用");
  assert.equal(presentedAccountStatusTagType(account({ id: "acc-1" }), row), "success");
  assert.equal(presentedAccountStatusTagType(account({ id: "acc-2" }), row), "error");
});

test("platform observer credentials are not inference Keys", () => {
  const observer = credential({
    credential: {
      ...credential().credential,
      id: "obs-1",
      purpose: "platform_observer",
      auth_state: "valid",
    },
  });
  const row = identity({ credentials: [observer, credential()] });
  assert.deepEqual(inferenceCredentials(row).map((item) => item.credential.id), ["cred-1"]);
  assert.equal(inferenceAuthState(row, "acc-1"), "unknown");

  const observerOnly = identity({ credentials: [observer] });
  assert.deepEqual(inferenceCredentials(observerOnly), []);
  assert.equal(inferenceAuthState(observerOnly, "acc-1"), null);
  assert.equal(presentedAccountStatusLabel(account(), observerOnly), "已启用");
  assert.equal(accountCredentialCountLabel(observerOnly), null);
});

test("d04 declared relations are a declared tag, never a verified-wallet claim", () => {
  assert.equal(accountShowsDeclaredRelation(identity()), false);
  assert.equal(accountShowsDeclaredRelation(identity({
    declared_relations: [{ group: "team-a", platform_account_id: "plat-1" }],
  })), true);
  assert.equal(accountShowsDeclaredRelation(null), false);
});

test("multiple credentials show a count; a single credential adds no chrome", () => {
  assert.equal(accountCredentialCountLabel(identity()), null);
  assert.equal(accountCredentialCountLabel(null), null);
  assert.equal(
    accountCredentialCountLabel(identity({
      credentials: [credential(), credential({
        credential: { ...credential().credential, id: "cred-2" },
      })],
    })),
    "2 个凭据",
  );
});

test("d07 null V4 subscription hides invented dynamic dates and keeps real Go purchase dates", () => {
  const go = account();
  const labId = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
  const catalog = [dynamicCatalog(labId)];
  const dynamic = account({
    id: "dyn-1",
    provider_id: labId,
    purchase_date: "2026-08-01",
    expires_on: "2026-09-01",
  });

  assert.equal(v3AccountShowsExpiry(go, null), true);
  assert.equal(accountExpiryDisplay(go, identity({
    credentials: [credential({ subscription: null })],
  }), null), "v3");

  assert.equal(v3AccountShowsExpiry(dynamic, catalog), true);
  assert.equal(accountExpiryDisplay(dynamic, identity({
    legacy: { kind: "account", id: "dyn-1" },
    credentials: [credential({ subscription: null })],
  }), catalog), "unknown");
  assert.equal(accountExpiryDisplay(dynamic, null, catalog), "v3");

  const custom = account({
    provider_id: "custom",
    purchase_date: "2026-08-01",
    expires_on: "2026-09-01",
  });
  assert.equal(v3AccountShowsExpiry(custom, null), false);
  assert.equal(accountExpiryDisplay(custom, identity({
    credentials: [credential({ subscription: null })],
  }), null), "hidden");
});

test("authState unknown follows the enable switch, invalid is auth_error, and valid does not upgrade V3 pending", () => {
  const ready = account({ verification_status: "verified" });
  assert.equal(presentedAccountStatusLabel(ready, identity()), "已启用");
  assert.equal(presentedAccountStatusTagType(ready, identity()), "success");

  const draftReady = account({ verification_status: "verified", enabled: false });
  assert.equal(presentedAccountStatusLabel(draftReady, identity()), "已禁用");
  assert.equal(presentedAccountStatusTagType(draftReady, identity()), "error");

  const invalid = identity({
    credentials: [credential({
      credential: { ...credential().credential, auth_state: "invalid" },
      last_error: "401",
    })],
  });
  assert.equal(presentedAccountStatusLabel(ready, invalid), "不可用");
  assert.equal(presentedAccountStatusTagType(ready, invalid), "error");

  const valid = identity({
    credentials: [credential({
      credential: { ...credential().credential, auth_state: "valid" },
    })],
  });
  assert.equal(presentedAccountStatusLabel(ready, valid), "已启用");
  assert.equal(presentedAccountStatusTagType(ready, valid), "success");

  const pendingDraft = account({
    plan_routable: false,
    verification_status: "pending",
    enabled: false,
  });
  assert.equal(presentedAccountStatusLabel(pendingDraft, valid), "待验证");
  assert.equal(presentedAccountStatusTagType(pendingDraft, valid), "warning");

  const failedDraft = account({
    plan_routable: false,
    verification_status: "failed",
    enabled: false,
  });
  assert.equal(presentedAccountStatusLabel(failedDraft, valid), "验证失败");
  assert.equal(presentedAccountStatusTagType(failedDraft, valid), "error");

  const v3FailedReady = account({ verification_status: "failed", auth_error: "401" });
  assert.equal(presentedAccountStatusLabel(v3FailedReady, valid), "不可用");
  assert.equal(presentedAccountStatusTagType(v3FailedReady, valid), "error");

  assert.equal(presentedAccountStatusLabel(ready, null), "已启用");
  assert.equal(presentedAccountStatusTagType(ready, null), "success");
});

test("quota pool id names shared siblings and leaves an independent third Key alone", () => {
  const names = (id: string) => ({ "acc-1": "Key A", "acc-2": "Key B", "acc-3": "Key C" }[id] ?? null);
  const sharedA = credential({
    credential: { ...credential().credential, id: "cred-a" },
    legacy: { kind: "account", id: "acc-1" },
    quota_pool_id: "pool-ab",
    quota_windows: [],
  });
  const sharedB = credential({
    credential: { ...credential().credential, id: "cred-b" },
    legacy: { kind: "account", id: "acc-2" },
    quota_pool_id: "pool-ab",
    quota_windows: [],
  });
  const independent = credential({
    credential: { ...credential().credential, id: "cred-c" },
    legacy: { kind: "account", id: "acc-3" },
    quota_pool_id: "pool-c",
    quota_windows: [],
  });
  const none = credential({
    credential: { ...credential().credential, id: "cred-d" },
    legacy: { kind: "account", id: "acc-4" },
    quota_pool_id: null,
    quota_windows: [],
  });
  const row = identity({
    credentials: [sharedA, sharedB, independent, none],
  });

  assert.deepEqual(sharedQuotaSiblings(row, "acc-1").map((item) => item.credential.id), ["cred-b"]);
  assert.deepEqual(sharedQuotaSiblings(row, "acc-2").map((item) => item.credential.id), ["cred-a"]);
  assert.deepEqual(sharedQuotaSiblings(row, "acc-3"), []);
  assert.deepEqual(sharedQuotaSiblings(row, "acc-4"), []);
  assert.equal(selectedQuotaShareLabel(row, "acc-1", names), "与 Key B 共享额度");
  assert.equal(selectedQuotaShareLabel(row, "acc-2", names), "与 Key A 共享额度");
  assert.equal(selectedQuotaShareLabel(row, "acc-3", names), null);
  assert.equal(selectedQuotaShareLabel(row, "acc-4", names), null);
  assert.equal(selectedQuotaShareLabel(identity({
    credentials: [credential({ quota_pool_id: "solo", quota_windows: [] })],
  }), "acc-1", names), null);
});
