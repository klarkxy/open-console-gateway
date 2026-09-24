import assert from "node:assert/strict";
import test from "node:test";
import { providerApi } from "./providers.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";

test("dynamic Provider update 409 refreshes catalog and provider without replaying PATCH", async () => {
  setupControlPlane(4, 11, "p1");
  let patchCalls = 0;
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/providers/lab-id") && method === "PATCH") {
      patchCalls += 1;
      if (patchCalls === 1) {
        return new Response(JSON.stringify({
          code: "revisionConflict",
          message: "revision conflict",
          currentRevision: 5,
          processGeneration: 11,
        }), { status: 409, headers: { "Content-Type": "application/json" } });
      }
      throw new Error("update must not auto-replay");
    }
    if (url.endsWith("/providers") && method === "GET") {
      return { entries: [], revision: 5, processGeneration: 11, pricingRevision: "p1" };
    }
    if (url.endsWith("/providers/lab-id") && method === "GET") {
      return {
        id: "lab-id",
        name: "Lab",
        origin: "custom",
        offering: "api",
        editable: true,
        deletable: true,
        endpointUrl: "http://127.0.0.1:9",
        upstreamProtocol: "chat_completions",
        authKind: "bearer",
        models: [{ publicModel: "lab-opus", upstreamModel: "vendor/opus" }],
        createdAt: "2026-01-01T00:00:00Z",
        updatedAt: "2026-01-01T00:00:00Z",
        revision: 5,
        processGeneration: 11,
      };
    }
    if (url.endsWith("/contract") && method === "GET") {
      return { revision: 5, processGeneration: 11, pricingRevision: "p1" };
    }
    throw new Error(`unexpected request ${method} ${url}`);
  });

  await assert.rejects(
    () => providerApi.updateProviderDefinition("lab-id", {
      name: "Lab",
      endpointUrl: "http://127.0.0.1:9",
      upstreamProtocol: "chat_completions",
      authKind: "bearer",
      models: [{ publicModel: "lab-opus", upstreamModel: "vendor/opus" }],
    }),
    (error: unknown) => error instanceof Error && error.message.includes("revision conflict"),
  );
  assert.equal(requests.filter((request) => request.method === "PATCH").length, 1);
  assert.ok(requests.some((request) => request.url.endsWith("/providers") && request.method === "GET"));
  assert.ok(requests.some((request) => request.url.endsWith("/providers/lab-id") && request.method === "GET"));
});

test("dynamic Provider update uses a captured definition pair even after the store advances", async () => {
  setupControlPlane(4, 11, "p1");
  useControlPlaneStore().sync({ revision: 8, processGeneration: 11, pricingRevision: "p1" });
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/providers/lab-id") && method === "PATCH") {
      return {
        provider: {
          id: "lab-id",
          name: "Lab",
          origin: "custom",
          offering: "api",
          editable: true,
          deletable: true,
          endpointUrl: "http://127.0.0.1:9",
          upstreamProtocol: "chat_completions",
          authKind: "bearer",
          models: [{ publicModel: "lab-opus", upstreamModel: "vendor/opus" }],
          createdAt: "2026-01-01T00:00:00Z",
          updatedAt: "2026-01-01T00:00:00Z",
          revision: 5,
          processGeneration: 11,
        },
        revision: 5,
        processGeneration: 11,
        pricingRevision: "p1",
      };
    }
    throw new Error(`unexpected request ${method} ${url}`);
  });

  await providerApi.updateProviderDefinition("lab-id", {
    name: "Lab Local",
    endpointUrl: "http://127.0.0.1:9",
    upstreamProtocol: "chat_completions",
    authKind: "bearer",
    models: [{ publicModel: "lab-opus", upstreamModel: "vendor/opus" }],
  }, { expectedRevision: 4, processGeneration: 11 });
  assert.equal(requests[0]?.body?.expectedRevision, 4);
  assert.equal(requests[0]?.body?.processGeneration, 11);
  assert.equal(requests[0]?.body?.name, "Lab Local");
});

test("dynamic Provider discover and test never persist a Key in the presented result", async () => {
  setupControlPlane(4, 11, "p1");
  installFetchMock(({ url }) => {
    if (url.endsWith("/providers/models/discover")) {
      return { models: ["vendor/opus"], truncated: false, revision: 4, processGeneration: 11 };
    }
    if (url.endsWith("/providers/test")) {
      return { ok: true, error: null, revision: 4, processGeneration: 11 };
    }
    throw new Error(`unexpected request ${url}`);
  });
  const discovered = await providerApi.discoverProviderDefinitionModels({
    endpoint_url: "http://127.0.0.1:9",
    upstream_protocol: "chat_completions",
    auth_kind: "bearer",
    key: "sk-probe",
  });
  const tested = await providerApi.testProviderDefinition({
    endpoint_url: "http://127.0.0.1:9",
    upstream_protocol: "chat_completions",
    auth_kind: "bearer",
    public_model: "lab-opus",
    upstream_model: "vendor/opus",
    key: "sk-probe",
  });
  assert.deepEqual(discovered, { models: ["vendor/opus"], truncated: false });
  assert.deepEqual(tested, { ok: true, error: null });
  assert.equal("key" in discovered, false);
  assert.equal("key" in tested, false);
});

test("Go protocol probe sends only provider, model, and protocol intent", async () => {
  setupControlPlane(12, 42, "p1");
  const requests = installFetchMock(({ url }) => {
    if (url.endsWith("/providers/opencode/protocol-probes")) {
      return {
        accountId: null,
        providerId: "opencode",
        modelId: "gpt-5.6-luna",
        results: [{ protocol: "responses", success: true, skipped: false, error: null }],
        contract: null,
        revision: 12,
        processGeneration: 42,
        pricingRevision: "p1",
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const result = await providerApi.runProtocolProbes("opencode", {
    model_id: "gpt-5.6-luna",
    protocols: ["responses"],
  });

  assert.equal(result.model_id, "gpt-5.6-luna");
  assert.deepEqual(requests[0], {
    url: "/dashboard/api/v4/providers/opencode/protocol-probes",
    method: "POST",
    body: {
      modelId: "gpt-5.6-luna",
      protocols: ["responses"],
      expectedRevision: 12,
      processGeneration: 42,
    },
  });
});

test("unified catalog refresh sends only the selected contract scope and CAS tokens", async () => {
  setupControlPlane(12, 42, "p1");
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/provider-contracts/provider/opencode/catalog/refresh") && method === "POST") {
      return {
        revision: 13,
        processGeneration: 42,
        pricingRevision: "p1",
        providers: [],
        customEndpoints: [],
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  await providerApi.refreshContractCatalog("provider", "opencode");

  assert.deepEqual(requests, [{
    url: "/dashboard/api/v4/provider-contracts/provider/opencode/catalog/refresh",
    method: "POST",
    body: { expectedRevision: 12, processGeneration: 42 },
  }]);
});

test("provider protocol override sends only selected Key grants with its captured CAS pair", async () => {
  setupControlPlane(12, 42, "p1");
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/provider-contracts/provider/opencode/model-protocol-overrides") && method === "PUT") {
      return {
        revision: 9,
        processGeneration: 42,
        pricingRevision: "p1",
        providers: [],
        customEndpoints: [],
      };
    }
    throw new Error(`unexpected request ${method} ${url}`);
  });

  await providerApi.updateModelProtocolOverrides(
    "provider",
    "opencode",
    [{ model_id: "mimo-v2.6-flash", protocol: "responses", state: "force_on" }],
    ["key-selected"],
    { expectedRevision: 8, processGeneration: 42 },
  );

  assert.deepEqual(requests, [{
    url: "/dashboard/api/v4/provider-contracts/provider/opencode/model-protocol-overrides",
    method: "PUT",
    body: {
      overrides: [{ modelId: "mimo-v2.6-flash", protocol: "responses", state: "force_on" }],
      authorizeCredentialIds: ["key-selected"],
      expectedRevision: 8,
      processGeneration: 42,
    },
  }]);
});

test("catalog remove posts V4 model ids then reloads contracts", async () => {
  setupControlPlane(12, 42, "p1");
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/provider-contracts/provider/opencode/catalog/remove") && method === "POST") {
      return {
        revision: { revision: 13, processGeneration: 42, pricingRevision: "p1" },
        removedIds: ["drop-me"],
        catalogModels: ["keep-me"],
      };
    }
    if (url.endsWith("/provider-contracts") && method === "GET") {
      return {
        revision: 13,
        processGeneration: 42,
        pricingRevision: "p1",
        providers: [],
        customEndpoints: [],
      };
    }
    throw new Error(`unexpected request ${method} ${url}`);
  });

  await providerApi.removeContractCatalogModels("provider", "opencode", ["drop-me"]);

  assert.deepEqual(requests, [
    {
      url: "/dashboard/api/v4/provider-contracts/provider/opencode/catalog/remove",
      method: "POST",
      body: { modelIds: ["drop-me"], expectedRevision: 12, processGeneration: 42 },
    },
    {
      url: "/dashboard/api/v4/provider-contracts",
      method: "GET",
      body: null,
    },
  ]);
});

test("Custom endpoint protocol probe stays blocked while overrides use the model-protocol-overrides route", async () => {
  setupControlPlane(8, 42, "p1");
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/accounts/custom-1")) {
      return { id: "custom-1", providerId: "custom", revision: 8, processGeneration: 42 };
    }
    if (url.endsWith("/provider-contracts/custom-endpoint/custom-1/model-protocol-overrides") && method === "PUT") {
      return {
        revision: 9,
        processGeneration: 42,
        pricingRevision: "p1",
        providers: [],
        customEndpoints: [],
      };
    }
    throw new Error(`unsupported request ${url}`);
  });

  // The probe is guarded client-side: it rejects as an Error and never
  // reaches the network.
  await assert.rejects(
    () => providerApi.runProtocolProbes("custom", {
      model_id: "Org/Model",
      protocols: ["chat_completions"],
    }),
    Error,
  );
  assert.equal(requests.length, 0, "blocked custom probe must not issue any request");
  await providerApi.updateModelProtocolOverrides(
    "custom_endpoint",
    "custom-1",
    [{ model_id: "Org/Model", protocol: "chat_completions", state: "force_off" }],
  );
  assert.deepEqual(requests.map(({ method, url }) => ({ method, url })), [
    {
      method: "PUT",
      url: "/dashboard/api/v4/provider-contracts/custom-endpoint/custom-1/model-protocol-overrides",
    },
  ]);
  assert.deepEqual(requests[0]?.body, {
    overrides: [{ modelId: "Org/Model", protocol: "chat_completions", state: "force_off" }],
    expectedRevision: 8,
    processGeneration: 42,
  });
});

const ZEN_FREE_ACCOUNT_ID = "00000000-0000-0000-0000-000000000002";

function zenFreeAccountDto(overrides: Record<string, unknown> = {}) {
  return {
    id: ZEN_FREE_ACCOUNT_ID,
    name: "OpenCode Zen Free",
    username: null,
    enabled: true,
    accountType: "key",
    setupStep: "ready",
    providerId: "opencode-zen-free",
    credentialKind: "none",
    quotaScope: "egress-ip",
    revision: 12,
    processGeneration: 42,
    purchaseDate: "2026-01-01",
    expiresOn: "2026-02-01",
    cooldownUntil: null,
    cooldownGenericUntil: null,
    cooldown5hUntil: null,
    cooldownWeekUntil: null,
    cooldownMonthUntil: null,
    cooldownFreeUntil: null,
    lastError: null,
    authError: null,
    notes: null,
    usageSyncLastSuccessAt: null,
    usageSyncNextAllowedAt: null,
    createdAt: "2026-01-01T00:00:00Z",
    updatedAt: "2026-01-01T00:00:00Z",
    verificationStatus: "not_required",
    connectionVerifiedAt: null,
    verificationError: null,
    planRoutable: true,
    customConfig: null,
    modelCapabilities: [],
    ...overrides,
  };
}

test("Zen Free provider settings reject non-Zen accounts before the dedicated write", async () => {
  setupControlPlane(12, 42, "p1");
  const requests = installFetchMock(({ url }) => {
    if (url.endsWith("/accounts/go-account-2")) {
      return { id: "go-account-2", providerId: "opencode", revision: 12, processGeneration: 42 };
    }
    throw new Error(`unexpected request ${url}`);
  });

  await assert.rejects(
    () => providerApi.updateProviderSettings("go-account-2", { enabled: false }),
    (error: unknown) => error instanceof Error && error.message.includes("Zen Free"),
  );
  assert.deepEqual(requests.map(({ method, url }) => ({ method, url })), [
    { method: "GET", url: "/dashboard/api/v4/accounts/go-account-2" },
  ]);
});

test("Zen Free enable switch writes the catalog provider through PATCH /providers/zen-free", async () => {
  setupControlPlane(12, 42, "p1");
  let enabled = true;
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith(`/accounts/${ZEN_FREE_ACCOUNT_ID}`)) {
      return zenFreeAccountDto({ enabled, revision: enabled ? 12 : 13 });
    }
    if (url.endsWith("/providers/zen-free") && method === "PATCH") {
      enabled = false;
      return {
        accountId: ZEN_FREE_ACCOUNT_ID,
        enabled: false,
        revision: 13,
        processGeneration: 42,
        pricingRevision: "p1",
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const result = await providerApi.updateProviderSettings(ZEN_FREE_ACCOUNT_ID, { enabled: false });

  assert.equal(result.account.provider_id, "opencode-zen-free");
  assert.equal(result.account.enabled, false);
  assert.equal(result.revision, 13);
  assert.deepEqual(requests[1], {
    url: "/dashboard/api/v4/providers/zen-free",
    method: "PATCH",
    body: {
      enabled: false,
      expectedRevision: 12,
      processGeneration: 42,
    },
  });
});
