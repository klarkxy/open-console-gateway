import type {
  AccountModelCapability,
  AccountModelCapabilityInput,
  AccountProtocol,
} from "../api/dashboard.ts";
import type { Destination, DestinationCatalogModel } from "../api/destinations.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { isCustomApiAccount } from "./custom-account.ts";
import { findPlanDefinition } from "./plans.ts";

/**
 * How **Refresh quota** should also refresh models for this card, so Accounts
 * does not send the operator to Providers to hunt for 刷新模型目录.
 */
export type UsageCompanionCatalog =
  | { kind: "provider_catalog"; providerId: string }
  | { kind: "http_destination" }
  | { kind: "http_account" }
  | { kind: "none" };

export function usageCompanionCatalog(input: {
  providerId: string;
  catalog: readonly ProviderCatalogEntry[] | null;
  destination: Pick<Destination, "adapter" | "capabilities" | "legacy"> | null;
}): UsageCompanionCatalog {
  if (input.destination?.legacy.kind === "platform_parent") return { kind: "none" };
  const surface = findPlanDefinition(input.providerId, input.catalog);
  if (
    surface
    && !surface.dynamic
    && surface.kind !== "custom"
    && surface.usage_availability === "available"
  ) {
    return { kind: "provider_catalog", providerId: input.providerId };
  }
  if (
    input.destination?.legacy.kind === "dynamic"
    && input.destination.capabilities.discoverable_models
  ) {
    return { kind: "http_destination" };
  }
  if (
    isCustomApiAccount({ provider_id: input.providerId })
    || input.destination?.legacy.kind === "custom_account"
    || input.destination?.capabilities.discoverable_models
  ) {
    return { kind: "http_account" };
  }
  return { kind: "none" };
}

export function usageCompanionCatalogLockKey(
  companion: UsageCompanionCatalog,
  accountId: string,
  destinationId: string | null,
): string | null {
  if (companion.kind === "none") return null;
  if (companion.kind === "provider_catalog") return `provider:${companion.providerId}`;
  if (companion.kind === "http_destination") return `destination:${destinationId ?? accountId}`;
  return `account:${accountId}`;
}

/** Append newly discovered public ids; keep enablement of existing rows. */
export function mergeDiscoveredCatalogModels(
  catalog: readonly DestinationCatalogModel[],
  discoveredIds: readonly string[],
  protocol: DestinationCatalogModel["protocols"][number],
): { catalog: DestinationCatalogModel[]; added: number } {
  const seen = new Set(
    catalog.map((row) => row.public_model.trim().toLocaleLowerCase()).filter(Boolean),
  );
  const extra: DestinationCatalogModel[] = [];
  for (const id of discoveredIds) {
    const trimmed = id.trim();
    if (!trimmed) continue;
    const key = trimmed.toLocaleLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    extra.push({
      enabled: true,
      preferred: null,
      protocols: [protocol],
      public_model: trimmed,
      upstream_model: trimmed,
      upstream_override: null,
    });
  }
  return { catalog: extra.length === 0 ? [...catalog] : [...catalog, ...extra], added: extra.length };
}

export function mergeDiscoveredAccountCapabilities(
  existing: readonly AccountModelCapability[],
  discoveredIds: readonly string[],
  protocol: AccountProtocol,
): { capabilities: AccountModelCapabilityInput[]; added: number } {
  const capabilities: AccountModelCapabilityInput[] = existing.map((row) => ({
    public_model: row.public_model,
    upstream_model: row.upstream_model,
    protocol: row.protocol,
    source: row.source,
  }));
  const seen = new Set(
    capabilities.map((row) => row.public_model.trim().toLocaleLowerCase()).filter(Boolean),
  );
  let added = 0;
  for (const id of discoveredIds) {
    const trimmed = id.trim();
    if (!trimmed) continue;
    const key = trimmed.toLocaleLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    capabilities.push({
      public_model: trimmed,
      upstream_model: trimmed,
      protocol,
      source: "discovery",
    });
    added += 1;
  }
  return { capabilities, added };
}
