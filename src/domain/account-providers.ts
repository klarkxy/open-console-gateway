import type { Account } from "../api/dashboard";

/**
 * Built-in provider registry. The backend owns the DTO fields
 * (`provider_id`, `credential_kind`, `quota_scope`); this module only
 * holds the frontend's static knowledge of the built-in providers so
 * forms and cards can branch without inventing new endpoints.
 */

/** Existing and migrated accounts default to OpenCode Go. */
export const DEFAULT_PROVIDER_ID = "opencode";

export const OLLAMA_PROVIDER_ID = "ollama";

/** Built-in singleton Zen Free account; created and owned by the backend. */
export const ZEN_FREE_ACCOUNT_ID = "00000000-0000-0000-0000-000000000002";
export const ZEN_FREE_PROVIDER_ID = "opencode-zen-free";

/** Static external-integration singleton; it is routable but not a Provider Plan. */
const CPA_ACCOUNT_ID = "00000000-0000-0000-0000-000000000003";
export const CPA_PROVIDER_ID = "cpa";

export function isZenFreeAccount(
  account: Pick<Account, "id" | "provider_id">,
): boolean {
  return account.id === ZEN_FREE_ACCOUNT_ID
    || account.provider_id === ZEN_FREE_PROVIDER_ID;
}

export function isCpaIntegrationAccount(
  account: Pick<Account, "id" | "provider_id">,
): boolean {
  return account.id === CPA_ACCOUNT_ID
    || account.provider_id === CPA_PROVIDER_ID;
}

/** Sealed Ollama Cloud Key accounts; billing is account-scoped, not Cookie scrape. */
export function isOllamaCloudAccount(
  account: Pick<Account, "provider_id">,
): boolean {
  return account.provider_id === OLLAMA_PROVIDER_ID;
}
