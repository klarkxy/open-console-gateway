import assert from "node:assert/strict";
import test from "node:test";
import type { ProviderContractsResponse } from "../api/providers.ts";
import {
  isFirstReadyProviderAccount,
  providerContractAllowsCatalogRefresh,
  shouldRefreshCatalogForNewProviderAccount,
} from "./provider-catalog-refresh.ts";

function account(id: string, providerId: string, setupStep: "ready" | "google_account" = "ready") {
  return { id, provider_id: providerId, setup_step: setupStep };
}

function contracts(providerId: string, supported: boolean): ProviderContractsResponse {
  return {
    revision: 1,
    process_generation: 1,
    providers: [{
      scope_kind: "provider",
      scope_id: providerId,
      provider_id: providerId,
      static_protocol_snapshot_date: null,
      accounts: [],
      catalog: {
        source: "test",
        source_url: "",
        refreshed_at: null,
        models: [],
        refresh_supported: supported,
      },
      models: [],
      pricing: { availability: "available" },
      usage: { availability: "available" },
      card: {
        fetch_zen_models: false,
        discover_models: false,
        protocol_probe: false,
        catalog_refresh: supported,
      },
      catalog_routable: true,
      production_inference: true,
      disabled_reasons: [],
      revision: 1,
    }],
    custom_endpoints: [],
  };
}

test("refresh capability comes from the provider contract, including invented providers", () => {
  assert.equal(providerContractAllowsCatalogRefresh(contracts("future-plan", true), "future-plan"), true);
  assert.equal(providerContractAllowsCatalogRefresh(contracts("future-plan", false), "future-plan"), false);
  assert.equal(providerContractAllowsCatalogRefresh(contracts("future-plan", true), "missing"), false);
});

test("only the first ready account with refresh capability triggers", () => {
  const created = account("future-1", "future-plan");
  assert.equal(isFirstReadyProviderAccount(created, [created]), true);
  assert.equal(shouldRefreshCatalogForNewProviderAccount(created, [created], contracts("future-plan", true)), true);
  assert.equal(shouldRefreshCatalogForNewProviderAccount(created, [created], contracts("future-plan", false)), false);
  assert.equal(
    shouldRefreshCatalogForNewProviderAccount(
      account("future-2", "future-plan"),
      [created, account("future-2", "future-plan")],
      contracts("future-plan", true),
    ),
    false,
  );
  assert.equal(
    shouldRefreshCatalogForNewProviderAccount(
      account("draft", "future-plan", "google_account"),
      [],
      contracts("future-plan", true),
    ),
    false,
  );
});
