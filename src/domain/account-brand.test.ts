import assert from "node:assert/strict";
import test from "node:test";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { accountBrandFamily, destinationBrandFamily, platformBrandFamily } from "./account-brand.ts";
import { PLATFORM_KIND_LABELS } from "./platform-accounts.ts";

function catalogEntry(
  provider_id: string,
  extra: Partial<ProviderCatalogEntry> = {},
): ProviderCatalogEntry {
  return {
    provider_id,
    kind: "sealed",
    origin: "builtin",
    version: 1,
    dynamic: false,
    deletable: false,
    offering: "plan",
    display_name: provider_id,
    display_family: provider_id,
    credential_kind: "api_key",
    quota_scope: "account",
    singleton: false,
    managed_registration: false,
    pricing_availability: "unavailable",
    usage_availability: "unavailable",
    manual_usage_calibration: false,
    quota_unit: "credits",
    model_source: "test",
    auth_schemes: ["bearer"],
    upstream_protocols: ["chat_completions"],
    form_fields: [],
    model_aliases: [],
    ...extra,
  } as ProviderCatalogEntry;
}

test("account brand follows the catalog entry family, with vendor aliases", () => {
  const catalog = [
    catalogEntry("kimi", { display_family: "Kimi" }),
    catalogEntry("custom", { display_family: "Custom API", display_name: "Custom API" }),
  ];
  assert.equal(accountBrandFamily({ provider_id: "kimi" }, catalog).id, "moonshot");
  const custom = accountBrandFamily({ provider_id: "custom" }, catalog);
  assert.equal(custom.id, "custom");
  assert.equal(custom.label, "Custom API");
});

test("account brand degrades to a neutral monogram when the catalog is missing", () => {
  const missing = accountBrandFamily({ provider_id: "cpa" }, null);
  assert.equal(missing.id, "cpa");
  assert.equal(missing.label, "cpa");
  assert.match(missing.tint, /^#[0-9A-Fa-f]{6}$/);
  const unlisted = accountBrandFamily({ provider_id: "my-lab" }, [catalogEntry("kimi")]);
  assert.equal(unlisted.id, "my-lab");
});

test("platform brands are keyed by kind and never collide with provider ids", () => {
  const newApi = platformBrandFamily("new_api");
  assert.equal(newApi.id, "platform:new_api");
  assert.equal(newApi.label, PLATFORM_KIND_LABELS.new_api);
  assert.notEqual(platformBrandFamily("sub2api").id, newApi.id);
});

test("destination brand prefers the catalog row, then brand_family, then name", () => {
  const catalog = [catalogEntry("minimax", { display_family: "MiniMax" })];
  const fromAccount = destinationBrandFamily(
    { id: "d1", name: "MiniMax CN", brand_family: "MiniMax", adapter: "minimax" },
    { provider_id: "minimax" },
    catalog,
  );
  assert.equal(fromAccount.id, "minimax");
  const known = destinationBrandFamily(
    { id: "d2", name: "MiniMax CN", brand_family: "MiniMax", adapter: "minimax" },
    null,
    null,
  );
  assert.equal(known.id, "minimax");
  const named = destinationBrandFamily(
    { id: "d3", name: "lab.example", brand_family: null, adapter: "http" },
    null,
    null,
  );
  assert.equal(named.id, "d3");
  assert.equal(named.label, "lab.example");
});
