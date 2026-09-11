//! Unified connection identities and eligibility derivation.
//!
//! These types project the existing provider/account rows into a single
//! connection vocabulary. They are I/O-free: no persistence, HTTP, or
//! gateway execution. Connection, endpoint, and target ids are deterministic
//! UUIDv5 values over [`CONNECTION_ID_NAMESPACE`].

use crate::catalog::{CredentialKind, UpstreamAuthScheme, UpstreamProtocolKind};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(feature = "schemars")]
use schemars::JsonSchema;

/// Immutable UUIDv5 namespace for connection, endpoint, and target ids.
///
/// This literal is part of the identity contract and must never change.
/// Recalculating ids under a new namespace would rewrite every persisted
/// reference that later stages bind to these values.
pub const CONNECTION_ID_NAMESPACE: Uuid = Uuid::from_bytes([
    0x7c, 0x3a, 0x5e, 0x10, 0x9b, 0x2d, 0x4f, 0x81, 0xa6, 0xc4, 0x0d, 0x1e, 0x2f, 0x3a, 0x4b, 0x5c,
]);

/// Stable id for one projected connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(transparent)]
pub struct ConnectionId(String);

impl ConnectionId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ConnectionId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable id for one connection endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(transparent)]
pub struct EndpointId(String);

impl EndpointId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for EndpointId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for EndpointId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Stable id for one public model target on a connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(transparent)]
pub struct TargetId(String);

impl TargetId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for TargetId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::fmt::Display for TargetId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Pre-unification row a connection was projected from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum LegacyConnectionKind {
    BuiltinProvider,
    DynamicProvider,
    CustomAccount,
}

impl LegacyConnectionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BuiltinProvider => "builtin_provider",
            Self::DynamicProvider => "dynamic_provider",
            Self::CustomAccount => "custom_account",
        }
    }
}

/// Inference operation advertised by one endpoint. Mapped 1:1 from
/// [`UpstreamProtocolKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum EndpointOperation {
    ChatCreate,
    ResponseCreate,
    MessageCreate,
}

impl EndpointOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCreate => "chat_create",
            Self::ResponseCreate => "response_create",
            Self::MessageCreate => "message_create",
        }
    }
}

impl From<UpstreamProtocolKind> for EndpointOperation {
    fn from(value: UpstreamProtocolKind) -> Self {
        match value {
            UpstreamProtocolKind::ChatCompletions => Self::ChatCreate,
            UpstreamProtocolKind::Responses => Self::ResponseCreate,
            UpstreamProtocolKind::Messages => Self::MessageCreate,
        }
    }
}

impl From<EndpointOperation> for UpstreamProtocolKind {
    fn from(value: EndpointOperation) -> Self {
        match value {
            EndpointOperation::ChatCreate => Self::ChatCompletions,
            EndpointOperation::ResponseCreate => Self::Responses,
            EndpointOperation::MessageCreate => Self::Messages,
        }
    }
}

/// Whether the projected connection is currently intended to be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum ConnectionLifecycle {
    Configured,
    Disabled,
}

/// Local authorization projection. Unknown is not a verified success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum AuthorizationState {
    NotRequired,
    Missing,
    Unknown,
    Valid,
    Invalid,
}

/// Local attempt eligibility. Unknown authorization may still be Eligible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum EligibilityState {
    Eligible,
    Ineligible,
    Cooling,
}

/// Why a connection is eligible or not. `None` is a successful Eligible row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum EligibilityReason {
    MissingCredential,
    AllCredentialsDisabled,
    AllCredentialsInvalid,
    Cooling,
    NoEnabledTarget,
    ConnectionDisabled,
    None,
}

/// Provenance of a projected connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "snake_case"))]
pub enum ConnectionOrigin {
    Builtin,
    Preset,
    Custom,
    CustomAccount,
}

/// Auth advertised on a connection endpoint. `Sealed` is used for built-in
/// adapters whose scheme is owned by code, not by the projected row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(JsonSchema))]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schemars", schemars(rename_all = "kebab-case"))]
pub enum EndpointAuthScheme {
    Bearer,
    XApiKey,
    Sealed,
    None,
}

impl From<UpstreamAuthScheme> for EndpointAuthScheme {
    fn from(value: UpstreamAuthScheme) -> Self {
        match value {
            UpstreamAuthScheme::Bearer => Self::Bearer,
            UpstreamAuthScheme::XApiKey => Self::XApiKey,
        }
    }
}

/// One projected provider/account connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConnection {
    pub id: ConnectionId,
    pub name: String,
    pub origin: ConnectionOrigin,
    pub adapter_kind: String,
    pub lifecycle: ConnectionLifecycle,
    pub authorization: AuthorizationState,
    pub eligibility: EligibilityState,
    pub eligibility_reason: EligibilityReason,
    pub endpoints: Vec<EndpointBinding>,
    pub targets: Vec<ModelTarget>,
    pub legacy: LegacyRef,
}

/// One inference URL/protocol binding owned by a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointBinding {
    pub id: EndpointId,
    pub connection_id: ConnectionId,
    pub operation: EndpointOperation,
    pub wire_protocol: UpstreamProtocolKind,
    pub url: Option<String>,
    pub auth_scheme: EndpointAuthScheme,
    pub locked: bool,
}

/// One public model that a connection can target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTarget {
    pub id: TargetId,
    pub connection_id: ConnectionId,
    pub public_name: String,
    pub upstream_model_id: String,
    pub endpoint_ids: Vec<EndpointId>,
    pub enabled: bool,
}

/// Identity of the pre-unification row this connection was projected from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyRef {
    pub kind: LegacyConnectionKind,
    pub id: String,
}

/// Account-row facts used to derive authorization and cooling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialFacts {
    pub enabled: bool,
    pub has_auth_error: bool,
    pub verified: bool,
    pub cooling: bool,
}

/// Deterministic connection id for a legacy provider or Custom account row.
pub fn connection_id_for_legacy(kind: LegacyConnectionKind, legacy_id: &str) -> ConnectionId {
    ConnectionId(uuid_v5(&format!("{}:{legacy_id}", kind.as_str())))
}

/// Deterministic endpoint id for one operation on a connection.
pub fn endpoint_id_for(connection: &ConnectionId, operation: EndpointOperation) -> EndpointId {
    EndpointId(uuid_v5(&format!(
        "endpoint:{}:{}",
        connection.as_str(),
        operation.as_str()
    )))
}

/// Deterministic endpoint id when the same operation can bind more than one URL.
pub fn endpoint_id_for_route(
    connection: &ConnectionId,
    operation: EndpointOperation,
    url: &str,
) -> EndpointId {
    EndpointId(uuid_v5(&format!(
        "endpoint:{}:{}:{url}",
        connection.as_str(),
        operation.as_str()
    )))
}

/// Deterministic target id for one public model key on a connection.
pub fn target_id_for(connection: &ConnectionId, public_model_key: &str) -> TargetId {
    TargetId(uuid_v5(&format!(
        "target:{}:{public_model_key}",
        connection.as_str()
    )))
}

fn uuid_v5(name: &str) -> String {
    Uuid::new_v5(&CONNECTION_ID_NAMESPACE, name.as_bytes()).to_string()
}

/// Derive authorization from credential kind and the current account facts.
///
/// `CredentialKind::None` is always [`AuthorizationState::NotRequired`].
/// Unknown means an enabled credential exists and is not in `auth_error`, but
/// has not been verified. It is never a verified success.
pub fn derive_authorization(
    credential_kind: CredentialKind,
    credentials: &[CredentialFacts],
) -> AuthorizationState {
    if credential_kind == CredentialKind::None {
        return AuthorizationState::NotRequired;
    }
    if credentials.is_empty() {
        return AuthorizationState::Missing;
    }
    if credentials
        .iter()
        .any(|credential| credential.enabled && !credential.has_auth_error && credential.verified)
    {
        return AuthorizationState::Valid;
    }
    if credentials
        .iter()
        .any(|credential| credential.enabled && !credential.has_auth_error && !credential.verified)
    {
        return AuthorizationState::Unknown;
    }
    if credentials
        .iter()
        .all(|credential| credential.has_auth_error)
    {
        return AuthorizationState::Invalid;
    }
    AuthorizationState::Unknown
}

/// Derive local eligibility. Unknown authorization may still be Eligible.
///
/// `enabled_credential_count` is required so all-disabled rows can be
/// distinguished from missing credentials. `CredentialKind::None` connections
/// pass `0` here and skip the all-disabled branch.
pub fn derive_eligibility(
    lifecycle: ConnectionLifecycle,
    authorization: AuthorizationState,
    enabled_target_count: usize,
    cooling_all: bool,
    enabled_credential_count: usize,
) -> (EligibilityState, EligibilityReason) {
    if lifecycle == ConnectionLifecycle::Disabled {
        return (
            EligibilityState::Ineligible,
            EligibilityReason::ConnectionDisabled,
        );
    }
    if authorization == AuthorizationState::Missing {
        return (
            EligibilityState::Ineligible,
            EligibilityReason::MissingCredential,
        );
    }
    if authorization != AuthorizationState::NotRequired && enabled_credential_count == 0 {
        return (
            EligibilityState::Ineligible,
            EligibilityReason::AllCredentialsDisabled,
        );
    }
    if authorization == AuthorizationState::Invalid {
        return (
            EligibilityState::Ineligible,
            EligibilityReason::AllCredentialsInvalid,
        );
    }
    if enabled_target_count == 0 {
        return (
            EligibilityState::Ineligible,
            EligibilityReason::NoEnabledTarget,
        );
    }
    if cooling_all {
        return (EligibilityState::Cooling, EligibilityReason::Cooling);
    }
    (EligibilityState::Eligible, EligibilityReason::None)
}

/// True when every enabled, non-error credential is currently cooling.
pub fn cooling_all_usable(credentials: &[CredentialFacts]) -> bool {
    let mut usable = 0usize;
    for credential in credentials {
        if credential.enabled && !credential.has_auth_error {
            usable += 1;
            if !credential.cooling {
                return false;
            }
        }
    }
    usable > 0
}

#[cfg(test)]
mod tests;
