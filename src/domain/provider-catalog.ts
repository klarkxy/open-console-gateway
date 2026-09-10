import type { ProviderCatalogEntry } from "../api/providers.ts";
import { PROVIDER_FAMILIES, type ProviderFamily } from "./provider-families.ts";
import { PROVIDER_PRESETS } from "./provider-presets.ts";

/**
 * Presentation logic for the Providers page: the rail lists saved connections
 * grouped by offering. Unused built-in templates stay off the rail; persisted
 * preset/custom rows stay visible even with no Key. The add flow is a small
 * browse → form state machine mirrored in the URL.
 */

const FAMILIES_BY_ID: ReadonlyMap<string, ProviderFamily> = new Map(
  PROVIDER_FAMILIES.map((family) => [family.id, family]),
);

/** Built-in provider ids whose brand family id differs from the provider id. */
const PROVIDER_FAMILY_ALIASES: Readonly<Record<string, string>> = {
  kimi: "moonshot",
};

const FALLBACK_FAMILY_TINT = "#5F6068";

/**
 * Brand for a rail row or detail header. A known vendor family wins (logo or
 * tinted monogram); everything else gets a neutral monogram carrying the
 * catalog display family. Identity is never inferred from endpoint URLs.
 */
export function catalogEntryFamily(
  entry: Pick<ProviderCatalogEntry, "provider_id" | "display_family" | "display_name">,
): ProviderFamily {
  const alias = PROVIDER_FAMILY_ALIASES[entry.provider_id] ?? entry.provider_id;
  const known = FAMILIES_BY_ID.get(alias);
  if (known) return known;
  return {
    id: entry.provider_id,
    label: entry.display_family.trim() || entry.display_name,
    tint: FALLBACK_FAMILY_TINT,
  };
}

/**
 * Offering groups for the rail. Catalog order is preserved within each group:
 * built-in seeds keep their curated order and dynamic Providers follow by
 * creation time, exactly as the backend emits them.
 */
export function groupCatalogEntriesByOffering(
  entries: readonly ProviderCatalogEntry[],
): { plan: ProviderCatalogEntry[]; api: ProviderCatalogEntry[] } {
  return {
    plan: entries.filter((entry) => entry.offering === "plan"),
    api: entries.filter((entry) => entry.offering !== "plan"),
  };
}

function normalizeProviderId(id: string): string {
  return id.trim().toLocaleLowerCase();
}

/**
 * Rail rows: hide unused built-in templates; keep every persisted preset or
 * custom connection, and any built-in that already has an account. Catalog
 * order is preserved.
 */
export function railCatalogEntries(
  entries: readonly ProviderCatalogEntry[],
  accountProviderIds: readonly string[],
): ProviderCatalogEntry[] {
  const connected = new Set(
    accountProviderIds.map(normalizeProviderId).filter(Boolean),
  );
  return entries.filter((entry) => (
    entry.origin !== "builtin"
    || connected.has(normalizeProviderId(entry.provider_id))
  ));
}

export type CatalogEntryCredentialState = "not_required" | "missing" | "present";

/**
 * Credential marker from persisted facts only: `credential_kind` and the
 * account count the view already computed. No health or probe inference.
 */
export function catalogEntryCredentialState(
  entry: Pick<ProviderCatalogEntry, "credential_kind">,
  accountCount: number,
): CatalogEntryCredentialState {
  if (entry.credential_kind === "none") return "not_required";
  if (accountCount === 0) return "missing";
  return "present";
}

/** Account counts keyed by the same normalized provider id the rail uses. */
export function accountCountByProviderId(
  accounts: readonly { provider_id: string }[],
): Map<string, number> {
  const counts = new Map<string, number>();
  for (const account of accounts) {
    const id = normalizeProviderId(account.provider_id);
    if (!id) continue;
    counts.set(id, (counts.get(id) ?? 0) + 1);
  }
  return counts;
}

/** Case-insensitive rail filter over the display name and the provider id. */
export function filterCatalogEntries(
  entries: readonly ProviderCatalogEntry[],
  query: string,
): ProviderCatalogEntry[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [...entries];
  return entries.filter((entry) => (
    entry.display_name.toLocaleLowerCase().includes(needle)
    || entry.provider_id.toLocaleLowerCase().includes(needle)
  ));
}

/** Query value representing the manual (preset-less) embedded form. */
export const MANUAL_PRESET_QUERY_VALUE = "manual";

export type ProviderAddStage =
  | { stage: "browse" }
  | { stage: "form"; presetId: string | null };

/**
 * Add-flow stage from URL parameters. An unknown preset id degrades to the
 * preset browser instead of a blank form.
 */
export function providerAddStageFromQuery(
  add: boolean,
  preset: string | null,
): ProviderAddStage | null {
  if (!add) return null;
  if (!preset) return { stage: "browse" };
  if (preset === MANUAL_PRESET_QUERY_VALUE) return { stage: "form", presetId: null };
  if (PROVIDER_PRESETS.some((entry) => entry.id === preset)) {
    return { stage: "form", presetId: preset };
  }
  return { stage: "browse" };
}

/** URL parameters mirroring an add-flow stage (see {@link providerAddStageFromQuery}). */
export function providerAddStageToQuery(
  stage: ProviderAddStage,
): { add: boolean; preset?: string } {
  if (stage.stage === "browse") return { add: true };
  return { add: true, preset: stage.presetId ?? MANUAL_PRESET_QUERY_VALUE };
}
