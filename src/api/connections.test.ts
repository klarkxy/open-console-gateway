import assert from "node:assert/strict";
import test from "node:test";
import { connectionsApi, presentConnection } from "./connections.ts";
import type { ConnectionSummary } from "./generated/dashboard-v4.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { installFetchMock, setupControlPlane } from "../test-helpers/dashboard-v3-fetch.ts";

function summary(overrides: Partial<ConnectionSummary> = {}): ConnectionSummary {
  return {
    adapterKind: "configurable_http",
    authorization: "missing",
    credentialCount: 0,
    displayFamily: "Lab",
    eligibility: { reason: "missing_credential", state: "ineligible" },
    enabledCredentialCount: 0,
    endpoints: [{
      authScheme: "bearer",
      connectionId: "conn-1",
      id: "ep-1",
      locked: false,
      operation: "chat_create",
      url: "https://lab.example/v1/chat/completions",
      wireProtocol: "chat_completions",
    }],
    id: "conn-1",
    legacy: { id: "lab-http", kind: "dynamic_provider" },
    lifecycle: "configured",
    name: "Lab HTTP",
    offering: "api",
    origin: "custom",
    targetCount: 1,
    targets: [{
      connectionId: "conn-1",
      enabled: true,
      endpointIds: ["ep-1"],
      id: "tgt-1",
      publicName: "lab-opus",
      upstreamModelId: "vendor/opus",
    }],
    templateRef: { id: "custom-http", version: 1 },
    ...overrides,
  };
}

test("presentConnection maps the V4 wire row onto snake_case presentation fields", () => {
  const presented = presentConnection(summary());
  assert.equal(presented.id, "conn-1");
  assert.equal(presented.name, "Lab HTTP");
  assert.equal(presented.origin, "custom");
  assert.deepEqual(presented.template_ref, { id: "custom-http", version: 1 });
  assert.equal(presented.adapter_kind, "configurable_http");
  assert.equal(presented.lifecycle, "configured");
  assert.equal(presented.authorization, "missing");
  assert.deepEqual(presented.eligibility, { state: "ineligible", reason: "missing_credential" });
  assert.equal(presented.credential_count, 0);
  assert.equal(presented.enabled_credential_count, 0);
  assert.equal(presented.target_count, 1);
  assert.equal(presented.endpoints[0]?.connection_id, "conn-1");
  assert.equal(presented.endpoints[0]?.wire_protocol, "chat_completions");
  assert.equal(presented.endpoints[0]?.auth_scheme, "bearer");
  assert.equal(presented.targets[0]?.public_name, "lab-opus");
  assert.equal(presented.targets[0]?.upstream_model_id, "vendor/opus");
  assert.deepEqual(presented.legacy, { kind: "dynamic_provider", id: "lab-http" });
  assert.equal(presented.display_family, "Lab");
  assert.equal(presented.offering, "api");
});

test("connectionsApi.list presents the V4 projection and syncs nested CAS tokens", async () => {
  setupControlPlane(4, 11, "p1");
  installFetchMock(({ url, method }) => {
    if (url.endsWith("/connections") && method === "GET") {
      assert.match(url, /\/dashboard\/api\/v4\/connections$/);
      return {
        revision: { revision: 8, processGeneration: 11, pricingRevision: "p2" },
        connections: [summary()],
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const listed = await connectionsApi.list();
  assert.equal(listed.length, 1);
  assert.equal(listed[0]?.legacy.id, "lab-http");
  assert.equal(listed[0]?.authorization, "missing");
  const control = useControlPlaneStore();
  assert.equal(control.revision, 8);
  assert.equal(control.processGeneration, 11);
  assert.equal(control.pricingRevision, "p2");
});

test("connectionsApi.commitOnboarding does not replay a 409 revisionConflict", async () => {
  setupControlPlane(4, 11, "p1");
  let commits = 0;
  const requests = installFetchMock(({ url, method }) => {
    if (url.endsWith("/onboarding/commit") && method === "POST") {
      commits += 1;
      if (commits === 1) {
        return new Response(JSON.stringify({
          code: "revisionConflict",
          message: "revision conflict",
          currentRevision: 5,
          processGeneration: 11,
        }), { status: 409, headers: { "Content-Type": "application/json" } });
      }
      throw new Error("commit must not auto-replay");
    }
    if (url.endsWith("/contract") && method === "GET") {
      return { revision: 5, processGeneration: 11, pricingRevision: "p1" };
    }
    throw new Error(`unexpected request ${url}`);
  });

  await assert.rejects(
    () => connectionsApi.commitOnboarding({
      operationId: "11111111-1111-4111-8111-111111111111",
      connection: {
        kind: "new",
        templateId: "custom-http",
        name: "Lab",
        endpointUrl: "http://127.0.0.1:9",
        upstreamProtocol: "chat_completions",
        authKind: "bearer",
      },
      targets: [{ publicModel: "lab-opus", upstreamModel: "vendor/opus" }],
    }),
    (error: unknown) => error instanceof Error && error.message.includes("revision conflict"),
  );
  assert.equal(requests.filter((request) => request.method === "POST").length, 1);
});

test("connectionsApi.commitOnboarding publishes nested V4 CAS tokens", async () => {
  setupControlPlane(4, 11, "p1");
  installFetchMock(({ url, method }) => {
    if (url.endsWith("/onboarding/commit") && method === "POST") {
      return {
        connectionId: "conn-1",
        credentialId: null,
        replayed: false,
        revision: { revision: 9, processGeneration: 11, pricingRevision: "p3" },
        targetIds: ["tgt-1"],
      };
    }
    throw new Error(`unexpected request ${url}`);
  });

  const result = await connectionsApi.commitOnboarding({
    operationId: "11111111-1111-4111-8111-111111111111",
    connection: {
      kind: "new",
      templateId: "custom-http",
      name: "Lab",
      endpointUrl: "http://127.0.0.1:9",
      upstreamProtocol: "chat_completions",
      authKind: "bearer",
    },
    targets: [{ publicModel: "lab-opus", upstreamModel: "vendor/opus" }],
  });
  assert.equal(result.connection_id, "conn-1");
  assert.equal(result.replayed, false);
  const control = useControlPlaneStore();
  assert.equal(control.revision, 9);
  assert.equal(control.processGeneration, 11);
  assert.equal(control.pricingRevision, "p3");
});
