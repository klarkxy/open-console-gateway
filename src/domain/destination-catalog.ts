import type { DestinationCatalogModelUpdate } from "../api/dashboard-v4.ts";
import type {
  Destination,
  DestinationCatalogModel,
  DestinationCredential,
  DestinationProtocolRoute,
} from "../api/destinations.ts";
import type {
  ContractEvidenceSource,
  EffectiveProtocolEvidence,
  ModelProtocolOverrideUpdate,
  ProviderProtocol,
} from "../api/providers.ts";
import {
  CATALOG_SOURCE_STATIC,
  PROVIDER_PROTOCOLS,
  providerScopeKey,
  type ProviderModelContract,
  type ProviderScopeView,
} from "./provider-contracts.ts";

export const MAX_HTTP_PROTOCOL_ROUTES = 3;

export interface DestinationCatalogProjectionOptions {
  /** Catalog source label. Configuration is never a successful probe claim. */
  source?: "preset" | "static";
  source_url?: string;
  revision?: number;
}

function destinationProviderId(destination: Pick<Destination, "legacy">): string {
  return destination.legacy.kind === "custom_account" ? "custom" : destination.legacy.id;
}

/** Explicit routes when present; otherwise one legacy route per destination.protocols member. */
export function destinationConfiguredRoutes(
  destination: Pick<Destination, "auth_scheme" | "base_url" | "protocol_routes" | "protocols">,
): DestinationProtocolRoute[] {
  const explicit = destination.protocol_routes ?? [];
  if (explicit.length > 0) return explicit.map((route) => ({ ...route }));
  return destination.protocols.map((protocol) => ({
    protocol,
    endpoint_url: destination.base_url ?? "",
    auth_scheme: destination.auth_scheme,
  }));
}

export function destinationConfiguredProtocols(
  destination: Pick<Destination, "auth_scheme" | "base_url" | "protocol_routes" | "protocols">,
): ProviderProtocol[] {
  const seen = new Set<ProviderProtocol>();
  const protocols: ProviderProtocol[] = [];
  for (const route of destinationConfiguredRoutes(destination)) {
    if (!PROVIDER_PROTOCOLS.includes(route.protocol)) continue;
    if (seen.has(route.protocol)) continue;
    seen.add(route.protocol);
    protocols.push(route.protocol);
  }
  return protocols;
}

function modelAvailableProtocolsFor(
  destination: Pick<Destination, "auth_scheme" | "base_url" | "protocol_routes" | "protocols">,
  model: DestinationCatalogModel,
): ProviderProtocol[] {
  if (model.upstream_override) {
    const protocol = model.upstream_override.protocol;
    return PROVIDER_PROTOCOLS.includes(protocol) ? [protocol] : [];
  }
  return destinationConfiguredProtocols(destination);
}

function emptyProtocolEvidence(
  protocol: ProviderProtocol,
  source: ContractEvidenceSource,
): EffectiveProtocolEvidence {
  return {
    protocol,
    available: false,
    enabled: false,
    source,
    verified_at: null,
    observed_at: null,
    last_probe_result: null,
    last_probe_at: null,
    last_probe_error: null,
    override: "auto",
  };
}

function projectModel(
  destination: Destination,
  model: DestinationCatalogModel,
  source: ContractEvidenceSource,
): ProviderModelContract {
  const available = modelAvailableProtocolsFor(destination, model);
  const enabledDeclared = new Set(model.protocols);
  const protocols: Record<string, EffectiveProtocolEvidence> = {};
  for (const protocol of PROVIDER_PROTOCOLS) {
    const isAvailable = available.includes(protocol);
    if (!isAvailable && destination.legacy.kind !== "custom_account" && destination.legacy.kind !== "dynamic") {
      continue;
    }
    if (!isAvailable) {
      // Custom endpoint contracts keep three slots; only available ones are writable.
      protocols[protocol] = emptyProtocolEvidence(protocol, source);
      continue;
    }
    const enabled = model.enabled && enabledDeclared.has(protocol);
    protocols[protocol] = {
      protocol,
      available: true,
      enabled,
      source,
      verified_at: null,
      observed_at: null,
      last_probe_result: null,
      last_probe_at: null,
      last_probe_error: null,
      override: "auto",
    };
  }
  const preferred = (
    model.preferred && available.includes(model.preferred)
      ? model.preferred
      : available[0] ?? model.preferred ?? "chat_completions"
  );
  return {
    alias: model.public_model,
    secondary: model.upstream_model,
    model_id: model.public_model,
    preferred_protocol: preferred,
    protocols,
    routable: model.enabled && available.some((protocol) => enabledDeclared.has(protocol)),
    disabled_reasons: model.enabled ? [] : ["model_disabled"],
  };
}

/**
 * Pure HTTP destination → ProviderScopeView projection. Scope identity is the
 * destination id; upstream names stay visible on alias/secondary.
 */
export function projectDestinationCatalog(
  destination: Destination,
  options: DestinationCatalogProjectionOptions = {},
): ProviderScopeView {
  const source: ContractEvidenceSource = options.source === "preset" ? "preset" : "static";
  const catalogSource = options.source === "preset" ? "preset" : CATALOG_SOURCE_STATIC;
  const models = destination.catalog.map((model) => projectModel(destination, model, source));
  const refreshSupported = destination.capabilities.discoverable_models;
  return {
    key: providerScopeKey("custom_endpoint", destination.id),
    scope_kind: "custom_endpoint",
    scope_id: destination.id,
    provider_id: destinationProviderId(destination),
    static_protocol_snapshot_date: null,
    label: destination.name,
    accounts: [],
    catalog: {
      source: catalogSource,
      source_url: options.source_url ?? "",
      refreshed_at: null,
      models: models.map((model) => model.model_id),
      refresh_supported: refreshSupported,
    },
    models,
    pricing: { availability: "not_applicable" },
    usage: { availability: "not_applicable" },
    card: {
      fetch_zen_models: false,
      discover_models: refreshSupported,
      protocol_probe: destination.capabilities.testable,
      catalog_refresh: refreshSupported,
    },
    catalog_routable: destination.enabled,
    production_inference: destination.enabled,
    disabled_reasons: destination.enabled ? [] : ["destination_disabled"],
    revision: options.revision ?? 0,
  };
}

function currentEnabledSet(
  model: DestinationCatalogModel,
  available: readonly ProviderProtocol[],
): Set<ProviderProtocol> {
  const enabled = new Set<ProviderProtocol>();
  if (!model.enabled) return enabled;
  for (const protocol of model.protocols) {
    if (available.includes(protocol)) enabled.add(protocol);
  }
  return enabled;
}

/**
 * Translate a matrix override batch into native catalog updates. Missing
 * models are skipped; protocol identity is the public model name.
 */
export function catalogUpdatesFromOverrides(
  destination: Destination,
  overrides: readonly ModelProtocolOverrideUpdate[],
): { updates: DestinationCatalogModelUpdate[] } {
  const grouped = new Map<string, ModelProtocolOverrideUpdate[]>();
  for (const override of overrides) {
    const list = grouped.get(override.model_id) ?? [];
    list.push(override);
    grouped.set(override.model_id, list);
  }
  const updates: DestinationCatalogModelUpdate[] = [];
  for (const [publicModel, rows] of grouped) {
    const model = destination.catalog.find((entry) => entry.public_model === publicModel);
    if (!model) continue;
    const available = modelAvailableProtocolsFor(destination, model);
    const enabledSet = currentEnabledSet(model, available);
    let preferred = model.preferred && available.includes(model.preferred)
      ? model.preferred
      : available[0] ?? model.preferred ?? undefined;
    for (const row of rows) {
      if (!available.includes(row.protocol)) continue;
      if (row.state === "force_on") enabledSet.add(row.protocol);
      if (row.state === "force_off") enabledSet.delete(row.protocol);
      if (row.preferred) preferred = row.protocol;
    }
    const protocols = available.filter((protocol) => enabledSet.has(protocol));
    if (protocols.length > 0 && (!preferred || !protocols.includes(preferred))) {
      preferred = protocols[0];
    }
    updates.push({
      publicModel,
      enabled: protocols.length > 0,
      protocols,
      ...(preferred ? { preferred } : {}),
    });
  }
  return { updates };
}

/**
 * Identity of the HTTP route and Keys a probe observed. Preference or token
 * changes must not keep showing that receipt.
 */
export function destinationProbeIdentity(
  destination: Pick<Destination, "id" | "auth_scheme" | "base_url" | "protocol_routes" | "protocols"> & Partial<Pick<Destination, "catalog">>,
  credentials: readonly (Pick<DestinationCredential, "auth_state" | "destination_id" | "has_secret" | "id" | "last_error"> & Partial<Pick<DestinationCredential, "grants" | "scope" | "enabled">>)[],
  protocol: string,
  publicModel?: string,
): string {
  const routes = destinationConfiguredRoutes(destination)
    .map((route) => `${route.protocol}\n${route.endpoint_url}\n${route.auth_scheme}`)
    .join("|");
  const keys = credentials
    .filter((row) => row.destination_id === destination.id)
    .map((row) => JSON.stringify([row.id, row.auth_state, row.has_secret, row.last_error, row.grants, row.scope, row.enabled]))
    .sort()
    .join(",");
  const models = destination.catalog?.filter((model) => !publicModel || model.public_model === publicModel);
  return JSON.stringify([destination.id, protocol, routes, keys, models]);
}
