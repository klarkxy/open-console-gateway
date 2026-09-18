import type { Connection, LegacyConnectionKind } from "../api/connections.ts";
import type { Destination, LegacyDestinationRef } from "../api/destinations.ts";

/** Catalog key for OpenCode Go. Not a reserved account id. */
export const DEFAULT_PROVIDER_ID = "opencode";
export const OLLAMA_PROVIDER_ID = "ollama";
export const ZEN_FREE_PROVIDER_ID = "opencode-zen-free";
export const CPA_PROVIDER_ID = "cpa";

const DESTINATION_TO_CONNECTION_KIND: Record<
  Exclude<LegacyDestinationRef["kind"], "platform_parent">,
  LegacyConnectionKind
> = {
  builtin: "builtin_provider",
  dynamic: "dynamic_provider",
  custom_account: "custom_account",
};

function normalizeLegacyId(id: string): string {
  return id.trim().toLocaleLowerCase();
}

/** Join a destination to the V4 connection that still owns mutations. */
export function connectionForDestination(
  connections: readonly Connection[],
  destination: Pick<Destination, "legacy">,
): Connection | undefined {
  if (destination.legacy.kind === "platform_parent") return undefined;
  const kind = DESTINATION_TO_CONNECTION_KIND[destination.legacy.kind];
  const needle = normalizeLegacyId(destination.legacy.id);
  return connections.find((connection) => (
    connection.legacy.kind === kind
    && normalizeLegacyId(connection.legacy.id) === needle
  ));
}

export function destinationOffering(
  destination: Pick<Destination, "plan">,
): "plan" | "api" {
  return destination.plan ? "plan" : "api";
}

export function groupDestinationsByOffering(
  destinations: readonly Destination[],
): { plan: Destination[]; api: Destination[] } {
  return {
    plan: destinations.filter((destination) => destinationOffering(destination) === "plan"),
    api: destinations.filter((destination) => destinationOffering(destination) === "api"),
  };
}

export function filterDestinations(
  destinations: readonly Destination[],
  query: string,
): Destination[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [...destinations];
  return destinations.filter((destination) => (
    destination.name.toLocaleLowerCase().includes(needle)
    || destination.legacy.id.toLocaleLowerCase().includes(needle)
    || (destination.brand_family ?? "").toLocaleLowerCase().includes(needle)
    || (destination.base_url ?? "").toLocaleLowerCase().includes(needle)
  ));
}

/** Rail / URL key: connection id when a join exists, else the destination id. */
export function railKeyForDestination(
  destination: Destination,
  connections: readonly Connection[],
): string {
  return connectionForDestination(connections, destination)?.id ?? destination.id;
}
