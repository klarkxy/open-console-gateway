import assert from "node:assert/strict";
import test from "node:test";
import type { Destination } from "../api/destinations.ts";
import {
  canEditProviderModels,
  planProviderModelEdit,
  providerModelDraft,
  providerModelEditFingerprint,
  providerModelProtocols,
  type ProviderModelDraft,
  type ProviderModelEditPlan,
} from "./provider-model-edit.ts";

function destination(): Destination {
  return {
    id: "destination:test",
    name: "Existing service",
    adapter: "http",
    legacy: { kind: "dynamic", id: "service-test" },
    auth_scheme: "bearer",
    base_url: "https://example.test/tenant/v1/chat/completions",
    brand_family: null,
    enabled: true,
    max_credentials: null,
    observer_credential_id: null,
    plan: null,
    account_controls: {} as Destination["account_controls"],
    capabilities: {
      billing_tier_required: false,
      discoverable_models: true,
      external_integration: false,
      identity_headers: false,
      managed_signup: false,
      observer: false,
      official_balance_probe: [],
      redirect_policy: "deny" as Destination["capabilities"]["redirect_policy"],
      testable: true,
    },
    protocols: ["chat_completions", "responses"],
    protocol_routes: [
      { protocol: "chat_completions", endpoint_url: "https://example.test/tenant/v1/chat/completions", auth_scheme: "bearer" },
      { protocol: "responses", endpoint_url: "https://example.test/tenant/v1/responses", auth_scheme: "bearer" },
    ],
    catalog: [{
      public_model: "existing",
      upstream_model: "vendor/existing-v2",
      enabled: false,
      protocols: ["responses"],
      preferred: "responses",
      upstream_override: null,
    }],
  };
}

function draft(): ProviderModelDraft {
  return {
    public_model: "my-alias",
    upstream_model: "vendor/model-v1",
    enabled: true,
    protocols: ["responses"],
    preferred: "responses",
  };
}

function saved(plan: ProviderModelEditPlan) {
  assert.equal(plan.kind, "save");
  if (plan.kind !== "save") throw new Error("expected a save plan");
  return plan.input;
}

function invalid(plan: ProviderModelEditPlan, issue: string) {
  assert.deepEqual(plan, { kind: "invalid", issue });
}

 test("add a model with independent public/upstream names in one replacement", () => {
  const source = destination();
  const before = structuredClone(source);
  const input = saved(planProviderModelEdit(source, draft(), null));
  assert.deepEqual(input.models[1], {
    publicModel: "my-alias", upstreamModel: "vendor/model-v1", enabled: true,
    protocols: ["responses"], preferred: "responses", upstreamOverride: null,
  });
  assert.deepEqual(input.models[0], {
    publicModel: "existing", upstreamModel: "vendor/existing-v2", enabled: false,
    protocols: ["responses"], preferred: "responses", upstreamOverride: null,
  });
  assert.deepEqual(source, before);
});

test("empty catalog can receive its first model without discovery", () => {
  const source = destination();
  source.catalog = [];
  assert.equal(saved(planProviderModelEdit(source, draft(), null)).models.length, 1);
});

test("a blank alias falls back to the exact trimmed upstream ID", () => {
  const input = saved(planProviderModelEdit(destination(), {
    ...draft(), public_model: "  ", upstream_model: "  Vendor/Model-free  ",
  }, null));
  assert.equal(input.models[1].publicModel, "Vendor/Model-free");
  assert.equal(input.models[1].upstreamModel, "Vendor/Model-free");
});

test("rename replaces the existing row rather than retaining an old alias", () => {
  const input = saved(planProviderModelEdit(destination(), draft(), "EXISTING"));
  assert.equal(input.models.length, 1);
  assert.equal(input.models[0].publicModel, "my-alias");
  assert.equal(input.models[0].upstreamModel, "vendor/model-v1");
});

test("an unchanged alias can be edited without colliding with itself", () => {
  const input = saved(planProviderModelEdit(destination(), { ...draft(), public_model: "existing" }, "existing"));
  assert.equal(input.models.length, 1);
});

test("duplicate aliases are rejected after trimming and ASCII case folding", () => {
  invalid(planProviderModelEdit(destination(), { ...draft(), public_model: " EXISTING " }, null), "duplicate_public_model");
  invalid(planProviderModelEdit(destination(), { ...draft(), public_model: "", upstream_model: "EXISTING" }, null), "duplicate_public_model");
});

test("different aliases can intentionally point to the same upstream model", () => {
  const input = saved(planProviderModelEdit(destination(), { ...draft(), upstream_model: "vendor/existing-v2" }, null));
  assert.equal(input.models.length, 2);
});

test("blank upstream IDs and removed edit targets fail before persistence", () => {
  invalid(planProviderModelEdit(destination(), { ...draft(), upstream_model: " \n " }, null), "missing_upstream_model");
  invalid(planProviderModelEdit(destination(), draft(), "removed"), "missing_model");
  assert.equal(providerModelDraft(destination(), "removed"), null);
});

test("enabled models require at least one selected protocol", () => {
  invalid(planProviderModelEdit(destination(), { ...draft(), protocols: [], preferred: null }, null), "invalid_protocols");
});

test("disabled models can have no active protocol without being silently enabled", () => {
  const input = saved(planProviderModelEdit(destination(), { ...draft(), enabled: false, protocols: [], preferred: null }, null));
  assert.equal(input.models[1].enabled, false);
  assert.deepEqual(input.models[1].protocols, []);
  assert.equal(input.models[1].preferred, undefined);
});

test("duplicate and unconfigured protocols are rejected", () => {
  invalid(planProviderModelEdit(destination(), { ...draft(), protocols: ["responses", "responses"] }, null), "invalid_protocols");
  invalid(planProviderModelEdit(destination(), { ...draft(), protocols: ["messages"], preferred: "messages" }, null), "invalid_protocols");
});

test("preferred protocol must be among the selected protocols", () => {
  invalid(planProviderModelEdit(destination(), { ...draft(), preferred: "chat_completions" }, null), "invalid_preferred");
  assert.equal(saved(planProviderModelEdit(destination(), { ...draft(), preferred: null }, null)).models[1].preferred, "responses");
});

test("all declared routes and their authentication are preserved, without Key grants", () => {
  const source = destination();
  source.enabled = false;
  const input = saved(planProviderModelEdit(source, draft(), null));
  assert.equal(input.enabled, false);
  assert.equal(input.name, source.name);
  assert.equal(input.endpointUrl, source.base_url);
  assert.equal(input.authScheme, source.auth_scheme);
  assert.deepEqual(input.protocolRoutes, source.protocol_routes!.map((route) => ({
    protocol: route.protocol, endpointUrl: route.endpoint_url, authScheme: route.auth_scheme,
  })));
  assert.deepEqual(input.authorizeCredentialIds, []);
});

test("editing a model preserves its exact endpoint override and restricts protocol choices", () => {
  const source = destination();
  source.catalog[0].upstream_override = { protocol: "messages", endpoint_url: "https://other.example.test/private/messages" };
  source.catalog[0].protocols = ["messages"];
  source.catalog[0].preferred = "messages";
  assert.deepEqual(providerModelProtocols(source, "existing"), ["messages"]);
  const input = saved(planProviderModelEdit(source, { ...draft(), protocols: ["messages"], preferred: "messages" }, "existing"));
  assert.deepEqual(input.models[0].upstreamOverride, { protocol: "messages", endpointUrl: "https://other.example.test/private/messages" });
  invalid(planProviderModelEdit(source, draft(), "existing"), "invalid_protocols");
});

test("legacy single-route providers are not implicitly migrated to explicit routes", () => {
  const source = destination();
  source.protocol_routes = [];
  source.protocols = ["responses"];
  const input = saved(planProviderModelEdit(source, draft(), null));
  assert.equal(input.protocolRoutes, undefined);
  assert.equal(input.endpointUrl, source.base_url);
  assert.equal(input.upstreamProtocol, "responses");
});

test("legacy multi-protocol sets are not silently collapsed by a full PATCH", () => {
  const source = destination();
  source.protocol_routes = [];
  invalid(planProviderModelEdit(source, draft(), null), "unsupported_legacy_routes");
});

test("built-in adapters and managed observers cannot enter the HTTP editor", () => {
  const builtin = { ...destination(), adapter: "opencode_go" as Destination["adapter"] };
  const observer = destination();
  observer.capabilities.observer = true;
  for (const source of [builtin, observer]) {
    assert.equal(canEditProviderModels(source), false);
    invalid(planProviderModelEdit(source, draft(), null), "immutable_destination");
  }
  assert.equal(canEditProviderModels(null), false);
});

test("existing model drafts retain disabled state and do not mutate the store", () => {
  const source = destination();
  const value = providerModelDraft(source, "existing")!;
  assert.equal(value.enabled, false);
  assert.deepEqual(value.protocols, ["responses"]);
  value.protocols.push("chat_completions");
  assert.deepEqual(source.catalog[0].protocols, ["responses"]);
});

test("new drafts choose a single configured route and no synthetic alias", () => {
  const value = providerModelDraft(destination(), null)!;
  assert.equal(value.public_model, "");
  assert.equal(value.upstream_model, "");
  assert.deepEqual(value.protocols, ["chat_completions"]);
  assert.equal(value.preferred, "chat_completions");
});

test("configuration fingerprints invalidate an open editor after catalog or route changes", () => {
  const source = destination();
  const fingerprint = providerModelEditFingerprint(source);
  assert.equal(providerModelEditFingerprint(structuredClone(source)), fingerprint);
  const changed = structuredClone(source);
  changed.catalog[0].enabled = true;
  assert.notEqual(providerModelEditFingerprint(changed), fingerprint);
  changed.catalog = source.catalog;
  changed.protocol_routes![0].endpoint_url = "https://changed.example.test/chat";
  assert.notEqual(providerModelEditFingerprint(changed), fingerprint);
});
