import assert from "node:assert/strict";
import test from "node:test";

import type { Account } from "../api/dashboard.ts";
import type { ProviderCatalogEntry, ProviderContractsResponse } from "../api/providers.ts";
import { accountTestModels, filterAccountTestModels } from "./account-model-test.ts";

const account = {
  id: "account-1",
  provider_id: "provider-1",
} as Account;

const contracts = {
  revision: 1,
  providers: [{
    scope_kind: "provider",
    scope_id: "provider-1",
    provider_id: "provider-1",
    static_protocol_snapshot_date: null,
    accounts: [{
      id: "account-1",
      name: "Account One",
      enabled: true,
      verification_status: "not_required",
    }],
    catalog: { source: "test", source_url: "", refreshed_at: null, models: [], refresh_supported: false },
    models: [
      { alias: "Beta", model_id: "provider/beta", preferred_protocol: "messages", protocols: {}, routable: true, disabled_reasons: [] },
      { alias: "alpha", model_id: "provider/alpha", preferred_protocol: "chat_completions", protocols: {}, routable: true, disabled_reasons: [] },
      { alias: "off", model_id: "provider/off", preferred_protocol: "responses", protocols: {}, routable: false, disabled_reasons: ["off"] },
    ],
    pricing: { availability: "unpriced" },
    usage: { availability: "unavailable" },
    card: { fetch_zen_models: false, discover_models: false, protocol_probe: false, catalog_refresh: false },
    catalog_routable: true,
    production_inference: true,
    disabled_reasons: [],
    revision: 1,
  }],
  custom_endpoints: [],
} satisfies ProviderContractsResponse;

test("account tests use only routable models from the exact account scope", () => {
  assert.deepEqual(accountTestModels(account, contracts), [
    { modelId: "provider/alpha", alias: "alpha", protocol: "chat_completions" },
    { modelId: "provider/beta", alias: "Beta", protocol: "messages" },
  ]);
  assert.deepEqual(accountTestModels({ ...account, provider_id: "other" }, contracts), []);
});

test("account model filtering matches raw ids and aliases without changing order", () => {
  const models = accountTestModels(account, contracts);
  assert.deepEqual(filterAccountTestModels(models, "BETA"), [models[1]]);
  assert.deepEqual(filterAccountTestModels(models, "provider/a"), [models[0]]);
  assert.deepEqual(filterAccountTestModels(models, ""), models);
});

function dynamicCatalog(
  extra: Partial<ProviderCatalogEntry> = {},
): ProviderCatalogEntry {
  return {
    provider_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    origin: "preset",
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
    quota_unit: "none",
    model_source: "dynamic_provider",
    auth_schemes: ["bearer"],
    upstream_protocols: ["messages"],
    form_fields: [],
    model_aliases: ["lab-opus", "lab-beta", "lab-opus"],
    ...extra,
  };
}

const dynamicAccount = {
  id: "dyn-1",
  provider_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
} as Account;

const expectedDynamicModels = [
  { modelId: "lab-beta", alias: "lab-beta", protocol: "messages" },
  { modelId: "lab-opus", alias: "lab-opus", protocol: "messages" },
] as const;

test("dynamic catalog aliases fill the test list only when no exact contract scope exists", () => {
  const emptyContracts = { revision: 1, providers: [], custom_endpoints: [] } satisfies ProviderContractsResponse;
  const catalog = [dynamicCatalog()];
  assert.deepEqual(accountTestModels(dynamicAccount, emptyContracts, catalog), expectedDynamicModels);
  assert.deepEqual(
    accountTestModels(dynamicAccount, contracts, catalog),
    expectedDynamicModels,
    "catalog fallback still applies when contracts have no exact account scope",
  );
  assert.deepEqual(accountTestModels(account, contracts, catalog), [
    { modelId: "provider/alpha", alias: "alpha", protocol: "chat_completions" },
    { modelId: "provider/beta", alias: "Beta", protocol: "messages" },
  ]);
});

test("dynamic catalog aliases are used when contracts are unavailable", () => {
  const catalog = [dynamicCatalog()];
  assert.deepEqual(accountTestModels(dynamicAccount, null, catalog), expectedDynamicModels);
  assert.deepEqual(accountTestModels(dynamicAccount, undefined, catalog), expectedDynamicModels);
  assert.deepEqual(accountTestModels(account, null, catalog), []);
});

test("dynamic catalog aliases fail closed unless there is exactly one upstream protocol", () => {
  const catalog = [dynamicCatalog()];
  assert.deepEqual(
    accountTestModels(dynamicAccount, null, [dynamicCatalog({ upstream_protocols: [] })]),
    [],
  );
  assert.deepEqual(
    accountTestModels(dynamicAccount, null, [dynamicCatalog({
      upstream_protocols: ["messages", "chat_completions"],
    })]),
    [],
  );
  assert.deepEqual(accountTestModels(dynamicAccount, null, catalog), expectedDynamicModels);
});
