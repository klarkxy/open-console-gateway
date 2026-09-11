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
    AuthorizationState, ConnectionLifecycle, ConnectionOrigin, EligibilityReason, EligibilityState,
    EndpointAuthScheme, EndpointOperation, LegacyConnectionKind,
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
];

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StoredOnboardingCommitResult {
    pub connection_id: String,
    pub credential_id: Option<String>,
    pub target_ids: Vec<String>,
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
    let mut defs = serialize.take_definitions(true);

    let mut deserialize = SchemaSettings::draft2020_12().into_generator();
    include_type::<OnboardingCommitRequest>(&mut deserialize);
    include_type::<OnboardingConnection>(&mut deserialize);
    include_type::<OnboardingAuthorization>(&mut deserialize);
    include_type::<OnboardingTarget>(&mut deserialize);
    include_type::<CredentialRotateRequest>(&mut deserialize);
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
        "$comment": "Extensible Dashboard V4 contract catalog. Add new $defs for later DTOs; do not rename or reshape existing definitions. Connection and template listings are secret-free. OnboardingCommitRequest.secretInput and CredentialRotateRequest.secretInput are write-only.",
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
