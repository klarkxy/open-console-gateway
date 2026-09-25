import type { ProviderCatalogEntry } from "../api/providers.ts";
import { familyOf, PROVIDER_FAMILIES, type ProviderFamily } from "./provider-families.ts";
import { providerPresetById } from "./provider-presets.ts";

/**
 * Brand families for rail rows and detail headers. Add/preset deep links are
 * mapped in `app-navigation.ts` onto the shared Accounts chooser.
 */

const FAMILIES_BY_ID: ReadonlyMap<string, ProviderFamily> = new Map(
  PROVIDER_FAMILIES.map((family) => [family.id, family]),
);

/** Built-in provider ids whose brand family id differs from the provider id. */
const PROVIDER_FAMILY_ALIASES: Readonly<Record<string, string>> = {
  kimi: "moonshot",
  "opencode-zen-free": "opencode",
};

const FALLBACK_FAMILY_TINT = "#5F6068";

/**
 * Brand for a rail row or detail header. A known vendor family wins (logo or
 * tinted monogram); for preset-derived dynamic rows the persisted `presetId`
 * resolves the preset's family next; everything else gets a neutral monogram
 * carrying the catalog display family. Identity is never inferred from
 * endpoint URLs.
 */
export function catalogEntryFamily(
  entry: Pick<ProviderCatalogEntry, "provider_id" | "display_family" | "display_name">,
  presetId?: string | null,
): ProviderFamily {
  const alias = PROVIDER_FAMILY_ALIASES[entry.provider_id] ?? entry.provider_id;
  const known = FAMILIES_BY_ID.get(alias);
  if (known) return known;
  const preset = providerPresetById(presetId);
  if (preset) return familyOf(preset);
  return {
    id: entry.provider_id,
    label: entry.display_family.trim() || entry.display_name,
    tint: FALLBACK_FAMILY_TINT,
  };
}
