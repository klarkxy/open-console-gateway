import assert from "node:assert/strict";
import test from "node:test";
import type { QuotaRecoveryDto } from "./dashboard-v4.ts";
import {
  credentialsApi,
  destinationsApi,
  presentDestination,
  presentDestinationCredential,
  presentDestinationListSnapshot,
  presentQuotaRecovery,
} from "./destinations.ts";
import type {
  DestinationCredentialDto,
  DestinationDto,
} from "./generated/dashboard-v4.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";

function destination(overrides: Partial<DestinationDto> = {}): DestinationDto {
  return {
    adapter: "http",
    authScheme: "bearer",
    baseUrl: "https://lab.example/v1",
    brandFamily: null,
    capabilities: {
      billingTierRequired: false,
      discoverableModels: true,
      externalIntegration: false,
      identityHeaders: false,
      managedSignup: false,
      observer: false,
      officialBalanceProbe: ["api.deepseek.com"],
      redirectPolicy: "no_follow",
      testable: true,
    },
    catalog: [{
      enabled: true,
      preferred: "chat_completions",
      protocols: ["chat_completions"],
      publicModel: "lab-opus",
      upstreamModel: "vendor/opus",
      upstreamOverride: null,
    }],
    enabled: true,
    id: "dest-1",
    legacy: { kind: "custom_account", id: "acct-1" },
    maxCredentials: 1,
    modelResolution: "public_only",
    name: "Lab HTTP",
    observerCredentialId: null,
    plan: null,
    protocols: ["chat_completions"],
    protocolRoutes: [],
    ...overrides,
  };
}

function credential(
  overrides: Partial<DestinationCredentialDto> = {},
): Omit<DestinationCredentialDto, "quotaRecovery"> & { quotaRecovery?: QuotaRecoveryDto | null } {
  return {
    authState: "unknown",
    cooldowns: {
      fiveHourUntil: null,
      freeUntil: null,
      genericUntil: null,
      monthUntil: null,
      weekUntil: null,
    },
    destinationId: "dest-1",
    enabled: true,
    grants: {
      allowedEndpointIds: ["ep-1"],
      allowedOrigins: ["https://lab.example"],
    },
    hasSecret: true,
    id: "cred-1",
    lastError: null,
    legacyAccountId: "acct-1",
    name: "Lab Key",
    notes: null,
    onboardingTask: null,
    purchaseDate: null,
    quotaPoolId: null,
    routingRank: 1,
    scope: { kind: "all" },
    ...overrides,
  };
}

test("presentDestination maps the V4 wire row onto snake_case presentation fields", () => {
  const presented = presentDestination(destination());
  assert.equal(presented.id, "dest-1");
  assert.equal(presented.adapter, "http");
  assert.equal(presented.auth_scheme, "bearer");
  assert.equal(presented.base_url, "https://lab.example/v1");
  assert.equal(presented.max_credentials, 1);
  assert.equal(presented.capabilities.discoverable_models, true);
  assert.equal(presented.capabilities.redirect_policy, "no_follow");
  assert.equal(presented.catalog[0]?.public_model, "lab-opus");
  assert.equal(presented.observer_credential_id, null);
  assert.deepEqual(presented.protocol_routes, []);
});

test("presentDestination maps protocolRoutes with an empty fallback", () => {
  const row = destination() as ReturnType<typeof destination> & {
    protocolRoutes: { protocol: "responses"; endpointUrl: string; authScheme: "bearer" }[];
  };
  row.protocolRoutes = [{
    protocol: "responses",
    endpointUrl: "https://lab.example/responses",
    authScheme: "bearer",
  }];
  const presented = presentDestination(row);
  assert.deepEqual(presented.protocol_routes, [{
    protocol: "responses",
    endpoint_url: "https://lab.example/responses",
    auth_scheme: "bearer",
  }]);
});

test("presentDestinationCredential maps grants, cooldowns, and has_secret", () => {
  const presented = presentDestinationCredential(credential({
    hasSecret: false,
    onboardingTask: { kind: "managed_registration", state: "in_progress", step: "email" },
  }));
  assert.equal(presented.destination_id, "dest-1");
  assert.equal(presented.has_secret, false);
  assert.deepEqual(presented.grants.allowed_endpoint_ids, ["ep-1"]);
  assert.equal(presented.cooldowns.generic_until, null);
  assert.deepEqual(presented.onboarding_task, {
    kind: "managed_registration",
    state: "in_progress",
    step: "email",
  });
});

test("destinationsApi.list presents the V4 projection", async () => {
  setupControlPlane(4, 11, "p1");
  installFetchMock(({ url, method }) => {
    if (url.endsWith("/destinations") && method === "GET") {
      assert.match(url, /\/dashboard\/api\/v4\/destinations$/);
      return {
        revision: { revision: 8, processGeneration: 11, pricingRevision: "p2" },
        destinations: [destination()],
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const listed = await destinationsApi.list();
  assert.equal(listed.length, 1);
  assert.equal(listed[0]?.id, "dest-1");
  assert.equal(listed[0]?.adapter, "http");
  const control = useControlPlaneStore();
  assert.equal(control.revision, 8);
  assert.equal(control.processGeneration, 11);
  assert.equal(control.pricingRevision, "p2");
});

test("credentialsApi.list presents the V4 projection", async () => {
  setupControlPlane(4, 11, "p1");
  installFetchMock(({ url, method }) => {
    if (url.endsWith("/credentials") && method === "GET") {
      return {
        revision: { revision: 8, processGeneration: 11, pricingRevision: "p2" },
        credentials: [credential()],
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const listed = await credentialsApi.list();
  assert.equal(listed.length, 1);
  assert.equal(listed[0]?.has_secret, true);
  assert.equal(listed[0]?.destination_id, "dest-1");
});

test("presentDestinationListSnapshot pairs the GET revision with presented rows", () => {
  const snapshot = presentDestinationListSnapshot({
    revision: { revision: 4, processGeneration: 11, pricingRevision: "p2" },
    destinations: [destination({ adapter: "zen", maxCredentials: 1 })],
  });
  assert.equal(snapshot.destinations[0]?.adapter, "zen");
  assert.deepEqual(snapshot.expectation, { expectedRevision: 4, processGeneration: 11 });
});

function quotaRecoveryDto(overrides: Partial<QuotaRecoveryDto> = {}): QuotaRecoveryDto {
  return {
    status: "waiting",
    reason: "quota_exhausted",
    window: "week",
    observedAt: "2026-09-20T11:00:00Z",
    resetsAt: "2026-09-27T00:00:00Z",
    nextRetryAt: "2026-09-20T12:30:00Z",
    failureCount: 1,
    ...overrides,
  };
}

test("presentDestinationCredential maps optional quotaRecovery without treating absence as health", () => {
  const absent = presentDestinationCredential(credential());
  assert.equal(absent.quota_recovery, null);

  const presented = presentDestinationCredential({
    ...credential(),
    quotaRecovery: quotaRecoveryDto({ status: "ready", window: "unknown", resetsAt: null }),
  });
  assert.deepEqual(presented.quota_recovery, {
    status: "ready",
    reason: "quota_exhausted",
    window: "unknown",
    observed_at: "2026-09-20T11:00:00Z",
    resets_at: null,
    next_retry_at: "2026-09-20T12:30:00Z",
    failure_count: 1,
  });
});

test("presentQuotaRecovery rejects malformed recovery objects", () => {
  assert.equal(presentQuotaRecovery(null), null);
  assert.equal(presentQuotaRecovery(quotaRecoveryDto({ status: "paused" as QuotaRecoveryDto["status"] })), null);
  assert.equal(presentQuotaRecovery(quotaRecoveryDto({ failureCount: "2" as unknown as number })), null);
});

test("credentialsApi.retryQuota posts flattened CAS and presents the returned Key", async () => {
  setupControlPlane(4, 11, "p1");
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/credentials/cred-1/quota-retry") && method === "POST") {
      return {
        revision: { revision: 9, processGeneration: 11, pricingRevision: "p2" },
        credential: {
          ...credential(),
          quotaRecovery: quotaRecoveryDto({ status: "ready" }),
        },
      };
    }
    throw new Error(`unexpected request ${method} ${url}`);
  });

  const result = await credentialsApi.retryQuota("cred-1");
  assert.equal(requests.length, 1);
  assert.match(requests[0]!.url, /\/dashboard\/api\/v4\/credentials\/cred-1\/quota-retry$/);
  assert.deepEqual(requests[0]!.body, { expectedRevision: 4, processGeneration: 11 });
  assert.equal(result.credential.quota_recovery?.status, "ready");
  assert.deepEqual(result.expectation, { expectedRevision: 9, processGeneration: 11 });
  assert.equal(useControlPlaneStore().revision, 9);
});
