import type { ProviderCatalogEntry } from "../api/providers.ts";
import type { ProviderDetailTab } from "../views/app-navigation.ts";

type DetailCapabilities = Pick<ProviderCatalogEntry,
  "provider_id" | "origin" | "editable" | "deletable" | "managed_registration"
>;

export function providerDetailTabs(entry: DetailCapabilities | null): ProviderDetailTab[] {
  const tabs: ProviderDetailTab[] = ["models"];
  if (!entry) return tabs;
  const hasSettings = entry.origin === "builtin"
    ? entry.managed_registration || entry.provider_id === "custom"
    : entry.editable || entry.deletable;
  if (hasSettings) tabs.push("settings");
  return tabs;
}
