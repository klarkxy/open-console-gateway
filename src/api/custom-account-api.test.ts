import assert from "node:assert/strict";
import test from "node:test";
import { dashboardApi } from "./dashboard.ts";
import {
  installFetchMock,
  setupControlPlane,
  v3AccountDto,
} from "../test-helpers/dashboard-v3-fetch.ts";

function customAccount(id: string) {
  return v3AccountDto(id, {
    name: "Custom",
    enabled: false,
    providerId: "custom",
    purchaseDate: "",
    expiresOn: "",
    createdAt: "2026-08-21T00:00:00Z",
    updatedAt: "2026-08-21T00:00:00Z",
    verificationStatus: "verified",
  });
}

test("account model tests target one encoded account without CAS tokens", async () => {
  setupControlPlane(7);
  const requests = installFetchMock(() => ({
    accountId: "account/1",
    modelId: "Org/Model-A",
    protocol: "chat_completions",
    success: true,
    httpStatus: 200,
    durationMs: 12,
    error: null,
  }));

  const result = await dashboardApi.testAccountModel("account/1", "Org/Model-A");

  assert.equal(result.success, true);
  assert.equal(requests[0]?.url, "/dashboard/api/v3/accounts/account%2F1/model-tests");
  assert.equal(requests[0]?.method, "POST");
  assert.deepEqual(requests[0]?.body, { modelId: "Org/Model-A" });
});

test("custom config PUT sends one Endpoint, protocol, and capability list with CAS tokens", async () => {
  setupControlPlane(9);
  const requests = installFetchMock(() => ({ account: customAccount("custom-1") }));

  await dashboardApi.updateAccountCustomConfig("custom-1", {
    endpoint_url: "http://192.168.1.10:8080/v1/messages",
    upstream_protocol: "messages",
    model_capabilities: [{ public_model: "model-a", upstream_model: "provider/model-a", protocol: "messages", source: "manual" }],
  });

  assert.equal(requests[0]?.url, "/dashboard/api/v3/accounts/custom-1/custom-config");
  assert.equal(requests[0]?.method, "PUT");
  assert.deepEqual(requests[0]?.body, {
    endpointUrl: "http://192.168.1.10:8080/v1/messages",
    upstreamProtocol: "messages",
    modelCapabilities: [{ publicModel: "model-a", upstreamModel: "provider/model-a", protocol: "messages", source: "manual" }],
    expectedRevision: 9,
    processGeneration: 99,
  });
});

test("model capabilities PUT wraps the list and keeps exact model IDs and order", async () => {
  setupControlPlane(10);
  const requests = installFetchMock(() => ({ account: customAccount("custom-1") }));

  await dashboardApi.updateAccountModelCapabilities("custom-1", [
    { public_model: "Org/Model-B", upstream_model: "vendor/model-b", protocol: "chat_completions", source: "manual" },
    { public_model: "custom_model.a", upstream_model: "vendor/model-b", protocol: "chat_completions", source: "manual" },
  ]);

  assert.equal(requests[0]?.url, "/dashboard/api/v3/accounts/custom-1/model-capabilities");
  assert.equal(requests[0]?.method, "PUT");
  assert.deepEqual(requests[0]?.body, {
    capabilities: [
      { publicModel: "Org/Model-B", upstreamModel: "vendor/model-b", protocol: "chat_completions", source: "manual" },
      { publicModel: "custom_model.a", upstreamModel: "vendor/model-b", protocol: "chat_completions", source: "manual" },
    ],
    expectedRevision: 10,
    processGeneration: 99,
  });
});

test("model discovery posts only the transient form fields to its protected route", async () => {
  setupControlPlane(1);
  const requests = installFetchMock(() => ({ models: ["model-a"], truncated: false }));

  await dashboardApi.discoverCustomModels({
    endpoint_url: "https://api.example.com/v1/messages",
    upstream_protocol: "messages",
    api_key: "new-key",
    account_id: "custom-1",
  });

  assert.equal(requests[0]?.url, "/dashboard/api/v3/custom/models/discover");
  assert.equal(requests[0]?.method, "POST");
  assert.deepEqual(requests[0]?.body, {
    endpointUrl: "https://api.example.com/v1/messages",
    upstreamProtocol: "messages",
    apiKey: "new-key",
    accountId: "custom-1",
  });
});
