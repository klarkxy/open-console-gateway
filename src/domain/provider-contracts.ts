import type { Account } from "../api/dashboard.ts";
import type {
  CardCapabilitySummary,
  CapabilitySummary,
  ContractScopeKind,
  CustomEndpointContract,
  EffectiveCatalog,
  EffectiveModelContract,
  ModelProtocolOverrideUpdate,
  ProviderAccountChoice,
  ProviderCatalogEntry,
  ProviderContractsResponse,
  ProviderProtocol,
} from "../api/providers.ts";
import {
  findCatalogEntry,
} from "./plans.ts";

export const PROVIDER_PROTOCOLS: readonly ProviderProtocol[] = [
  "chat_completions",
  "responses",
  "messages",
];

export function modelProtocolOverrideKey(
  scopeKind: ContractScopeKind,
  scopeId: string,
  modelId: string,
  protocol: ProviderProtocol,
): string {
  return JSON.stringify([scopeKind, scopeId, modelId, protocol]);
}

/**
 * The single protocol a built-in row connection test submits: the preferred
 * protocol when its effective state is enabled, otherwise the first enabled
 * fallback. Null when nothing is enabled — no blind multi-protocol scan.
 */
export function effectiveModelTestProtocol(
  model: Pick<EffectiveModelContract, "preferred_protocol" | "protocols"> | undefined,
): ProviderProtocol | null {
  if (!model) return null;
  if (model.protocols[model.preferred_protocol]?.enabled) return model.preferred_protocol;
  for (const protocol of PROVIDER_PROTOCOLS) {
    if (model.protocols[protocol]?.enabled) return protocol;
  }
  return null;
}

export const CATALOG_SOURCE_STATIC = "static";
export const CATALOG_SOURCE_OFFICIAL_ZEN = "official_zen";
export const CATALOG_SOURCE_CUSTOM_DISCOVERY = "custom_discovery";
export const CATALOG_SOURCE_DECLARED = "account_declared";
export const CATALOG_SOURCE_OPENCODE_MODELS = "opencode_get_models";
export const CATALOG_SOURCE_COMMAND_CODE_MODELS = "command_code_get_models";

export interface ProviderScopeRef {
  scope_kind: ContractScopeKind;
  scope_id: string;
}

export interface ProviderScopeView {
  key: string;
  scope_kind: ContractScopeKind;
  scope_id: string;
  provider_id: string;
  static_protocol_snapshot_date: string | null;
  label: string;
  accounts: ProviderAccountChoice[];
  catalog: EffectiveCatalog;
  models: ProviderModelContract[];
  pricing: CapabilitySummary;
  usage: CapabilitySummary;
  card: CardCapabilitySummary;
  catalog_routable: boolean;
  production_inference: boolean;
  disabled_reasons: string[];
  revision: number;
}

/** Provider rows may publish a stable client Alias alongside their raw upstream id. */
export type ProviderModelContract = EffectiveModelContract & {
  alias?: string;
};

export function providerScopeKey(scopeKind: string, scopeId: string): string {
  return `${scopeKind}:${scopeId}`;
}

/** Match an account to the backend-owned exact contract scope. */
export function findAccountScopeView(
  scopes: readonly ProviderScopeView[],
  account: Pick<Account, "id" | "provider_id">,
): ProviderScopeView | undefined {
  if (account.provider_id === "custom") {
    return scopes.find((scope) => (
      scope.scope_kind === "custom_endpoint" && scope.scope_id === account.id
    ));
  }
  return scopes.find((scope) => (
    scope.scope_kind === "provider"
    && scope.provider_id === account.provider_id
  ));
}

/** Protocols the contract marks available for this model. */
export function modelAvailableProtocols(
  model: ProviderModelContract,
): ProviderProtocol[] {
  return PROVIDER_PROTOCOLS.filter((protocol) => (
    model.protocols[protocol]?.available === true
  ));
}

/**
 * The protocol whose row switch should read ON: the enabled preferred
 * protocol, then the first enabled fallback. When any protocol is actually
 * enabled the UI must never present the row as off, so this always wins over
 * available-but-disabled evidence.
 */
function modelEnabledTarget(model: ProviderModelContract): ProviderProtocol | null {
  const preferred = model.preferred_protocol;
  if (preferred && model.protocols[preferred]?.enabled) return preferred;
  for (const protocol of PROVIDER_PROTOCOLS) {
    if (model.protocols[protocol]?.enabled) return protocol;
  }
  return null;
}

/**
 * The protocol shown while the row is off: the available preferred protocol,
 * then the first available fallback. Null when the model has no protocol
 * evidence at all (the row stays disabled).
 */
function modelAvailableTarget(model: ProviderModelContract): ProviderProtocol | null {
  const preferred = model.preferred_protocol;
  if (preferred && model.protocols[preferred]?.available) return preferred;
  for (const protocol of PROVIDER_PROTOCOLS) {
    if (model.protocols[protocol]?.available) return protocol;
  }
  return null;
}

export function modelTargetProtocol(
  model: ProviderModelContract,
  _scope: ProviderScopeView,
): ProviderProtocol | null {
  const enabled = modelEnabledTarget(model);
  if (enabled) return enabled;
  if (model.preferred_protocol && model.protocols[model.preferred_protocol]?.available) {
    return model.preferred_protocol;
  }
  return modelAvailableTarget(model);
}

/**
 * Whether any protocol is enabled. The allow-routing switch binds to this.
 */
export function modelEffectiveOn(
  model: ProviderModelContract,
  _scope?: ProviderScopeView,
): boolean {
  return PROVIDER_PROTOCOLS.some((protocol) => model.protocols[protocol]?.enabled === true);
}

/**
 * The protocols an override batch may legally write for one model. Built-in
 * provider scopes accept exactly the model's non-null protocol evidence rows
 * (a CN model's Responses slot is null and absent, so it is never written);
 * Custom endpoint contracts retain all three rows but only the declared
 * (available) ones are writable. The backend validator rejects anything
 * outside this ceiling — even `force_off` — so batches must never exceed it.
 * Filtering is by writability, never by enabled state, so a fully disabled
 * row stays re-enableable.
 */
function modelWritableProtocols(
  model: ProviderModelContract,
  scope: ProviderScopeView,
): ProviderProtocol[] {
  return PROVIDER_PROTOCOLS.filter((protocol) => (
    scope.scope_kind === "custom_endpoint"
      ? model.protocols[protocol]?.available === true
      : model.protocols[protocol] !== undefined
  ));
}

/**
 * Build the override batch that toggles each model in `modelIds` fully on or
 * fully off, touching only the model's legal writable protocols (see
 * {@link modelWritableProtocols}).
 *
 * on=true force-enables every available protocol and never force-disables
 * siblings — never `auto` on the enabled set, because under `auto` GOAT
 * extras stay off. on=false force-disables every writable protocol and
 * stamps `preferred: true` on the current preferred so the choice survives.
 */
export function buildModelToggleOverrides(
  scope: ProviderScopeView,
  modelIds: readonly string[],
  on: boolean,
): ModelProtocolOverrideUpdate[] {
  const overrides: ModelProtocolOverrideUpdate[] = [];
  for (const modelId of modelIds) {
    const model = scope.models.find((entry) => entry.model_id === modelId);
    if (!model) continue;
    const writable = modelWritableProtocols(model, scope);
    if (writable.length === 0) continue;
    if (!on) {
      const preferred = scope.scope_kind === "provider"
        ? modelPreferredStamp(model, writable)
        : null;
      for (const protocol of writable) {
        overrides.push({
          model_id: modelId,
          protocol,
          state: "force_off",
          ...(preferred === protocol ? { preferred: true } : {}),
        });
      }
      continue;
    }
    const available = modelAvailableProtocols(model).filter((protocol) => (
      writable.includes(protocol)
    ));
    if (available.length === 0) continue;
    for (const protocol of available) {
      overrides.push({
        model_id: modelId,
        protocol,
        state: "force_on",
      });
    }
  }
  return overrides;
}

function modelPreferredStamp(
  model: ProviderModelContract,
  writable: readonly ProviderProtocol[],
): ProviderProtocol | null {
  if (model.preferred_protocol && writable.includes(model.preferred_protocol)) {
    return model.preferred_protocol;
  }
  return modelAvailableTarget(model);
}

/**
 * Persist the conversion default. When the row is on, the chosen protocol is
 * force-enabled and siblings are left alone. When the row is off, every
 * writable protocol stays force_off and only the choice is stored.
 */
export function buildPreferredProtocolOverrides(
  scope: ProviderScopeView,
  modelId: string,
  protocol: ProviderProtocol,
): ModelProtocolOverrideUpdate[] {
  const model = scope.models.find((entry) => entry.model_id === modelId);
  if (!model) return [];
  const available = modelAvailableProtocols(model);
  if (!available.includes(protocol)) return [];
  const writable = modelWritableProtocols(model, scope);
  if (!writable.includes(protocol)) return [];
  if (scope.scope_kind !== "provider") return [];
  if (modelEffectiveOn(model, scope)) {
    return [{
      model_id: modelId,
      protocol,
      state: "force_on",
      preferred: true,
    }];
  }
  return writable.map((choice) => ({
    model_id: modelId,
    protocol: choice,
    state: "force_off" as const,
    ...(choice === protocol ? { preferred: true } : {}),
  }));
}

export function protocolDisplayName(protocol: ProviderProtocol): string {
  if (protocol === "chat_completions") return "Chat Completions";
  if (protocol === "responses") return "Responses";
  return "Messages";
}

export function isSafeSourceUrl(value: string): boolean {
  const trimmed = value.trim();
  if (!trimmed) return false;
  try {
    const url = new URL(trimmed);
    if (url.protocol !== "https:" && url.protocol !== "http:") return false;
    if (url.username || url.password) return false;
    return Boolean(url.hostname);
  } catch {
    return false;
  }
}

export function normalizeProviderContractsResponse(
  raw: ProviderContractsResponse | null | undefined,
): ProviderContractsResponse {
  if (raw == null) {
    throw new Error("provider contracts response is missing");
  }
  return raw;
}

function providerLabel(
  providerId: string,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): string {
  return findCatalogEntry(catalog, providerId)?.display_name.trim() || providerId;
}

function customEndpointLabel(
  endpoint: CustomEndpointContract,
  catalog: readonly ProviderCatalogEntry[] | null | undefined,
): string {
  const name = endpoint.account.name.trim();
  if (name) return name;
  return findCatalogEntry(catalog, "custom")?.display_name.trim() || endpoint.scope_id;
}

export function flattenProviderScopes(
  response: ProviderContractsResponse,
  catalog: readonly ProviderCatalogEntry[] | null | undefined = null,
): ProviderScopeView[] {
  const providers = response.providers.map((group) => ({
    key: providerScopeKey(group.scope_kind, group.scope_id),
    scope_kind: group.scope_kind,
    scope_id: group.scope_id,
    provider_id: group.provider_id,
    static_protocol_snapshot_date: group.static_protocol_snapshot_date,
    label: providerLabel(group.provider_id, catalog),
    accounts: group.accounts,
    catalog: group.catalog,
    models: group.models,
    pricing: group.pricing,
    usage: group.usage,
    card: group.card,
    catalog_routable: group.catalog_routable,
    production_inference: group.production_inference,
    disabled_reasons: group.disabled_reasons,
    revision: group.revision,
  }));
  const custom = response.custom_endpoints.map((endpoint) => ({
    key: providerScopeKey(endpoint.scope_kind, endpoint.scope_id),
    scope_kind: endpoint.scope_kind,
    scope_id: endpoint.scope_id,
    provider_id: endpoint.provider_id,
    static_protocol_snapshot_date: null,
    label: customEndpointLabel(endpoint, catalog),
    accounts: [endpoint.account],
    catalog: endpoint.catalog,
    models: endpoint.models,
    pricing: endpoint.pricing,
    usage: endpoint.usage,
    card: endpoint.card,
    catalog_routable: endpoint.catalog_routable,
    production_inference: endpoint.production_inference,
    disabled_reasons: endpoint.disabled_reasons,
    revision: endpoint.revision,
  }));
  return [...providers, ...custom];
}

export function catalogRefreshSupported(scope: Pick<ProviderScopeView, "card" | "catalog">): boolean {
  return scope.card.catalog_refresh || scope.catalog.refresh_supported;
}

function mergeModelContract(
  models: readonly ProviderModelContract[],
  next: ProviderModelContract,
): ProviderModelContract[] {
  const index = models.findIndex((model) => model.model_id === next.model_id);
  if (index < 0) return [...models, next];
  return models.map((model, itemIndex) => (itemIndex === index ? next : model));
}

export function applyModelContractToResponse(
  response: ProviderContractsResponse,
  scope: ProviderScopeRef,
  contract: EffectiveModelContract,
): ProviderContractsResponse {
  const normalized = normalizeProviderContractsResponse(response);
  if (scope.scope_kind === "custom_endpoint") {
    return {
      ...normalized,
      custom_endpoints: normalized.custom_endpoints.map((endpoint) => (
        endpoint.scope_id === scope.scope_id
          ? { ...endpoint, models: mergeModelContract(endpoint.models, contract) }
          : endpoint
      )),
    };
  }
  return {
    ...normalized,
    providers: normalized.providers.map((group) => (
      group.scope_id === scope.scope_id
        ? { ...group, models: mergeModelContract(group.models, contract) }
        : group
    )),
  };
}
