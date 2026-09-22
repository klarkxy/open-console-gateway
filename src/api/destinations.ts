/**
 * Dashboard V4 destination / credential projection presenter.
 *
 * `GET /dashboard/api/v4/destinations` and `GET /dashboard/api/v4/credentials`
 * are secret-free. `PATCH`/`DELETE /dashboard/api/v4/destinations/{id}` edit
 * and remove configurable HTTP destinations under CAS, and
 * `GET /dashboard/api/v4/routing/explain` is the read-only routing prediction.
 * The wire uses camelCase; the view model is snake_case,
 * matching `connections.ts`.
 */

import {
  dashboardV4,
  type DestinationCatalogUpdate,
  type HttpProtocolRouteDto,
} from "./dashboard-v4.ts";
import type { WithoutExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import type {
  AdapterKindDto,
  AuthSchemeDto,
  AuthState,
  CapabilitiesDto,
  CatalogModelDto,
  CredentialList,
  DestinationCredentialDto,
  DestinationDto,
  DestinationList,
  DestinationOnboardingTaskDto,
  DestinationPatchRequest,
  LegacyDestinationRefDto,
  ModelScope,
  QuotaRecoveryDto,
  PlanDto,
  ProtocolDto,
  RedirectPolicyDto,
  RoutingCard,
  RoutingCardList,
  RoutingCardUpdate,
  RoutingChannel,
  RoutingClientProtocol,
  RoutingConversationBinding,
  RoutingEligibleCandidate,
  RoutingExclusion,
  RoutingExclusionCode,
  RoutingExplanation,
  RoutingMode,
  RoutingResolvedKind,
  RuntimeOnlyUncertainty,
} from "./generated/dashboard-v4.ts";

export type {
  AdapterKindDto,
  AuthSchemeDto,
  AuthState,
  ModelScope,
  ProtocolDto,
  RedirectPolicyDto,
  RoutingChannel,
  RoutingClientProtocol,
  RoutingExclusionCode,
  RoutingMode,
  RoutingResolvedKind,
  RuntimeOnlyUncertainty,
};

export interface DestinationCapabilities {
  billing_tier_required: boolean;
  discoverable_models: boolean;
  external_integration: boolean;
  identity_headers: boolean;
  managed_signup: boolean;
  observer: boolean;
  official_balance_probe: string[];
  redirect_policy: RedirectPolicyDto;
  testable: boolean;
}

export interface DestinationPlanWindow {
  kind: PlanDto["windows"][number]["kind"];
}

export interface DestinationPlan {
  expiry_cadence: PlanDto["expiryCadence"];
  manual_calibration: boolean;
  pricing_source: PlanDto["pricingSource"];
  usage_source: PlanDto["usageSource"];
  windows: DestinationPlanWindow[];
}

export interface DestinationCatalogModel {
  enabled: boolean;
  preferred: ProtocolDto | null;
  protocols: ProtocolDto[];
  public_model: string;
  upstream_model: string;
  /** Persisted per-model route override; null inherits the connection default. */
  upstream_override: { protocol: ProtocolDto; endpoint_url: string } | null;
}

/** Migration-era bridge back to the V3 row that still owns mutations. */
export interface LegacyDestinationRef {
  kind: LegacyDestinationRefDto["kind"];
  id: string;
}

export interface Destination {
  adapter: AdapterKindDto;
  legacy: LegacyDestinationRef;
  auth_scheme: AuthSchemeDto;
  base_url: string | null;
  brand_family: string | null;
  capabilities: DestinationCapabilities;
  catalog: DestinationCatalogModel[];
  enabled: boolean;
  id: string;
  max_credentials: number | null;
  name: string;
  observer_credential_id: string | null;
  plan: DestinationPlan | null;
  protocols: ProtocolDto[];
  protocol_routes?: DestinationProtocolRoute[];
}

/** One configured HTTP protocol route on the destination view model. */
export interface DestinationProtocolRoute {
  protocol: ProtocolDto;
  endpoint_url: string;
  auth_scheme: AuthSchemeDto;
}

export interface DestinationCredentialGrants {
  allowed_endpoint_ids: string[];
  allowed_origins: string[];
}

export interface DestinationCredentialCooldowns {
  five_hour_until: string | null;
  free_until: string | null;
  generic_until: string | null;
  month_until: string | null;
  week_until: string | null;
}

export interface DestinationOnboardingTask {
  kind: DestinationOnboardingTaskDto["kind"];
  state: DestinationOnboardingTaskDto["state"];
  step: string;
}

export type QuotaRecoveryStatus = QuotaRecoveryDto["status"];
export type QuotaRecoveryReason = QuotaRecoveryDto["reason"];
export type QuotaRecoveryWindow = QuotaRecoveryDto["window"];

export interface QuotaRecovery {
  status: QuotaRecoveryStatus;
  reason: QuotaRecoveryReason;
  window: QuotaRecoveryWindow;
  observed_at: string;
  resets_at: string | null;
  next_retry_at: string;
  failure_count: number;
}

export interface DestinationCredential {
  auth_state: AuthState;
  cooldowns: DestinationCredentialCooldowns;
  destination_id: string;
  enabled: boolean;
  grants: DestinationCredentialGrants;
  has_secret: boolean;
  id: string;
  last_error: string | null;
  /** The V3 account row this credential was projected from. */
  legacy_account_id: string;
  name: string;
  notes: string | null;
  onboarding_task: DestinationOnboardingTask | null;
  purchase_date: string | null;
  quota_pool_id: string | null;
  /**
   * Confirmed exhaustion/recovery for this Key only. Absent or null means no
   * confirmed exhaustion, not verified upstream health. Pools do not fan out.
   */
  quota_recovery?: QuotaRecovery | null;
  routing_rank: number;
  scope: ModelScope;
}

export interface DestinationListSnapshot {
  destinations: Destination[];
  expectation: MutationExpectation;
}

export interface CredentialListSnapshot {
  credentials: DestinationCredential[];
  expectation: MutationExpectation;
}

/** One routing card: a stable presentation group over credentials of one destination. */
export interface RoutingCardView {
  id: string;
  destination_id: string;
  credential_ids: string[];
}

/** Atomic snapshot of cards plus the resources they show; the sole routing layout source. */
export interface RoutingCardListSnapshot {
  cards: RoutingCardView[];
  destinations: Destination[];
  credentials: DestinationCredential[];
  expectation: MutationExpectation;
}

/** Submitted full layout; the CAS pair is attached per attempt. */
export type RoutingCardLayoutInput = WithoutExpectation<RoutingCardUpdate>;

/** Presented body of a destination PATCH; the CAS pair is supplied per attempt. */
export type DestinationPatchInput = Omit<WithoutExpectation<DestinationPatchRequest>, "models"> & {
  protocolRoutes?: HttpProtocolRouteDto[];
  models: Array<DestinationPatchRequest["models"][number] & {
    protocols?: ProtocolDto[];
    preferred?: ProtocolDto;
  }>;
};

export type DestinationCatalogUpdateInput = DestinationCatalogUpdate;

export interface DestinationModelTestView {
  public_model: string;
  protocol: ProtocolDto;
  ok: boolean;
  error: string | null;
  expectation: MutationExpectation;
}

export interface DestinationPatchView {
  destination: Destination;
  credentials: DestinationCredential[];
  expectation: MutationExpectation;
}

export interface RoutingResolvedMappingView {
  provider_id: string;
  routeable: boolean;
  upstream_model: string;
}

export interface RoutingResolvedModelView {
  alias: string | null;
  kind: RoutingResolvedKind;
  mappings: RoutingResolvedMappingView[];
}

export interface RoutingEligibleCandidateView {
  account_id: string;
  account_name: string;
  adapter_kind: string;
  channel: RoutingChannel;
  destination_id: string | null;
  destination_name: string | null;
  provider_id: string;
  resolved_model: string;
  routing_rank: number;
  upstream_protocol: RoutingClientProtocol;
}

export interface RoutingExclusionView {
  account_id: string | null;
  code: RoutingExclusionCode;
  detail: string;
  provider_id: string | null;
  upstream_model: string | null;
}

export interface RoutingExplanationView {
  client_protocol: RoutingClientProtocol;
  conversation_binding: RoutingConversationBinding;
  conversation_sticky: boolean;
  eligible: RoutingEligibleCandidateView[];
  exclusions: RoutingExclusionView[];
  expected_base_policy_first_pick: RoutingEligibleCandidateView | null;
  observed_at: string;
  requested_model: string;
  resolved: RoutingResolvedModelView;
  expectation: MutationExpectation;
  routing_mode: RoutingMode;
  runtime_only_uncertainty: RuntimeOnlyUncertainty[];
}

function presentCapabilities(value: CapabilitiesDto): DestinationCapabilities {
  return {
    billing_tier_required: value.billingTierRequired,
    discoverable_models: value.discoverableModels,
    external_integration: value.externalIntegration,
    identity_headers: value.identityHeaders,
    managed_signup: value.managedSignup,
    observer: value.observer,
    official_balance_probe: [...value.officialBalanceProbe],
    redirect_policy: value.redirectPolicy,
    testable: value.testable,
  };
}

function presentPlan(value: PlanDto | null): DestinationPlan | null {
  if (!value) return null;
  return {
    expiry_cadence: value.expiryCadence,
    manual_calibration: value.manualCalibration,
    pricing_source: value.pricingSource,
    usage_source: value.usageSource,
    windows: value.windows.map((window) => ({ kind: window.kind })),
  };
}

function presentCatalogModel(value: CatalogModelDto): DestinationCatalogModel {
  return {
    enabled: value.enabled,
    preferred: value.preferred,
    protocols: [...value.protocols],
    public_model: value.publicModel,
    upstream_model: value.upstreamModel,
    upstream_override: value.upstreamOverride
      ? { protocol: value.upstreamOverride.protocol, endpoint_url: value.upstreamOverride.endpointUrl }
      : null,
  };
}

function presentProtocolRoutes(value: DestinationDto): DestinationProtocolRoute[] {
  const routes = (value as DestinationDto & { protocolRoutes?: HttpProtocolRouteDto[] }).protocolRoutes;
  if (!Array.isArray(routes)) return [];
  return routes.map((route) => ({
    protocol: route.protocol,
    endpoint_url: route.endpointUrl,
    auth_scheme: route.authScheme,
  }));
}

export function presentDestination(value: DestinationDto): Destination {
  return {
    adapter: value.adapter,
    auth_scheme: value.authScheme,
    base_url: value.baseUrl,
    brand_family: value.brandFamily,
    capabilities: presentCapabilities(value.capabilities),
    catalog: value.catalog.map(presentCatalogModel),
    enabled: value.enabled,
    id: value.id,
    legacy: { kind: value.legacy.kind, id: value.legacy.id },
    max_credentials: value.maxCredentials,
    name: value.name,
    observer_credential_id: value.observerCredentialId,
    plan: presentPlan(value.plan),
    protocols: [...value.protocols],
    protocol_routes: presentProtocolRoutes(value),
  };
}

const QUOTA_RECOVERY_STATUSES = new Set<QuotaRecoveryStatus>(["waiting", "ready", "probing"]);
const QUOTA_RECOVERY_REASONS = new Set<QuotaRecoveryReason>([
  "quota_exhausted",
  "insufficient_balance",
]);
const QUOTA_RECOVERY_WINDOWS = new Set<QuotaRecoveryWindow>([
  "five_hours",
  "week",
  "month",
  "unknown",
]);

export function presentQuotaRecovery(
  value: QuotaRecoveryDto | null | undefined,
): QuotaRecovery | null {
  if (!value) return null;
  if (!QUOTA_RECOVERY_STATUSES.has(value.status)) return null;
  if (!QUOTA_RECOVERY_REASONS.has(value.reason)) return null;
  if (!QUOTA_RECOVERY_WINDOWS.has(value.window)) return null;
  if (typeof value.observedAt !== "string" || typeof value.nextRetryAt !== "string") return null;
  if (value.resetsAt !== null && typeof value.resetsAt !== "string") return null;
  if (typeof value.failureCount !== "number") return null;
  return {
    status: value.status,
    reason: value.reason,
    window: value.window,
    observed_at: value.observedAt,
    resets_at: value.resetsAt,
    next_retry_at: value.nextRetryAt,
    failure_count: value.failureCount,
  };
}

export function presentDestinationCredential(
  value: DestinationCredentialDto,
): DestinationCredential {
  return {
    auth_state: value.authState,
    cooldowns: {
      five_hour_until: value.cooldowns.fiveHourUntil,
      free_until: value.cooldowns.freeUntil,
      generic_until: value.cooldowns.genericUntil,
      month_until: value.cooldowns.monthUntil,
      week_until: value.cooldowns.weekUntil,
    },
    destination_id: value.destinationId,
    enabled: value.enabled,
    grants: {
      allowed_endpoint_ids: [...value.grants.allowedEndpointIds],
      allowed_origins: [...value.grants.allowedOrigins],
    },
    has_secret: value.hasSecret,
    id: value.id,
    last_error: value.lastError,
    legacy_account_id: value.legacyAccountId,
    name: value.name,
    notes: value.notes,
    onboarding_task: value.onboardingTask
      ? {
          kind: value.onboardingTask.kind,
          state: value.onboardingTask.state,
          step: value.onboardingTask.step,
        }
      : null,
    purchase_date: value.purchaseDate,
    quota_pool_id: value.quotaPoolId,
    quota_recovery: presentQuotaRecovery(value.quotaRecovery),
    routing_rank: value.routingRank,
    scope: value.scope,
  };
}

export function presentDestinationListSnapshot(
  value: DestinationList,
): DestinationListSnapshot {
  return {
    destinations: value.destinations.map(presentDestination),
    expectation: {
      expectedRevision: value.revision.revision,
      processGeneration: value.revision.processGeneration,
    },
  };
}

export function presentCredentialListSnapshot(value: CredentialList): CredentialListSnapshot {
  return {
    credentials: value.credentials.map(presentDestinationCredential),
    expectation: {
      expectedRevision: value.revision.revision,
      processGeneration: value.revision.processGeneration,
    },
  };
}

function presentRoutingCard(value: RoutingCard): RoutingCardView {
  return {
    id: value.id,
    destination_id: value.destinationId,
    credential_ids: [...value.credentialIds],
  };
}

export function presentRoutingCardListSnapshot(value: RoutingCardList): RoutingCardListSnapshot {
  return {
    cards: value.cards.map(presentRoutingCard),
    destinations: value.destinations.map(presentDestination),
    credentials: value.credentials.map(presentDestinationCredential),
    expectation: {
      expectedRevision: value.revision.revision,
      processGeneration: value.revision.processGeneration,
    },
  };
}

function presentEligibleCandidate(value: RoutingEligibleCandidate): RoutingEligibleCandidateView {
  return {
    account_id: value.accountId,
    account_name: value.accountName,
    adapter_kind: value.adapterKind,
    channel: value.channel,
    destination_id: value.destinationId,
    destination_name: value.destinationName,
    provider_id: value.providerId,
    resolved_model: value.resolvedModel,
    routing_rank: value.routingRank,
    upstream_protocol: value.upstreamProtocol,
  };
}

function presentRoutingExclusion(value: RoutingExclusion): RoutingExclusionView {
  return {
    account_id: value.accountId,
    code: value.code,
    detail: value.detail,
    provider_id: value.providerId,
    upstream_model: value.upstreamModel,
  };
}

export function presentRoutingExplanation(value: RoutingExplanation): RoutingExplanationView {
  return {
    client_protocol: value.clientProtocol,
    conversation_binding: value.conversationBinding,
    conversation_sticky: value.conversationSticky,
    eligible: value.eligible.map(presentEligibleCandidate),
    exclusions: value.exclusions.map(presentRoutingExclusion),
    expected_base_policy_first_pick: value.expectedBasePolicyFirstPick
      ? presentEligibleCandidate(value.expectedBasePolicyFirstPick)
      : null,
    observed_at: value.observedAt,
    requested_model: value.requestedModel,
    resolved: {
      alias: value.resolved.alias,
      kind: value.resolved.kind,
      mappings: value.resolved.mappings.map((mapping) => ({
        provider_id: mapping.providerId,
        routeable: mapping.routeable,
        upstream_model: mapping.upstreamModel,
      })),
    },
    expectation: {
      expectedRevision: value.revision.revision,
      processGeneration: value.revision.processGeneration,
    },
    routing_mode: value.routingMode,
    runtime_only_uncertainty: [...value.runtimeOnlyUncertainty],
  };
}

async function withCas<T>(
  run: (expectation: MutationExpectation) => Promise<T>,
  captured?: MutationExpectation,
): Promise<T> {
  const control = useControlPlaneStore();
  if (!captured && !control.hasTokens()) await control.refresh();
  return control.runMutation(run, captured);
}

async function fetchDestinationSnapshot(): Promise<DestinationListSnapshot> {
  const value = await dashboardV4.getDestinations();
  return presentDestinationListSnapshot(value);
}

async function fetchCredentialSnapshot(): Promise<CredentialListSnapshot> {
  const value = await dashboardV4.getCredentials();
  return presentCredentialListSnapshot(value);
}

export const destinationsApi = {
  refreshCatalog: async (id: string, expectation?: MutationExpectation) => {
    const value = await withCas((tokens) => dashboardV4.refreshDestinationCatalog(id, tokens), expectation);
    return {
      destination: presentDestination(value.destination),
      addedCount: value.addedCount,
      truncated: value.truncated,
      expectation: {
        expectedRevision: value.revision.revision,
        processGeneration: value.revision.processGeneration,
      },
    };
  },
  updateCatalog: async (
    id: string,
    input: DestinationCatalogUpdateInput,
    expectation?: MutationExpectation,
  ): Promise<DestinationPatchView> => {
    const value = await withCas(
      (tokens) => dashboardV4.updateDestinationCatalog(id, input, tokens),
      expectation,
    );
    return {
      destination: presentDestination(value.destination),
      credentials: value.credentials.map(presentDestinationCredential),
      expectation: {
        expectedRevision: value.revision.revision,
        processGeneration: value.revision.processGeneration,
      },
    };
  },
  testModel: async (
    id: string,
    publicModel: string,
    protocol: ProtocolDto,
    expectation?: MutationExpectation,
  ): Promise<DestinationModelTestView> => {
    const value = await withCas(
      (tokens) => dashboardV4.testDestinationModel(id, publicModel, protocol, tokens),
      expectation,
    );
    return {
      public_model: value.publicModel,
      protocol: value.protocol,
      ok: value.ok,
      error: value.error ?? null,
      expectation: {
        expectedRevision: value.revision.revision,
        processGeneration: value.revision.processGeneration,
      },
    };
  },
  list: async (): Promise<Destination[]> => {
    const snapshot = await fetchDestinationSnapshot();
    return snapshot.destinations;
  },
  listSnapshot: fetchDestinationSnapshot,
  /**
   * Full-replacement PATCH of one configurable HTTP destination. Pass the
   * expectation the editor captured with its snapshot; omit it to use the
   * control-plane pair. On 409 `runMutation` refreshes tokens and never
   * replays — the caller reloads the projection and asks the user to re-apply.
   */
  patch: async (
    id: string,
    input: DestinationPatchInput,
    expectation?: MutationExpectation,
  ): Promise<DestinationPatchView> => {
    const value = await withCas(
      (tokens) => dashboardV4.patchDestination(id, input, tokens),
      expectation,
    );
    return {
      destination: presentDestination(value.destination),
      credentials: value.credentials.map(presentDestinationCredential),
      expectation: {
        expectedRevision: value.revision.revision,
        processGeneration: value.revision.processGeneration,
      },
    };
  },
  /** Only empty destinations delete; the server 400s while Keys reference it. */
  delete: async (id: string, expectation?: MutationExpectation): Promise<MutationExpectation> => {
    const value = await withCas((tokens) => dashboardV4.deleteDestination(id, tokens), expectation);
    return {
      expectedRevision: value.revision.revision,
      processGeneration: value.revision.processGeneration,
    };
  },
};

export const routingApi = {
  explain: async (
    model: string,
    clientProtocol: RoutingClientProtocol,
  ): Promise<RoutingExplanationView> => {
    const value = await dashboardV4.explainRouting(model, clientProtocol);
    return presentRoutingExplanation(value);
  },
};

export interface CredentialQuotaRetryView {
  credential: DestinationCredential;
  expectation: MutationExpectation;
}

export const credentialsApi = {
  list: async (): Promise<DestinationCredential[]> => {
    const snapshot = await fetchCredentialSnapshot();
    return snapshot.credentials;
  },
  listSnapshot: fetchCredentialSnapshot,
  /**
   * Marks one exhausted Key ready for the next normal selection. Flattened
   * CAS body only; does not test, enable, or clear backoff. Idempotent while
   * already ready or probing.
   */
  retryQuota: async (
    id: string,
    expectation?: MutationExpectation,
  ): Promise<CredentialQuotaRetryView> => {
    const value = await withCas(
      (tokens) => dashboardV4.retryCredentialQuota(id, tokens),
      expectation,
    );
    return {
      credential: presentDestinationCredential(value.credential),
      expectation: {
        expectedRevision: value.revision.revision,
        processGeneration: value.revision.processGeneration,
      },
    };
  },
};

async function fetchRoutingCardSnapshot(): Promise<RoutingCardListSnapshot> {
  const value = await dashboardV4.getRoutingCards();
  return presentRoutingCardListSnapshot(value);
}

export const routingCardsApi = {
  /** One atomic snapshot of cards and the resources they show. */
  listSnapshot: fetchRoutingCardSnapshot,
  /**
   * Full-replacement layout write under CAS. `cards` is the complete visible
   * layout (grouping plus flattened rank). Returns the committed snapshot,
   * which the caller commits in place; a 409 conflict is never replayed.
   */
  replace: async (
    input: RoutingCardLayoutInput,
    expectation?: MutationExpectation,
  ): Promise<RoutingCardListSnapshot> => {
    const value = await withCas(
      (tokens) => dashboardV4.putRoutingCards(input, tokens),
      expectation,
    );
    return presentRoutingCardListSnapshot(value);
  },
};
