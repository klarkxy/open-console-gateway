import { dashboardV4 } from "./dashboard-v4.ts";
import type { WithoutExpectation } from "./dashboard-v3.ts";
import { useControlPlaneStore } from "../stores/controlPlane.ts";
import type {
  AccountUpstreamProtocol,
  AuthorizationState,
  ConnectionEndpoint as V4ConnectionEndpoint,
  ConnectionLifecycle,
  ConnectionOrigin,
  ConnectionSummary,
  ConnectionTarget as V4ConnectionTarget,
  EligibilityReason,
  EligibilityState,
  EndpointAuthScheme,
  EndpointOperation,
  LegacyConnectionKind,
  LegacyIdentity as V4LegacyIdentity,
  OfferingKind,
  OnboardingCommitRequest,
  OnboardingCommitResult,
  TemplateRef,
} from "./generated/dashboard-v4.ts";

export type {
  AccountUpstreamProtocol,
  AuthorizationState,
  ConnectionLifecycle,
  ConnectionOrigin,
  EligibilityReason,
  EligibilityState,
  EndpointAuthScheme,
  EndpointOperation,
  LegacyConnectionKind,
  OfferingKind,
};

export interface ConnectionEndpoint {
  id: string;
  connection_id: string;
  auth_scheme: EndpointAuthScheme;
  locked: boolean;
  operation: EndpointOperation;
  url: string | null;
  wire_protocol: AccountUpstreamProtocol;
}

export interface ConnectionTarget {
  id: string;
  connection_id: string;
  enabled: boolean;
  endpoint_ids: string[];
  public_name: string;
  upstream_model_id: string;
}

export interface ConnectionEligibility {
  state: EligibilityState;
  reason: EligibilityReason;
}

export interface LegacyIdentity {
  kind: LegacyConnectionKind;
  id: string;
}

export interface ConnectionTemplateRef {
  id: string;
  version: number;
}

export interface Connection {
  id: string;
  name: string;
  origin: ConnectionOrigin;
  template_ref: ConnectionTemplateRef | null;
  adapter_kind: string;
  lifecycle: ConnectionLifecycle;
  authorization: AuthorizationState;
  eligibility: ConnectionEligibility;
  credential_count: number;
  enabled_credential_count: number;
  target_count: number;
  endpoints: ConnectionEndpoint[];
  targets: ConnectionTarget[];
  legacy: LegacyIdentity;
  display_family: string | null;
  offering: OfferingKind;
}

export interface OnboardingCommitView {
  connection_id: string;
  credential_id: string | null;
  replayed: boolean;
  target_ids: string[];
}

function presentEndpoint(value: V4ConnectionEndpoint): ConnectionEndpoint {
  return {
    id: value.id,
    connection_id: value.connectionId,
    auth_scheme: value.authScheme,
    locked: value.locked,
    operation: value.operation,
    url: value.url,
    wire_protocol: value.wireProtocol,
  };
}

function presentTarget(value: V4ConnectionTarget): ConnectionTarget {
  return {
    id: value.id,
    connection_id: value.connectionId,
    enabled: value.enabled,
    endpoint_ids: [...value.endpointIds],
    public_name: value.publicName,
    upstream_model_id: value.upstreamModelId,
  };
}

function presentLegacy(value: V4LegacyIdentity): LegacyIdentity {
  return { kind: value.kind, id: value.id };
}

function presentTemplateRef(value: TemplateRef | null): ConnectionTemplateRef | null {
  if (!value) return null;
  return { id: value.id, version: value.version };
}

export function presentConnection(value: ConnectionSummary): Connection {
  return {
    id: value.id,
    name: value.name,
    origin: value.origin,
    template_ref: presentTemplateRef(value.templateRef),
    adapter_kind: value.adapterKind,
    lifecycle: value.lifecycle,
    authorization: value.authorization,
    eligibility: {
      state: value.eligibility.state,
      reason: value.eligibility.reason,
    },
    credential_count: value.credentialCount,
    enabled_credential_count: value.enabledCredentialCount,
    target_count: value.targetCount,
    endpoints: value.endpoints.map(presentEndpoint),
    targets: value.targets.map(presentTarget),
    legacy: presentLegacy(value.legacy),
    display_family: value.displayFamily,
    offering: value.offering,
  };
}

function presentOnboardingCommit(value: OnboardingCommitResult): OnboardingCommitView {
  return {
    connection_id: value.connectionId,
    credential_id: value.credentialId,
    replayed: value.replayed,
    target_ids: [...value.targetIds],
  };
}

export const connectionsApi = {
  list: async (): Promise<Connection[]> => {
    const value = await dashboardV4.getConnections();
    return value.connections.map(presentConnection);
  },
  /**
   * CAS tokens come from the same control-plane store `providerApi` uses.
   * Callers pass the semantic payload only; `runMutation` attaches the pair.
   * Nested V4 `revision` tokens are published by `requestV4` (no presenter
   * re-sync). On 409, `runMutation` already refreshes via V3 `GET /contract`;
   * a discarded connections GET cannot update the store list and must not
   * replace the original `revisionConflict`.
   */
  commitOnboarding: async (
    input: WithoutExpectation<OnboardingCommitRequest>,
  ): Promise<OnboardingCommitView> => {
    const control = useControlPlaneStore();
    if (!control.hasTokens()) await control.refresh();
    const value = await control.runMutation((expectation) =>
      dashboardV4.commitOnboarding(input, expectation));
    return presentOnboardingCommit(value);
  },
};
