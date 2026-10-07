import type { ProviderPageItem, ProviderPageDetail, ProviderModelsPage, ProviderEditDetail } from "../api/pages.ts";
import { presentCatalogEntry, presentModel, presentProviderDefinition, presentAccountChoice } from "../api/providers.ts";
import { presentConnection, type Connection } from "../api/connections.ts";
import { presentDestination } from "../api/destinations.ts";
import { catalogEntryFamily } from "./provider-catalog.ts";
import { connectionStatus } from "./connections.ts";
import { PROVIDER_PROTOCOLS, type ProviderScopeView } from "./provider-contracts.ts";
import type { ProviderProtocol } from "../api/providers.ts";

export function providerPageQueryKey(query: { destination: string | null; connection: string | null; provider: string | null }): string | null {
  if (query.destination) return `d:${query.destination}`;
  if (query.connection) return `c:${query.connection}`;
  return query.provider ? `p:${query.provider}` : null;
}

export function providerPageItemStatus(item: ProviderPageItem) {
  return connectionStatus({ authorization: item.authorization, lifecycle: item.lifecycle, eligibility: item.eligibility });
}

export function providerPageBrand(item: ProviderPageItem | null, entry: Pick<ReturnType<typeof presentCatalogEntry>, "provider_id" | "display_family" | "display_name"> | null) {
  return catalogEntryFamily(entry ?? { provider_id: item?.providerId ?? "", display_family: item?.brandFamily ?? "", display_name: item?.name ?? "" }, item?.presetId);
}

/** Header-only connection presentation, explicitly omitting its complete target inventory. */
export function providerPageConnection(detail: ProviderPageDetail) {
  const item = detail.item;
  if (!item.connectionId) return null;
  return {
    id: item.connectionId, name: item.name, lifecycle: item.lifecycle, authorization: item.authorization,
    eligibility: item.eligibility, legacy: item.legacy, credential_count: item.credentialCount,
    enabled_credential_count: item.enabledCredentialCount, target_count: item.catalogCount,
    endpoints: detail.endpoints.map(endpoint => ({
      id: endpoint.id, connection_id: endpoint.connectionId, auth_scheme: endpoint.authScheme,
      locked: endpoint.locked, operation: endpoint.operation, url: endpoint.url, wire_protocol: endpoint.wireProtocol,
    })),
  } satisfies Pick<Connection, "id" | "name" | "lifecycle" | "authorization" | "eligibility" | "legacy" | "credential_count" | "enabled_credential_count" | "target_count" | "endpoints">;
}

/** This is a table presentation aggregate, never an authoritative full contract. */
export interface ProviderPageScope extends ProviderScopeView {
  modelsComplete: false; totalModels: number; accountsComplete: false; totalAccounts: number;
}
export function providerPageScope(detail: ProviderPageDetail | null, page: ProviderModelsPage | null): ProviderPageScope | null {
  const scope = detail?.scope;
  if (!scope) return null;
  const models = (page?.models ?? []).map(row => ({
    ...presentModel(row.contract), alias: row.publicModel, secondary: row.upstreamModel,
  }));
  return {
    modelsComplete: false, totalModels: scope.catalog.modelCount, accountsComplete: false, totalAccounts: scope.accountCount,
    key: scope.key, scope_kind: scope.scopeKind, scope_id: scope.scopeId, provider_id: scope.providerId,
    static_protocol_snapshot_date: scope.staticProtocolSnapshotDate, label: scope.label,
    accounts: [],
    catalog: { source: scope.catalog.source, source_url: scope.catalog.sourceUrl, refreshed_at: scope.catalog.refreshedAt,
      refresh_supported: scope.catalog.refreshSupported, models: models.map(model => model.model_id) },
    models, usage: { availability: scope.usage.availability },
    card: { fetch_zen_models: scope.card.fetchZenModels, discover_models: scope.card.discoverModels,
      protocol_probe: scope.card.protocolProbe, catalog_refresh: scope.card.catalogRefresh },
    catalog_routable: scope.catalogRoutable, production_inference: scope.productionInference,
    disabled_reasons: scope.disabledReasons, revision: scope.revision,
  };
}

export function providerPageAction(detail: ProviderPageDetail | null, key: string): boolean {
  return detail?.actions.some(action => action.key === key && action.allowed) ?? false;
}

export interface ProviderPageMatrixRow {
  modelId: string; alias: string; secondary: string;
  chips: ProviderProtocol[]; preferred: ProviderProtocol | null;
  protocolOn: Partial<Record<ProviderProtocol, boolean>>;
  effectiveOn: boolean; controllable: boolean; unverified: ProviderProtocol[];
  testable: boolean; editable: boolean; metadataEditable: boolean;
}

/** Only presentation fields are derived; operation eligibility is server-owned. */
export function providerPageMatrixRows(rows: ProviderModelsPage["models"], scopeKind: string): ProviderPageMatrixRow[] {
  return rows.map(row => {
    const chips = PROVIDER_PROTOCOLS.filter(protocol => row.contract.protocols[protocol]?.available);
    const allowed = (key: string) => row.actions.some(action => action.key === key && action.allowed);
    return {
      modelId: row.contract.modelId, alias: row.publicModel,
      secondary: row.upstreamModel !== row.publicModel ? row.upstreamModel : "",
      chips, preferred: row.contract.preferredProtocol,
      protocolOn: Object.fromEntries(PROVIDER_PROTOCOLS.map(protocol => [protocol, row.contract.protocols[protocol]?.enabled === true])),
      effectiveOn: row.effectiveOn, controllable: allowed("toggle") && row.targetProtocol !== null,
      unverified: scopeKind === "provider" && chips.length === 0
        ? row.writableProtocols.filter(protocol => PROVIDER_PROTOCOLS.includes(protocol)) : [],
      testable: allowed("test"), editable: allowed("modelEditable"), metadataEditable: allowed("metadataEditable"),
    };
  });
}

/** Complete selected scope is only materialized for explicit operations. */
export function providerPageEditProjection(value: ProviderEditDetail) {
  const entry = value.catalogEntry ? presentCatalogEntry(value.catalogEntry) : null;
  const destination = value.destination ? presentDestination(value.destination) : null;
  const projected = value.scope ? providerPageScope({ scope: value.scope.summary } as ProviderPageDetail,
    { models: value.scope.models } as ProviderModelsPage) : null;
  return {
    catalogEntry: entry,
    destination,
    definition: value.definition ? presentProviderDefinition(value.definition) : null,
    connection: value.connection ? presentConnection(value.connection) : null,
    scope: projected ? { ...projected, modelsComplete: true, accountsComplete: true,
      accounts: value.scope!.accounts.map(presentAccountChoice) } : null,
  };
}
