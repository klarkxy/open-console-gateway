/**
 * Dashboard V4 identity projection presenter.
 *
 * `GET /dashboard/api/v4/accounts` is secret-free. The wire uses camelCase;
 * the view model is snake_case, matching `connections.ts`. Join key is
 * `legacy.kind+id` (V3 account id or platform_account id).
 */

import { useControlPlaneStore } from "../stores/controlPlane.ts";
import { dashboardV4 } from "./dashboard-v4.ts";
import type { WithoutExpectation } from "./dashboard-v3.ts";
import type { MutationExpectation } from "./generated/dashboard-v3.ts";
import type {
  AuthState,
  AuthorityRefDto,
  BindingDto,
  BindingPatchRequest,
  BindingPatchResult,
  CredentialDto,
  CredentialPurpose,
  CredentialRotateRequest,
  CredentialRotateResult,
  CredentialSummary,
  DeclaredRelationDto,
  IdentityConfidence,
  IdentityCredentialCreateRequest,
  IdentityCredentialCreateResult,
  IdentityLegacy as V4IdentityLegacy,
  IdentityLegacyKind,
  IdentityList,
  IdentitySummary,
  MaterialKind,
  ModelScope,
  OnboardingTaskDto,
  QuotaMetricDto,
  QuotaPeriod,
  QuotaPolicyMode,
  QuotaSharing,
  QuotaSubject,
  QuotaWindowDto,
  RelationConfidence,
  RuntimeSubjectKind,
  SubscriptionDto,
  SubscriptionSource,
  UpstreamAccountDto,
} from "./generated/dashboard-v4.ts";

export type {
  AuthState,
  CredentialPurpose,
  IdentityConfidence,
  IdentityLegacyKind,
  MaterialKind,
  ModelScope,
  QuotaPeriod,
  QuotaPolicyMode,
  QuotaSharing,
  QuotaSubject,
  RelationConfidence,
  RuntimeSubjectKind,
  SubscriptionSource,
};

export type IdentityCredentialCreateInput = WithoutExpectation<IdentityCredentialCreateRequest>;
export type BindingPatchInput = WithoutExpectation<BindingPatchRequest>;

export interface IdentityLegacy {
  kind: IdentityLegacyKind;
  id: string;
}

export interface AuthorityRef {
  issuer_or_site: string;
  tenant_or_subject: string | null;
}

export interface UpstreamAccount {
  id: string;
  label: string;
  authority_ref: AuthorityRef | null;
  identity_confidence: IdentityConfidence;
  enabled: boolean;
  notes: string | null;
}

export interface IdentityCredentialRecord {
  id: string;
  purpose: CredentialPurpose;
  material_kind: MaterialKind;
  has_material: boolean;
  version: number;
  enabled: boolean;
  auth_state: AuthState;
  auth_state_version: number;
  expires_at: string | null;
}

export interface IdentityBinding {
  id: string;
  connection_id: string;
  allowed_endpoint_ids: string[];
  allowed_origins: string[];
  model_scope: ModelScope;
  enabled: boolean;
  routing_rank: number;
}

export interface IdentityOnboardingTask {
  id: string;
  kind: OnboardingTaskDto["kind"];
  state: OnboardingTaskDto["state"];
  step: string;
}

export interface IdentityQuotaMetric {
  limit: number | null;
  remaining: number | null;
}

export interface IdentityQuotaWindow {
  blocked_until: string | null;
  metric: IdentityQuotaMetric | null;
  period: QuotaPeriod;
  policy_mode: QuotaPolicyMode;
  relation_confidence: RelationConfidence;
  subject: QuotaSubject;
  subject_ref: string;
}

export interface IdentitySubscription {
  expires_on: string;
  purchase_date: string;
  source: SubscriptionSource;
}

export interface IdentityCredential {
  bindings: IdentityBinding[];
  credential: IdentityCredentialRecord;
  last_error: string | null;
  legacy: IdentityLegacy;
  onboarding_task: IdentityOnboardingTask | null;
  quota_pool_id: string | null;
  quota_windows: IdentityQuotaWindow[];
  subject: RuntimeSubjectKind;
  subscription: IdentitySubscription | null;
}

export interface DeclaredRelation {
  group: string;
  platform_account_id: string;
}

export interface Identity {
  credentials: IdentityCredential[];
  declared_relations: DeclaredRelation[];
  identity: UpstreamAccount;
  legacy: IdentityLegacy;
}

/** Stable overlay join: `legacy.kind+id` (account id or platform_account id). */
export function identityJoinKey(legacy: Pick<IdentityLegacy, "kind" | "id">): string {
  return `${legacy.kind}+${legacy.id}`;
}

function presentLegacy(value: V4IdentityLegacy): IdentityLegacy {
  return { kind: value.kind, id: value.id };
}

function presentAuthority(value: AuthorityRefDto | null): AuthorityRef | null {
  if (!value) return null;
  return {
    issuer_or_site: value.issuerOrSite,
    tenant_or_subject: value.tenantOrSubject,
  };
}

function presentUpstream(value: UpstreamAccountDto): UpstreamAccount {
  return {
    id: value.id,
    label: value.label,
    authority_ref: presentAuthority(value.authorityRef),
    identity_confidence: value.identityConfidence,
    enabled: value.enabled,
    notes: value.notes,
  };
}

function presentCredentialRecord(value: CredentialDto): IdentityCredentialRecord {
  return {
    id: value.id,
    purpose: value.purpose,
    material_kind: value.materialKind,
    has_material: value.hasMaterial,
    version: value.version,
    enabled: value.enabled,
    auth_state: value.authState,
    auth_state_version: value.authStateVersion,
    expires_at: value.expiresAt,
  };
}

function presentBinding(value: BindingDto): IdentityBinding {
  return {
    id: value.id,
    connection_id: value.connectionId,
    allowed_endpoint_ids: [...value.allowedEndpointIds],
    allowed_origins: [...value.allowedOrigins],
    model_scope: value.modelScope,
    enabled: value.enabled,
    routing_rank: value.routingRank,
  };
}

function presentOnboarding(value: OnboardingTaskDto | null): IdentityOnboardingTask | null {
  if (!value) return null;
  return {
    id: value.id,
    kind: value.kind,
    state: value.state,
    step: value.step,
  };
}

function presentMetric(value: QuotaMetricDto | null): IdentityQuotaMetric | null {
  if (!value) return null;
  return { limit: value.limit, remaining: value.remaining };
}

function presentQuotaWindow(value: QuotaWindowDto): IdentityQuotaWindow {
  return {
    blocked_until: value.blockedUntil,
    metric: presentMetric(value.metric),
    period: value.period,
    policy_mode: value.policyMode,
    relation_confidence: value.relationConfidence,
    subject: value.subject,
    subject_ref: value.subjectRef,
  };
}

function presentSubscription(value: SubscriptionDto | null): IdentitySubscription | null {
  if (!value) return null;
  return {
    expires_on: value.expiresOn,
    purchase_date: value.purchaseDate,
    source: value.source,
  };
}

function presentCredential(value: CredentialSummary): IdentityCredential {
  return {
    bindings: value.bindings.map(presentBinding),
    credential: presentCredentialRecord(value.credential),
    last_error: value.lastError,
    legacy: presentLegacy(value.legacy),
    onboarding_task: presentOnboarding(value.onboardingTask),
    quota_pool_id: value.quotaPoolId ?? null,
    quota_windows: value.quotaWindows.map(presentQuotaWindow),
    subject: value.subject,
    subscription: presentSubscription(value.subscription),
  };
}

function presentRelation(value: DeclaredRelationDto): DeclaredRelation {
  return {
    group: value.group,
    platform_account_id: value.platformAccountId,
  };
}

export function presentIdentity(value: IdentitySummary): Identity {
  return {
    credentials: value.credentials.map(presentCredential),
    declared_relations: value.declaredRelations.map(presentRelation),
    identity: presentUpstream(value.identity),
    legacy: presentLegacy(value.legacy),
  };
}

export function presentIdentityList(value: IdentityList): Identity[] {
  return value.identities.map(presentIdentity);
}

export interface IdentityListSnapshot {
  identities: Identity[];
  expectation: MutationExpectation;
}

export function presentIdentityListSnapshot(value: IdentityList): IdentityListSnapshot {
  return {
    identities: presentIdentityList(value),
    expectation: {
      expectedRevision: value.revision.revision,
      processGeneration: value.revision.processGeneration,
    },
  };
}

export interface CreatedIdentityCredential {
  account_id: string;
  auth_state_version: number;
  binding_id: string;
  connection_id: string;
  credential_id: string;
  identity_id: string;
  replayed: boolean;
  version: number;
}

export interface RotatedCredential {
  auth_state_version: number;
  credential_id: string;
  replayed: boolean;
  version: number;
}

export interface PatchedBinding {
  binding: IdentityBinding;
}

function presentRotatedCredential(value: CredentialRotateResult): RotatedCredential {
  return {
    auth_state_version: value.authStateVersion,
    credential_id: value.credentialId,
    replayed: value.replayed,
    version: value.version,
  };
}

function presentPatchedBinding(value: BindingPatchResult): PatchedBinding {
  return { binding: presentBinding(value.binding) };
}

function presentCreatedCredential(value: IdentityCredentialCreateResult): CreatedIdentityCredential {
  return {
    account_id: value.accountId,
    auth_state_version: value.authStateVersion,
    binding_id: value.bindingId,
    connection_id: value.connectionId,
    credential_id: value.credentialId,
    identity_id: value.identityId,
    replayed: value.replayed,
    version: value.version,
  };
}

async function fetchIdentitySnapshot(): Promise<IdentityListSnapshot> {
  const value = await dashboardV4.getAccounts();
  return presentIdentityListSnapshot(value);
}

async function withCas<T>(
  run: (expectation: MutationExpectation) => Promise<T>,
  captured?: MutationExpectation,
): Promise<T> {
  const control = useControlPlaneStore();
  if (!captured && !control.hasTokens()) await control.refresh();
  return control.runMutation(run, captured);
}

export const identitiesApi = {
  list: async (): Promise<Identity[]> => {
    const snapshot = await fetchIdentitySnapshot();
    return snapshot.identities;
  },
  /**
   * Presented identities plus the GET's own CAS pair. Callers that open an
   * editor must capture this view pair; a later global control-plane GET
   * must not silently rebase the draft.
   */
  listSnapshot: fetchIdentitySnapshot,
  /**
   * Replace the Key on one credential. Pass `expectation` from the open form
   * so a later GET cannot silently rebase the draft. Omit it to use the
   * store's current pair. `runMutation` never auto-replays a 409.
   */
  rotateCredential: async (
    id: string,
    input: WithoutExpectation<CredentialRotateRequest>,
    expectation?: MutationExpectation,
  ): Promise<RotatedCredential> => {
    const value = await withCas(
      (tokens) => dashboardV4.rotateCredential(id, input, tokens),
      expectation,
    );
    return presentRotatedCredential(value);
  },
  /**
   * Edit one inference binding's enabled flag and/or model scope. Same CAS
   * pairing as rotate; a 409 refreshes tokens and surfaces the original error.
   */
  patchBinding: async (
    id: string,
    input: BindingPatchInput,
    expectation?: MutationExpectation,
  ): Promise<PatchedBinding> => {
    const value = await withCas(
      (tokens) => dashboardV4.patchBinding(id, input, tokens),
      expectation,
    );
    return presentPatchedBinding(value);
  },
  /**
   * Add an inference Key on an existing identity. Pass `expectation` from
   * the open view so a newer global revision cannot rebase the draft.
   * `runMutation` never auto-replays a 409.
   */
  createIdentityCredential: async (
    id: string,
    input: IdentityCredentialCreateInput,
    expectation?: MutationExpectation,
  ): Promise<CreatedIdentityCredential> => {
    const value = await withCas(
      (tokens) => dashboardV4.createIdentityCredential(id, input, tokens),
      expectation,
    );
    return presentCreatedCredential(value);
  },
};
