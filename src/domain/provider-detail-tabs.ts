import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { ProviderDetailTab } from "../views/app-navigation.ts";

type DetailCapabilities = Pick<ProviderCatalogEntry,
  "provider_id" | "origin" | "editable" | "deletable" | "managed_registration"
  | "pricing_availability" | "model_source"
>;

export function providerDetailTabs(entry: DetailCapabilities | null): ProviderDetailTab[] {
  const tabs: ProviderDetailTab[] = ["models"];
  if (!entry) return tabs;
  if (entry.pricing_availability === "available" || entry.model_source === "official_api_preset") {
    tabs.push("pricing");
  }
  const hasSettings = entry.origin === "builtin"
    ? entry.managed_registration || entry.provider_id === "custom"
    : entry.editable || entry.deletable;
  if (hasSettings) tabs.push("settings");
  return tabs;
}
