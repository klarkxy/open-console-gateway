import { dashboardV3, isRevisionConflict, type WithoutExpectation } from "./dashboard-v3.ts";
import { dashboardV4 } from "./dashboard-v4.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import type {
  AccountCredentialKind,
  AccountQuotaScope,
  ProviderDefinition as V3ProviderDefinition,
  ProviderDefinitionDiscoverResponse as V3ProviderDefinitionDiscoverResponse,
  ProviderDefinitionMutation as V3ProviderDefinitionMutation,
  ProviderDefinitionTestResponse as V3ProviderDefinitionTestResponse,
  ModelProtocolOverridesUpdate,
  ProviderCatalogEntry as V3ProviderCatalogEntry,
  ProviderContracts as V3ProviderContracts,
  ProviderPricing as V3ProviderPricing,
  ProviderPricingSnapshot as V3ProviderPricingSnapshot,
  ProviderUsage as V3ProviderUsage,
  ProtocolOverrideState as V3ProtocolOverrideState,
  ProtocolProbeResponse as V3ProtocolProbeResponse,
  MutationExpectation,
} from "./generated/dashboard-v3.ts";
import { presentAccount, presentPricing, type Account, type AccountProtocol, type PricingSnapshot } from "./dashboard-presenters.ts";

export { isRevisionConflict };

/**
 * Typed wrappers for the provider-scoped dashboard endpoints. These live
 * outside the page layer so provider catalog/pricing/usage/settings calls share
 * the `http.ts` transport without growing the legacy account surface; Zen
 * provider settings must go through `updateProviderSettings`, never the
 * generic account PATCH.
 */

export interface ProviderCatalogFormField {
  id: string;
  kind: "text" | "secret" | "date" | "url" | "select" | "models";
  required: boolean;
  immutable_after_create: boolean;
}

export interface ProviderCatalogEntry {
  provider_id: string;
  /** Row provenance in the unified `providers` table: `builtin` | `preset` | `custom`. */
  origin: "builtin" | "preset" | "custom";
  /** Whether the dashboard may PATCH this entry. Always `false` for builtin. */
  editable: boolean;
  /** Whether the dashboard may DELETE this entry. Always `false` for builtin. */
  deletable: boolean;
  /** Plan/api offering label carried by the catalog row. */
  offering: "plan" | "api";
  display_name: string;
  display_family: string;
  credential_kind: AccountCredentialKind;
  quota_scope: AccountQuotaScope;
  singleton: boolean;
  creation_availability: "available" | "unavailable";
  creation_unavailable_reason?: string | null;
  verification_policy: "not_required" | "required";
  verification_runtime_availability: "optional" | "unavailable" | "not_applicable" | "available";
  routable: boolean;
  managed_registration: boolean;
  pricing_availability: "available" | "unavailable" | "not_applicable" | "unpriced";
  usage_availability: "available" | "unavailable" | "local_state";
  manual_usage_calibration: boolean;
  quota_unit: string;
  model_source: string;
  key_prefix?: string | null;
  auth_schemes: ("bearer" | "x-api-key")[];
  upstream_protocols: ("chat_completions" | "responses" | "messages")[];
  form_fields: ProviderCatalogFormField[];
  model_aliases: string[];
}

export type ProviderDefinitionAuthKind = "bearer" | "x-api-key" | "none";

export interface ProviderDefinitionModelView {
  public_model: string;
  upstream_model: string;
  /** Explicit per-model upstream; null inherits the supplier endpoint/protocol. */
  upstream_override: {
    protocol: "chat_completions" | "responses" | "messages";
    endpoint_url: string;
  } | null;
}

export interface ProviderDefinitionView {
  id: string;
  name: string;
  /** Row provenance in the unified `providers` table: `builtin` | `preset` | `custom`. */
  origin: "builtin" | "preset" | "custom";
  /** Plan/api offering label persisted alongside the row. */
  offering: "plan" | "api";
  /** Whether the dashboard may PATCH this row. Always `false` for builtin. */
  editable: boolean;
  /** Whether the dashboard may DELETE this row. Always `false` for builtin. */
  deletable: boolean;
  /** Nullable: builtin rows leave the field empty. */
  endpoint_url: string | null;
  /** Nullable: builtin rows leave the field empty. */
  upstream_protocol: "chat_completions" | "responses" | "messages" | null;
  /** Nullable: builtin rows leave the field empty. */
  auth_kind: ProviderDefinitionAuthKind | null;
  models: ProviderDefinitionModelView[];
  /** Persisted source-template preset ID; null for manual providers. */
  preset_id: string | null;
  created_at: string;
  updated_at: string;
  revision: number;
  process_generation: number;
}

export type ProviderProtocol = AccountProtocol;

export interface StoredProviderPricingValue {
  model_id: string;
  display_name: string;
  input_per_million: number | null;
  output_per_million: number | null;
  cache_read_per_million: number | null;
  cache_write_per_million: number | null;
  plan_limit: number | null;
  model_allowance: number | null;
  quota_multiplier: number | null;
  paid_plan_price: number | null;
  currency: string | null;
}

export interface ProviderNeutralPricingSnapshot {
  revision: string;
  activated_at: string;
  document_updated_at: string | null;
  source_url: string;
  content_hash: string;
  evidence: string;
  values: StoredProviderPricingValue[];
}

export type StoredProviderPricingSnapshot = PricingSnapshot | ProviderNeutralPricingSnapshot | {
  provider_id: string;
  revision: string;
  activated_at: string;
  document_updated_at: string | null;
  source_url: string;
  content_hash: string;
  snapshot_json: string;
};

export interface ProviderPricingResponse {
  provider_id: string;
  availability: "available" | "unavailable" | "not_applicable" | "unpriced";
  snapshot?: StoredProviderPricingSnapshot;
  revision: number;
  process_generation: number;
  pricing_revision: string;
  provider_pricing_revision: string;
}

export interface ProviderQuotaWindow {
  account_id: string;
  window_kind: string;
  used: number;
  limit_value: number | null;
  started_at: string | null;
  resets_at: string | null;
  calibration_offset: number;
  unit: string;
  source: string;
  observed_at: string | null;
  updated_at: string;
}

export interface ProviderCreditBalance {
  account_id: string;
  balance_kind: string;
  amount: number;
  unit: string;
  source: string;
  observed_at: string | null;
  updated_at: string;
}

export interface ProviderUsageSyncState {
  last_success_at: string | null;
  last_attempt_at: string | null;
  next_eligible_at: string | null;
  failure_streak: number;
  last_expedited_at: string | null;
}

export interface ProviderUsageResponse {
  account_id: string;
  provider_id: string;
  availability: string;
  quota_windows: ProviderQuotaWindow[];
  credit_balances: ProviderCreditBalance[];
  sync_state: ProviderUsageSyncState | null;
}

export interface ProviderSettingsUpdate {
  enabled: boolean;
}

export interface ProviderSettingsResponse {
  account: Account;
  revision: number;
}

export type ContractScopeKind = "provider" | "custom_endpoint";
export type ContractEvidenceSource = "static" | "preset" | "probe_confirmed" | "probe_observed";
export type ProbeResultKind = "success" | "failure";
export type ConnectionVerificationStatus = "not_required" | "pending" | "verified" | "failed";
export type ProtocolOverrideState = V3ProtocolOverrideState;

export interface EffectiveCatalog {
  source: string;
  source_url: string;
  refreshed_at: string | null;
  models: string[];
  refresh_supported: boolean;
}

export interface EffectiveProtocolEvidence {
  protocol: ProviderProtocol;
  available: boolean;
  enabled: boolean;
  source: ContractEvidenceSource;
  verified_at: string | null;
  observed_at: string | null;
  last_probe_result: ProbeResultKind | null;
  last_probe_at: string | null;
  last_probe_error: string | null;
  override: ProtocolOverrideState;
}

export interface EffectiveModelContract {
  alias: string;
  model_id: string;
  preferred_protocol: ProviderProtocol;
  protocols: Record<string, EffectiveProtocolEvidence>;
  routable: boolean;
  disabled_reasons: string[];
}

export interface ProviderAccountChoice {
  id: string;
  name: string;
  enabled: boolean;
  verification_status: ConnectionVerificationStatus;
}

export interface CapabilitySummary {
  availability: string;
}

export interface CardCapabilitySummary {
  fetch_zen_models: boolean;
  discover_models: boolean;
  protocol_probe: boolean;
  catalog_refresh: boolean;
}

export interface ProviderContractGroup {
  scope_kind: ContractScopeKind;
  scope_id: string;
  provider_id: string;
  static_protocol_snapshot_date: string | null;
  accounts: ProviderAccountChoice[];
  catalog: EffectiveCatalog;
  models: EffectiveModelContract[];
  pricing: CapabilitySummary;
  usage: CapabilitySummary;
  card: CardCapabilitySummary;
  catalog_routable: boolean;
  production_inference: boolean;
  disabled_reasons: string[];
  revision: number;
}

export interface CustomEndpointContract {
  scope_kind: ContractScopeKind;
  scope_id: string;
  provider_id: string;
  account: ProviderAccountChoice;
  catalog: EffectiveCatalog;
  models: EffectiveModelContract[];
  pricing: CapabilitySummary;
  usage: CapabilitySummary;
  card: CardCapabilitySummary;
  catalog_routable: boolean;
  production_inference: boolean;
  disabled_reasons: string[];
  revision: number;
}

export interface ProviderContractsResponse {
  /** Shared settings revision for PUT `expected_revision`. Distinct from each scope `revision`. */
  revision: number;
  providers: ProviderContractGroup[];
  custom_endpoints: CustomEndpointContract[];
}

export interface ProtocolProbeRequest {
  model_id: string;
  protocols: ProviderProtocol[];
}

export interface ModelProtocolOverrideUpdate {
  model_id: string;
  protocol: ProviderProtocol;
  state: ProtocolOverrideState;
  /**
   * Persist this item's protocol as the model's saved selection, atomically
   * with the override write. Omitted/false preserves the current choice.
   */
  preferred?: boolean;
}

export interface ProtocolProbeResult {
  protocol: ProviderProtocol;
  success: boolean;
  skipped: boolean;
  error: string | null;
}

export interface ProtocolProbeResponse {
  model_id: string;
  results: ProtocolProbeResult[];
  contract: EffectiveModelContract | null;
}

function creationAvailability(value: string): ProviderCatalogEntry["creation_availability"] {
  return value === "available" ? "available" : "unavailable";
}

function verificationPolicy(value: string): ProviderCatalogEntry["verification_policy"] {
  if (value === "required" || value === "not_required") return value;
  throw new Error(`unknown verification policy: ${value}`);
}

function verificationRuntime(value: string): ProviderCatalogEntry["verification_runtime_availability"] {
  if (value === "available" || value === "unavailable" || value === "optional") return value;
  return "not_applicable";
}

function pricingAvailability(value: string): ProviderCatalogEntry["pricing_availability"] {
  if (value === "available" || value === "unavailable" || value === "unpriced") return value;
  return "not_applicable";
}

function usageAvailability(value: string): ProviderCatalogEntry["usage_availability"] {
  if (value === "available" || value === "local_state") return value;
  return "unavailable";
}

function formFieldKind(value: string): ProviderCatalogFormField["kind"] {
  if (value === "text" || value === "secret" || value === "date"
    || value === "url" || value === "select" || value === "models") return value;
  throw new Error(`unknown form field kind: ${value}`);
}

function presentProviderDefinitionMappingOverride(
  model: V3ProviderDefinition["models"][number],
): ProviderDefinitionModelView["upstream_override"] {
  const override = model.upstreamOverride;
  if (!override) return null;
  const protocol = override.protocol;
  const endpointUrl = override.endpointUrl.trim();
  if (!endpointUrl) return null;
  if (protocol !== "chat_completions" && protocol !== "responses" && protocol !== "messages") {
    return null;
  }
  return { protocol, endpoint_url: endpointUrl };
}

function presentProviderDefinition(value: V3ProviderDefinition): ProviderDefinitionView {
  return {
    id: value.id,
    name: value.name,
    origin: value.origin,
    offering: value.offering === "plan" ? "plan" : "api",
    editable: value.editable,
    deletable: value.deletable,
    endpoint_url: value.endpointUrl ?? null,
    upstream_protocol: value.upstreamProtocol ?? null,
    auth_kind: value.authKind ?? null,
    models: value.models.map((model) => ({
      public_model: model.publicModel,
      upstream_model: model.upstreamModel,
      upstream_override: presentProviderDefinitionMappingOverride(model),
    })),
    preset_id: value.presetId ?? null,
    created_at: value.createdAt,
    updated_at: value.updatedAt,
    revision: value.revision,
    process_generation: value.processGeneration,
  };
}

function assertNoSecret(value: object): void {
  const record = value as Record<string, unknown>;
  if ("key" in record || "apiKey" in record) {
    throw new Error("dynamic Provider response must not include a Key");
  }
}

function presentCatalogEntry(value: V3ProviderCatalogEntry): ProviderCatalogEntry {
  return {
    provider_id: value.providerId,
    origin: value.origin,
    editable: value.editable,
    deletable: value.deletable,
    offering: value.offering === "plan" ? "plan" : "api",
    display_name: value.displayName,
    display_family: value.displayFamily,
    credential_kind: value.credentialKind,
    quota_scope: value.quotaScope,
    singleton: value.singleton,
    creation_availability: creationAvailability(value.creationAvailability),
    creation_unavailable_reason: value.creationUnavailableReason,
    verification_policy: verificationPolicy(value.verificationPolicy),
    verification_runtime_availability: verificationRuntime(value.verificationRuntimeAvailability),
    routable: value.routable,
    managed_registration: value.managedRegistration,
    pricing_availability: pricingAvailability(value.pricingAvailability),
    usage_availability: usageAvailability(value.usageAvailability),
    manual_usage_calibration: value.manualUsageCalibration,
    quota_unit: value.quotaUnit,
    model_source: value.modelSource,
    key_prefix: value.keyPrefix,
    auth_schemes: [...value.authSchemes],
    upstream_protocols: [...value.upstreamProtocols],
    form_fields: value.formFields.map((field) => ({
      id: field.id,
      kind: formFieldKind(field.kind),
      required: field.required,
      immutable_after_create: field.immutableAfterCreate,
    })),
    model_aliases: [...value.modelAliases],
  };
}

function presentEvidence(value: V3ProviderContracts["providers"][number]["models"][number]["protocols"]["chat_completions"]): EffectiveProtocolEvidence | undefined {
  if (value === null) return undefined;
  return {
    protocol: value.protocol,
    available: value.available,
    enabled: value.enabled,
    source: value.source,
    verified_at: value.verifiedAt,
    observed_at: value.observedAt,
    last_probe_result: value.lastProbeResult,
    last_probe_at: value.lastProbeAt,
    last_probe_error: value.lastProbeError,
    override: value.override,
  };
}

function presentModel(value: V3ProviderContracts["providers"][number]["models"][number]): EffectiveModelContract {
  const protocols: Record<string, EffectiveProtocolEvidence> = {};
  const chat = presentEvidence(value.protocols.chat_completions);
  const responses = presentEvidence(value.protocols.responses);
  const messages = presentEvidence(value.protocols.messages);
  if (chat) protocols.chat_completions = chat;
  if (responses) protocols.responses = responses;
  if (messages) protocols.messages = messages;
  return {
    alias: value.alias,
    model_id: value.modelId,
    preferred_protocol: value.preferredProtocol,
    protocols,
    routable: value.routable,
    disabled_reasons: [...value.disabledReasons],
  };
}

function presentCard(value: V3ProviderContracts["providers"][number]["card"]): CardCapabilitySummary {
  return {
    fetch_zen_models: value.fetchZenModels,
    discover_models: value.discoverModels,
    protocol_probe: value.protocolProbe,
    catalog_refresh: value.catalogRefresh,
  };
}

function presentCatalog(value: V3ProviderContracts["providers"][number]["catalog"]): EffectiveCatalog {
  return {
    source: value.source,
    source_url: value.sourceUrl,
    refreshed_at: value.refreshedAt,
    models: [...value.models],
    refresh_supported: value.refreshSupported,
  };
}

function presentAccountChoice(value: V3ProviderContracts["providers"][number]["accounts"][number]): ProviderAccountChoice {
  return {
    id: value.id,
    name: value.name,
    enabled: value.enabled,
    verification_status: value.verificationStatus,
  };
}

function presentContracts(value: V3ProviderContracts): ProviderContractsResponse {
  return {
    revision: value.revision,
    providers: value.providers.map((scope) => ({
      scope_kind: scope.scopeKind,
      scope_id: scope.scopeId,
      provider_id: scope.providerId,
      static_protocol_snapshot_date: scope.staticProtocolSnapshotDate,
      accounts: scope.accounts.map(presentAccountChoice),
      catalog: presentCatalog(scope.catalog),
      models: scope.models.map(presentModel),
      pricing: { availability: scope.pricing.availability },
      usage: { availability: scope.usage.availability },
      card: presentCard(scope.card),
      catalog_routable: scope.catalogRoutable,
      production_inference: scope.productionInference,
      disabled_reasons: [...scope.disabledReasons],
      revision: scope.revision,
    })),
    custom_endpoints: value.customEndpoints.map((scope) => ({
      scope_kind: scope.scopeKind,
      scope_id: scope.scopeId,
      provider_id: scope.providerId,
      account: presentAccountChoice(scope.account),
      catalog: presentCatalog(scope.catalog),
      models: scope.models.map(presentModel),
      pricing: { availability: scope.pricing.availability },
      usage: { availability: scope.usage.availability },
      card: {
        ...presentCard(scope.card),
        // Custom per-protocol probing has no V3 endpoint yet (deferred).
        protocol_probe: false,
      },
      catalog_routable: scope.catalogRoutable,
      production_inference: scope.productionInference,
      disabled_reasons: [...scope.disabledReasons],
      revision: scope.revision,
    })),
  };
}

function presentProviderPricing(value: V3ProviderPricing): ProviderPricingResponse {
  return {
    provider_id: value.providerId,
    availability: value.availability,
    snapshot: value.providerSnapshot === null
      ? (value.snapshot === null ? undefined : presentPricing(value.snapshot))
      : presentProviderPricingSnapshot(value.providerSnapshot),
    revision: value.revision,
    process_generation: value.processGeneration,
    pricing_revision: value.pricingRevision,
    provider_pricing_revision: value.providerPricingRevision,
  };
}

function presentProviderPricingSnapshot(value: V3ProviderPricingSnapshot): ProviderNeutralPricingSnapshot {
  return {
    revision: value.revision,
    activated_at: value.activatedAt,
    document_updated_at: value.documentUpdatedAt,
    source_url: value.sourceUrl,
    content_hash: value.contentHash,
    evidence: value.evidence,
    values: value.values.map((row) => ({
      model_id: row.modelId,
      display_name: row.displayName,
      input_per_million: row.inputPerMillion,
      output_per_million: row.outputPerMillion,
      cache_read_per_million: row.cacheReadPerMillion,
      cache_write_per_million: row.cacheWritePerMillion,
      plan_limit: row.planLimit,
      model_allowance: row.modelAllowance,
      quota_multiplier: row.quotaMultiplier,
      paid_plan_price: row.paidPlanPrice,
      currency: row.currency,
    })),
  };
}

function presentProviderUsage(value: V3ProviderUsage): ProviderUsageResponse {
  return {
    account_id: value.accountId,
    provider_id: value.providerId,
    availability: value.availability,
    quota_windows: value.quotaWindows.map((window) => ({
      account_id: window.accountId,
      window_kind: window.windowKind,
      used: window.used,
      limit_value: window.limitValue,
      started_at: window.startedAt,
      resets_at: window.resetsAt,
      calibration_offset: window.calibrationOffset,
      unit: window.unit,
      source: window.source,
      observed_at: window.observedAt,
      updated_at: window.updatedAt,
    })),
    credit_balances: value.creditBalances.map((balance) => ({
      account_id: balance.accountId,
      balance_kind: balance.balanceKind,
      amount: balance.amount,
      unit: balance.unit,
      source: balance.source,
      observed_at: balance.observedAt,
      updated_at: balance.updatedAt,
    })),
    sync_state: value.syncState === null ? null : {
      last_success_at: value.syncState.lastSuccessAt,
      last_attempt_at: value.syncState.lastAttemptAt,
      next_eligible_at: value.syncState.nextEligibleAt,
      failure_streak: value.syncState.failureStreak,
      last_expedited_at: value.syncState.lastExpeditedAt,
    },
  };
}

function presentProbe(value: V3ProtocolProbeResponse): ProtocolProbeResponse {
  return {
    model_id: value.modelId,
    results: value.results.map((result) => ({
      protocol: result.protocol,
      success: result.success,
      skipped: result.skipped,
      error: result.error,
    })),
    contract: value.contract === null ? null : presentModel(value.contract),
  };
}

export const providerApi = {
  getProviderCatalog: async () => (await dashboardV3.getProviders()).entries.map(presentCatalogEntry),
  getProviderPricing: async (providerId: string) =>
    presentProviderPricing(await dashboardV3.getProviderPricing(providerId)),
  updateProviderPricingMultipliers: async (
    providerId: string,
    expectedPricingRevision: string,
    multipliers: Array<{ model_id: string; multiplier: number }>,
  ) => {
    const control = useControlPlaneStore();
    if (!control.hasTokens()) await control.refresh();
    return presentProviderPricing(await control.runMutation((expectation) => (
      dashboardV3.putProviderPricingMultipliers(providerId, {
        expectedPricingRevision,
        multipliers: multipliers.map((multiplier) => ({
          modelId: multiplier.model_id,
          multiplier: multiplier.multiplier,
        })),
      }, expectation)
    )));
  },
  getProviderUsage: async (accountId: string) =>
    presentProviderUsage(await dashboardV3.getProviderUsage(accountId)),
  refreshProviderUsage: async (accountId: string) => {
    const control = useControlPlaneStore();
    if (!control.hasTokens()) await control.refresh();
    return presentProviderUsage(await control.runMutation((expectation) =>
      dashboardV3.refreshProviderUsage(accountId, expectation)
    ));
  },
  updateProviderSettings: async (accountId: string, update: ProviderSettingsUpdate) => {
    const control = useControlPlaneStore();
    if (!control.hasTokens()) await control.refresh();
    const account = await dashboardV3.getAccount(accountId);
    // Catalog provider_id is `opencode-zen-free`; `/providers/zen-free` is only the V3 route slug.
    if (account.providerId !== "opencode-zen-free") throw new Error("only Zen Free has provider settings");
    try {
      await control.runMutation((expectation) => dashboardV3.patchZenFreeSettings(update.enabled, expectation));
    } catch (cause) {
      if (isRevisionConflict(cause)) await dashboardV3.getZenFreeSettings();
      throw cause;
    }
    const refreshed = await dashboardV3.getAccount(accountId);
    return { account: presentAccount(refreshed), revision: refreshed.revision };
  },
  refreshContractCatalog: async (scopeKind: ContractScopeKind, scopeId: string) => {
    const control = useControlPlaneStore();
    if (!control.hasTokens()) await control.refresh();
    return presentContracts(await control.runMutation((expectation) =>
      dashboardV3.refreshContractCatalog(scopeKind, scopeId, expectation)
    ));
  },
  removeContractCatalogModels: async (
    scopeKind: ContractScopeKind,
    scopeId: string,
    modelIds: string[],
  ) => {
    const control = useControlPlaneStore();
    if (!control.hasTokens()) await control.refresh();
    try {
      await control.runMutation((expectation) =>
        dashboardV4.removeCatalogModels(scopeKind, scopeId, { modelIds }, expectation));
    } catch (cause) {
      if (isRevisionConflict(cause)) await dashboardV3.getProviderContracts();
      throw cause;
    }
    return presentContracts(await dashboardV3.getProviderContracts());
  },
  getProviderContracts: async () => presentContracts(await dashboardV3.getProviderContracts()),
  updateModelProtocolOverrides: async (
    scopeKind: ContractScopeKind,
    scopeId: string,
    overrides: ModelProtocolOverrideUpdate[],
  ): Promise<ProviderContractsResponse> => {
    const control = useControlPlaneStore();
    if (!control.hasTokens()) await control.refresh();
    try {
      return presentContracts(await control.runMutation((expectation) =>
        dashboardV3.putModelProtocolOverrides(
          scopeKind,
          scopeId,
          { overrides: overrides.map((item) => ({
            modelId: item.model_id,
            protocol: item.protocol,
            state: item.state,
            ...(item.preferred !== undefined ? { preferred: item.preferred } : {}),
          })) } satisfies WithoutExpectation<ModelProtocolOverridesUpdate>,
          expectation,
        )));
    } catch (cause) {
      if (isRevisionConflict(cause)) await dashboardV3.getProviderContracts();
      throw cause;
    }
  },
  runProtocolProbes: async (providerId: string, input: ProtocolProbeRequest) => {
    const control = useControlPlaneStore();
    if (!control.hasTokens()) await control.refresh();
    if (providerId === "custom") {
      throw new Error("Custom API 协议探测尚未纳入 Dashboard V3 合同");
    }
    return presentProbe(await control.runMutation((expectation) =>
      dashboardV3.runProviderProtocolProbes(providerId, {
        modelId: input.model_id,
        protocols: input.protocols,
      }, expectation)));
  },
  getProviderDefinition: async (providerId: string) => {
    const value = await dashboardV3.getProviderDefinition(providerId);
    assertNoSecret(value);
    return presentProviderDefinition(value);
  },
  updateProviderDefinition: async (
    providerId: string,
    input: WithoutExpectation<import("./generated/dashboard-v3.ts").ProviderDefinitionUpdate>,
    expectation?: MutationExpectation,
  ) => {
    const control = useControlPlaneStore();
    if (!expectation && !control.hasTokens()) await control.refresh();
    try {
      const value: V3ProviderDefinitionMutation = await control.runMutation(
        (tokens) => dashboardV3.updateProviderDefinition(providerId, input, tokens),
        expectation,
      );
      assertNoSecret(value);
      assertNoSecret(value.provider);
      return presentProviderDefinition(value.provider);
    } catch (cause) {
      if (isRevisionConflict(cause)) {
        await dashboardV3.getProviders();
        await dashboardV3.getProviderDefinition(providerId);
      }
      throw cause;
    }
  },
  deleteProviderDefinition: async (providerId: string) => {
    const control = useControlPlaneStore();
    if (!control.hasTokens()) await control.refresh();
    try {
      return await control.runMutation((expectation) =>
        dashboardV3.deleteProviderDefinition(providerId, expectation));
    } catch (cause) {
      if (isRevisionConflict(cause)) await dashboardV3.getProviders();
      throw cause;
    }
  },
  discoverProviderDefinitionModels: async (input: {
    endpoint_url: string;
    upstream_protocol: "chat_completions" | "responses" | "messages";
    auth_kind: ProviderDefinitionAuthKind;
    key?: string;
  }) => {
    const value: V3ProviderDefinitionDiscoverResponse = await dashboardV3.discoverProviderDefinitionModels({
      endpointUrl: input.endpoint_url,
      upstreamProtocol: input.upstream_protocol,
      authKind: input.auth_kind,
      key: input.key,
    });
    assertNoSecret(value);
    return { models: value.models, truncated: value.truncated };
  },
  testProviderDefinition: async (input: {
    endpoint_url: string;
    upstream_protocol: "chat_completions" | "responses" | "messages";
    auth_kind: ProviderDefinitionAuthKind;
    public_model: string;
    upstream_model: string;
    key?: string;
  }) => {
    const value: V3ProviderDefinitionTestResponse = await dashboardV3.testProviderDefinition({
      endpointUrl: input.endpoint_url,
      upstreamProtocol: input.upstream_protocol,
      authKind: input.auth_kind,
      publicModel: input.public_model,
      upstreamModel: input.upstream_model,
      key: input.key,
    });
    assertNoSecret(value);
    return { ok: value.ok, error: value.error };
  },
};
