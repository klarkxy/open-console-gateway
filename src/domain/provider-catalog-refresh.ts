import type { Account } from "../api/dashboard.ts";
import type { ProviderContractsResponse } from "../api/providers.ts";
import { accountIsReady } from "./account-display.ts";

/** True only for the first ready account in an exact provider-id scope. */
export function isFirstReadyProviderAccount(
  created: Pick<Account, "id" | "provider_id" | "setup_step">,
  existing: readonly Pick<Account, "id" | "provider_id" | "setup_step">[],
): boolean {
  if (!accountIsReady(created)) return false;
  return !existing.some((account) => (
    account.id !== created.id
    && account.provider_id === created.provider_id
    && accountIsReady(account)
  ));
}

/** Existing V3 Provider Contract fields remain the catalog-action authority. */
export function providerContractAllowsCatalogRefresh(
  contracts: ProviderContractsResponse,
  providerId: string,
): boolean {
  const scope = contracts.providers.find((provider) => provider.provider_id === providerId);
  return Boolean(scope && (scope.card.catalog_refresh || scope.catalog.refresh_supported));
}

export function shouldRefreshCatalogForNewProviderAccount(
  created: Pick<Account, "id" | "provider_id" | "setup_step">,
  existing: readonly Pick<Account, "id" | "provider_id" | "setup_step">[],
  contracts: ProviderContractsResponse,
): boolean {
  return isFirstReadyProviderAccount(created, existing)
    && providerContractAllowsCatalogRefresh(contracts, created.provider_id);
}
