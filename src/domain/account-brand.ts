import type { Account } from "../api/dashboard.ts";
import type { Destination } from "../api/destinations.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { PlatformKind } from "../api/platform-accounts.ts";
import { PLATFORM_KIND_LABELS } from "./platform-accounts.ts";
import { findCatalogEntry } from "./plans.ts";
import { catalogEntryFamily } from "./provider-catalog.ts";
import { PROVIDER_FAMILIES, type ProviderFamily } from "./provider-families.ts";

/**
 * Brand mark for one account-list card. Every card kind — built-in Plan,
 * user-defined Provider, Custom API, the CPA/Zen singletons, and New API /
 * Sub2API platform sites — resolves to one `ProviderFamily` so the list uses
 * a single visual for "what type is this". Known vendors keep their logo or
 * tinted monogram; anything else gets a neutral monogram carrying the
 * catalog display family or the platform kind label. Identity is never
 * inferred from endpoint URLs or account names.
 */

const NEUTRAL_TINT = "#5F6068";

export function accountBrandFamily(
  account: Pick<Account, "provider_id">,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): ProviderFamily {
  const entry = findCatalogEntry(catalog, account.provider_id);
  if (entry) return catalogEntryFamily(entry);
  return { id: account.provider_id, label: account.provider_id, tint: NEUTRAL_TINT };
}

export function platformBrandFamily(kind: PlatformKind): ProviderFamily {
  return { id: `platform:${kind}`, label: PLATFORM_KIND_LABELS[kind], tint: NEUTRAL_TINT };
}

/** Brand mark for a destination card. Prefers the live catalog row when a
 *  credential is present so vendor logos stay; otherwise uses destination
 *  `brand_family` / name. */
export function destinationBrandFamily(
  destination: Pick<Destination, "id" | "name" | "brand_family" | "adapter">,
  account: Pick<Account, "provider_id"> | null | undefined,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): ProviderFamily {
  if (account) return accountBrandFamily(account, catalog);
  if (destination.brand_family) {
    const known = PROVIDER_FAMILIES.find((family) => (
      family.label === destination.brand_family || family.id === destination.adapter
    ));
    if (known) return known;
    return { id: destination.adapter, label: destination.brand_family, tint: NEUTRAL_TINT };
  }
  return { id: destination.id, label: destination.name, tint: NEUTRAL_TINT };
}
