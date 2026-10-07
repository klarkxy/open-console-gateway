import assert from "node:assert/strict";
import test from "node:test";
import type { ProviderPageDetail, ProviderModelsPage } from "../api/pages.ts";
import { providerPageQueryKey, providerPageMatrixRows, providerPageScope, providerPageItemStatus, providerPageEditProjection } from "./provider-page.ts";

test("provider query priority preserves destination, connection, and provider bookmarks", () => {
  assert.equal(providerPageQueryKey({ destination: "dest", connection: "connection", provider: "provider" }), "d:dest");
  assert.equal(providerPageQueryKey({ destination: null, connection: "connection", provider: "provider" }), "c:connection");
  assert.equal(providerPageQueryKey({ destination: null, connection: null, provider: "provider" }), "p:provider");
  assert.equal(providerPageQueryKey({ destination: null, connection: null, provider: null }), null);
});
test("model presentation respects server operation facts instead of guessing eligibility", () => {
  const row = { publicModel: "alias", upstreamModel: "upstream", effectiveOn: false, targetProtocol: null,
    testProtocol: null, writableProtocols: ["responses"], actions: [{ key: "toggle", allowed: false }, { key: "test", allowed: false }],
    contract: { modelId: "upstream", preferredProtocol: "responses", alias: "alias",
      protocols: { chat_completions: null, responses: { available: false, enabled: true }, messages: null } } } as unknown as ProviderModelsPage["models"][number];
  const view = providerPageMatrixRows([row], "provider")[0]!;
  assert.equal(view.effectiveOn, false); assert.equal(view.controllable, false); assert.equal(view.testable, false);
  assert.deepEqual(view.unverified, ["responses"]); assert.equal(view.secondary, "upstream");
});
test("a bounded provider scope remains explicitly incomplete and retains global count", () => {
  const detail = { scope: { key: "provider:p", scopeKind: "provider", scopeId: "p", providerId: "p", label: "P",
    staticProtocolSnapshotDate: null, accountCount: 12000, catalog: { source: "static", sourceUrl: "", refreshedAt: null, modelCount: 9000, refreshSupported: false },
    usage: { availability: "unavailable" }, card: { fetchZenModels: false, discoverModels: false, protocolProbe: true, catalogRefresh: false },
    catalogRoutable: false, productionInference: false, disabledReasons: ["unknown"], revision: 7 } } as unknown as ProviderPageDetail;
  const scope = providerPageScope(detail, null)!;
  assert.equal(scope.modelsComplete, false); assert.equal(scope.totalModels, 9000);
  assert.equal(scope.accountsComplete, false); assert.equal(scope.totalAccounts, 12000); assert.deepEqual(scope.accounts, []);
  assert.equal(scope.models.length, 0); assert.equal(scope.production_inference, false);
  assert.deepEqual(scope.disabled_reasons, ["unknown"]);
});
test("provider status keeps draft and missing credentials distinct", () => {
  const draft = { authorization: "missing", lifecycle: "draft", eligibility: { reason: "missing_credential", state: "ineligible" } };
  assert.equal(providerPageItemStatus(draft as unknown as ProviderPageDetail["item"]).kind, "draft");
  assert.equal(providerPageItemStatus({ ...draft, lifecycle: "configured" } as unknown as ProviderPageDetail["item"]).kind, "missing_credential");
});

test("editor scope consumes the captured server projection without rebuilding protocols from a destination", () => {
  const summary = { key: "custom_endpoint:d", scopeKind: "custom_endpoint", scopeId: "d", providerId: "p", label: "P",
    staticProtocolSnapshotDate: null, accountCount: 0, catalog: { source: "static", sourceUrl: "", refreshedAt: null, modelCount: 1, refreshSupported: false },
    usage: { availability: "unavailable" }, card: { fetchZenModels: false, discoverModels: false, protocolProbe: false, catalogRefresh: false },
    catalogRoutable: false, productionInference: false, disabledReasons: ["server_reason"], revision: 7 };
  const value = { scope: { summary, accounts: [], models: [{ publicModel: "alias", upstreamModel: "upstream",
    contract: { modelId: "alias", alias: "alias", preferredProtocol: "responses", routable: false,
      disabledReasons: ["server_reason"], protocols: { chat_completions: null, responses: null, messages: null } } }] },
    destination: null, definition: null, connection: null, catalogEntry: null,
  } as unknown as import("../api/pages.ts").ProviderEditDetail;
  const scope = providerPageEditProjection(value).scope!;
  assert.equal(scope.scope_id, "d");
  assert.equal(scope.modelsComplete, true);
  assert.equal(scope.models[0]?.preferred_protocol, "responses");
  assert.equal(scope.production_inference, false);
  assert.deepEqual(scope.disabled_reasons, ["server_reason"]);
});
