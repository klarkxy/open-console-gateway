import { dashboardV4 } from "./dashboard-v4.ts";
import type { WithoutExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
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
  ConnectionList,
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
  credit_presets?: import("./billing.ts").CreditPreset[] | null;
}

export interface OnboardingCommitView {
  connection_id: string;
  credential_id: string | null;
  account_id: string | null;
  replayed: boolean;
  target_ids: string[];
}

export interface ConnectionListSnapshot {
  connections: Connection[];
  expectation: MutationExpectation;
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
    credit_presets: value.creditPresets ?? null,
  };
}

function presentOnboardingCommit(value: OnboardingCommitResult): OnboardingCommitView {
  return {
    connection_id: value.connectionId,
    credential_id: value.credentialId,
    account_id: value.accountId ?? null,
    replayed: value.replayed,
    target_ids: [...value.targetIds],
  };
}

export function presentConnectionListSnapshot(value: ConnectionList): ConnectionListSnapshot {
  return {
    connections: value.connections.map(presentConnection),
    expectation: {
      expectedRevision: value.revision.revision,
      processGeneration: value.revision.processGeneration,
    },
  };
}

async function fetchConnectionSnapshot(): Promise<ConnectionListSnapshot> {
  const value = await dashboardV4.getConnections();
  return presentConnectionListSnapshot(value);
}

async function withCas<T>(
  run: (expectation: MutationExpectation) => Promise<T>,
  captured?: MutationExpectation,
): Promise<T> {
  const control = useControlPlaneStore();
  if (!captured && !control.hasTokens()) await control.refresh();
  return control.runMutation(run, captured);
}

export const connectionsApi = {
  list: async (): Promise<Connection[]> => {
    const snapshot = await fetchConnectionSnapshot();
    return snapshot.connections;
  },
  /**
   * Presented connections plus the GET's own CAS pair. Callers that open an
   * editor must capture this view pair; a later global control-plane GET
   * must not silently rebase the draft.
   */
  listSnapshot: fetchConnectionSnapshot,
  /**
   * CAS tokens come from the same control-plane store `providerApi` uses.
   * Pass `expectation` from the open form so a later GET cannot silently
   * rebase the draft. Omit it to use the store's current pair.
   * Nested V4 `revision` tokens are published by `requestV4` (no presenter
   * re-sync). On 409, `runMutation` already refreshes via V3 `GET /contract`
   * and never auto-replays.
   */
  commitOnboarding: async (
    input: WithoutExpectation<OnboardingCommitRequest>,
    expectation?: MutationExpectation,
  ): Promise<OnboardingCommitView> => {
    const value = await withCas(
      (tokens) => dashboardV4.commitOnboarding(input, tokens),
      expectation,
    );
    return presentOnboardingCommit(value);
  },
};
