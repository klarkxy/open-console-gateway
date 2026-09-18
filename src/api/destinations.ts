/**
 * Dashboard V4 destination / credential projection presenter.
 *
 * `GET /dashboard/api/v4/destinations` and `GET /dashboard/api/v4/credentials`
 * are secret-free. The wire uses camelCase; the view model is snake_case,
 * matching `connections.ts`.
 */

import { dashboardV4 } from "./dashboard-v4.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
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
  LegacyDestinationRefDto,
  ModelScope,
  PlanDto,
  ProtocolDto,
  RedirectPolicyDto,
} from "./generated/dashboard-v4.ts";

export type {
  AdapterKindDto,
  AuthSchemeDto,
  AuthState,
  ModelScope,
  ProtocolDto,
  RedirectPolicyDto,
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
  };
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

async function fetchDestinationSnapshot(): Promise<DestinationListSnapshot> {
  const value = await dashboardV4.getDestinations();
  return presentDestinationListSnapshot(value);
}

async function fetchCredentialSnapshot(): Promise<CredentialListSnapshot> {
  const value = await dashboardV4.getCredentials();
  return presentCredentialListSnapshot(value);
}

export const destinationsApi = {
  list: async (): Promise<Destination[]> => {
    const snapshot = await fetchDestinationSnapshot();
    return snapshot.destinations;
  },
  listSnapshot: fetchDestinationSnapshot,
};

export const credentialsApi = {
  list: async (): Promise<DestinationCredential[]> => {
    const snapshot = await fetchCredentialSnapshot();
    return snapshot.credentials;
  },
  listSnapshot: fetchCredentialSnapshot,
};
