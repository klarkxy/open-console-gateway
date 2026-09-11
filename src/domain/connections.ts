import type { Connection } from "../api/connections.ts";
import type { ProviderCatalogEntry } from "../api/providers.ts";
import { catalogEntryFamily } from "./provider-catalog.ts";
import type { ProviderFamily } from "./provider-families.ts";

/**
 * Presentation helpers for the V4 connection projection. Status labels are
 * derived only from authorization / lifecycle / eligibility — never from
 * inferred upstream health.
 */

export type ConnectionStatusKind =
  | "missing_credential"
  | "disabled"
  | "invalid"
  | "no_target"
  | "cooling"
  | "ok";

export const CONNECTION_STATUS_LABELS = {
  missing_credential: "待补充凭据",
  disabled: "已停用",
  invalid: "凭据无效",
  no_target: "无可用模型",
  cooling: "冷却中",
} as const;

export type ConnectionStatusLabel = typeof CONNECTION_STATUS_LABELS[Exclude<ConnectionStatusKind, "ok">];

export interface ConnectionStatus {
  kind: ConnectionStatusKind;
  label: ConnectionStatusLabel | null;
}

export function groupConnectionsByOffering(
  connections: readonly Connection[],
): { plan: Connection[]; api: Connection[] } {
  return {
    plan: connections.filter((connection) => connection.offering === "plan"),
    api: connections.filter((connection) => connection.offering !== "plan"),
  };
}

export function filterConnections(
  connections: readonly Connection[],
  query: string,
): Connection[] {
  const needle = query.trim().toLocaleLowerCase();
  if (!needle) return [...connections];
  return connections.filter((connection) => (
    connection.name.toLocaleLowerCase().includes(needle)
    || connection.legacy.id.toLocaleLowerCase().includes(needle)
    || (connection.display_family ?? "").toLocaleLowerCase().includes(needle)
  ));
}

/**
 * Local eligibility projection only. `unknown` authorization is not a badge
 * and is never treated as verified success.
 */
export function connectionStatus(
  connection: Pick<Connection, "authorization" | "lifecycle" | "eligibility">,
): ConnectionStatus {
  const reason = connection.eligibility.reason;
  if (
    connection.lifecycle === "disabled"
    || reason === "connection_disabled"
    || reason === "all_credentials_disabled"
  ) {
    return { kind: "disabled", label: CONNECTION_STATUS_LABELS.disabled };
  }
  if (connection.eligibility.state === "cooling" || reason === "cooling") {
    return { kind: "cooling", label: CONNECTION_STATUS_LABELS.cooling };
  }
  if (connection.authorization === "invalid" || reason === "all_credentials_invalid") {
    return { kind: "invalid", label: CONNECTION_STATUS_LABELS.invalid };
  }
  if (connection.authorization === "missing" || reason === "missing_credential") {
    return { kind: "missing_credential", label: CONNECTION_STATUS_LABELS.missing_credential };
  }
  if (reason === "no_enabled_target") {
    return { kind: "no_target", label: CONNECTION_STATUS_LABELS.no_target };
  }
  return { kind: "ok", label: null };
}

function normalizeLegacyId(id: string): string {
  return id.trim().toLocaleLowerCase();
}

export function connectionForLegacyProvider(
  connections: readonly Connection[],
  providerId: string,
): Connection | undefined {
  const needle = normalizeLegacyId(providerId);
  if (!needle) return undefined;
  return connections.find((connection) => (
    (connection.legacy.kind === "builtin_provider" || connection.legacy.kind === "dynamic_provider")
    && normalizeLegacyId(connection.legacy.id) === needle
  ));
}

export function catalogEntryForConnection(
  connection: Pick<Connection, "legacy">,
  catalog: readonly ProviderCatalogEntry[],
): ProviderCatalogEntry | null {
  if (connection.legacy.kind === "custom_account") return null;
  const needle = normalizeLegacyId(connection.legacy.id);
  return catalog.find((entry) => normalizeLegacyId(entry.provider_id) === needle) ?? null;
}

const CUSTOM_ACCOUNT_FAMILY_ID = "custom";

export function connectionBrandFamily(
  connection: Pick<Connection, "legacy" | "display_family" | "name">,
  catalog: readonly ProviderCatalogEntry[],
): ProviderFamily {
  const entry = catalogEntryForConnection(connection, catalog);
  if (entry) return catalogEntryFamily(entry);
  return {
    id: CUSTOM_ACCOUNT_FAMILY_ID,
    label: connection.display_family?.trim() || "Custom",
    tint: "#5F6068",
  };
}

export interface ConnectionPageQuery {
  connection: string | null;
  provider: string | null;
}

/**
 * Resolve the selected connection id. `connection=` wins when it matches a
 * row; a legacy `provider=` bookmark maps onto the builtin/dynamic row whose
 * `legacy.id` matches.
 */
export function selectedConnectionIdFromQuery(
  query: ConnectionPageQuery,
  connections: readonly Connection[],
): string | null {
  if (query.connection) {
    const match = connections.find((connection) => connection.id === query.connection);
    if (match) return match.id;
  }
  if (query.provider) {
    return connectionForLegacyProvider(connections, query.provider)?.id ?? null;
  }
  return null;
}

export function connectionPageWrite(connectionId: string | null): { connection?: string } {
  return connectionId ? { connection: connectionId } : {};
}
