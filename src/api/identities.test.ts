import assert from "node:assert/strict";
import test from "node:test";
import { identitiesApi, identityJoinKey, presentIdentity, presentIdentityList } from "./identities.ts";
import type { IdentitySummary } from "./generated/dashboard-v4.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";

function summary(overrides: Partial<IdentitySummary> = {}): IdentitySummary {
  return {
    credentials: [{
      bindings: [{
        allowedEndpointIds: ["ep-1"],
        allowedOrigins: ["https://lab.example"],
        connectionId: "conn-1",
        enabled: true,
        id: "bind-1",
        modelScope: { kind: "all" },
        routingRank: 0,
      }],
      credential: {
        authState: "unknown",
        authStateVersion: 1,
        enabled: true,
        expiresAt: null,
        hasMaterial: true,
        id: "cred-1",
        materialKind: "api_key",
        purpose: "inference",
        secretRef: "opaque-handle-must-not-leak-as-key",
        version: 1,
      },
      lastError: null,
      legacy: { id: "acc-1", kind: "account" },
      onboardingTask: null,
      quotaWindows: [{
        blockedUntil: null,
        metric: { limit: null, remaining: null },
        period: "month",
        policyMode: "authoritative_limit",
        relationConfidence: "declared",
        subject: "credential",
        subjectRef: "cred-1",
      }],
      subject: "account_credential",
      subscription: null,
    }],
    declaredRelations: [{
      group: "team-a",
      platformAccountId: "plat-1",
    }],
    identity: {
      authorityRef: { issuerOrSite: "https://lab.example", tenantOrSubject: "u1" },
      enabled: true,
      id: "ident-1",
      identityConfidence: "opaque",
      label: "Lab",
      notes: null,
    },
    legacy: { id: "acc-1", kind: "account" },
    ...overrides,
  };
}

test("presentIdentity maps the V4 wire row onto snake_case fields and strips secrets", () => {
  const presented = presentIdentity(summary());
  assert.equal(presented.identity.id, "ident-1");
  assert.equal(presented.identity.label, "Lab");
  assert.deepEqual(presented.identity.authority_ref, {
    issuer_or_site: "https://lab.example",
    tenant_or_subject: "u1",
  });
  assert.equal(presented.identity.identity_confidence, "opaque");
  assert.equal(presented.legacy.kind, "account");
  assert.equal(presented.legacy.id, "acc-1");
  assert.equal(identityJoinKey(presented.legacy), "account+acc-1");
  assert.deepEqual(presented.declared_relations, [{
    group: "team-a",
    platform_account_id: "plat-1",
  }]);

  const credential = presented.credentials[0];
  assert.ok(credential);
  assert.equal(credential.credential.id, "cred-1");
  assert.equal(credential.credential.purpose, "inference");
  assert.equal(credential.credential.material_kind, "api_key");
  assert.equal(credential.credential.has_material, true);
  assert.equal(credential.credential.auth_state, "unknown");
  assert.equal(credential.credential.auth_state_version, 1);
  assert.equal(credential.credential.expires_at, null);
  assert.equal(credential.subscription, null);
  assert.equal(credential.bindings[0]?.connection_id, "conn-1");
  assert.equal(credential.bindings[0]?.routing_rank, 0);
  assert.deepEqual(credential.bindings[0]?.allowed_endpoint_ids, ["ep-1"]);
  assert.equal(credential.quota_windows[0]?.policy_mode, "authoritative_limit");
  assert.equal(credential.quota_windows[0]?.subject_ref, "cred-1");
  assert.equal(credential.legacy.id, "acc-1");

  assert.equal("secretRef" in credential.credential, false);
  assert.equal("secret_ref" in credential.credential, false);
  assert.equal("key" in credential.credential, false);
  assert.equal(JSON.stringify(presented).includes("opaque-handle-must-not-leak-as-key"), false);
});

test("identityJoinKey distinguishes account and platform_account rows", () => {
  assert.equal(identityJoinKey({ kind: "account", id: "same" }), "account+same");
  assert.equal(
    identityJoinKey({ kind: "platform_account", id: "same" }),
    "platform_account+same",
  );
});

test("identitiesApi.list presents the V4 projection and syncs nested CAS tokens", async () => {
  setupControlPlane(4, 11, "p1");
  installFetchMock(({ url, method }) => {
    if (url.endsWith("/accounts") && method === "GET") {
      assert.match(url, /\/dashboard\/api\/v4\/accounts$/);
      return {
        revision: { revision: 8, processGeneration: 11, pricingRevision: "p2" },
        identities: [summary()],
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const listed = await identitiesApi.list();
  assert.equal(listed.length, 1);
  assert.equal(listed[0]?.legacy.id, "acc-1");
  assert.equal(listed[0]?.credentials[0]?.credential.auth_state, "unknown");
  assert.deepEqual(presentIdentityList({
    identities: [summary()],
    revision: { revision: 8, processGeneration: 11, pricingRevision: "p2" },
  }).map((row) => identityJoinKey(row.legacy)), ["account+acc-1"]);
  const control = useControlPlaneStore();
  assert.equal(control.revision, 8);
  assert.equal(control.processGeneration, 11);
  assert.equal(control.pricingRevision, "p2");
});
