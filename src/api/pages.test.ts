import assert from "node:assert/strict";
import test from "node:test";
import { pagesApi } from "./pages.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { installFetchMock, setupControlPlane, v3AccountDto } from "../test-helpers/dashboard-v3-fetch.ts";

const revision = { revision: 7, processGeneration: 99 };

test("account page reads retain totals and map filter labels without fetching detail", async () => {
  setupControlPlane();
  const calls = installFetchMock(() => ({
    revision, readVersion: "read-1", asOf: "2026-10-07T00:00:00Z", validUntil: null,
    totalCards: 20, totalCredentials: 100, matchedCards: 2, matchedCredentials: 12,
    cards: [], offset: 0, limit: 10, hasMore: true, errors: [],
    planOptions: [{ value: "custom", label: "Custom API", cardCount: 2, credentialCount: 12 }],
    routingMode: "strict_priority", conversationSticky: true,
  }));
  const value = await pagesApi.accounts({ search: "a&b/模型", plan: "custom", limit: 10 });
  assert.equal(calls.length, 1);
  const query = new URL(calls[0]!.url, "http://localhost").searchParams;
  assert.equal(query.get("search"), "a&b/模型");
  assert.equal(query.get("plan"), "custom");
  assert.equal(query.get("limit"), "10");
  assert.equal(value.matchedCredentials, 12);
  assert.equal(value.hasMore, true);
  assert.deepEqual(value.planFilters, [{ providerId: "custom", label: "Custom API", count: 12 }]);
});

test("lazy account detail maps complete account models and preserves absent related resources", async () => {
  setupControlPlane();
  const calls = installFetchMock(() => ({
    revision,
    account: v3AccountDto("a/b", {
      providerId: "custom",
      modelCapabilities: [{ publicModel: "Public/Model", protocol: "responses", upstreamModel: "upstream-v1", source: "operator", verifiedAt: null }],
    }),
    destination: null, credential: null, identity: null, connection: null, platform: null, platformLink: null,
    operations: { rotate: false, binding: false, create: false, unsupportedReason: "missing_identity",
      credentialId: null, bindingId: null, identityId: null, allowedConnections: [], shareTargets: [], testModels: [],
      grantedEndpointIds: [], staleEndpointIds: [], staleOrigins: [] },
  }));
  const value = await pagesApi.accountDetail("a/b");
  assert.equal(calls.length, 1);
  assert.ok(calls[0]!.url.includes("a%2Fb"));
  assert.equal(value.account.provider_id, "custom");
  assert.equal(value.account.model_capabilities[0]!.public_model, "Public/Model");
  assert.equal(value.account.model_capabilities[0]!.upstream_model, "upstream-v1");
  assert.equal(value.destination, null);
  assert.equal(value.identity, null);
});

test("account refresh submits explicit mode and CAS without replaying the operation", async () => {
  setupControlPlane();
  const calls = installFetchMock(() => ({
    revision: { revision: 8, processGeneration: 99 }, outcome: "refreshed", billing: null,
    account: v3AccountDto("account", { revision: 8 }),
    refresh: { supported: true, observedAt: null, freshUntil: null, nextAllowedAt: null },
  }));
  const value = await pagesApi.refreshAccount("account", "manual", { expectedRevision: 7, processGeneration: 99 });
  assert.equal(calls.length, 1);
  assert.equal(calls[0]!.method, "POST");
  assert.deepEqual(calls[0]!.body, { mode: "manual", expectedRevision: 7, processGeneration: 99 });
  assert.equal(value.outcome, "refreshed");
  assert.equal(value.account?.setup_step, "ready");
  assert.equal(useControlPlaneStore().revision, 8);
});

test("provider model query encodes the complete rail identity and locates the exact model", async () => {
  setupControlPlane();
  const calls = installFetchMock(() => ({ revision, readVersion: "read-1", total: 250, filteredTotal: 1, allDisabled: false, models: [], offset: 0, limit: 50, hasMore: false }));
  const value = await pagesApi.providerModels("d:provider/a", { model: "Model/Case", enabledOnly: false, search: "a&b" });
  assert.equal(calls.length, 1);
  assert.ok(calls[0]!.url.includes("d%3Aprovider%2Fa"));
  const query = new URL(calls[0]!.url, "http://localhost").searchParams;
  assert.equal(query.get("model"), "Model/Case");
  assert.equal(query.get("enabledOnly"), "false");
  assert.equal(query.get("search"), "a&b");
  assert.equal(value.total, 250);
  assert.equal(value.filteredTotal, 1);
});

test("alias page keeps global overlap, publication and partial-source failures", async () => {
  setupControlPlane();
  const payload = {
    revision, readVersion: "read-1", asOf: "2026-10-07T00:00:00Z", validUntil: null,
    totalGroups: 20, totalRows: 1000, filteredGroups: 1, filteredRows: 200,
    groups: [{ publicModel: "Case/Model", publicationKey: "case/model", published: false, totalRows: 200, matchingRows: 200, hasOverlap: true, continued: true, rows: [] }],
    offset: 50, limit: 50, hasMore: true,
    errors: [{ resource: "capabilities", id: "provider", code: "unavailable" }],
  };
  const calls = installFetchMock(() => payload);
  const value = await pagesApi.aliases({ search: "Case/Model", offset: 50, limit: 50 });
  assert.equal(calls.length, 1);
  assert.equal(value.groups[0]!.hasOverlap, true);
  assert.equal(value.groups[0]!.published, false);
  assert.equal(value.groups[0]!.publicModel, "Case/Model");
  assert.equal(value.totalRows, 1000);
  assert.deepEqual(value.errors, payload.errors);
});
