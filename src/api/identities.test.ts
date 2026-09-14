import assert from "node:assert/strict";
import test from "node:test";
import {
  identitiesApi,
  identityJoinKey,
  presentIdentity,
} from "./identities.ts";
import { DashboardConflictError } from "./dashboard-v3.ts";
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
      quotaPoolId: null,
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
  assert.equal(credential.quota_pool_id, null);
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

test("presentIdentity projects quotaPoolId even without quota windows", () => {
  const presented = presentIdentity(summary({
    credentials: [summary().credentials[0]!, {
      ...summary().credentials[0]!,
      credential: { ...summary().credentials[0]!.credential, id: "cred-2" },
      legacy: { id: "acc-2", kind: "account" },
      quotaPoolId: "pool-shared",
      quotaWindows: [],
    }],
  }));
  assert.equal(presented.credentials[0]?.quota_pool_id, null);
  assert.equal(presented.credentials[1]?.quota_pool_id, "pool-shared");
  assert.equal(presented.credentials[1]?.quota_windows.length, 0);
});

test("identityJoinKey distinguishes account and platform_account rows", () => {
  assert.equal(identityJoinKey({ kind: "account", id: "same" }), "account+same");
  assert.equal(
    identityJoinKey({ kind: "platform_account", id: "same" }),
    "platform_account+same",
  );
});

test("identitiesApi.listSnapshot presents the V4 projection and syncs nested CAS tokens", async () => {
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

  const snapshot = await identitiesApi.listSnapshot();
  const listed = snapshot.identities;
  assert.equal(listed.length, 1);
  assert.equal(listed[0]?.legacy.id, "acc-1");
  assert.equal(listed[0]?.credentials[0]?.credential.auth_state, "unknown");
  const control = useControlPlaneStore();
  assert.equal(control.revision, 8);
  assert.equal(control.processGeneration, 11);
  assert.equal(control.pricingRevision, "p2");
});

test("identitiesApi.rotateCredential posts CAS tokens and publishes nested revision", async () => {
  setupControlPlane(4, 11, "p1");
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/credentials/cred-1/rotate") && method === "POST") {
      assert.match(url, /\/dashboard\/api\/v4\/credentials\/cred-1\/rotate$/);
      return {
        credentialId: "cred-1",
        version: 2,
        authStateVersion: 3,
        replayed: false,
        revision: { revision: 9, processGeneration: 11, pricingRevision: "p3" },
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const result = await identitiesApi.rotateCredential("cred-1", { secretInput: "sk-new" });
  assert.deepEqual(result, {
    credential_id: "cred-1",
    version: 2,
    auth_state_version: 3,
    replayed: false,
  });
  assert.equal("secretInput" in result, false);
  assert.deepEqual(requests[0]?.body, {
    secretInput: "sk-new",
    expectedRevision: 4,
    processGeneration: 11,
  });
  const control = useControlPlaneStore();
  assert.equal(control.revision, 9);
  assert.equal(control.processGeneration, 11);
  assert.equal(control.pricingRevision, "p3");
});

test("identitiesApi.rotateCredential does not replay a 409 revisionConflict", async () => {
  setupControlPlane(4, 11, "p1");
  let rotates = 0;
  const requests = installFetchMock(({ url, method }) => {
    if (url.includes("/credentials/") && url.endsWith("/rotate") && method === "POST") {
      rotates += 1;
      if (rotates === 1) {
        return new Response(JSON.stringify({
          code: "revisionConflict",
          message: "revision conflict",
          currentRevision: 5,
          processGeneration: 11,
        }), { status: 409, headers: { "Content-Type": "application/json" } });
      }
      throw new Error("rotate must not auto-replay");
    }
    if (url.endsWith("/contract") && method === "GET") {
      return { revision: 5, processGeneration: 11, pricingRevision: "p1" };
    }
    throw new Error(`unexpected request ${url}`);
  });

  await assert.rejects(
    () => identitiesApi.rotateCredential("cred/a", { secretInput: "sk-new" }),
    (error: unknown) => error instanceof DashboardConflictError,
  );
  assert.equal(requests.filter((request) => request.method === "POST").length, 1);
  assert.match(requests[0]?.url ?? "", /\/credentials\/cred%2Fa\/rotate$/);
  const control = useControlPlaneStore();
  assert.equal(control.revision, 5);
});

test("identitiesApi.patchBinding sends enabled and exact modelScope and does not replay 409", async () => {
  setupControlPlane(4, 11, "p1");
  let patches = 0;
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/bindings/bind-1") && method === "PATCH") {
      patches += 1;
      if (patches === 1) {
        return {
          binding: {
            allowedEndpointIds: [],
            allowedOrigins: [],
            connectionId: "conn-1",
            enabled: false,
            id: "bind-1",
            modelScope: { kind: "only", models: ["gpt-4-turbo"] },
            routingRank: 0,
          },
          revision: { revision: 10, processGeneration: 11, pricingRevision: "p4" },
        };
      }
      throw new Error("patch must not auto-replay");
    }
    throw new Error(`unexpected request ${url}`);
  });

  const result = await identitiesApi.patchBinding("bind-1", {
    enabled: false,
    modelScope: { kind: "only", models: ["gpt-4-turbo"] },
  });
  assert.equal(result.binding.id, "bind-1");
  assert.equal(result.binding.enabled, false);
  assert.deepEqual(result.binding.model_scope, { kind: "only", models: ["gpt-4-turbo"] });
  assert.deepEqual(requests[0]?.body, {
    enabled: false,
    modelScope: { kind: "only", models: ["gpt-4-turbo"] },
    expectedRevision: 4,
    processGeneration: 11,
  });
  assert.equal(useControlPlaneStore().revision, 10);

  setupControlPlane(10, 11, "p4");
  installFetchMock(({ url, method }) => {
    if (url.endsWith("/bindings/bind-1") && method === "PATCH") {
      return new Response(JSON.stringify({
        code: "revisionConflict",
        message: "stale binding",
        currentRevision: 12,
        processGeneration: 11,
      }), { status: 409, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/contract") && method === "GET") {
      return { revision: 12, processGeneration: 11, pricingRevision: "p4" };
    }
    throw new Error(`unexpected request ${url}`);
  });
  await assert.rejects(
    () => identitiesApi.patchBinding("bind-1", { enabled: true, modelScope: { kind: "all" } }),
    (error: unknown) => error instanceof DashboardConflictError,
  );
});

test("identitiesApi.rotateCredential uses a captured expectation even after the store advances", async () => {
  setupControlPlane(4, 11, "p1");
  useControlPlaneStore().sync({ revision: 8, processGeneration: 11, pricingRevision: "p1" });
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/credentials/cred-1/rotate") && method === "POST") {
      return {
        credentialId: "cred-1",
        version: 2,
        authStateVersion: 3,
        replayed: false,
        revision: { revision: 9, processGeneration: 11, pricingRevision: "p1" },
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  await identitiesApi.rotateCredential(
    "cred-1",
    { secretInput: "sk-new" },
    { expectedRevision: 4, processGeneration: 11 },
  );
  assert.deepEqual(requests[0]?.body, {
    secretInput: "sk-new",
    expectedRevision: 4,
    processGeneration: 11,
  });
});

test("identitiesApi.rotateCredential keeps the original 409 when GET /contract fails", async () => {
  setupControlPlane(4, 11, "p1");
  const requests = installFetchMock(({ url, method }) => {
    if (url.includes("/credentials/") && url.endsWith("/rotate") && method === "POST") {
      return new Response(JSON.stringify({
        code: "revisionConflict",
        message: "revision conflict",
        currentRevision: 5,
        processGeneration: 11,
      }), { status: 409, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/contract") && method === "GET") {
      throw new Error("contract unavailable");
    }
    throw new Error(`unexpected request ${url}`);
  });

  await assert.rejects(
    () => identitiesApi.rotateCredential("cred-1", { secretInput: "sk-new" }),
    (error: unknown) => (
      error instanceof DashboardConflictError && error.message === "revision conflict"
    ),
  );
  assert.equal(requests.filter((request) => request.method === "POST").length, 1);
  assert.equal(requests.filter((request) => request.method === "GET").length, 1);
});

test("identitiesApi.listSnapshot returns presented identities plus the GET pair", async () => {
  setupControlPlane(9, 11, "p1");
  installFetchMock(({ url, method }) => {
    if (url.endsWith("/accounts") && method === "GET") {
      return {
        revision: { revision: 4, processGeneration: 11, pricingRevision: "p2" },
        identities: [summary()],
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const snapshot = await identitiesApi.listSnapshot();
  assert.equal(snapshot.identities[0]?.legacy.id, "acc-1");
  assert.deepEqual(snapshot.expectation, { expectedRevision: 4, processGeneration: 11 });
  const control = useControlPlaneStore();
  assert.equal(control.revision, 9);
});

test("identitiesApi.createIdentityCredential posts the exact body with a captured pair", async () => {
  setupControlPlane(4, 11, "p1");
  useControlPlaneStore().sync({ revision: 8, processGeneration: 11, pricingRevision: "p1" });
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/identities/ident-1/credentials") && method === "POST") {
      return {
        identityId: "ident-1",
        credentialId: "cred-2",
        bindingId: "bind-2",
        accountId: "acc-2",
        connectionId: "conn-1",
        version: 1,
        authStateVersion: 1,
        replayed: false,
        revision: { revision: 9, processGeneration: 11, pricingRevision: "p1" },
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const independent = await identitiesApi.createIdentityCredential(
    "ident-1",
    {
      connectionId: "conn-1",
      secretInput: "sk-new",
      accountLabel: "Key B",
      operationId: "00000000-0000-4000-8000-000000000001",
      quotaSharing: { kind: "independent" },
    },
    { expectedRevision: 4, processGeneration: 11 },
  );
  assert.deepEqual(independent, {
    identity_id: "ident-1",
    credential_id: "cred-2",
    binding_id: "bind-2",
    account_id: "acc-2",
    connection_id: "conn-1",
    version: 1,
    auth_state_version: 1,
    replayed: false,
  });
  assert.deepEqual(requests[0]?.body, {
    connectionId: "conn-1",
    secretInput: "sk-new",
    accountLabel: "Key B",
    operationId: "00000000-0000-4000-8000-000000000001",
    quotaSharing: { kind: "independent" },
    expectedRevision: 4,
    processGeneration: 11,
  });

  const sharedRequests = installFetchMock(({ url, method }) => {
    if (url.includes("/identities/") && url.endsWith("/credentials") && method === "POST") {
      return {
        identityId: "ident-1",
        credentialId: "cred-3",
        bindingId: "bind-3",
        accountId: "acc-3",
        connectionId: "conn-1",
        version: 1,
        authStateVersion: 1,
        replayed: false,
        revision: { revision: 10, processGeneration: 11, pricingRevision: "p1" },
      };
    }
    throw new Error(`unexpected request ${url}`);
  });
  await identitiesApi.createIdentityCredential(
    "ident/a",
    {
      connectionId: "conn-1",
      secretInput: "sk-shared",
      quotaSharing: { kind: "shared", credentialId: "cred-1" },
    },
    { expectedRevision: 4, processGeneration: 11 },
  );
  assert.match(sharedRequests[0]?.url ?? "", /\/identities\/ident%2Fa\/credentials$/);
  assert.deepEqual(sharedRequests[0]?.body, {
    connectionId: "conn-1",
    secretInput: "sk-shared",
    quotaSharing: { kind: "shared", credentialId: "cred-1" },
    expectedRevision: 4,
    processGeneration: 11,
  });
});

test("identitiesApi.createIdentityCredential does not replay a 409 revisionConflict", async () => {
  setupControlPlane(4, 11, "p1");
  let creates = 0;
  const requests = installFetchMock(({ url, method }) => {
    if (url.includes("/identities/") && url.endsWith("/credentials") && method === "POST") {
      creates += 1;
      if (creates === 1) {
        return new Response(JSON.stringify({
          code: "revisionConflict",
          message: "stale identity",
          currentRevision: 5,
          processGeneration: 11,
        }), { status: 409, headers: { "Content-Type": "application/json" } });
      }
      throw new Error("create must not auto-replay");
    }
    if (url.endsWith("/contract") && method === "GET") {
      return { revision: 5, processGeneration: 11, pricingRevision: "p1" };
    }
    throw new Error(`unexpected request ${url}`);
  });

  await assert.rejects(
    () => identitiesApi.createIdentityCredential("ident-1", {
      connectionId: "conn-1",
      secretInput: "sk-new",
    }),
    (error: unknown) => error instanceof DashboardConflictError,
  );
  assert.equal(requests.filter((request) => request.method === "POST").length, 1);
});

test("identitiesApi.patchBinding omits grant fields unless both lists are submitted", async () => {
  setupControlPlane(4, 11, "p1");
  const omitted = installFetchMock(({ url, method }) => {
    if (url.endsWith("/bindings/bind-1") && method === "PATCH") {
      return {
        binding: {
          allowedEndpointIds: ["ep-1"],
          allowedOrigins: ["https://lab.example"],
          connectionId: "conn-1",
          enabled: true,
          id: "bind-1",
          modelScope: { kind: "all" },
          routingRank: 0,
        },
        revision: { revision: 5, processGeneration: 11, pricingRevision: "p1" },
      };
    }
    throw new Error(`unexpected request ${url}`);
  });
  await identitiesApi.patchBinding("bind-1", {
    enabled: true,
    modelScope: { kind: "all" },
  }, { expectedRevision: 4, processGeneration: 11 });
  assert.deepEqual(omitted[0]?.body, {
    enabled: true,
    modelScope: { kind: "all" },
    expectedRevision: 4,
    processGeneration: 11,
  });

  const empty = installFetchMock(({ url, method }) => {
    if (url.endsWith("/bindings/bind-1") && method === "PATCH") {
      return {
        binding: {
          allowedEndpointIds: [],
          allowedOrigins: [],
          connectionId: "conn-1",
          enabled: true,
          id: "bind-1",
          modelScope: { kind: "all" },
          routingRank: 0,
        },
        revision: { revision: 6, processGeneration: 11, pricingRevision: "p1" },
      };
    }
    throw new Error(`unexpected request ${url}`);
  });
  await identitiesApi.patchBinding("bind-1", {
    enabled: true,
    modelScope: { kind: "all" },
    allowedEndpointIds: [],
    allowedOrigins: [],
  }, { expectedRevision: 4, processGeneration: 11 });
  assert.deepEqual(empty[0]?.body, {
    enabled: true,
    modelScope: { kind: "all" },
    allowedEndpointIds: [],
    allowedOrigins: [],
    expectedRevision: 4,
    processGeneration: 11,
  });

  const reselect = installFetchMock(({ url, method }) => {
    if (url.endsWith("/bindings/bind-1") && method === "PATCH") {
      return {
        binding: {
          allowedEndpointIds: ["ep-1"],
          allowedOrigins: ["https://lab.example"],
          connectionId: "conn-1",
          enabled: true,
          id: "bind-1",
          modelScope: { kind: "all" },
          routingRank: 0,
        },
        revision: { revision: 7, processGeneration: 11, pricingRevision: "p1" },
      };
    }
    throw new Error(`unexpected request ${url}`);
  });
  await identitiesApi.patchBinding("bind-1", {
    enabled: false,
    modelScope: { kind: "only", models: ["gpt-4"] },
    allowedEndpointIds: ["ep-1"],
    allowedOrigins: ["https://lab.example"],
  }, { expectedRevision: 4, processGeneration: 11 });
  assert.deepEqual(reselect[0]?.body, {
    enabled: false,
    modelScope: { kind: "only", models: ["gpt-4"] },
    allowedEndpointIds: ["ep-1"],
    allowedOrigins: ["https://lab.example"],
    expectedRevision: 4,
    processGeneration: 11,
  });
});
