//! Shared Dashboard V4 wire types and the JSON Schema catalog.
//!
//! V4 is a parallel additive control plane. Response objects serialize nullable
//! fields as `T | null`. Listings and onboarding results are secret-free. The
//! error envelope reuses the V3 DTO so clients can share one decoder.

use schemars::JsonSchema;
use schemars::generate::{SchemaGenerator, SchemaSettings};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::dashboard_v3::{
    AccountAuthScheme, AccountCredentialKind, AccountUpstreamProtocol, ControlRevision,
    MutationExpectation, ProviderDefinitionAuthKind, V3Error,
};
use ocg_domain::connection::{
    AuthorizationState, ConnectionLifecycle as DomainConnectionLifecycle, ConnectionOrigin,
    EligibilityReason, EligibilityState, EndpointAuthScheme, EndpointOperation,
    LegacyConnectionKind,
};
use ocg_domain::credential::{
    AuthState, CredentialPurpose, IdentityConfidence, MaterialKind, ModelScope, OnboardingTaskKind,
    OnboardingTaskState, QuotaPeriod, QuotaPolicyMode, QuotaSubject, RelationConfidence,
    RuntimeSubjectKind, SubscriptionSource,
};

/// JSON Schema `$defs` names for the V4 catalog.
pub const CATALOG_TYPE_NAMES: &[&str] = &[
    "ControlRevision",
    "V3Error",
    "EndpointSpec",
    "ProviderTemplate",
    "TemplateList",
    "ConnectionEndpoint",
    "ConnectionTarget",
    "LegacyIdentity",
    "Eligibility",
    "TemplateRef",
    "ConnectionSummary",
    "ConnectionList",
    "OnboardingCommitRequest",
    "OnboardingCommitMode",
    "OnboardingConnection",
    "OnboardingAuthorization",
    "OnboardingTarget",
    "OnboardingCommitResult",
    "IdentityList",
    "IdentitySummary",
    "UpstreamAccountDto",
    "CredentialSummary",
    "CredentialDto",
    "BindingDto",
    "QuotaWindowDto",
    "OnboardingTaskDto",
    "SubscriptionDto",
    "DeclaredRelationDto",
    "IdentityLegacy",
    "CredentialRotateRequest",
    "CredentialRotateResult",
    "BindingPatchRequest",
    "BindingPatchResult",
    "QuotaSharing",
    "IdentityCredentialCreateRequest",
    "IdentityCredentialCreateResult",
    "CpaCatalogEntry",
    "CpaCatalog",
    "CpaCatalogUpdate",
    "CatalogModelsRemoveRequest",
    "CatalogModelsRemoveResult",
    "AliasPublication",
    "AliasPublicationUpdate",
    "DshApplicationStatus",
    "DshApplication",
    "DshApplicationInstallRequest",
    "PlatformKeyImportRequest",
    "PlatformKeyImportResult",
    "PlatformKeyImportFailure",
    "DestinationList",
    "DestinationDto",
    "DestinationCredentialDto",
    "CredentialList",
    "CapabilitiesDto",
    "PlanDto",
    "CatalogModelDto",
    "DestinationProjectionRefusedError",
    "OfficialApiKind",
    "OfficialPriceRow",
    "OfficialPriceSheet",
    "OfficialBalance",
    "OfficialSpend",
    "OfficialApiStatus",
    "OfficialApiPrices",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum DshApplicationStatus {
    UnsupportedRuntime,
    NotDetected,
    Ready,
    Installed,
    Incompatible,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DshApplication {
    pub status: DshApplicationStatus,
    pub detected: bool,
    pub installed: bool,
    pub install_supported: bool,
    pub activation_required: bool,
    pub version: Option<String>,
    pub detail: Option<String>,
    pub target_paths: Vec<String>,
    pub fingerprint: Option<String>,
    pub revision: ControlRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DshApplicationInstallRequest {
    #[serde(flatten)]
    #[schemars(flatten)]
    pub expectation: MutationExpectation,
    pub key_id: String,
    pub expected_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct EndpointSpec {
    pub operation: EndpointOperation,
    pub wire_protocol: AccountUpstreamProtocol,
    pub url: Option<String>,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum TemplateSource {
    Builtin,
    Preset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum OfferingKind {
    Plan,
    Api,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTemplate {
    pub id: String,
    pub version: u32,
    pub display_name: String,
    pub family_id: Option<String>,
    pub offering_tags: Vec<OfferingKind>,
    pub adapter_kind: String,
    pub source: TemplateSource,
    pub credential_kind: AccountCredentialKind,
    pub auth_schemes: Vec<AccountAuthScheme>,
    pub upstream_protocols: Vec<AccountUpstreamProtocol>,
    pub editable_fields: Vec<String>,
    pub default_endpoints: Vec<EndpointSpec>,
    pub pricing_multiplier_editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateList {
    pub templates: Vec<ProviderTemplate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionEndpoint {
    pub id: String,
    pub connection_id: String,
    pub operation: EndpointOperation,
    pub wire_protocol: AccountUpstreamProtocol,
    pub url: Option<String>,
    pub auth_scheme: EndpointAuthScheme,
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionTarget {
    pub id: String,
    pub connection_id: String,
    pub public_name: String,
    pub upstream_model_id: String,
    pub endpoint_ids: Vec<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyIdentity {
    pub kind: LegacyConnectionKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct Eligibility {
    pub state: EligibilityState,
    pub reason: EligibilityReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemplateRef {
    pub id: String,
    pub version: u32,
}

/// V4 connection lifecycle, including persisted onboarding drafts.
///
/// Draft is control-plane only: routing snapshots never carry it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ConnectionLifecycle {
    Configured,
    Disabled,
    Draft,
}

impl From<DomainConnectionLifecycle> for ConnectionLifecycle {
    fn from(value: DomainConnectionLifecycle) -> Self {
        match value {
            DomainConnectionLifecycle::Configured => Self::Configured,
            DomainConnectionLifecycle::Disabled => Self::Disabled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionSummary {
    pub id: String,
    pub name: String,
    pub origin: ConnectionOrigin,
    pub template_ref: Option<TemplateRef>,
    pub adapter_kind: String,
    pub lifecycle: ConnectionLifecycle,
    pub authorization: AuthorizationState,
    pub eligibility: Eligibility,
    pub credential_count: u32,
    pub enabled_credential_count: u32,
    pub target_count: u32,
    pub endpoints: Vec<ConnectionEndpoint>,
    pub targets: Vec<ConnectionTarget>,
    pub legacy: LegacyIdentity,
    pub display_family: Option<String>,
    pub offering: OfferingKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionList {
    pub revision: ControlRevision,
    pub connections: Vec<ConnectionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OnboardingCommitRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub operation_id: String,
    pub connection: OnboardingConnection,
    #[serde(default)]
    pub authorization: Option<OnboardingAuthorization>,
    pub targets: Vec<OnboardingTarget>,
    /// Omitted preserves legacy commit behavior and HMAC digest bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<OnboardingCommitMode>,
    /// Explicit consent to union safe current default/same-origin grants.
    /// Ignored unless `mode` is present; false is omitted from the digest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub authorize_current_endpoint: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum OnboardingCommitMode {
    Draft,
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum OnboardingConnection {
    New(OnboardingConnectionNew),
    Existing(OnboardingConnectionExisting),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OnboardingConnectionNew {
    pub template_id: String,
    pub name: String,
    pub endpoint_url: String,
    pub upstream_protocol: AccountUpstreamProtocol,
    pub auth_kind: ProviderDefinitionAuthKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OnboardingConnectionExisting {
    pub connection_id: String,
    /// Required when `mode` is present. Legacy mode-None existing rejects this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configuration: Option<OnboardingConnectionNew>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum OnboardingAuthorization {
    ApiKey(OnboardingAuthorizationApiKey),
    None {},
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OnboardingAuthorizationApiKey {
    pub secret_input: String,
    #[serde(default)]
    pub account_label: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OnboardingTarget {
    pub public_model: String,
    pub upstream_model: String,
    #[serde(default)]
    pub upstream_override: Option<OnboardingUpstreamOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OnboardingUpstreamOverride {
    pub protocol: AccountUpstreamProtocol,
    pub endpoint_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OnboardingCommitResult {
    pub revision: ControlRevision,
    pub connection_id: String,
    pub credential_id: Option<String>,
    pub target_ids: Vec<String>,
    pub replayed: bool,
    /// Legacy receipts omit this; replay emits null via serde default.
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredOnboardingCommitResult {
    pub connection_id: String,
    pub credential_id: Option<String>,
    pub target_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum IdentityLegacyKind {
    Account,
    PlatformAccount,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityLegacy {
    pub kind: IdentityLegacyKind,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityRefDto {
    pub issuer_or_site: String,
    pub tenant_or_subject: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpstreamAccountDto {
    pub id: String,
    pub label: String,
    pub authority_ref: Option<AuthorityRefDto>,
    pub identity_confidence: IdentityConfidence,
    pub enabled: bool,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialDto {
    pub id: String,
    pub purpose: CredentialPurpose,
    pub material_kind: MaterialKind,
    pub secret_ref: String,
    pub has_material: bool,
    pub version: u64,
    pub enabled: bool,
    pub auth_state: AuthState,
    pub auth_state_version: u64,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingDto {
    pub id: String,
    pub connection_id: String,
    pub allowed_endpoint_ids: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub model_scope: ModelScope,
    pub enabled: bool,
    pub routing_rank: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuotaWindowDto {
    pub subject: QuotaSubject,
    pub subject_ref: String,
    pub period: QuotaPeriod,
    pub blocked_until: Option<String>,
    pub metric: Option<QuotaMetricDto>,
    pub relation_confidence: RelationConfidence,
    pub policy_mode: QuotaPolicyMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuotaMetricDto {
    pub remaining: Option<f64>,
    pub limit: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct OnboardingTaskDto {
    pub id: String,
    pub kind: OnboardingTaskKind,
    pub step: String,
    pub state: OnboardingTaskState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubscriptionDto {
    pub source: SubscriptionSource,
    pub purchase_date: String,
    pub expires_on: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclaredRelationDto {
    pub platform_account_id: String,
    pub group: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialSummary {
    pub credential: CredentialDto,
    pub subject: RuntimeSubjectKind,
    pub bindings: Vec<BindingDto>,
    pub quota_windows: Vec<QuotaWindowDto>,
    #[serde(default)]
    pub quota_pool_id: Option<String>,
    pub onboarding_task: Option<OnboardingTaskDto>,
    pub subscription: Option<SubscriptionDto>,
    pub last_error: Option<String>,
    pub legacy: IdentityLegacy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentitySummary {
    pub identity: UpstreamAccountDto,
    pub credentials: Vec<CredentialSummary>,
    pub declared_relations: Vec<DeclaredRelationDto>,
    pub legacy: IdentityLegacy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityList {
    pub revision: ControlRevision,
    pub identities: Vec<IdentitySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialRotateRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub secret_input: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialRotateResult {
    pub revision: ControlRevision,
    pub credential_id: String,
    pub version: u64,
    pub auth_state_version: u64,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingPatchRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_scope: Option<ModelScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_endpoint_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_origins: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingPatchResult {
    pub revision: ControlRevision,
    pub binding: BindingDto,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[schemars(rename_all = "camelCase")]
pub enum QuotaSharing {
    #[default]
    Independent,
    Shared {
        #[serde(rename = "credentialId")]
        #[schemars(rename = "credentialId")]
        credential_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityCredentialCreateRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub connection_id: String,
    pub secret_input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "quota_sharing_is_independent")]
    pub quota_sharing: QuotaSharing,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_label: Option<String>,
}

fn quota_sharing_is_independent(value: &QuotaSharing) -> bool {
    matches!(value, QuotaSharing::Independent)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityCredentialCreateResult {
    pub revision: ControlRevision,
    pub identity_id: String,
    pub credential_id: String,
    pub binding_id: String,
    pub account_id: String,
    pub connection_id: String,
    pub version: u64,
    pub auth_state_version: u64,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaCatalogEntry {
    pub id: String,
    pub owned_by: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaCatalog {
    pub revision: ControlRevision,
    pub models: Vec<CpaCatalogEntry>,
    pub source_url: Option<String>,
    pub refreshed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CpaCatalogUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub enabled_ids: Vec<String>,
}

/// Remove models from a persisted built-in Provider catalog snapshot.
///
/// Local-only. An official catalog refresh may add the same IDs back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogModelsRemoveRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub model_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogModelsRemoveResult {
    pub revision: ControlRevision,
    pub removed_ids: Vec<String>,
    pub catalog_models: Vec<String>,
}

/// Public names currently hidden from authenticated `GET /v1/models`.
///
/// Missing names default to published. Hidden names remain routable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AliasPublication {
    pub revision: ControlRevision,
    pub unpublished: Vec<String>,
}

/// Toggle one public name's downstream listing. `publicModel` is
/// case-folded on write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct AliasPublicationUpdate {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
    pub public_model: String,
    pub published: bool,
}

/// Import New API inference tokens as local Custom Keys. Secret-free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformKeyImportRequest {
    #[serde(flatten)]
    pub expectation: MutationExpectation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformKeyImportFailure {
    pub name: String,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformKeyImportResult {
    pub imported: u32,
    pub skipped_existing: u32,
    pub skipped_disabled: u32,
    pub failed: Vec<PlatformKeyImportFailure>,
    pub revision: ControlRevision,
}

/// Sealed adapter kind. Wire values match the domain serde names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum AdapterKindDto {
    OpencodeGo,
    Zen,
    Goat,
    Minimax,
    Kimi,
    Ollama,
    Cpa,
    Http,
}

/// Destination auth scheme. Wire values match the domain serde names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum AuthSchemeDto {
    None,
    Bearer,
    XApiKey,
}

/// Wire protocol for destination catalog and transport rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ProtocolDto {
    ChatCompletions,
    Responses,
    Messages,
}

/// Redirect policy advertised on a destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum RedirectPolicyDto {
    NoFollow,
    FollowKeyless,
}

/// Where a Plan reads usage from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum UsageSourceDto {
    OfficialApi,
    LocalProjection,
    None,
}

/// One Plan usage window kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum PlanWindowKindDto {
    FiveHours,
    Week,
    Month,
    Free,
}

/// One Plan usage window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanWindowDto {
    pub kind: PlanWindowKindDto,
}

/// How a Plan expires, when it has a cadence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum ExpiryCadenceDto {
    Monthly,
}

/// Where a Plan's prices come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum PricingSourceDto {
    Official,
    VerifiedSnapshot,
    Unpriced,
}

/// Destination capability flags. Mirrors the domain `Capabilities` record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapabilitiesDto {
    /// Test connection is offered on every ready credential except external integrations.
    pub testable: bool,
    /// The destination can discover models from the upstream.
    pub discoverable_models: bool,
    /// Hosts Configurable HTTP may probe for an official current-balance API.
    pub official_balance_probe: Vec<String>,
    /// The destination holds a non-inference observer credential (platform parent).
    pub observer: bool,
    /// Managed signup can start an onboarding task on a credential.
    pub managed_signup: bool,
    /// The destination is an external integration and holds no local inference secret.
    pub external_integration: bool,
    /// A billing tier must be selected before the destination is usable.
    pub billing_tier_required: bool,
    pub redirect_policy: RedirectPolicyDto,
    /// The adapter attaches identity headers on egress.
    pub identity_headers: bool,
}

/// Commercial offering embedded in a destination. Mirrors the domain `Plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanDto {
    pub usage_source: UsageSourceDto,
    pub windows: Vec<PlanWindowDto>,
    pub expiry_cadence: Option<ExpiryCadenceDto>,
    pub pricing_source: PricingSourceDto,
    /// Operators may enter local usage numbers by hand.
    pub manual_calibration: bool,
}

/// One catalog model on a destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogModelDto {
    /// Public name exposed to clients.
    pub public_model: String,
    /// Upstream model id sent on egress.
    pub upstream_model: String,
    pub protocols: Vec<ProtocolDto>,
    pub preferred: Option<ProtocolDto>,
    /// Whether any protocol is enabled.
    pub enabled: bool,
}

/// Which V3-era row a destination was projected from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum LegacyDestinationKindDto {
    /// Sealed builtin; `id` is the `provider_id`.
    Builtin,
    /// User-defined Provider; `id` is the dynamic `provider_id`.
    Dynamic,
    /// Account-owned Custom API endpoint; `id` is the account id.
    CustomAccount,
    /// New API / Sub2API site; `id` is the platform parent id.
    PlatformParent,
}

/// Migration-era bridge from a projected destination back to the V3 row that
/// still owns its mutations. Removed once destinations become primary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyDestinationRefDto {
    pub kind: LegacyDestinationKindDto,
    /// Row id in the V3 model named by `kind`.
    pub id: String,
}

/// RFC destination projection row. Secret-free; field-for-field from the domain `Destination`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationDto {
    /// Stable destination id (deterministic UUIDv5 of the legacy row).
    pub id: String,
    /// V3 row this destination was projected from.
    pub legacy: LegacyDestinationRefDto,
    pub adapter: AdapterKindDto,
    /// Display name.
    pub name: String,
    /// Brand family for grouping, when the adapter or platform kind has one.
    pub brand_family: Option<String>,
    /// Upstream origin. Sealed for builtin adapters; user data for `http`.
    pub base_url: Option<String>,
    pub protocols: Vec<ProtocolDto>,
    pub auth_scheme: AuthSchemeDto,
    pub catalog: Vec<CatalogModelDto>,
    pub capabilities: CapabilitiesDto,
    pub plan: Option<PlanDto>,
    /// `1` for singletons and account-owned endpoints; `null` otherwise.
    pub max_credentials: Option<u32>,
    /// Non-inference observer credential id when `capabilities.observer` is set.
    pub observer_credential_id: Option<String>,
    /// Destination enablement.
    pub enabled: bool,
}

/// RFC destination list. Revision-tagged and secret-free.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationList {
    pub revision: ControlRevision,
    pub destinations: Vec<DestinationDto>,
}

/// Allowed egress grants on a projected credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialGrantsDto {
    /// Endpoint ids this secret may be sent to.
    pub allowed_endpoint_ids: Vec<String>,
    /// Origins this secret may be sent to.
    pub allowed_origins: Vec<String>,
}

/// Per-window cooldown timestamps on a projected credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialCooldownsDto {
    /// Generic cooldown expiry as RFC 3339, or null.
    pub generic_until: Option<String>,
    /// Five-hour window cooldown expiry as RFC 3339, or null.
    pub five_hour_until: Option<String>,
    /// Weekly window cooldown expiry as RFC 3339, or null.
    pub week_until: Option<String>,
    /// Monthly window cooldown expiry as RFC 3339, or null.
    pub month_until: Option<String>,
    /// Free-window cooldown expiry as RFC 3339, or null.
    pub free_until: Option<String>,
}

/// Managed-signup task on a projected credential. Domain shape; no task id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationOnboardingTaskDto {
    pub kind: OnboardingTaskKind,
    pub state: OnboardingTaskState,
    /// Current setup step string.
    pub step: String,
}

/// RFC credential projection row. Carries `hasSecret` only; never ciphertext or plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationCredentialDto {
    /// Stable credential id (deterministic UUIDv5 of the legacy account).
    pub id: String,
    /// The V3 `accounts` row this credential was projected from (migration-era
    /// bridge to the V3 mutation routes).
    pub legacy_account_id: String,
    /// Destination this credential routes through.
    pub destination_id: String,
    /// Display name.
    pub name: String,
    /// Operator notes.
    pub notes: Option<String>,
    /// Whether a secret is stored. Always false for keyless Zen and CPA.
    pub has_secret: bool,
    /// Routing enable switch (`account.enabled && binding.enabled`).
    pub enabled: bool,
    /// Position in the persisted account order.
    pub routing_rank: u32,
    pub scope: ModelScope,
    pub grants: CredentialGrantsDto,
    pub auth_state: AuthState,
    /// Last error string from the legacy account row, or null.
    pub last_error: Option<String>,
    pub cooldowns: CredentialCooldownsDto,
    /// Shared quota pool id when the credential joined one.
    pub quota_pool_id: Option<String>,
    pub onboarding_task: Option<DestinationOnboardingTaskDto>,
    /// Purchase date when the destination has a Plan.
    pub purchase_date: Option<String>,
}

/// RFC credential list. Revision-tagged and secret-free.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialList {
    pub revision: ControlRevision,
    pub credentials: Vec<DestinationCredentialDto>,
}

/// Legacy row kind named in a destination-projection refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename_all = "snake_case")]
pub enum RefusedRowKindDto {
    Account,
    DynamicProvider,
    PlatformParent,
}

/// Identity of one legacy row the stage-4a projection refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct RefusedRowDto {
    pub kind: RefusedRowKindDto,
    /// Persisted row id.
    pub id: String,
    /// Provider id when the refused row is an account.
    pub provider_id: Option<String>,
}

/// Mapping-error variant name in camelCase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase")]
pub enum MappingErrorCodeDto {
    UnknownProvider,
    MissingDestination,
    CustomRequiresAccount,
    CustomAccountMissingEndpoint,
    DynamicMissingEndpoint,
    PlatformMissingBaseUrl,
}

/// One refused row in a `destinationProjectionRefused` 409.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationProjectionRefusalDto {
    pub row: RefusedRowDto,
    pub error: MappingErrorCodeDto,
    /// Human-readable mapping error.
    pub detail: String,
}

/// 409 envelope when `destination_projection` refuses. Same V3 fields plus `details`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename_all = "camelCase", deny_unknown_fields)]
pub struct DestinationProjectionRefusedError {
    /// Stable error code (`destinationProjectionRefused`).
    pub code: String,
    /// Human-readable summary.
    pub message: String,
    /// Live settings revision.
    pub current_revision: Option<u64>,
    /// Live process generation.
    pub process_generation: Option<u64>,
    pub details: Vec<DestinationProjectionRefusalDto>,
}

/// Deterministic JSON Schema catalog for the V4 contract.
///
/// Generator settings match V3: draft 2020-12, serialize-mode for response
/// DTOs so `Option` fields stay required `T | null`. Request DTOs use the
/// deserialize contract so optional fields may be omitted.
pub fn contract_schema() -> Value {
    let mut serialize = SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator();
    include_type::<ControlRevision>(&mut serialize);
    include_type::<V3Error>(&mut serialize);
    include_type::<EndpointSpec>(&mut serialize);
    include_type::<ProviderTemplate>(&mut serialize);
    include_type::<TemplateList>(&mut serialize);
    include_type::<ConnectionEndpoint>(&mut serialize);
    include_type::<ConnectionTarget>(&mut serialize);
    include_type::<LegacyIdentity>(&mut serialize);
    include_type::<Eligibility>(&mut serialize);
    include_type::<TemplateRef>(&mut serialize);
    include_type::<ConnectionSummary>(&mut serialize);
    include_type::<ConnectionList>(&mut serialize);
    include_type::<OnboardingCommitResult>(&mut serialize);
    include_type::<IdentityList>(&mut serialize);
    include_type::<IdentitySummary>(&mut serialize);
    include_type::<UpstreamAccountDto>(&mut serialize);
    include_type::<CredentialSummary>(&mut serialize);
    include_type::<CredentialDto>(&mut serialize);
    include_type::<BindingDto>(&mut serialize);
    include_type::<QuotaWindowDto>(&mut serialize);
    include_type::<OnboardingTaskDto>(&mut serialize);
    include_type::<SubscriptionDto>(&mut serialize);
    include_type::<DeclaredRelationDto>(&mut serialize);
    include_type::<IdentityLegacy>(&mut serialize);
    include_type::<CredentialRotateResult>(&mut serialize);
    include_type::<BindingPatchResult>(&mut serialize);
    include_type::<QuotaSharing>(&mut serialize);
    include_type::<IdentityCredentialCreateResult>(&mut serialize);
    include_type::<CpaCatalogEntry>(&mut serialize);
    include_type::<CpaCatalog>(&mut serialize);
    include_type::<CatalogModelsRemoveResult>(&mut serialize);
    include_type::<AliasPublication>(&mut serialize);
    include_type::<DshApplicationStatus>(&mut serialize);
    include_type::<DshApplication>(&mut serialize);
    include_type::<PlatformKeyImportResult>(&mut serialize);
    include_type::<PlatformKeyImportFailure>(&mut serialize);
    include_type::<DestinationList>(&mut serialize);
    include_type::<DestinationDto>(&mut serialize);
    include_type::<DestinationCredentialDto>(&mut serialize);
    include_type::<CredentialList>(&mut serialize);
    include_type::<CapabilitiesDto>(&mut serialize);
    include_type::<PlanDto>(&mut serialize);
    include_type::<CatalogModelDto>(&mut serialize);
    include_type::<DestinationProjectionRefusedError>(&mut serialize);
    include_type::<crate::official_api::OfficialApiStatus>(&mut serialize);
    include_type::<crate::official_api::OfficialApiPrices>(&mut serialize);
    let mut defs = serialize.take_definitions(true);

    let mut deserialize = SchemaSettings::draft2020_12().into_generator();
    include_type::<OnboardingCommitRequest>(&mut deserialize);
    include_type::<OnboardingCommitMode>(&mut deserialize);
    include_type::<OnboardingConnection>(&mut deserialize);
    include_type::<OnboardingAuthorization>(&mut deserialize);
    include_type::<OnboardingTarget>(&mut deserialize);
    include_type::<CredentialRotateRequest>(&mut deserialize);
    include_type::<BindingPatchRequest>(&mut deserialize);
    include_type::<QuotaSharing>(&mut deserialize);
    include_type::<IdentityCredentialCreateRequest>(&mut deserialize);
    include_type::<CpaCatalogUpdate>(&mut deserialize);
    include_type::<CatalogModelsRemoveRequest>(&mut deserialize);
    include_type::<AliasPublicationUpdate>(&mut deserialize);
    include_type::<DshApplicationInstallRequest>(&mut deserialize);
    include_type::<PlatformKeyImportRequest>(&mut deserialize);
    for (name, schema) in deserialize.take_definitions(true) {
        defs.entry(name).or_insert(schema);
    }

    for name in CATALOG_TYPE_NAMES {
        if !defs.contains_key(*name) {
            panic!("dashboard v4 schema catalog is missing $defs/{name}");
        }
    }

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "DashboardApiV4",
        "$comment": "Extensible Dashboard V4 contract catalog. Add new $defs for later DTOs; do not rename or reshape existing definitions. Connection, template, destination, and credential listings are secret-free. OnboardingCommitRequest.secretInput, CredentialRotateRequest.secretInput, and IdentityCredentialCreateRequest.secretInput are write-only.",
        "anyOf": catalog_refs(&defs),
        "$defs": defs })
}

/// Pretty-printed catalog JSON with a trailing newline.
pub fn contract_schema_pretty() -> String {
    let mut encoded = serde_json::to_string_pretty(&contract_schema())
        .expect("dashboard v4 schema should serialize");
    if !encoded.ends_with('\n') {
        encoded.push('\n');
    }
    encoded
}

fn include_type<T: JsonSchema>(generator: &mut SchemaGenerator) {
    generator.subschema_for::<T>();
}

fn catalog_refs(defs: &Map<String, Value>) -> Vec<Value> {
    CATALOG_TYPE_NAMES
        .iter()
        .filter(|name| defs.contains_key(**name))
        .map(|name| json!({ "$ref": format!("#/$defs/{name}") }))
        .collect()
}

#[cfg(test)]
mod tests;
