//! Typed product projection. Secrets are not `Debug` and are not `Serialize`.

use crate::models::{ProxyListDirection, ProxyMode, RoutingMode};
use crate::routing_snapshot::RoutingSnapshot;
use ocg_domain::credential::ModelScope;
use ocg_infra::crypto::KeyCipher;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use zeroize::Zeroize;

/// Caller-owned revision pair. The two numbers are stored separately.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProjectionRevisions {
    pub desired: u64,
    pub applied: u64,
}

/// Reference to a CPA-owned OAuth file. The token stays in that file.
///
/// `material_revision` is the opaque generation CPA reports (a hash of token
/// material fields, not the file bytes). This module stores that reported
/// value and does not read or decrypt the token. A CPA refresh replaces the
/// generation without rotating `credential_id`, `credential_version`, or
/// `routing_rank`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OAuthFileRef {
    pub provider: crate::cpa::CpaOAuthProvider,
    /// Product provider. Native executor labels stay on `provider`.
    pub product_provider_id: String,
    pub relative_path: String,
    /// Opaque CPA-reported material generation. Not a file hash computed here.
    pub material_revision: String,
    /// CPA auth id for this file. Callers supply it; this module does not mint one.
    pub auth_id: String,
    /// Stable OCG reference id. Not an upstream API key.
    pub credential_id: String,
    pub credential_version: u64,
    /// Same order key as `credentials.routing_rank`. A lower rank is earlier.
    /// Zero is the earliest rank. A disabled pool is omitted, not ranked zero.
    pub routing_rank: u32,
    pub models: Vec<String>,
    /// Trimmed discovery label before effective-provider normalization.
    pub raw_provider_label: String,
    /// `""`, `com`, `ai`, `cli`, or `api`. Empty is not a guessed mode.
    pub native_mode: String,
    /// Redacted base fact. It does not authorize a URL.
    pub reported_base: String,
}

/// Caller-supplied child process facts. This module never mints or stores them.
pub(crate) struct RuntimeEnvelope<'a> {
    pub process_generation: u64,
    pub port: u16,
    pub auth_dir: &'a str,
    pub policy_url: &'a str,
    pub policy_origin: &'a str,
    pub ready_key: &'a str,
    pub hop_secret: &'a str,
    pub policy_token: &'a str,
}

pub(crate) struct ProjectionInput<'a> {
    pub snapshot: &'a RoutingSnapshot,
    pub config: &'a crate::models::AppConfig,
    pub cipher: &'a dyn KeyCipher,
    pub revisions: ProjectionRevisions,
    pub owned_listener: &'a str,
    /// Extra exact origins the caller confirms are this owned process, such as
    /// the listener persisted before a port change. Host names are not inferred.
    pub owned_origins: &'a [&'a str],
    /// Destination ids the caller names as the owned pool. A named id whose
    /// base is a different origin is ambiguous and is not dropped.
    pub owned_destination_ids: &'a [&'a str],
    pub oauth_refs: &'a [OAuthFileRef],
    /// `destinations.preset_id`, which [`Destination`] does not carry.
    pub preset_ids: &'a BTreeMap<String, String>,
    /// Persisted validation facts. An empty slice keeps today's omissions.
    pub validation: ValidationSidecar<'a>,
    pub runtime: RuntimeEnvelope<'a>,
    pub endpoints: &'a crate::cpa_test_endpoints::EndpointAuthority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Historical remote CPA rows are omitted before an auth is built.
/// `NotRemote` is the only projected value. It is not upstream authority.
pub(crate) enum RemoteInner {
    NotRemote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RequestIdentityFact {
    None,
    OpenCodeSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WireFact {
    None,
    OllamaReasoning,
}

pub(crate) struct SecretMaterial {
    value: String,
}

impl SecretMaterial {
    pub(crate) fn new(value: String) -> Self {
        Self { value }
    }

    pub(crate) fn expose(&self) -> &str {
        &self.value
    }

    pub(crate) fn fingerprint(&self) -> String {
        super::auth_id::material_fingerprint(&self.value)
    }
}

impl Clone for SecretMaterial {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
        }
    }
}

impl Drop for SecretMaterial {
    fn drop(&mut self) {
        self.value.zeroize();
    }
}

impl std::fmt::Debug for SecretMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretMaterial([redacted])")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectedRoute {
    pub protocol: String,
    pub endpoint_url: String,
    pub auth: String,
    pub endpoint_id: String,
    /// Client traffic stays false. True only for a non-client credential or a
    /// pending approved alternate on a client credential.
    pub validation_only: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProjectedModel {
    pub public_alias: String,
    pub upstream_name: String,
    pub routes: Vec<ProjectedRoute>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SourceProvenance {
    pub provider_id: String,
    pub adapter: String,
    pub legacy_kind: String,
    pub legacy_id: String,
    pub preset_id: Option<String>,
    pub destination_id: String,
    pub quota_scope: String,
}

#[derive(Clone)]
pub(crate) struct ProjectedAuth {
    pub auth_id: String,
    pub credential_id: String,
    pub legacy_account_id: String,
    pub credential_version: u64,
    pub binding_id: String,
    pub routing_rank: u32,
    pub sequence: u32,
    pub provenance: SourceProvenance,
    pub scope: ModelScope,
    pub material: Option<SecretMaterial>,
    pub models: Vec<ProjectedModel>,
    pub remote_inner: RemoteInner,
    pub request_identity: RequestIdentityFact,
    pub wire: WireFact,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Omission {
    pub credential_id: String,
    pub reason: &'static str,
    pub model: Option<String>,
}

pub(crate) struct ProductProjection {
    pub revisions: ProjectionRevisions,
    pub digest: String,
    pub routing_mode: RoutingMode,
    pub conversation_sticky: bool,
    pub conversation_ttl_secs: u64,
    pub proxy_mode: ProxyMode,
    pub proxy_url: String,
    pub proxy_list_direction: ProxyListDirection,
    pub proxy_list_models: Vec<String>,
    pub auths: Vec<ProjectedAuth>,
    pub omissions: Vec<Omission>,
    pub oauth_refs: Vec<OAuthFileRef>,
    /// API route sets, then native sets attached before the logical digest.
    pub route_sets: Vec<CredentialRouteSet>,
    /// Coordinator-chosen owned child generation. Not the typed effective digest.
    pub process_generation: u64,
    pub listen_port: u16,
    pub auth_dir: String,
    pub policy_url: String,
    pub policy_origin: String,
    pub ready_key: SecretMaterial,
    pub hop_secret: SecretMaterial,
    pub policy_token: SecretMaterial,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProjectionError {
    pub code: &'static str,
    pub detail: String,
}

impl ProjectionError {
    pub(crate) fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.detail)
    }
}

impl std::fmt::Debug for ProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectionError")
            .field("code", &self.code)
            .field("detail", &self.detail)
            .finish()
    }
}

impl std::error::Error for ProjectionError {}

impl std::fmt::Debug for ProductProjection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProductProjection")
            .field("revisions_desired", &self.revisions.desired)
            .field("revisions_applied", &self.revisions.applied)
            .field("digest", &self.digest)
            .field("routing_mode", &self.routing_mode)
            .field("conversation_sticky", &self.conversation_sticky)
            .field("proxy_mode", &self.proxy_mode)
            .field("auth_count", &self.auths.len())
            .field("omission_count", &self.omissions.len())
            .field("oauth_ref_count", &self.oauth_refs.len())
            .field("process_generation", &self.process_generation)
            .field("listen_port", &self.listen_port)
            .field("secrets", &"[redacted]")
            .finish()
    }
}

impl std::fmt::Debug for ProjectedAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectedAuth")
            .field("auth_id", &self.auth_id)
            .field("credential_id", &self.credential_id)
            .field("credential_version", &self.credential_version)
            .field("binding_id", &self.binding_id)
            .field("provider_id", &self.provenance.provider_id)
            .field("material", &self.material)
            .finish()
    }
}

pub(crate) fn fingerprint(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ValidationCandidateKind {
    CompletePending,
    ManagedKeyVerification,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingApprovedRoute {
    pub public_model: String,
    pub upstream_model: String,
    /// `chat_completions`, `responses`, or `messages`.
    pub protocol: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidationCandidate {
    pub credential_id: String,
    pub credential_version: u64,
    pub kind: ValidationCandidateKind,
    pub setup_step: String,
    pub account_type: String,
    pub material_present: bool,
    pub destination_draft: bool,
    pub pending_routes: Vec<PendingApprovedRoute>,
}

pub(crate) struct ValidationSidecar<'a> {
    pub candidates: &'a [ValidationCandidate],
}

impl ValidationSidecar<'static> {
    pub(crate) fn none() -> Self {
        Self { candidates: &[] }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ValidationRow {
    pub credential_id: String,
    pub credential_version: u64,
    pub enabled: bool,
    pub binding_enabled: bool,
    pub setup_step: String,
    pub account_type: String,
    pub material_present: bool,
    pub destination_enabled: bool,
    pub destination_draft: bool,
    pub registration_complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeInclusion {
    Client,
    ValidationOnly,
}

/// One admitted native HTTP pin. The protocol is a callable route protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeEndpointPin {
    pub protocol: String,
    pub endpoint_id: String,
    pub origin: String,
    pub endpoint_fingerprint: String,
    pub http_method: String,
}

/// Operation association for one pin. Kinds are the seven SDK generation names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NativeDispatchTarget {
    pub pin: NativeEndpointPin,
    pub generation_kinds: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormalizedRoute {
    pub public_model: String,
    pub upstream_model: String,
    pub protocol: String,
    pub endpoint_id: String,
    pub origin: String,
    /// SHA-256 hex of the normalized endpoint URL. A native route omits the
    /// set when this pin is empty. The raw URL is not stored here.
    pub endpoint_fingerprint: String,
    pub validation_only: bool,
    /// Operation pins for this callable protocol. Empty is not network authority.
    pub native_targets: Vec<NativeDispatchTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CredentialRouteSet {
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub binding_id: String,
    pub material_fingerprint: String,
    pub routing_rank: u32,
    pub routes: Vec<NormalizedRoute>,
    pub fingerprint: String,
}

pub(super) struct AssignedPriorities {
    pub api: Vec<i64>,
    pub oauth: Vec<i64>,
}

/// Stock CPA tier for each eligible API row and native OAuth binding.
///
/// Round-robin shares priority 1. Strict and sticky sort `routing_rank`
/// ascending. Equal ranks keep API snapshot order ahead of the caller’s OAuth
/// slice order. Tiers then descend as `count - index`, and the minimum is 1.
/// This does not choose a credential.
pub(super) fn assigned_priorities(
    projection: &ProductProjection,
) -> Result<AssignedPriorities, ProjectionError> {
    let api_len = projection.auths.len();
    let oauth_len = projection.oauth_refs.len();
    let total = api_len.checked_add(oauth_len).ok_or_else(|| {
        ProjectionError::new("duplicate_priority", "credential priority is not positive")
    })?;
    if projection.routing_mode == RoutingMode::RoundRobin {
        return Ok(AssignedPriorities {
            api: vec![1; api_len],
            oauth: vec![1; oauth_len],
        });
    }
    let count = i64::try_from(total).map_err(|_| {
        ProjectionError::new("duplicate_priority", "credential priority is not positive")
    })?;
    let mut order = Vec::with_capacity(total);
    for (index, auth) in projection.auths.iter().enumerate() {
        order.push((auth.routing_rank, 0u8, index, false));
    }
    for (index, item) in projection.oauth_refs.iter().enumerate() {
        order.push((item.routing_rank, 1u8, index, true));
    }
    order.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(&right.2))
    });
    let mut api = vec![0; api_len];
    let mut oauth = vec![0; oauth_len];
    for (place, (_, _, index, is_oauth)) in order.into_iter().enumerate() {
        let priority = count - i64::try_from(place).unwrap_or(i64::MAX);
        if priority <= 0 {
            return Err(ProjectionError::new(
                "duplicate_priority",
                "credential priority is not positive",
            ));
        }
        if is_oauth {
            oauth[index] = priority;
        } else {
            api[index] = priority;
        }
    }
    Ok(AssignedPriorities { api, oauth })
}
