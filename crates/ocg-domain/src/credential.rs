//! Identity, credential, and binding projections for the account model split.
//!
//! These types are I/O-free. New ids are deterministic UUIDv5 values over
//! [`crate::connection::CONNECTION_ID_NAMESPACE`] so migrations and projections
//! cannot drift.

use crate::connection::{
    CONNECTION_ID_NAMESPACE, ConnectionId, EndpointOperation, endpoint_id_for,
    endpoint_id_for_route,
};
use crate::ids::model_ids_match;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

#[cfg(feature = "schemars")]
use schemars::JsonSchema;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "schemars", derive(JsonSchema))]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

string_id!(IdentityId);
string_id!(CredentialId);
string_id!(BindingId);
string_id!(QuotaPoolId);
string_id!(OnboardingTaskId);

fn namespaced_uuid(name: &str) -> String {
    Uuid::new_v5(&CONNECTION_ID_NAMESPACE, name.as_bytes()).to_string()
}

/// Deterministic identity id for a legacy `accounts` row.
pub fn identity_id_for_legacy_account(account_id: &str) -> IdentityId {
    IdentityId(namespaced_uuid(&format!("identity:account:{account_id}")))
}

/// Deterministic identity id for a `platform_accounts` parent.
pub fn identity_id_for_platform_account(platform_account_id: &str) -> IdentityId {
    IdentityId(namespaced_uuid(&format!(
        "identity:platform:{platform_account_id}"
    )))
}

/// Deterministic credential id for a legacy `accounts` row.
pub fn credential_id_for_legacy_account(account_id: &str) -> CredentialId {
    CredentialId(namespaced_uuid(&format!("credential:account:{account_id}")))
}

/// Deterministic observer credential id for a platform parent.
pub fn observer_credential_id_for_platform_account(platform_account_id: &str) -> CredentialId {
    CredentialId(namespaced_uuid(&format!(
        "credential:platform_observer:{platform_account_id}"
    )))
}

/// Deterministic identity id for the CPA management observer.
pub fn identity_id_for_cpa() -> IdentityId {
    IdentityId(namespaced_uuid("identity:cpa_observer:cpa"))
}

/// Deterministic observer credential id for the CPA management key.
pub fn observer_credential_id_for_cpa() -> CredentialId {
    CredentialId(namespaced_uuid("credential:cpa_observer:cpa"))
}

/// Deterministic binding id for one credential on one connection.
pub fn binding_id_for(credential_id: &CredentialId, connection_id: &ConnectionId) -> BindingId {
    BindingId(namespaced_uuid(&format!(
        "binding:{}:{}",
        credential_id.as_str(),
        connection_id.as_str()
    )))
}

/// Deterministic anonymous binding id for a no-auth connection.
pub fn anonymous_binding_id_for(connection_id: &ConnectionId) -> BindingId {
    BindingId(namespaced_uuid(&format!(
        "binding:anonymous:{}",
        connection_id.as_str()
    )))
}

/// Deterministic onboarding task id for a legacy managed account.
pub fn onboarding_task_id_for_legacy_account(account_id: &str) -> OnboardingTaskId {
    OnboardingTaskId(namespaced_uuid(&format!("onboarding:account:{account_id}")))
}

/// Deterministic quota pool id for one confirmed identity.
pub fn quota_pool_id_for_identity(identity_id: impl AsRef<str>) -> QuotaPoolId {
    QuotaPoolId(namespaced_uuid(&format!(
        "quota_pool:identity:{}",
        identity_id.as_ref()
    )))
}

/// Deterministic quota pool id for an explicit member set (sorted, unique).
pub fn quota_pool_id_for_accounts<I, S>(account_ids: I) -> QuotaPoolId
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut ids: Vec<String> = account_ids
        .into_iter()
        .map(|id| id.as_ref().to_string())
        .collect();
    ids.sort();
    ids.dedup();
    QuotaPoolId(namespaced_uuid(&format!(
        "quota_pool:accounts:{}",
        ids.join(",")
    )))
}

/// `All` admits every model. `Only` admits names that [`model_ids_match`]
/// the saved list: trim and ASCII case, with separators kept distinct.
pub fn model_scope_allows(scope: &ModelScope, public_or_routing_model: &str) -> bool {
    match scope {
        ModelScope::All => true,
        ModelScope::Only { models } => models
            .iter()
            .any(|model| model_ids_match(model, public_or_routing_model)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum CredentialPurpose {
    Inference,
    PlatformObserver,
}

impl CredentialPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inference => "inference",
            Self::PlatformObserver => "platform_observer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum MaterialKind {
    ApiKey,
    ExternalReference,
}

impl MaterialKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::ExternalReference => "external_reference",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum AuthState {
    Unknown,
    Valid,
    Invalid,
}

impl AuthState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Valid => "valid",
            Self::Invalid => "invalid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum IdentityConfidence {
    Opaque,
    Declared,
}

impl IdentityConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Declared => "declared",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelScope {
    All,
    Only { models: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum RuntimeSubjectKind {
    AccountCredential,
    Anonymous,
    ExternalRuntime,
}

impl RuntimeSubjectKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AccountCredential => "account_credential",
            Self::Anonymous => "anonymous",
            Self::ExternalRuntime => "external_runtime",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum QuotaSubject {
    Credential,
    Egress,
}

impl QuotaSubject {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Credential => "credential",
            Self::Egress => "egress",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum RelationConfidence {
    Unknown,
    Declared,
}

impl RelationConfidence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Declared => "declared",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum QuotaPolicyMode {
    ObserveOnly,
    AuthoritativeLimit,
}

impl QuotaPolicyMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ObserveOnly => "observe_only",
            Self::AuthoritativeLimit => "authoritative_limit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum QuotaPeriod {
    Generic,
    FiveHours,
    Week,
    Month,
    Free,
}

impl QuotaPeriod {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Generic => "generic",
            Self::FiveHours => "five_hours",
            Self::Week => "week",
            Self::Month => "month",
            Self::Free => "free",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum OnboardingTaskKind {
    ManagedRegistration,
}

impl OnboardingTaskKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManagedRegistration => "managed_registration",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum OnboardingTaskState {
    InProgress,
    Completed,
}

impl OnboardingTaskState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum SubscriptionSource {
    LegacyManual,
    ManagedPayment,
}

impl SubscriptionSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LegacyManual => "legacy_manual",
            Self::ManagedPayment => "managed_payment",
        }
    }
}

/// Observed quota quantity. Absence means unknown — never invent a zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct QuotaMetric {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AuthorityRef {
    pub issuer_or_site: String,
    pub tenant_or_subject: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UpstreamAccount {
    pub id: IdentityId,
    pub label: String,
    pub authority_ref: Option<AuthorityRef>,
    pub identity_confidence: IdentityConfidence,
    pub enabled: bool,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    pub id: CredentialId,
    pub identity_id: IdentityId,
    pub purpose: CredentialPurpose,
    pub material_kind: MaterialKind,
    /// Opaque storage pointer such as `account:<id>` or `platform:<id>`.
    pub secret_ref: String,
    pub version: u64,
    pub enabled: bool,
    pub auth_state: AuthState,
    pub auth_state_version: u64,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct CredentialBinding {
    pub id: BindingId,
    pub credential_id: CredentialId,
    pub connection_id: ConnectionId,
    pub allowed_endpoint_ids: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub model_scope: ModelScope,
    pub enabled: bool,
    pub routing_rank: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct AnonymousBinding {
    pub id: BindingId,
    pub connection_id: ConnectionId,
    pub allowed_endpoint_ids: Vec<String>,
    pub enabled: bool,
    pub routing_rank: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct OnboardingTask {
    pub id: OnboardingTaskId,
    pub identity_id: IdentityId,
    pub kind: OnboardingTaskKind,
    pub step: String,
    pub state: OnboardingTaskState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionRecord {
    pub source: SubscriptionSource,
    pub purchase_date: String,
    pub expires_on: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub subject: QuotaSubject,
    pub subject_ref: String,
    pub period: QuotaPeriod,
    pub blocked_until: Option<DateTime<Utc>>,
    pub metric: Option<QuotaMetric>,
    pub relation_confidence: RelationConfidence,
    pub policy_mode: QuotaPolicyMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct QuotaPool {
    pub id: QuotaPoolId,
    pub subject: QuotaSubject,
    pub subject_ref: String,
    pub relation_confidence: RelationConfidence,
    pub policy_mode: QuotaPolicyMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct DeclaredRelation {
    pub platform_account_id: String,
    pub group: String,
}

/// Account-row facts used by [`legacy_account_objects`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyAccountFacts {
    pub account_id: String,
    pub name: String,
    pub notes: Option<String>,
    pub enabled: bool,
    pub sort_order: u32,
    pub has_auth_error: bool,
    pub verified: bool,
    pub anonymous: bool,
    pub declared_relation: Option<DeclaredPlatformRelation>,
}

/// Platform parent facts that raise an identity from opaque to declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredPlatformRelation {
    pub platform_account_id: String,
    pub group: String,
    pub parent_base_url: String,
}

/// Stored cooldown timestamps for one legacy account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CooldownFacts {
    pub account_id: String,
    pub credential_id: String,
    pub generic: Option<DateTime<Utc>>,
    pub five_hours: Option<DateTime<Utc>>,
    pub week: Option<DateTime<Utc>>,
    pub month: Option<DateTime<Utc>>,
    pub free: Option<DateTime<Utc>>,
}

/// One connection endpoint used to populate binding allow-lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssignedEndpoint {
    pub id: String,
    pub url: Option<String>,
}

/// One configured route used to assign connection endpoint ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSpec {
    pub operation: EndpointOperation,
    pub url: Option<String>,
}

/// Assign endpoint ids the same way `/connections` does: first occurrence of
/// each operation uses [`endpoint_id_for`]; later URLs for the same operation
/// use [`endpoint_id_for_route`].
pub fn assigned_endpoints_for_routes(
    connection_id: &ConnectionId,
    routes: &[RouteSpec],
) -> Vec<AssignedEndpoint> {
    let mut used_operations = HashSet::new();
    routes
        .iter()
        .map(|route| {
            let id = if used_operations.insert(route.operation) {
                endpoint_id_for(connection_id, route.operation)
            } else {
                endpoint_id_for_route(
                    connection_id,
                    route.operation,
                    route.url.as_deref().unwrap_or(""),
                )
            };
            AssignedEndpoint {
                id: id.to_string(),
                url: route.url.clone(),
            }
        })
        .collect()
}

/// Preserve consent to the exact operation and resolved URL when route ordering
/// changes. Unknown/stale ids are discarded so a reused ordinal cannot revive them.
pub fn remap_route_grant_ids(
    connection_id: &ConnectionId,
    before: &[RouteSpec],
    after: &[RouteSpec],
    granted: &[String],
) -> Vec<String> {
    let old = assigned_endpoints_for_routes(connection_id, before);
    let new = assigned_endpoints_for_routes(connection_id, after);
    let mut result = Vec::new();
    for (route, endpoint) in after.iter().zip(new) {
        if before
            .iter()
            .zip(&old)
            .any(|(previous, assigned)| previous == route && granted.contains(&assigned.id))
            && !result.contains(&endpoint.id)
        {
            result.push(endpoint.id);
        }
    }
    result
}

/// Safe default grants for a new Key or one-time backfill.
///
/// Captures the default route and any additional configured routes that share
/// that default origin. Sealed endpoints (no URL) are included and contribute
/// no origins. A foreign-origin model override is not granted implicitly.
pub fn safe_default_grants(endpoints: &[AssignedEndpoint]) -> (Vec<String>, Vec<String>) {
    let default_origin = endpoints
        .first()
        .and_then(|endpoint| endpoint.url.as_deref().and_then(normalize_origin));
    let mut ids = Vec::new();
    let mut origins = Vec::new();
    let mut seen_origins = HashSet::new();
    for endpoint in endpoints {
        let origin = endpoint.url.as_deref().and_then(normalize_origin);
        let same_origin = match (&default_origin, &origin) {
            (None, None) => true,
            (Some(default), Some(got)) => default == got,
            (Some(_), None) => true,
            (None, Some(_)) => false,
        };
        if !same_origin {
            continue;
        }
        ids.push(endpoint.id.clone());
        if let Some(origin) = origin
            && seen_origins.insert(origin.clone())
        {
            origins.push(origin);
        }
    }
    (ids, origins)
}

/// Auth error wins; a verified row is Valid; otherwise Unknown.
pub fn derive_auth_state(has_auth_error: bool, verified: bool) -> AuthState {
    if has_auth_error {
        AuthState::Invalid
    } else if verified {
        AuthState::Valid
    } else {
        AuthState::Unknown
    }
}

/// Active cooldown windows. Past timestamps are omitted; surviving instants
/// are the stored values. Metric stays unknown — never a synthesized zero.
pub fn cooldown_windows(facts: &CooldownFacts, now: DateTime<Utc>) -> Vec<QuotaWindow> {
    let mut windows = Vec::new();
    push_credential_window(
        &mut windows,
        &facts.credential_id,
        QuotaPeriod::Generic,
        facts.generic,
        now,
    );
    push_credential_window(
        &mut windows,
        &facts.credential_id,
        QuotaPeriod::FiveHours,
        facts.five_hours,
        now,
    );
    push_credential_window(
        &mut windows,
        &facts.credential_id,
        QuotaPeriod::Week,
        facts.week,
        now,
    );
    push_credential_window(
        &mut windows,
        &facts.credential_id,
        QuotaPeriod::Month,
        facts.month,
        now,
    );
    if let Some(until) = facts.free.filter(|until| *until > now) {
        windows.push(QuotaWindow {
            subject: QuotaSubject::Egress,
            subject_ref: "free_channel".to_string(),
            period: QuotaPeriod::Free,
            blocked_until: Some(until),
            metric: None,
            relation_confidence: RelationConfidence::Declared,
            policy_mode: QuotaPolicyMode::AuthoritativeLimit,
        });
    }
    windows
}

fn push_credential_window(
    windows: &mut Vec<QuotaWindow>,
    credential_id: &str,
    period: QuotaPeriod,
    until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) {
    let Some(until) = until.filter(|until| *until > now) else {
        return;
    };
    windows.push(QuotaWindow {
        subject: QuotaSubject::Credential,
        subject_ref: credential_id.to_string(),
        period,
        blocked_until: Some(until),
        metric: None,
        relation_confidence: RelationConfidence::Declared,
        policy_mode: QuotaPolicyMode::AuthoritativeLimit,
    });
}

/// Single mapper for migration and V4 projection.
///
/// Identity label is the account name. Confidence is Opaque unless a declared
/// platform relation is supplied. Credential and binding enablement follow the
/// account. `routing_rank` is `sort_order`. Model scope is All. Origins come
/// from endpoint URLs; sealed endpoints (no URL) contribute none.
pub fn legacy_account_objects(
    input: LegacyAccountFacts,
    connection_id: &ConnectionId,
    endpoints: &[AssignedEndpoint],
) -> (UpstreamAccount, Credential, CredentialBinding) {
    let identity_id = identity_id_for_legacy_account(&input.account_id);
    let credential_id = credential_id_for_legacy_account(&input.account_id);
    let binding_id = if input.anonymous {
        anonymous_binding_id_for(connection_id)
    } else {
        binding_id_for(&credential_id, connection_id)
    };
    let (identity_confidence, authority_ref) = match &input.declared_relation {
        Some(relation) => (
            IdentityConfidence::Declared,
            Some(AuthorityRef {
                issuer_or_site: relation.parent_base_url.clone(),
                tenant_or_subject: None,
            }),
        ),
        None => (IdentityConfidence::Opaque, None),
    };
    let identity = UpstreamAccount {
        id: identity_id.clone(),
        label: input.name,
        authority_ref,
        identity_confidence,
        enabled: input.enabled,
        notes: input.notes,
    };
    let credential = Credential {
        id: credential_id.clone(),
        identity_id,
        purpose: CredentialPurpose::Inference,
        material_kind: MaterialKind::ApiKey,
        secret_ref: format!("account:{}", input.account_id),
        version: 1,
        enabled: input.enabled,
        auth_state: derive_auth_state(input.has_auth_error, input.verified),
        auth_state_version: 1,
        expires_at: None,
    };
    let allowed_endpoint_ids = endpoints
        .iter()
        .map(|endpoint| endpoint.id.clone())
        .collect();
    let allowed_origins = endpoints
        .iter()
        .filter_map(|endpoint| endpoint.url.as_deref().and_then(normalize_origin))
        .collect();
    let binding = CredentialBinding {
        id: binding_id,
        credential_id,
        connection_id: connection_id.clone(),
        allowed_endpoint_ids,
        allowed_origins,
        model_scope: ModelScope::All,
        enabled: input.enabled,
        routing_rank: input.sort_order,
    };
    (identity, credential, binding)
}

/// Anonymous binding for `credential_kind == None` accounts.
pub fn anonymous_binding_for(
    connection_id: &ConnectionId,
    endpoint_ids: &[String],
    rank: u32,
) -> AnonymousBinding {
    AnonymousBinding {
        id: anonymous_binding_id_for(connection_id),
        connection_id: connection_id.clone(),
        allowed_endpoint_ids: endpoint_ids.to_vec(),
        enabled: true,
        routing_rank: rank,
    }
}

/// Scheme + host [+ port] origin of an inference URL. None when unparseable.
pub fn origin_from_endpoint_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if rest.is_empty() || scheme.is_empty() {
        return None;
    }
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or("");
    if hostport.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{hostport}"))
}

/// Scheme, host, and port of an HTTP(S) URL after URL parsing.
///
/// Default ports are part of the value (`https` is 443) so an explicit
/// `:443` matches an omitted port. IPv6 is compressed and bracketed in
/// [`CanonicalOrigin::to_grant_string`]. HTTP and HTTPS, distinct ports, and
/// distinct hosts stay different. Whether a URL may be a target is a
/// separate check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalOrigin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl CanonicalOrigin {
    /// Stable origin string. Default ports are omitted so persisted grants
    /// and display comparisons share one form.
    pub fn to_grant_string(&self) -> String {
        let default_port = match self.scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => 0,
        };
        if self.port == default_port {
            format!("{}://{}", self.scheme, self.host)
        } else {
            format!("{}://{}:{}", self.scheme, self.host, self.port)
        }
    }
}

/// Parse `value` into the origin used for grant equality.
pub fn canonical_origin(value: &str) -> Option<CanonicalOrigin> {
    let parsed = url::Url::parse(value.trim()).ok()?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = match parsed.host()? {
        url::Host::Domain(domain) => domain.to_ascii_lowercase(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => match std::net::IpAddr::V6(ip).to_canonical() {
            std::net::IpAddr::V4(ip) => ip.to_string(),
            std::net::IpAddr::V6(ip) => format!("[{ip}]"),
        },
    };
    if host.is_empty() {
        return None;
    }
    Some(CanonicalOrigin {
        scheme: scheme.to_ascii_lowercase(),
        host,
        port: parsed.port_or_known_default()?,
    })
}

/// True when both values parse as the same [`CanonicalOrigin`].
pub fn origins_equivalent(left: &str, right: &str) -> bool {
    match (canonical_origin(left), canonical_origin(right)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// Normalized Origin string for persistence and comparison.
///
/// Accepts a full inference URL or an already-origin-shaped value. Equality
/// is [`origins_equivalent`]; this string omits default ports.
pub fn normalize_origin(value: &str) -> Option<String> {
    Some(canonical_origin(value)?.to_grant_string())
}

#[cfg(test)]
mod tests;
