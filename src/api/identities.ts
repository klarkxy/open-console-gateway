/**
 * Dashboard V4 identity projection presenter.
 *
 * `GET /dashboard/api/v4/accounts` is secret-free. The wire uses camelCase;
 * the view model is snake_case, matching `connections.ts`. Join key is
 * `legacy.kind+id` (V3 account id or platform_account id).
 */

import { dashboardV4 } from "./dashboard-v4.ts";
import type {
  AuthState,
  AuthorityRefDto,
  BindingDto,
  CredentialDto,
  CredentialPurpose,
  CredentialSummary,
  DeclaredRelationDto,
  IdentityConfidence,
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
  QuotaSubject,
  RelationConfidence,
  RuntimeSubjectKind,
  SubscriptionSource,
};

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

export const identitiesApi = {
  list: async (): Promise<Identity[]> => {
    const value = await dashboardV4.getAccounts();
    return presentIdentityList(value);
  },
};
