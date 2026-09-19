//! Password-encrypted node migration for Dashboard V3.
//!
//! Plaintext upstream Keys are decrypted and re-encrypted only inside the Host.
//! The dashboard receives a versioned Argon2id + AES-256-GCM envelope, plus
//! secret-free previews/results. Browser profiles, cookies, logs, usage, and
//! local Host settings stay off the package. V7 destinations and credentials
//! are the authoritative transfer model and carry plaintext secrets inside the
//! already-encrypted envelope. V6 preserves cooldown deadlines; V4/V5 retain
//! their host-local policy.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use zeroize::{Zeroize, Zeroizing};

use crate::dashboard_session;
use crate::db::identity::{
    IdentityImportSnapshot, ImportedAccountIdentity, ImportedIdentity, ImportedQuotaPool,
};
use crate::db::{AccountImportRecord, NodeImportRecord};
use crate::models::{
    Account as ModelAccount, AccountCustomConfigInput, AccountModelCapabilityInput,
    AccountSetupStep as ModelSetupStep, AccountType as ModelAccountType, AppConfig, SubGatewayKey,
    normalize_account_notes, normalize_purchase_date,
};
use ocg_domain::credential::ModelScope;
use ocg_domain::dynamic::{DynamicAuthKind, DynamicModelMapping, DynamicProviderDefinition};

use crate::dynamic::DynamicProviderRuntime;
use crate::provider::{
    ConnectionVerificationStatus, CreationAvailability, CredentialKind, QuotaScope,
    UpstreamProtocolKind, builtin_provider, provider_allows_enablement,
};
use crate::provider_contracts::{
    ContractEvidenceSource, ContractScope, ContractScopeKind, PersistedContracts,
    PersistedModelProtocol, PersistedModelProtocolOverride, PersistedScopeRow, ProbeResultKind,
    ProtocolOverrideState, build_effective_contracts, exclusive_available_force_off_repairs,
};
use crate::state::CoreState;

use super::types::{
    AccountExport, AccountExportRequest, AccountImportDisposition, AccountImportPreview,
    AccountImportPreviewItem, AccountImportPreviewRequest, AccountImportRequest,
    AccountImportResult,
};
use super::{V3ApiError, check_expectation, parse_json, parse_mutation_json};

mod new_model;
mod portable;

use new_model::{
    ValidatedMigration, export_new_model, finish_v7_migration, map_old_graph_to_unified,
    observer_plaintext_by_parent,
};
use portable::{PortableCredential, PortableDestination, credential_purpose, is_observer_purpose};

const ENVELOPE_FORMAT: &str = "ocg-manager-account-backup";
const ENVELOPE_VERSION: u32 = 1;
#[cfg(test)]
const LEGACY_PAYLOAD_VERSION: u32 = 1;
#[cfg(test)]
const NODE_PAYLOAD_VERSION: u32 = 2;
const PAYLOAD_VERSION: u32 = 7;
const MIN_SUPPORTED_PAYLOAD_VERSION: u32 = 4;
const V5_PAYLOAD_VERSION: u32 = 5;
const V6_PAYLOAD_VERSION: u32 = 6;
const AAD: &[u8] = b"ocg-manager-account-backup:v1:argon2id-m65536-t3-p1:aes-256-gcm";
const ARGON_MEMORY_KIB: u32 = 64 * 1024;
const ARGON_ITERATIONS: u32 = 3;
const ARGON_LANES: u32 = 1;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const MIN_BUNDLE_PASSWORD_CHARS: usize = 12;
const MAX_PASSWORD_CHARS: usize = 256;
pub(super) const MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_BUNDLE_BYTES: usize = 3 * 1024 * 1024;
const MAX_PLAINTEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_ACCOUNTS: usize = 200;
const MAX_NAME_CHARS: usize = 200;
const MAX_USERNAME_CHARS: usize = 320;
const MAX_KEY_CHARS: usize = 16 * 1024;
const MAX_NOTES_CHARS: usize = 4000;
const MAX_ENDPOINT_CHARS: usize = 2048;
const MAX_CAPABILITIES: usize = 200;
const MAX_ACCESS_KEYS: usize = 64;
const MAX_PROVIDER_SCOPES: usize = 32;
const MAX_PROVIDER_MODELS: usize = 500;

static CRYPTO_GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncryptedEnvelope {
    format: String,
    version: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortablePayload {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    platform_accounts: Vec<crate::platform::PortablePlatformAccount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    platform_links: Vec<crate::platform::PortablePlatformLink>,
    version: u32,
    exported_at: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    accounts: Vec<PortableAccount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dynamic_providers: Vec<PortableProviderDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    identities: Vec<PortableIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    quota_pools: Vec<PortableQuotaPool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    destinations: Vec<PortableDestination>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    credentials: Vec<PortableCredential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<PortableNodeState>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableAccount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    provider_id: String,

    name: String,
    username: Option<String>,
    key: String,
    enabled: bool,
    account_type: String,
    setup_step: String,
    purchase_date: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    expires_on: String,
    notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verification_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connection_verified_at: Option<String>,
    custom_config: Option<PortableCustomConfig>,
    model_capabilities: Vec<PortableModelCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ollama_billing_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credential_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_state_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding_model_scope: Option<ModelScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allowed_endpoint_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allowed_origins: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cooldowns: Option<PortableCooldowns>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableCooldowns {
    until: Option<DateTime<Utc>>,
    generic: Option<DateTime<Utc>>,
    five_hours: Option<DateTime<Utc>>,
    week: Option<DateTime<Utc>>,
    month: Option<DateTime<Utc>>,
    free: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableIdentity {
    id: String,
    label: String,
    identity_confidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority_site: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    authority_subject: Option<String>,
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableQuotaPool {
    id: String,
    subject_kind: String,
    subject_ref: String,
    relation_confidence: String,
    policy_mode: String,
    member_account_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableNodeState {
    config: AppConfig,
    access_keys: Vec<PortableAccessKey>,
    zen_free: PortableZenFree,
    account_order: Vec<String>,
    provider_contracts: Vec<PortableProviderContract>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableAccessKey {
    id: String,
    name: String,
    key: String,
    enabled: bool,
    created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableZenFree {
    enabled: bool,
    models: Vec<String>,
    refreshed_at: Option<String>,
    source_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableProviderContract {
    provider_id: String,
    catalog_models: Vec<String>,
    catalog_refreshed_at: Option<String>,
    catalog_source: String,
    catalog_source_url: String,
    evidence: Vec<PortableProtocolEvidence>,
    overrides: Vec<PortableProtocolOverride>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    preferences: Vec<PortableProtocolPreference>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableProtocolPreference {
    model_id: String,
    protocol: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableProtocolEvidence {
    model_id: String,
    protocol: String,
    source: String,
    verified_at: Option<String>,
    observed_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableProtocolOverride {
    model_id: String,
    protocol: String,
    state: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableCustomConfig {
    endpoint_url: String,
    upstream_protocol: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableProviderDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preset_id: Option<String>,
    id: String,
    name: String,
    endpoint_url: String,
    upstream_protocol: String,
    auth_kind: String,
    models: Vec<PortableProviderDefinitionModel>,
    /// Required on V6 packages; must be absent on V4/V5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    onboarding_draft: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableProviderDefinitionModel {
    public_model: String,
    upstream_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upstream_override: Option<PortableProviderDefinitionModelOverride>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableProviderDefinitionModelOverride {
    protocol: String,
    endpoint_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum PortableModelCapability {
    Canonical(PortableModelCapabilityCanonical),
    Legacy(PortableModelCapabilityLegacy),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableModelCapabilityCanonical {
    public_model: String,
    upstream_model: String,
    protocol: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortableModelCapabilityLegacy {
    model_id: String,
    protocol: String,
}

impl Zeroize for PortablePayload {
    fn zeroize(&mut self) {
        self.version.zeroize();
        self.exported_at.zeroize();
        self.accounts.zeroize();
        self.dynamic_providers.zeroize();
        self.identities.zeroize();
        self.quota_pools.zeroize();
        self.destinations.zeroize();
        self.credentials.zeroize();
        self.node.zeroize();
    }
}

impl Zeroize for PortableAccount {
    fn zeroize(&mut self) {
        self.provider_id.zeroize();
        self.name.zeroize();
        self.username.zeroize();
        self.id.zeroize();
        self.key.zeroize();
        self.enabled.zeroize();
        self.account_type.zeroize();
        self.setup_step.zeroize();
        self.purchase_date.zeroize();
        self.expires_on.zeroize();
        self.notes.zeroize();
        self.verification_status.zeroize();
        self.connection_verified_at.zeroize();
        self.custom_config.zeroize();
        self.model_capabilities.zeroize();
        self.ollama_billing_tier.zeroize();
        self.identity_id.zeroize();
        self.credential_id.zeroize();
        self.credential_version.zeroize();
        self.auth_state_version.zeroize();
        self.binding_id.zeroize();
        self.binding_enabled.zeroize();
        self.binding_model_scope = None;
        self.allowed_endpoint_ids.zeroize();
        self.allowed_origins.zeroize();
        self.cooldowns = None;
    }
}

impl Zeroize for PortableIdentity {
    fn zeroize(&mut self) {
        self.id.zeroize();
        self.label.zeroize();
        self.identity_confidence.zeroize();
        self.authority_site.zeroize();
        self.authority_subject.zeroize();
        self.enabled.zeroize();
        self.notes.zeroize();
    }
}

impl Zeroize for PortableQuotaPool {
    fn zeroize(&mut self) {
        self.id.zeroize();
        self.subject_kind.zeroize();
        self.subject_ref.zeroize();
        self.relation_confidence.zeroize();
        self.policy_mode.zeroize();
        self.member_account_ids.zeroize();
    }
}

impl Zeroize for PortableNodeState {
    fn zeroize(&mut self) {
        self.config.gateway_key.zeroize();
        self.config.proxy_url.zeroize();
        self.config.client_root_url.zeroize();
        self.access_keys.zeroize();
        self.zen_free.zeroize();
        self.account_order.zeroize();
        self.provider_contracts.zeroize();
    }
}

impl Zeroize for PortableAccessKey {
    fn zeroize(&mut self) {
        self.id.zeroize();
        self.name.zeroize();
        self.key.zeroize();
        self.enabled.zeroize();
        self.created_at.zeroize();
    }
}

impl Zeroize for PortableZenFree {
    fn zeroize(&mut self) {
        self.enabled.zeroize();
        self.models.zeroize();
        self.refreshed_at.zeroize();
        self.source_url.zeroize();
    }
}

impl Zeroize for PortableProviderContract {
    fn zeroize(&mut self) {
        self.provider_id.zeroize();
        self.catalog_models.zeroize();
        self.catalog_refreshed_at.zeroize();
        self.catalog_source.zeroize();
        self.catalog_source_url.zeroize();
        self.evidence.zeroize();
        self.overrides.zeroize();
        self.preferences.zeroize();
    }
}

impl Zeroize for PortableProtocolEvidence {
    fn zeroize(&mut self) {
        self.model_id.zeroize();
        self.protocol.zeroize();
        self.source.zeroize();
        self.verified_at.zeroize();
        self.observed_at.zeroize();
    }
}

impl Zeroize for PortableProtocolOverride {
    fn zeroize(&mut self) {
        self.model_id.zeroize();
        self.protocol.zeroize();
        self.state.zeroize();
    }
}

impl Zeroize for PortableProtocolPreference {
    fn zeroize(&mut self) {
        self.model_id.zeroize();
        self.protocol.zeroize();
    }
}

impl Zeroize for PortableCustomConfig {
    fn zeroize(&mut self) {
        self.endpoint_url.zeroize();
        self.upstream_protocol.zeroize();
    }
}

impl Zeroize for PortableProviderDefinition {
    fn zeroize(&mut self) {
        self.preset_id.zeroize();
        self.id.zeroize();
        self.name.zeroize();
        self.endpoint_url.zeroize();
        self.upstream_protocol.zeroize();
        self.auth_kind.zeroize();
        self.models.zeroize();
    }
}

impl Zeroize for PortableProviderDefinitionModel {
    fn zeroize(&mut self) {
        self.public_model.zeroize();
        self.upstream_model.zeroize();
        if let Some(value) = self.upstream_override.as_mut() {
            value.protocol.zeroize();
            value.endpoint_url.zeroize();
        }
    }
}

impl Zeroize for PortableModelCapability {
    fn zeroize(&mut self) {
        match self {
            Self::Canonical(capability) => {
                capability.public_model.zeroize();
                capability.upstream_model.zeroize();
                capability.protocol.zeroize();
            }
            Self::Legacy(capability) => {
                capability.model_id.zeroize();
                capability.protocol.zeroize();
            }
        }
    }
}

#[derive(Debug)]
struct ValidatedAccount {
    portable_index: usize,
    id: Option<String>,
    provider_id: String,

    name: String,
    username: Option<String>,
    key: Zeroizing<String>,
    password: Option<Zeroizing<String>>,
    enabled: bool,
    account_type: ModelAccountType,
    setup_step: ModelSetupStep,
    purchase_date: String,
    expires_on: String,
    notes: Option<String>,
    verification_status: ConnectionVerificationStatus,
    connection_verified_at: Option<DateTime<Utc>>,
    custom_config: Option<AccountCustomConfigInput>,
    capabilities: Vec<AccountModelCapabilityInput>,
    ollama_billing_tier: Option<crate::provider::OllamaBillingTier>,
    credential_kind: CredentialKind,
    quota_scope: QuotaScope,
    identity: Option<ImportedAccountIdentity>,
    cooldowns: PortableCooldowns,
}

#[derive(Debug)]
enum TransferError {
    Invalid(String),
    InvalidBundle,
    UnsupportedVersion(u32),
    Busy,
    InsecureTransport,
    Internal,
}

pub(super) async fn export_accounts(
    State(state): State<CoreState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    no_store(
        export_accounts_inner(state, headers, body)
            .await
            .into_response(),
    )
}

pub(super) async fn preview_import(
    State(state): State<CoreState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    no_store(
        preview_import_inner(state, headers, body)
            .await
            .into_response(),
    )
}

pub(super) async fn import_accounts(
    State(state): State<CoreState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    no_store(
        import_accounts_inner(state, headers, body)
            .await
            .into_response(),
    )
}

async fn export_accounts_inner(
    state: CoreState,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AccountExport>, V3ApiError> {
    ensure_transport(&state, &headers)?;
    ensure_body_bound(&state, &body)?;
    let input = parse_json::<AccountExportRequest>(&body)?;
    validate_bundle_password(&input.bundle_password)
        .map_err(|error| map_transfer_error(&state, error))?;
    let permit = crypto_permit().map_err(|error| map_transfer_error(&state, error))?;
    let blocking_state = state.clone();
    let bundle_password = Zeroizing::new(input.bundle_password);
    let exported = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let (payload, skipped_accounts, revision) = export_payload(&blocking_state)?;
        let payload = Zeroizing::new(payload);
        let exported_accounts = payload
            .credentials
            .iter()
            .filter(|credential| !is_observer_purpose(credential_purpose(credential)))
            .count() as u64;
        let bundle = encrypt_payload(&payload, bundle_password.as_str())?;
        Ok::<_, TransferError>((bundle, exported_accounts, skipped_accounts, revision))
    })
    .await
    .map_err(|_| V3ApiError::internal("account export worker failed"))?
    .map_err(|error| map_transfer_error(&state, error))?;
    let (bundle, exported_accounts, skipped_accounts, revision) = exported;
    Ok(Json(AccountExport {
        filename: format!(
            "ocg-manager-node-{}.ocgbackup",
            Utc::now().format("%Y%m%d-%H%M%S")
        ),
        bundle,
        exported_accounts,
        skipped_accounts,
        revision,
        process_generation: state.process_generation(),
    }))
}

async fn preview_import_inner(
    state: CoreState,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AccountImportPreview>, V3ApiError> {
    ensure_transport(&state, &headers)?;
    ensure_body_bound(&state, &body)?;
    let input = parse_json::<AccountImportPreviewRequest>(&body)?;
    validate_bundle_password(&input.password).map_err(|error| map_transfer_error(&state, error))?;
    ensure_bundle_bound(&input.bundle).map_err(|error| map_transfer_error(&state, error))?;
    let permit = crypto_permit().map_err(|error| map_transfer_error(&state, error))?;
    let password = Zeroizing::new(input.password);
    let bundle = Zeroizing::new(input.bundle);
    let validated = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decrypt_and_validate(bundle.as_str(), password.as_str())
    })
    .await
    .map_err(|_| V3ApiError::internal("account import preview worker failed"))?
    .map_err(|error| map_transfer_error(&state, error))?;
    let (items, importable_accounts, duplicate_accounts, revision) =
        preview_against_current(&state, &validated)?;
    Ok(Json(AccountImportPreview {
        exported_at: validated.exported_at,
        items,
        importable_accounts,
        duplicate_accounts,
        revision,
        process_generation: state.process_generation(),
    }))
}

async fn import_accounts_inner(
    state: CoreState,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<AccountImportResult>, V3ApiError> {
    ensure_transport(&state, &headers)?;
    ensure_body_bound(&state, &body)?;
    let input = parse_mutation_json::<AccountImportRequest>(&body)?;
    validate_bundle_password(&input.password).map_err(|error| map_transfer_error(&state, error))?;
    ensure_bundle_bound(&input.bundle).map_err(|error| map_transfer_error(&state, error))?;
    let expectation = input.expectation;
    let permit = crypto_permit().map_err(|error| map_transfer_error(&state, error))?;
    let password = Zeroizing::new(input.password);
    let bundle = Zeroizing::new(input.bundle);
    let validated = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decrypt_and_validate(bundle.as_str(), password.as_str())
    })
    .await
    .map_err(|_| V3ApiError::internal("account import worker failed"))?
    .map_err(|error| map_transfer_error(&state, error))?;

    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &expectation)?;
    validate_node_merge_against_current(&state, &validated.node)?;
    validate_platform_merge_against_current(&state, &validated.unified.platform_accounts)?;
    validate_identity_merge_against_current(&state, &validated)?;
    let existing_ids = current_account_ids(&state)?;
    let mut records = Vec::new();
    let mut items = Vec::with_capacity(validated.accounts.len());
    let duplicate_accounts = 0_u64;
    let mut account_id_map = HashMap::new();
    let mut imported_account_ids = HashSet::new();
    for account in validated.accounts {
        let now = Utc::now();
        let id = account.id.clone().ok_or_else(|| {
            V3ApiError::invalid_request_at(&state, "imported credential is missing a stable id")
        })?;
        account_id_map.insert(id.clone(), id.clone());
        imported_account_ids.insert(id.clone());
        let key_cipher = if account.key.is_empty() {
            String::new()
        } else {
            state
                .encrypt_key(account.key.as_str())
                .map_err(|_| V3ApiError::internal("failed to protect an imported credential"))?
        };
        let password_cipher = account
            .password
            .as_ref()
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|plaintext| state.encrypt_key(plaintext))
            .transpose()
            .map_err(|_| V3ApiError::internal("failed to protect an imported credential"))?;
        let model = ModelAccount {
            id,
            provider_id: account.provider_id.clone(),

            credential_kind: account.credential_kind,
            quota_scope: account.quota_scope,
            name: account.name.clone(),
            username: account.username.clone(),
            password_cipher,
            key_cipher,
            enabled: account.enabled,
            account_type: account.account_type,
            setup_step: account.setup_step,
            referral_code: None,
            purchase_date: account.purchase_date.clone(),
            expires_on: account.expires_on.clone(),
            cooldown_until: account.cooldowns.until,
            cooldown_generic_until: account.cooldowns.generic,
            cooldown_5h_until: account.cooldowns.five_hours,
            cooldown_week_until: account.cooldowns.week,
            cooldown_month_until: account.cooldowns.month,
            cooldown_free_until: account.cooldowns.free,
            last_error: None,
            auth_error: None,
            notes: account.notes.clone(),
            created_at: now,
            updated_at: now,
        };
        records.push(AccountImportRecord {
            account: model,
            custom_config: account.custom_config.clone(),
            capabilities: account.capabilities.clone(),
            verification_status: account.verification_status,
            connection_verified_at: account.connection_verified_at,
            ollama_billing_tier: account.ollama_billing_tier,
        });
        items.push(preview_item(
            &account,
            if existing_ids.contains(account.id.as_deref().unwrap_or_default()) {
                AccountImportDisposition::Merged
            } else {
                AccountImportDisposition::Imported
            },
            None,
        ));
    }

    let imported_accounts = records.len() as u64;
    let identity_snapshot = match validated.unified.identity_snapshot.as_ref() {
        Some(snapshot) => {
            let remapped = snapshot
                .remap_account_ids(&account_id_map)
                .filter_account_ids(&imported_account_ids);
            if remapped.accounts.len() != imported_account_ids.len() {
                return Err(V3ApiError::invalid_request_at(
                    &state,
                    "imported identity snapshot does not cover the selected accounts",
                ));
            }
            if let Some(conflict) = state
                .db
                .lock()
                .identity_import_conflict(&remapped, &imported_account_ids)
                .map_err(|_| V3ApiError::internal("failed to inspect destination identity model"))?
            {
                return Err(V3ApiError::conflict_at(&state, conflict));
            }
            Some(remapped)
        }
        None => None,
    };
    let mut node = validated.node;
    let previous = state.config();
    node.config.gateway_port = previous.gateway_port;
    node.config.client_root_url = previous.client_root_url;
    node.config.auto_start = previous.auto_start;
    node.config.show_dock_icon = previous.show_dock_icon;
    node.config
        .validate()
        .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    let now = Utc::now();
    let sub_keys = node
        .access_keys
        .iter()
        .map(|key| {
            Ok(SubGatewayKey {
                id: key.id.clone(),
                name: key.name.clone(),
                key: key.key.clone(),
                enabled: key.enabled,
                deleted_at: None,
                created_at: DateTime::parse_from_rfc3339(&key.created_at)
                    .map_err(|_| V3ApiError::invalid_request_at(&state, "invalid Access Key time"))?
                    .with_timezone(&Utc),
            })
        })
        .collect::<Result<Vec<_>, V3ApiError>>()?;
    let mut platform_observer_ciphers = HashMap::new();
    for (parent_id, plaintext) in observer_plaintext_by_parent(&validated.unified) {
        platform_observer_ciphers.insert(
            parent_id,
            state.encrypt_key(&plaintext).map_err(|_| {
                V3ApiError::internal("failed to protect an imported observer secret")
            })?,
        );
    }
    let cpa_management_key_cipher = validated
        .unified
        .cpa_management_key
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|plaintext| state.encrypt_key(plaintext))
        .transpose()
        .map_err(|_| V3ApiError::internal("failed to protect an imported CPA secret"))?;
    let node_record = NodeImportRecord {
        platform_links_authoritative: validated.unified.platform_links_authoritative,
        platform_accounts: validated.unified.platform_accounts,
        platform_links: validated.unified.platform_links,
        platform_catalogs: validated.unified.platform_catalogs,
        accounts: records,
        account_order: node.account_order.clone(),
        config_json: serde_json::to_string(&node.config)
            .map_err(|_| V3ApiError::internal("failed to encode imported settings"))?,
        sub_keys,
        zen_free_enabled: node.zen_free.enabled,
        zen_catalog: crate::kernel::zen::ZenFreeModelCatalog {
            models: node.zen_free.models.clone(),
            refreshed_at: node
                .zen_free
                .refreshed_at
                .as_deref()
                .map(DateTime::parse_from_rfc3339)
                .transpose()
                .map_err(|_| V3ApiError::invalid_request_at(&state, "invalid Zen catalog time"))?
                .map(|value| value.with_timezone(&Utc)),
            source_url: node.zen_free.source_url.clone(),
        },
        provider_contracts: persisted_contracts_from_portable(&node.provider_contracts, now)
            .map_err(|error| V3ApiError::invalid_request_at(&state, error))?,
        dynamic_providers: validated.unified.dynamic_providers,
        identity_snapshot,
        draft_provider_ids: validated.unified.draft_provider_ids,
        platform_observer_ciphers,
        platform_snapshots: validated.unified.platform_snapshots,
        platform_versions: validated.unified.platform_versions,
        cpa_base_url: validated.unified.cpa_base_url,
        cpa_management_key_cipher,
    };
    let runtime = state
        .db
        .lock()
        .import_node_state(&node_record, |db| state.prepare_imported_node_runtime(db))
        .map_err(|error| V3ApiError::conflict_at(&state, error.to_string()))?;
    state.install_imported_node_runtime(runtime);
    let revision = state.settings_revision();

    Ok(Json(AccountImportResult {
        items,
        imported_accounts,
        duplicate_accounts,
        revision,
        process_generation: state.process_generation(),
    }))
}

fn export_payload(state: &CoreState) -> Result<(PortablePayload, u64, u64), TransferError> {
    let _settings_update = state.settings_update.lock();
    let revision = state.settings_revision();
    let (destinations, credentials, skipped) = export_new_model(state)?;
    if credentials
        .iter()
        .filter(|credential| !is_observer_purpose(credential_purpose(credential)))
        .count()
        > MAX_ACCOUNTS
    {
        return Err(TransferError::Invalid(format!(
            "at most {MAX_ACCOUNTS} accounts can be exported at once"
        )));
    }
    let (sub_keys, persisted_contracts, quota_pools, account_order, zen_enabled) = {
        let db = state.db.lock();
        let sub_keys = db
            .list_active_sub_gateway_keys()
            .map_err(|_| TransferError::Internal)?;
        let persisted_contracts = db
            .load_persisted_contracts()
            .map_err(|_| TransferError::Internal)?;
        let quota_pools = db.list_quota_pools().map_err(|_| TransferError::Internal)?;
        let accounts = db.list_accounts().map_err(|_| TransferError::Internal)?;
        let exported_ids = credentials
            .iter()
            .filter(|credential| !is_observer_purpose(credential_purpose(credential)))
            .map(|credential| credential.legacy_account_id.clone())
            .collect::<HashSet<_>>();
        let mut account_order = Vec::new();
        let mut zen_enabled = false;
        for account in accounts {
            if account.id == crate::provider::CPA_ACCOUNT_ID {
                continue;
            }
            if account.is_zen_free() {
                zen_enabled = account.enabled;
                account_order.push(account.id);
                continue;
            }
            if exported_ids.contains(&account.id) {
                account_order.push(account.id);
            }
        }
        (
            sub_keys,
            persisted_contracts,
            quota_pools,
            account_order,
            zen_enabled,
        )
    };
    let access_keys = sub_keys
        .into_iter()
        .map(|key| PortableAccessKey {
            id: key.id,
            name: key.name,
            key: key.key,
            enabled: key.enabled,
            created_at: key.created_at.to_rfc3339(),
        })
        .collect::<Vec<_>>();
    if access_keys.len() > MAX_ACCESS_KEYS {
        return Err(TransferError::Invalid(format!(
            "at most {MAX_ACCESS_KEYS} sub Keys can be exported at once"
        )));
    }
    let mut provider_contracts = persisted_contracts
        .scopes
        .values()
        .filter(|row| row.scope.kind() == ContractScopeKind::Provider)
        .map(|row| {
            let evidence = persisted_contracts
                .evidence
                .get(&row.scope)
                .into_iter()
                .flatten()
                .map(|evidence| PortableProtocolEvidence {
                    model_id: evidence.model_id.clone(),
                    protocol: evidence.protocol.as_str().to_string(),
                    source: evidence.source.as_str().to_string(),
                    verified_at: evidence.verified_at.map(|value| value.to_rfc3339()),
                    observed_at: evidence.observed_at.map(|value| value.to_rfc3339()),
                })
                .collect();
            let overrides = persisted_contracts
                .overrides
                .get(&row.scope)
                .into_iter()
                .flatten()
                .map(|override_row| PortableProtocolOverride {
                    model_id: override_row.model_id.clone(),
                    protocol: override_row.protocol.as_str().to_string(),
                    state: override_row.state.as_str().to_string(),
                })
                .collect();
            PortableProviderContract {
                provider_id: row.scope.id().to_string(),
                catalog_models: row.catalog_models.clone(),
                catalog_refreshed_at: row.catalog_refreshed_at.map(|value| value.to_rfc3339()),
                catalog_source: row.catalog_source.clone(),
                catalog_source_url: row.catalog_source_url.clone(),
                evidence,
                overrides,
                preferences: persisted_contracts
                    .preferences
                    .get(&row.scope)
                    .into_iter()
                    .flatten()
                    .map(|(model_id, protocol)| PortableProtocolPreference {
                        model_id: model_id.clone(),
                        protocol: protocol.as_str().to_string(),
                    })
                    .collect(),
            }
        })
        .collect::<Vec<_>>();
    provider_contracts.sort_by(|left, right| left.provider_id.cmp(&right.provider_id));
    let exported_ids = credentials
        .iter()
        .filter(|credential| !is_observer_purpose(credential_purpose(credential)))
        .map(|credential| credential.legacy_account_id.clone())
        .collect::<HashSet<_>>();
    let mut portable_quota_pools = quota_pools
        .into_iter()
        .filter_map(|pool| {
            let members = pool
                .member_account_ids
                .into_iter()
                .filter(|id| exported_ids.contains(id))
                .collect::<Vec<_>>();
            if members.is_empty() {
                None
            } else {
                Some(PortableQuotaPool {
                    id: pool.id,
                    subject_kind: pool.subject_kind,
                    subject_ref: pool.subject_ref,
                    relation_confidence: pool.relation_confidence,
                    policy_mode: pool.policy_mode,
                    member_account_ids: members,
                })
            }
        })
        .collect::<Vec<_>>();
    portable_quota_pools.sort_by(|left, right| left.id.cmp(&right.id));
    let zen_catalog = state.zen_free_model_catalog();
    Ok((
        PortablePayload {
            platform_accounts: Vec::new(),
            platform_links: Vec::new(),
            version: PAYLOAD_VERSION,
            exported_at: Utc::now().to_rfc3339(),
            accounts: Vec::new(),
            dynamic_providers: Vec::new(),
            identities: Vec::new(),
            quota_pools: portable_quota_pools,
            destinations,
            credentials,
            node: Some(PortableNodeState {
                config: state.config(),
                access_keys,
                zen_free: PortableZenFree {
                    enabled: zen_enabled,
                    models: zen_catalog.models.clone(),
                    refreshed_at: zen_catalog.refreshed_at.map(|value| value.to_rfc3339()),
                    source_url: zen_catalog.source_url.clone(),
                },
                account_order,
                provider_contracts,
            }),
        },
        skipped,
        revision,
    ))
}

#[cfg(test)]
fn migration_exports_key(account_type: ModelAccountType, setup_step: ModelSetupStep) -> bool {
    account_type == ModelAccountType::Key
        || (account_type == ModelAccountType::Managed && setup_step == ModelSetupStep::Ready)
}

fn encrypt_payload(payload: &PortablePayload, password: &str) -> Result<String, TransferError> {
    let mut salt = [0_u8; SALT_LEN];
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|_| TransferError::Internal)?;
    getrandom::fill(&mut nonce).map_err(|_| TransferError::Internal)?;
    encrypt_payload_with_material(payload, password, salt, nonce)
}

fn encrypt_payload_with_material(
    payload: &PortablePayload,
    password: &str,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
) -> Result<String, TransferError> {
    let plaintext =
        Zeroizing::new(serde_json::to_vec(payload).map_err(|_| TransferError::Internal)?);
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(TransferError::Invalid(
            "account backup is too large".to_string(),
        ));
    }
    let key = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| TransferError::Internal)?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_slice(),
                aad: AAD,
            },
        )
        .map_err(|_| TransferError::Internal)?;
    let envelope = EncryptedEnvelope {
        format: ENVELOPE_FORMAT.to_string(),
        version: ENVELOPE_VERSION,
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        ciphertext: STANDARD.encode(ciphertext),
    };
    serde_json::to_string_pretty(&envelope).map_err(|_| TransferError::Internal)
}

fn decrypt_and_validate(bundle: &str, password: &str) -> Result<ValidatedMigration, TransferError> {
    let envelope: EncryptedEnvelope =
        serde_json::from_str(bundle).map_err(|_| TransferError::InvalidBundle)?;
    if envelope.format != ENVELOPE_FORMAT || envelope.version != ENVELOPE_VERSION {
        return Err(TransferError::InvalidBundle);
    }
    let salt = STANDARD
        .decode(envelope.salt)
        .map_err(|_| TransferError::InvalidBundle)?;
    let nonce = STANDARD
        .decode(envelope.nonce)
        .map_err(|_| TransferError::InvalidBundle)?;
    let ciphertext = STANDARD
        .decode(envelope.ciphertext)
        .map_err(|_| TransferError::InvalidBundle)?;
    if salt.len() != SALT_LEN
        || nonce.len() != NONCE_LEN
        || ciphertext.len() > MAX_PLAINTEXT_BYTES + 32
    {
        return Err(TransferError::InvalidBundle);
    }
    let key = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(key.as_ref()).map_err(|_| TransferError::Internal)?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: AAD,
                },
            )
            .map_err(|_| TransferError::InvalidBundle)?,
    );
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(TransferError::InvalidBundle);
    }
    let value: serde_json::Value =
        serde_json::from_slice(plaintext.as_slice()).map_err(|_| TransferError::InvalidBundle)?;
    match value.get("version").and_then(|version| version.as_u64()) {
        Some(version)
            if (MIN_SUPPORTED_PAYLOAD_VERSION..=PAYLOAD_VERSION).contains(&(version as u32)) => {}
        Some(version) => return Err(TransferError::UnsupportedVersion(version as u32)),
        None => return Err(TransferError::InvalidBundle),
    }
    let payload: PortablePayload =
        serde_json::from_value(value).map_err(|_| TransferError::InvalidBundle)?;
    validate_payload(payload)
}

fn derive_key(password: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>, TransferError> {
    let params = Params::new(ARGON_MEMORY_KIB, ARGON_ITERATIONS, ARGON_LANES, Some(32))
        .map_err(|_| TransferError::Internal)?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0_u8; 32]);
    argon
        .hash_password_into(password.as_bytes(), salt, key.as_mut())
        .map_err(|_| TransferError::Internal)?;
    Ok(key)
}

fn validate_payload(payload: PortablePayload) -> Result<ValidatedMigration, TransferError> {
    let mut payload = Zeroizing::new(payload);
    if !(MIN_SUPPORTED_PAYLOAD_VERSION..=PAYLOAD_VERSION).contains(&payload.version) {
        return Err(TransferError::UnsupportedVersion(payload.version));
    }
    let has_identity_semantics = !payload.identities.is_empty()
        || !payload.quota_pools.is_empty()
        || payload.accounts.iter().any(|account| {
            account.identity_id.is_some()
                || account.credential_id.is_some()
                || account.binding_id.is_some()
                || account.binding_model_scope.is_some()
                || account.binding_enabled.is_some()
                || account.allowed_endpoint_ids.is_some()
                || account.allowed_origins.is_some()
                || account.cooldowns.is_some()
        });
    if payload.version < V6_PAYLOAD_VERSION && has_identity_semantics {
        return Err(TransferError::Invalid(
            "this backup carries identity semantics that cannot be imported as a V4/V5 package"
                .to_string(),
        ));
    }
    let has_destination_semantics =
        !payload.destinations.is_empty() || !payload.credentials.is_empty();
    if payload.version < PAYLOAD_VERSION && has_destination_semantics {
        return Err(TransferError::Invalid(
            "this backup carries destination semantics that cannot be imported as a V4/V5/V6 package"
                .to_string(),
        ));
    }
    if payload.exported_at.chars().count() > 64
        || DateTime::parse_from_rfc3339(&payload.exported_at).is_err()
    {
        return Err(TransferError::InvalidBundle);
    }
    let exported_at = payload.exported_at.clone();
    if payload.version >= PAYLOAD_VERSION {
        return finish_v7_migration(&mut payload, exported_at);
    }
    if payload.accounts.len() > MAX_ACCOUNTS || payload.node.is_none() {
        return Err(TransferError::InvalidBundle);
    }
    let (validated_dynamics, draft_provider_ids) =
        validate_portable_dynamic_providers(&payload.dynamic_providers, payload.version)?;
    let mut account_ids = HashSet::new();
    let mut validated = Vec::with_capacity(payload.accounts.len());
    let carries_cooldowns = payload.version >= V6_PAYLOAD_VERSION;
    for (index, account) in payload.accounts.iter_mut().enumerate() {
        let prefix = || format!("account {}", index + 1);
        let id = match account.id.as_deref().map(str::trim) {
            Some(id) => {
                uuid::Uuid::parse_str(id).map_err(|_| {
                    TransferError::Invalid(format!("{} has an invalid account id", prefix()))
                })?;
                Some(id.to_string())
            }
            None => {
                return Err(TransferError::Invalid(format!(
                    "{} is missing its account id",
                    prefix()
                )));
            }
        };
        account.provider_id = account.provider_id.trim().to_string();
        account.name = account.name.trim().to_string();
        account.key = account.key.trim().to_string();
        if account.key.chars().count() > MAX_KEY_CHARS {
            return Err(TransferError::Invalid(format!(
                "{} has an account Key that is too long",
                prefix()
            )));
        }
        if account
            .username
            .as_deref()
            .is_some_and(|value| value.chars().count() > MAX_USERNAME_CHARS)
        {
            return Err(TransferError::Invalid(format!(
                "{} has a username that is too long",
                prefix()
            )));
        }
        if account.name.is_empty() || account.name.chars().count() > MAX_NAME_CHARS {
            return Err(TransferError::Invalid(format!(
                "{} has an invalid name",
                prefix()
            )));
        }
        let dynamic = validated_dynamics
            .iter()
            .find(|runtime| crate::dynamic::provider_ids_equal(&runtime.id, &account.provider_id));
        let plan = builtin_provider(&account.provider_id);
        if plan.is_none() && dynamic.is_none() {
            return Err(TransferError::Invalid(format!(
                "{} references an unknown provider",
                prefix()
            )));
        }
        if let Some(plan) = plan
            && (plan.singleton_account_id.is_some()
                || plan.creation_availability == CreationAvailability::Unavailable)
        {
            return Err(TransferError::Invalid(format!(
                "{} uses a Plan that cannot be imported",
                prefix()
            )));
        }
        let account_type =
            ModelAccountType::try_from(account.account_type.as_str()).map_err(|_| {
                TransferError::Invalid(format!("{} has an invalid account type", prefix()))
            })?;
        let source_setup = ModelSetupStep::try_from(account.setup_step.as_str()).map_err(|_| {
            TransferError::Invalid(format!("{} has an invalid setup step", prefix()))
        })?;
        let requires_key = dynamic
            .map(|runtime| runtime.auth_kind.requires_key())
            .unwrap_or(true);
        let (setup_step, key, enabled) = match account_type {
            ModelAccountType::Key => {
                if source_setup != ModelSetupStep::Ready || (requires_key && account.key.is_empty())
                {
                    return Err(TransferError::Invalid(format!(
                        "{} is missing its account Key",
                        prefix()
                    )));
                }
                if !requires_key && !account.key.is_empty() {
                    return Err(TransferError::Invalid(format!(
                        "{} must not include an account Key",
                        prefix()
                    )));
                }
                if let Some(plan) = plan {
                    crate::provider::validate_plan_key(plan, &account.key).map_err(|_| {
                        TransferError::Invalid(format!("{} has an invalid account Key", prefix()))
                    })?;
                }
                (
                    ModelSetupStep::Ready,
                    Zeroizing::new(std::mem::take(&mut account.key)),
                    account.enabled
                        && (dynamic.is_some() || provider_allows_enablement(&account.provider_id)),
                )
            }
            ModelAccountType::Managed => {
                if dynamic.is_some() || !plan.is_some_and(|plan| plan.managed_registration) {
                    return Err(TransferError::Invalid(format!(
                        "{} is not a supported managed account",
                        prefix()
                    )));
                }
                if source_setup == ModelSetupStep::Ready {
                    if account.key.is_empty() {
                        return Err(TransferError::Invalid(format!(
                            "{} is missing its managed account Key",
                            prefix()
                        )));
                    }
                    crate::provider::validate_plan_key(
                        plan.expect("managed accounts require a built-in Plan"),
                        &account.key,
                    )
                    .map_err(|_| {
                        TransferError::Invalid(format!("{} has an invalid account Key", prefix()))
                    })?;
                    (
                        ModelSetupStep::Ready,
                        Zeroizing::new(std::mem::take(&mut account.key)),
                        account.enabled && provider_allows_enablement(&account.provider_id),
                    )
                } else {
                    (
                        ModelSetupStep::GoogleAccount,
                        Zeroizing::new(String::new()),
                        false,
                    )
                }
            }
        };
        let purchase_date = if account.purchase_date.trim().is_empty() {
            String::new()
        } else {
            normalize_purchase_date(&account.purchase_date).map_err(|_| {
                TransferError::Invalid(format!("{} has an invalid purchase date", prefix()))
            })?
        };
        let ollama_billing_tier = match account.ollama_billing_tier.as_deref() {
            None | Some("") => None,
            Some(value) => {
                if account.provider_id != crate::provider::OLLAMA_PROVIDER_ID {
                    return Err(TransferError::Invalid(format!(
                        "{} has an Ollama billing tier on a non-Ollama account",
                        prefix()
                    )));
                }
                let tier = crate::provider::OllamaBillingTier::parse(value).map_err(|_| {
                    TransferError::Invalid(format!(
                        "{} has an invalid Ollama billing tier",
                        prefix()
                    ))
                })?;
                if tier.requires_purchase_date() && purchase_date.is_empty() {
                    return Err(TransferError::Invalid(format!(
                        "{} is a paid Ollama tier and requires a purchase date",
                        prefix()
                    )));
                }
                Some(tier)
            }
        };
        let notes = match account.notes.as_deref() {
            Some(value) if value.chars().count() > MAX_NOTES_CHARS => {
                return Err(TransferError::Invalid(format!(
                    "{} has notes that are too long",
                    prefix()
                )));
            }
            Some(value) => normalize_account_notes(value)
                .map_err(|_| TransferError::Invalid(format!("{} has invalid notes", prefix())))?,
            None => None,
        };
        if account.expires_on.chars().count() > 64 {
            return Err(TransferError::Invalid(format!(
                "{} has an invalid expiration date",
                prefix()
            )));
        }
        let verification_status = match account.verification_status.as_deref() {
            Some(value) => ConnectionVerificationStatus::try_from(value).map_err(|_| {
                TransferError::Invalid(format!("{} has an invalid verification state", prefix()))
            })?,
            None => plan.map_or(
                ConnectionVerificationStatus::NotRequired,
                crate::provider::default_verification_status,
            ),
        };
        let connection_verified_at = account
            .connection_verified_at
            .as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| {
                TransferError::Invalid(format!("{} has an invalid verification time", prefix()))
            })?
            .map(|value| value.with_timezone(&Utc));
        let verification_gates_enablement = plan.is_some_and(|plan| {
            plan.verification_policy == crate::provider::VerificationPolicy::Required
                && crate::provider::ProviderRegistry::get(&account.provider_id)
                    .is_some_and(|descriptor| descriptor.card_actions.enable_requires_verification)
        });
        if enabled && verification_gates_enablement && !verification_status.allows_enablement() {
            return Err(TransferError::Invalid(format!(
                "{} is enabled without a usable verification state",
                prefix()
            )));
        }
        let enabled =
            enabled && (!verification_gates_enablement || verification_status.allows_enablement());
        let requires_custom = plan.is_some_and(crate::provider::plan_requires_custom_config);
        let (custom_config, capabilities) = if requires_custom {
            let config = account.custom_config.as_ref().ok_or_else(|| {
                TransferError::Invalid(format!("{} is missing its Custom Endpoint", prefix()))
            })?;
            if config.endpoint_url.chars().count() > MAX_ENDPOINT_CHARS {
                return Err(TransferError::Invalid(format!(
                    "{} has a Custom Endpoint that is too long",
                    prefix()
                )));
            }
            let endpoint_url = crate::custom::validate_custom_endpoint_url(&config.endpoint_url)
                .map_err(|_| {
                    TransferError::Invalid(format!("{} has an invalid Custom Endpoint", prefix()))
                })?;
            let protocol = UpstreamProtocolKind::try_from(config.upstream_protocol.as_str())
                .map_err(|_| {
                    TransferError::Invalid(format!("{} has an invalid upstream protocol", prefix()))
                })?;
            if account.model_capabilities.is_empty()
                || account.model_capabilities.len() > MAX_CAPABILITIES
            {
                return Err(TransferError::Invalid(format!(
                    "{} has an invalid model capability list",
                    prefix()
                )));
            }
            let mut seen_models = HashSet::new();
            let capabilities = account
                .model_capabilities
                .iter()
                .map(|capability| {
                    let (public_model, upstream_model, protocol_value) = match capability {
                        PortableModelCapability::Canonical(capability) => (
                            capability.public_model.trim().to_string(),
                            capability.upstream_model.trim().to_string(),
                            capability.protocol.as_str(),
                        ),
                        PortableModelCapability::Legacy(capability) => {
                            let model_id = capability.model_id.trim().to_string();
                            (model_id.clone(), model_id, capability.protocol.as_str())
                        }
                    };
                    crate::provider::validate_custom_model_id(&public_model).map_err(|_| {
                        TransferError::Invalid(format!("{} has an invalid public model", prefix()))
                    })?;
                    crate::provider::validate_custom_model_id(&upstream_model).map_err(|_| {
                        TransferError::Invalid(format!(
                            "{} has an invalid upstream model",
                            prefix()
                        ))
                    })?;
                    if !seen_models.insert(public_model.to_ascii_lowercase()) {
                        return Err(TransferError::Invalid(format!(
                            "{} contains duplicate public models",
                            prefix()
                        )));
                    }
                    let capability_protocol = UpstreamProtocolKind::try_from(protocol_value)
                        .map_err(|_| {
                            TransferError::Invalid(format!(
                                "{} has an invalid model protocol",
                                prefix()
                            ))
                        })?;
                    if capability_protocol != protocol {
                        return Err(TransferError::Invalid(format!(
                            "{} has a model protocol mismatch",
                            prefix()
                        )));
                    }
                    Ok(AccountModelCapabilityInput {
                        public_model,
                        upstream_model,
                        protocol: capability_protocol,
                        source: Some("import".to_string()),
                    })
                })
                .collect::<Result<Vec<_>, TransferError>>()?;
            crate::custom::validate_custom_capability_expansion(protocol, &capabilities).map_err(
                |_| TransferError::Invalid(format!("{} has invalid Custom capabilities", prefix())),
            )?;
            (
                Some(AccountCustomConfigInput {
                    endpoint_url,
                    upstream_protocol: protocol,
                }),
                capabilities,
            )
        } else {
            if account.custom_config.is_some() || !account.model_capabilities.is_empty() {
                return Err(TransferError::Invalid(format!(
                    "{} contains Custom-only fields",
                    prefix()
                )));
            }
            (None, Vec::new())
        };
        let duplicate = !account_ids.insert(id.clone().expect("account id was validated"));
        if duplicate {
            return Err(TransferError::Invalid(format!(
                "{} duplicates an earlier account identity in the package",
                prefix()
            )));
        }
        validated.push(ValidatedAccount {
            portable_index: index,
            id,
            provider_id: account.provider_id.clone(),

            name: account.name.clone(),
            username: account.username.clone().and_then(trim_optional),
            key,
            password: None,
            enabled,
            account_type,
            setup_step,
            purchase_date,
            expires_on: account.expires_on.clone(),
            notes,
            verification_status,
            connection_verified_at,
            custom_config,
            capabilities,
            ollama_billing_tier,
            credential_kind: dynamic
                .map(|runtime| runtime.auth_kind.credential_kind())
                .or_else(|| plan.map(|plan| plan.credential_kind))
                .expect("imported account provider was resolved"),
            quota_scope: dynamic
                .map(|runtime| runtime.auth_kind.quota_scope())
                .or_else(|| plan.map(|plan| plan.quota_scope))
                .expect("imported account provider was resolved"),
            identity: None,
            cooldowns: if carries_cooldowns {
                account.cooldowns.clone().ok_or_else(|| {
                    TransferError::Invalid(format!("{} is missing its cooldown snapshot", prefix()))
                })?
            } else {
                PortableCooldowns::default()
            },
        });
    }
    if payload.platform_accounts.len() > MAX_ACCOUNTS || payload.platform_links.len() > MAX_ACCOUNTS
    {
        return Err(TransferError::InvalidBundle);
    }
    if payload.version == 4
        && (!payload.platform_accounts.is_empty() || !payload.platform_links.is_empty())
    {
        return Err(TransferError::InvalidBundle);
    }
    let mut parent_ids = HashSet::new();
    for parent in &mut payload.platform_accounts {
        if uuid::Uuid::parse_str(&parent.id).is_err()
            || !parent_ids.insert(parent.id.clone())
            || parent.name.trim().is_empty()
            || parent.name.len() > 200
        {
            return Err(TransferError::InvalidBundle);
        }
        parent.base_url = crate::platform::validate_platform_base_url(&parent.base_url)
            .map_err(|_| TransferError::InvalidBundle)?;
    }
    let mut linked_ids = HashSet::new();
    for link in &mut payload.platform_links {
        if !parent_ids.contains(&link.platform_account_id)
            || !linked_ids.insert(link.account_id.clone())
            || !validated.iter().any(|a| {
                a.id.as_deref() == Some(link.account_id.as_str())
                    && crate::provider::is_custom_api(&a.provider_id)
            })
        {
            return Err(TransferError::InvalidBundle);
        }
        link.group.verified = false;
        link.group.subscription_type = None;
    }
    let node = validate_node_state(payload.node.take().expect("node was required"), &validated)?;
    let identity_snapshot = if payload.version >= V6_PAYLOAD_VERSION {
        Some(validate_identity_snapshot(
            &payload.identities,
            &payload.quota_pools,
            &payload.accounts,
            &mut validated,
        )?)
    } else {
        None
    };
    let unified = map_old_graph_to_unified(
        &payload,
        &validated,
        validated_dynamics,
        draft_provider_ids,
        identity_snapshot,
    )?;
    Ok(ValidatedMigration {
        exported_at,
        accounts: validated,
        node: Zeroizing::new(node),
        unified,
    })
}

fn validate_identity_snapshot(
    identities: &[PortableIdentity],
    quota_pools: &[PortableQuotaPool],
    accounts: &[PortableAccount],
    validated: &mut [ValidatedAccount],
) -> Result<IdentityImportSnapshot, TransferError> {
    if accounts.is_empty() {
        if !identities.is_empty() || !quota_pools.is_empty() {
            return Err(TransferError::Invalid(
                "identity snapshot references accounts that are not in the package".to_string(),
            ));
        }
        return Ok(IdentityImportSnapshot {
            identities: Vec::new(),
            accounts: Vec::new(),
            quota_pools: Vec::new(),
        });
    }
    let account_ids = validated
        .iter()
        .filter_map(|account| account.id.clone())
        .collect::<HashSet<_>>();
    let mut identity_ids = HashSet::new();
    let mut imported_identities = Vec::with_capacity(identities.len());
    for (index, identity) in identities.iter().enumerate() {
        let prefix = || format!("identity {}", index + 1);
        if uuid::Uuid::parse_str(identity.id.trim()).is_err()
            || !identity_ids.insert(identity.id.trim().to_string())
            || identity.label.trim().is_empty()
            || identity.label.chars().count() > MAX_NAME_CHARS
        {
            return Err(TransferError::Invalid(format!(
                "{} has an invalid identity id or label",
                prefix()
            )));
        }
        match identity.identity_confidence.as_str() {
            "opaque" | "declared" => {}
            "verified" => {
                return Err(TransferError::Invalid(format!(
                    "{} must not claim a verified relation",
                    prefix()
                )));
            }
            _ => {
                return Err(TransferError::Invalid(format!(
                    "{} has an invalid identity confidence",
                    prefix()
                )));
            }
        }
        imported_identities.push(ImportedIdentity {
            id: identity.id.trim().to_string(),
            label: identity.label.trim().to_string(),
            identity_confidence: identity.identity_confidence.clone(),
            authority_site: identity.authority_site.clone().and_then(trim_optional),
            authority_subject: identity.authority_subject.clone().and_then(trim_optional),
            enabled: identity.enabled,
            notes: identity.notes.clone().and_then(trim_optional),
        });
    }
    let mut credential_ids = HashSet::new();
    let mut binding_ids = HashSet::new();
    let mut linked = Vec::with_capacity(validated.len());
    for (index, account) in accounts.iter().enumerate() {
        let prefix = || format!("account {}", index + 1);
        let account_id = account.id.as_deref().map(str::trim).ok_or_else(|| {
            TransferError::Invalid(format!("{} is missing its account id", prefix()))
        })?;
        let identity_id = account
            .identity_id
            .as_deref()
            .map(str::trim)
            .ok_or_else(|| {
                TransferError::Invalid(format!("{} is missing its identity id", prefix()))
            })?;
        let credential_id = account
            .credential_id
            .as_deref()
            .map(str::trim)
            .ok_or_else(|| {
                TransferError::Invalid(format!("{} is missing its credential id", prefix()))
            })?;
        let binding_id = account
            .binding_id
            .as_deref()
            .map(str::trim)
            .ok_or_else(|| {
                TransferError::Invalid(format!("{} is missing its binding id", prefix()))
            })?;
        let credential_version = account.credential_version.ok_or_else(|| {
            TransferError::Invalid(format!("{} is missing its credential version", prefix()))
        })?;
        let auth_state_version = account.auth_state_version.ok_or_else(|| {
            TransferError::Invalid(format!(
                "{} is missing its credential auth-state version",
                prefix()
            ))
        })?;
        let binding_enabled = account.binding_enabled.ok_or_else(|| {
            TransferError::Invalid(format!("{} is missing its binding enabled flag", prefix()))
        })?;
        if credential_version == 0
            || credential_version > i64::MAX as u64
            || auth_state_version == 0
            || auth_state_version > i64::MAX as u64
        {
            return Err(TransferError::Invalid(format!(
                "{} has an out-of-range credential version",
                prefix()
            )));
        }
        let binding_model_scope = account.binding_model_scope.clone().ok_or_else(|| {
            TransferError::Invalid(format!("{} is missing its binding model scope", prefix()))
        })?;
        let allowed_endpoint_ids = account.allowed_endpoint_ids.clone().ok_or_else(|| {
            TransferError::Invalid(format!(
                "{} is missing its binding endpoint grants",
                prefix()
            ))
        })?;
        let allowed_origins = account.allowed_origins.clone().ok_or_else(|| {
            TransferError::Invalid(format!("{} is missing its binding origin grants", prefix()))
        })?;
        let mut seen_ids = HashSet::new();
        for id in &allowed_endpoint_ids {
            if id.trim().is_empty()
                || uuid::Uuid::parse_str(id.trim()).is_err()
                || !seen_ids.insert(id.trim().to_string())
            {
                return Err(TransferError::Invalid(format!(
                    "{} has a malformed binding endpoint grant",
                    prefix()
                )));
            }
        }
        let mut seen_origins = HashSet::new();
        for origin in &allowed_origins {
            let Some(normalized) = ocg_domain::credential::normalize_origin(origin) else {
                return Err(TransferError::Invalid(format!(
                    "{} has a malformed binding origin grant",
                    prefix()
                )));
            };
            if !seen_origins.insert(normalized) {
                return Err(TransferError::Invalid(format!(
                    "{} has a duplicate binding origin grant",
                    prefix()
                )));
            }
        }
        if uuid::Uuid::parse_str(identity_id).is_err()
            || uuid::Uuid::parse_str(credential_id).is_err()
            || uuid::Uuid::parse_str(binding_id).is_err()
        {
            return Err(TransferError::Invalid(format!(
                "{} has an invalid identity, credential, or binding id",
                prefix()
            )));
        }
        if !identity_ids.contains(identity_id) {
            return Err(TransferError::Invalid(format!(
                "{} references an identity that is not in the package",
                prefix()
            )));
        }
        if !credential_ids.insert(credential_id.to_string())
            || !binding_ids.insert(binding_id.to_string())
        {
            return Err(TransferError::Invalid(format!(
                "{} duplicates a credential or binding id in the package",
                prefix()
            )));
        }
        if let ModelScope::Only { models } = &binding_model_scope
            && (models.is_empty() || models.iter().any(|model| model.trim().is_empty()))
        {
            return Err(TransferError::Invalid(format!(
                "{} has an invalid binding model scope",
                prefix()
            )));
        }
        let link = ImportedAccountIdentity {
            account_id: account_id.to_string(),
            identity_id: identity_id.to_string(),
            credential_id: credential_id.to_string(),
            credential_version,
            auth_state_version,
            binding_id: binding_id.to_string(),
            binding_enabled,
            binding_model_scope,
            allowed_endpoint_ids: allowed_endpoint_ids
                .into_iter()
                .map(|id| id.trim().to_string())
                .collect(),
            allowed_origins: allowed_origins
                .into_iter()
                .filter_map(|origin| ocg_domain::credential::normalize_origin(&origin))
                .collect(),
        };
        validated[index].identity = Some(link.clone());
        linked.push(link);
    }
    let referenced_identities = linked
        .iter()
        .map(|row| row.identity_id.clone())
        .collect::<HashSet<_>>();
    if referenced_identities.len() != identity_ids.len() {
        return Err(TransferError::Invalid(
            "identity snapshot contains an identity that is not referenced by any account"
                .to_string(),
        ));
    }
    let mut pool_ids = HashSet::new();
    let mut covered_accounts = HashSet::new();
    let mut imported_pools = Vec::with_capacity(quota_pools.len());
    for (index, pool) in quota_pools.iter().enumerate() {
        let prefix = || format!("quota pool {}", index + 1);
        if uuid::Uuid::parse_str(pool.id.trim()).is_err()
            || !pool_ids.insert(pool.id.trim().to_string())
            || pool.member_account_ids.is_empty()
        {
            return Err(TransferError::Invalid(format!(
                "{} has an invalid id or no members",
                prefix()
            )));
        }
        match pool.subject_kind.as_str() {
            "credential" | "egress" => {}
            _ => {
                return Err(TransferError::Invalid(format!(
                    "{} has an invalid subject",
                    prefix()
                )));
            }
        }
        match pool.relation_confidence.as_str() {
            "unknown" | "declared" => {}
            "verified" => {
                return Err(TransferError::Invalid(format!(
                    "{} must not claim a verified relation",
                    prefix()
                )));
            }
            _ => {
                return Err(TransferError::Invalid(format!(
                    "{} has an invalid relation confidence",
                    prefix()
                )));
            }
        }
        match pool.policy_mode.as_str() {
            "observe_only" | "authoritative_limit" => {}
            _ => {
                return Err(TransferError::Invalid(format!(
                    "{} has an invalid policy mode",
                    prefix()
                )));
            }
        }
        let mut members = Vec::with_capacity(pool.member_account_ids.len());
        let mut seen_members = HashSet::new();
        for member in &pool.member_account_ids {
            let member = member.trim();
            if member.is_empty()
                || !account_ids.contains(member)
                || !seen_members.insert(member.to_string())
            {
                return Err(TransferError::Invalid(format!(
                    "{} references a missing, duplicate, or unknown account",
                    prefix()
                )));
            }
            covered_accounts.insert(member.to_string());
            members.push(member.to_string());
        }
        imported_pools.push(ImportedQuotaPool {
            id: pool.id.trim().to_string(),
            subject_kind: pool.subject_kind.clone(),
            subject_ref: pool.subject_ref.trim().to_string(),
            relation_confidence: pool.relation_confidence.clone(),
            policy_mode: pool.policy_mode.clone(),
            member_account_ids: members,
        });
    }
    if !covered_accounts.is_subset(&account_ids) {
        return Err(TransferError::Invalid(
            "quota pool snapshot references an account that is not in the package".to_string(),
        ));
    }
    Ok(IdentityImportSnapshot {
        identities: imported_identities,
        accounts: linked,
        quota_pools: imported_pools,
    })
}

fn validate_portable_dynamic_providers(
    providers: &[PortableProviderDefinition],
    payload_version: u32,
) -> Result<(Vec<DynamicProviderRuntime>, HashSet<String>), TransferError> {
    let now = Utc::now();
    let mut seen_ids = HashSet::new();
    let mut validated = Vec::with_capacity(providers.len());
    let mut draft_ids = HashSet::new();
    let require_draft_flag = payload_version >= V6_PAYLOAD_VERSION;
    for (index, provider) in providers.iter().enumerate() {
        let prefix = || format!("dynamic provider {}", index + 1);
        let id = provider.id.trim();
        uuid::Uuid::parse_str(id).map_err(|_| {
            TransferError::Invalid(format!("{} has an invalid provider id", prefix()))
        })?;
        if !seen_ids.insert(id.to_ascii_lowercase()) {
            return Err(TransferError::Invalid(format!(
                "{} duplicates an earlier provider id in the package",
                prefix()
            )));
        }
        if crate::dynamic::collides_with_known_id(id, &[]) {
            return Err(TransferError::Invalid(format!(
                "{} collides with a built-in provider",
                prefix()
            )));
        }
        let onboarding_draft = match provider.onboarding_draft {
            Some(flag) if require_draft_flag => flag,
            None if require_draft_flag => {
                return Err(TransferError::Invalid(format!(
                    "{} is missing required onboardingDraft",
                    prefix()
                )));
            }
            Some(_) => {
                return Err(TransferError::Invalid(format!(
                    "{} carries onboardingDraft which cannot be imported as a V4/V5 package",
                    prefix()
                )));
            }
            None => false,
        };
        let auth_kind = DynamicAuthKind::try_from(provider.auth_kind.trim()).map_err(|_| {
            TransferError::Invalid(format!("{} has an invalid auth kind", prefix()))
        })?;
        let protocol =
            UpstreamProtocolKind::try_from(provider.upstream_protocol.trim()).map_err(|_| {
                TransferError::Invalid(format!("{} has an invalid upstream protocol", prefix()))
            })?;
        let endpoint_url = crate::custom::validate_custom_endpoint_url(&provider.endpoint_url)
            .map_err(|_| TransferError::Invalid(format!("{} has an invalid Endpoint", prefix())))?;
        let mappings = provider
            .models
            .iter()
            .map(|model| {
                Ok(DynamicModelMapping {
                    public_model: model.public_model.clone(),
                    upstream_model: model.upstream_model.clone(),
                    upstream_override: model
                        .upstream_override
                        .as_ref()
                        .map(|value| {
                            Ok::<_, TransferError>(
                                ocg_domain::dynamic::DynamicModelUpstreamOverride {
                                    protocol: UpstreamProtocolKind::try_from(
                                        value.protocol.as_str(),
                                    )
                                    .map_err(|_| {
                                        TransferError::Invalid(format!(
                                            "{} has an invalid model protocol override",
                                            prefix()
                                        ))
                                    })?,
                                    endpoint_url: value.endpoint_url.clone(),
                                },
                            )
                        })
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>, TransferError>>()?;
        if mappings.is_empty() && !onboarding_draft {
            return Err(TransferError::Invalid(format!(
                "{} has no model mappings",
                prefix()
            )));
        }
        let definition = if mappings.is_empty() {
            DynamicProviderDefinition {
                preset_id: provider.preset_id.clone(),
                id: id.to_string(),
                name: ocg_domain::dynamic::normalize_dynamic_provider_name(&provider.name)
                    .map_err(|error| {
                        TransferError::Invalid(format!("{} is invalid: {error}", prefix()))
                    })?,
                endpoint_url,
                upstream_protocol: protocol,
                auth_kind,
                mappings,
            }
        } else {
            crate::dynamic::validate_definition(DynamicProviderDefinition {
                preset_id: provider.preset_id.clone(),
                id: id.to_string(),
                name: provider.name.clone(),
                endpoint_url,
                upstream_protocol: protocol,
                auth_kind,
                mappings,
            })
            .map_err(|error| TransferError::Invalid(format!("{} is invalid: {error}", prefix())))?
        };
        let preset_for_origin = definition.preset_id.as_deref();
        let origin = ocg_domain::provider::provider_origin_from_preset(preset_for_origin);
        let offering =
            ocg_domain::provider::preset_offering(preset_for_origin.unwrap_or("")).to_string();
        if onboarding_draft {
            draft_ids.insert(definition.id.clone());
        }
        validated.push(DynamicProviderRuntime {
            preset_id: definition.preset_id,
            id: definition.id,
            name: definition.name,
            endpoint_url: definition.endpoint_url,
            upstream_protocol: definition.upstream_protocol,
            auth_kind: definition.auth_kind,
            mappings: definition.mappings,
            created_at: now,
            updated_at: now,
            origin,
            offering,
        });
    }
    Ok((validated, draft_ids))
}

fn validate_node_state(
    mut node: PortableNodeState,
    accounts: &[ValidatedAccount],
) -> Result<PortableNodeState, TransferError> {
    node.config.gateway_key = node.config.gateway_key.trim().to_string();
    if node.config.gateway_key.is_empty()
        || node.config.gateway_key.chars().count() > MAX_KEY_CHARS
        || node.access_keys.len() > MAX_ACCESS_KEYS
        || node.provider_contracts.len() > MAX_PROVIDER_SCOPES
        || node.zen_free.models.len() > MAX_PROVIDER_MODELS
    {
        return Err(TransferError::InvalidBundle);
    }
    node.config.validate().map_err(TransferError::Invalid)?;
    let mut key_values = HashSet::new();
    let mut key_ids = HashSet::new();
    key_values.insert(node.config.gateway_key.clone());
    for key in &mut node.access_keys {
        key.id = key.id.trim().to_string();
        key.name = key.name.trim().to_string();
        key.key = key.key.trim().to_string();
        if uuid::Uuid::parse_str(&key.id).is_err()
            || key.id == crate::gateway_keys::PRIMARY_KEY_ID
            || !key_ids.insert(key.id.clone())
            || key.name.is_empty()
            || key.name.chars().count() > 64
            || key.key.is_empty()
            || key.key.chars().count() > MAX_KEY_CHARS
            || !key_values.insert(key.key.clone())
        {
            return Err(TransferError::Invalid(
                "node migration contains an invalid or duplicate Access Key".to_string(),
            ));
        }
        DateTime::parse_from_rfc3339(&key.created_at).map_err(|_| {
            TransferError::Invalid("node migration contains an invalid Access Key time".to_string())
        })?;
    }
    let expected_order = std::iter::once(crate::kernel::ids::ZEN_FREE_ACCOUNT_ID.to_string())
        .chain(accounts.iter().filter_map(|account| account.id.clone()))
        .collect::<HashSet<_>>();
    let actual_order = node.account_order.iter().cloned().collect::<HashSet<_>>();
    if node.account_order.len() != expected_order.len()
        || actual_order.len() != node.account_order.len()
        || actual_order != expected_order
    {
        return Err(TransferError::Invalid(
            "node migration contains an invalid account order".to_string(),
        ));
    }
    let mut zen_models = HashSet::new();
    for model in &mut node.zen_free.models {
        *model = model.trim().to_string();
        if model.is_empty() || model.chars().count() > 256 || !zen_models.insert(model.clone()) {
            return Err(TransferError::Invalid(
                "node migration contains an invalid Zen model catalog".to_string(),
            ));
        }
    }
    if node.zen_free.source_url.chars().count() > MAX_ENDPOINT_CHARS {
        return Err(TransferError::InvalidBundle);
    }
    if let Some(value) = node.zen_free.refreshed_at.as_deref() {
        DateTime::parse_from_rfc3339(value).map_err(|_| TransferError::InvalidBundle)?;
    }
    if let Some(zen_scope) = node
        .provider_contracts
        .iter()
        .find(|contract| contract.provider_id == crate::kernel::ids::OPENCODE_ZEN_FREE_PROVIDER_ID)
    {
        let scope_models = zen_scope
            .catalog_models
            .iter()
            .map(|model| model.trim().to_string())
            .collect::<Vec<_>>();
        if scope_models != node.zen_free.models {
            return Err(TransferError::Invalid(
                "Zen catalog does not match its Provider contract".to_string(),
            ));
        }
    }
    persisted_contracts_from_portable(&node.provider_contracts, Utc::now())
        .map_err(TransferError::Invalid)?;
    Ok(node)
}

fn persisted_contracts_from_portable(
    portable: &[PortableProviderContract],
    default_time: DateTime<Utc>,
) -> Result<PersistedContracts, String> {
    let mut persisted = PersistedContracts::default();
    let mut provider_scope_ids = HashSet::new();
    for contract in portable {
        // `provider_id` is the historical wire name for the opaque Provider
        // contract scope id. Existing values remain unchanged; future
        // Offerings may declare a distinct static scope id.
        let scope_id = contract.provider_id.trim();
        if scope_id.is_empty()
            || !provider_scope_ids.insert(scope_id.to_string())
            || crate::provider_contracts::provider_scope_descriptor(scope_id).is_none()
            || contract.catalog_models.len() > MAX_PROVIDER_MODELS
            || contract.evidence.len() > MAX_PROVIDER_MODELS * 3
            || contract.overrides.len() > MAX_PROVIDER_MODELS * 3
            || contract.catalog_source_url.chars().count() > MAX_ENDPOINT_CHARS
        {
            return Err("node migration contains an invalid Provider contract".to_string());
        }
        let mut catalog_models = Vec::with_capacity(contract.catalog_models.len());
        let mut seen_models = HashSet::new();
        for model in &contract.catalog_models {
            let model = model.trim();
            if model.is_empty()
                || model.chars().count() > 256
                || !seen_models.insert(model.to_string())
            {
                return Err("node migration contains an invalid Provider catalog".to_string());
            }
            catalog_models.push(model.to_string());
        }
        let scope = ContractScope::provider(scope_id);
        let refreshed_at = contract
            .catalog_refreshed_at
            .as_deref()
            .map(DateTime::parse_from_rfc3339)
            .transpose()
            .map_err(|_| "node migration contains an invalid Provider catalog time".to_string())?
            .map(|value| value.with_timezone(&Utc));
        persisted.scopes.insert(
            scope.clone(),
            PersistedScopeRow {
                scope: scope.clone(),
                catalog_models,
                catalog_refreshed_at: refreshed_at,
                catalog_source: contract.catalog_source.trim().to_string(),
                catalog_source_url: contract.catalog_source_url.trim().to_string(),
                revision: 1,
                updated_at: refreshed_at.unwrap_or(default_time),
            },
        );
        let mut evidence_rows = Vec::with_capacity(contract.evidence.len());
        let mut evidence_keys = HashSet::new();
        for evidence in &contract.evidence {
            let model_id = evidence.model_id.trim().to_string();
            let protocol = UpstreamProtocolKind::try_from(evidence.protocol.as_str())
                .map_err(|_| "node migration contains an invalid protocol".to_string())?;
            let source = ContractEvidenceSource::try_from(evidence.source.as_str())
                .map_err(|_| "node migration contains an invalid evidence source".to_string())?;
            if model_id.is_empty()
                || !evidence_keys.insert((model_id.clone(), protocol.as_str().to_string()))
            {
                return Err("node migration contains duplicate protocol evidence".to_string());
            }
            let parse_time = |value: Option<&str>| -> Result<Option<DateTime<Utc>>, String> {
                value
                    .map(DateTime::parse_from_rfc3339)
                    .transpose()
                    .map_err(|_| "node migration contains an invalid evidence time".to_string())
                    .map(|value| value.map(|value| value.with_timezone(&Utc)))
            };
            evidence_rows.push(PersistedModelProtocol {
                scope: scope.clone(),
                model_id,
                protocol,
                source,
                verified_at: parse_time(evidence.verified_at.as_deref())?,
                observed_at: parse_time(evidence.observed_at.as_deref())?,
                last_probe_result: None::<ProbeResultKind>,
                last_probe_at: None,
                last_probe_error: None,
            });
        }
        persisted.evidence.insert(scope.clone(), evidence_rows);
        let mut override_rows = Vec::with_capacity(contract.overrides.len());
        let mut override_keys = HashSet::new();
        for override_row in &contract.overrides {
            let model_id = override_row.model_id.trim().to_string();
            let protocol = UpstreamProtocolKind::try_from(override_row.protocol.as_str())
                .map_err(|_| "node migration contains an invalid override protocol".to_string())?;
            let state = ProtocolOverrideState::try_from(override_row.state.as_str())
                .map_err(|_| "node migration contains an invalid protocol override".to_string())?;
            if model_id.is_empty()
                || !override_keys.insert((model_id.clone(), protocol.as_str().to_string()))
            {
                return Err("node migration contains duplicate protocol overrides".to_string());
            }
            override_rows.push(PersistedModelProtocolOverride {
                scope: scope.clone(),
                model_id,
                protocol,
                state,
                updated_at: default_time,
            });
        }
        persisted.overrides.insert(scope.clone(), override_rows);
        let mut preference_rows = Vec::new();
        let mut preference_keys = HashSet::new();
        for preference in &contract.preferences {
            let model_id = preference.model_id.trim().to_ascii_lowercase();
            let protocol = UpstreamProtocolKind::try_from(preference.protocol.as_str())
                .map_err(|_| "node migration contains an invalid preferred protocol".to_string())?;
            if model_id.is_empty()
                || !crate::provider_contracts::selectable_model_protocol(scope_id, protocol)
                || !preference_keys.insert(model_id.clone())
            {
                return Err(
                    "node migration contains an invalid or duplicate model protocol preference"
                        .to_string(),
                );
            }
            preference_rows.push((model_id, protocol));
        }
        persisted.preferences.insert(scope, preference_rows);
    }
    apply_exclusive_available_override_repair(&mut persisted);
    Ok(persisted)
}

fn apply_exclusive_available_override_repair(persisted: &mut PersistedContracts) {
    let set = build_effective_contracts(
        &crate::kernel::zen::ZenFreeModelCatalog::default(),
        &[],
        persisted.clone(),
    );
    let repairs = exclusive_available_force_off_repairs(&set, persisted);
    for (scope, model_id, protocol) in repairs {
        if let Some(rows) = persisted.overrides.get_mut(&scope) {
            rows.retain(|row| {
                !(row.model_id.eq_ignore_ascii_case(&model_id) && row.protocol == protocol)
            });
        }
    }
}

fn preview_against_current(
    state: &CoreState,
    validated: &ValidatedMigration,
) -> Result<(Vec<AccountImportPreviewItem>, u64, u64, u64), V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let revision = state.settings_revision();
    validate_node_merge_against_current(state, &validated.node)?;
    validate_platform_merge_against_current(state, &validated.unified.platform_accounts)?;
    validate_identity_merge_against_current(state, validated)?;
    let existing_ids = current_account_ids(state)?;
    let mut importable = 0_u64;
    let items = validated
        .accounts
        .iter()
        .map(|account| {
            importable += 1;
            preview_item(
                account,
                if existing_ids.contains(account.id.as_deref().unwrap_or_default()) {
                    AccountImportDisposition::Merge
                } else {
                    AccountImportDisposition::Import
                },
                None,
            )
        })
        .collect();
    Ok((items, importable, 0, revision))
}

fn validate_node_merge_against_current(
    state: &CoreState,
    node: &PortableNodeState,
) -> Result<(), V3ApiError> {
    let source_ids = node
        .access_keys
        .iter()
        .map(|key| key.id.as_str())
        .collect::<HashSet<_>>();
    let target_only = state
        .db
        .lock()
        .list_active_sub_gateway_keys()
        .map_err(|_| V3ApiError::internal("failed to inspect destination Access Keys"))?
        .into_iter()
        .filter(|key| !source_ids.contains(key.id.as_str()))
        .collect::<Vec<_>>();
    if node.access_keys.len() + target_only.len() > MAX_ACCESS_KEYS {
        return Err(V3ApiError::conflict_at(
            state,
            "the merged node would exceed the 64 active sub Key limit",
        ));
    }
    let mut values = HashSet::new();
    values.insert(node.config.gateway_key.as_str());
    for key in &node.access_keys {
        values.insert(key.key.as_str());
    }
    if target_only
        .iter()
        .any(|key| !values.insert(key.key.as_str()))
    {
        return Err(V3ApiError::conflict_at(
            state,
            "the migration contains an Access Key value already owned by a different ID",
        ));
    }
    Ok(())
}

fn validate_platform_merge_against_current(
    state: &CoreState,
    parents: &[crate::platform::PortablePlatformAccount],
) -> Result<(), V3ApiError> {
    if parents.is_empty() {
        return Ok(());
    }
    let existing = state
        .db
        .lock()
        .list_platform_accounts()
        .map_err(|_| V3ApiError::internal("failed to inspect destination platform accounts"))?;
    for parent in parents {
        if existing.iter().any(|row| {
            row.id == parent.id && (row.kind != parent.kind || row.base_url != parent.base_url)
        }) {
            return Err(V3ApiError::conflict_at(
                state,
                "imported platform identity conflicts with immutable origin",
            ));
        }
    }
    Ok(())
}

fn validate_identity_merge_against_current(
    state: &CoreState,
    validated: &ValidatedMigration,
) -> Result<(), V3ApiError> {
    let Some(snapshot) = validated.unified.identity_snapshot.as_ref() else {
        return Ok(());
    };
    let imported = validated
        .accounts
        .iter()
        .filter_map(|account| account.id.clone())
        .collect::<HashSet<_>>();
    if let Some(conflict) = state
        .db
        .lock()
        .identity_import_conflict(snapshot, &imported)
        .map_err(|_| V3ApiError::internal("failed to inspect destination identity model"))?
    {
        return Err(V3ApiError::conflict_at(state, conflict));
    }
    Ok(())
}

fn current_account_ids(state: &CoreState) -> Result<HashSet<String>, V3ApiError> {
    Ok(state
        .db
        .lock()
        .list_accounts()
        .map_err(|_| V3ApiError::internal("failed to inspect existing account ids"))?
        .into_iter()
        .filter(|account| !account.is_zen_free() && account.id != crate::provider::CPA_ACCOUNT_ID)
        .map(|account| account.id)
        .collect())
}

fn preview_item(
    account: &ValidatedAccount,
    disposition: AccountImportDisposition,
    reason: Option<String>,
) -> AccountImportPreviewItem {
    AccountImportPreviewItem {
        index: account.portable_index as u64,
        name: account.name.clone(),
        provider_id: account.provider_id.clone(),

        account_type: account.account_type.into(),
        disposition,
        reason,
    }
}

fn trim_optional(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn validate_bundle_password(password: &str) -> Result<(), TransferError> {
    let length = password.chars().count();
    if !(MIN_BUNDLE_PASSWORD_CHARS..=MAX_PASSWORD_CHARS).contains(&length) {
        return Err(TransferError::Invalid(format!(
            "migration password must contain {MIN_BUNDLE_PASSWORD_CHARS} to {MAX_PASSWORD_CHARS} characters"
        )));
    }
    Ok(())
}

fn ensure_body_bound(state: &CoreState, body: &Bytes) -> Result<(), V3ApiError> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(V3ApiError::invalid_request_at(
            state,
            "account migration request is too large",
        ));
    }
    Ok(())
}

fn ensure_bundle_bound(bundle: &str) -> Result<(), TransferError> {
    if bundle.len() > MAX_BUNDLE_BYTES {
        return Err(TransferError::Invalid(
            "account backup is too large".to_string(),
        ));
    }
    Ok(())
}

fn ensure_transport(state: &CoreState, headers: &HeaderMap) -> Result<(), V3ApiError> {
    let local =
        dashboard_session::is_local_dashboard_request(state.dashboard_local_mode(), headers);
    if !local {
        return Err(map_transfer_error(state, TransferError::InsecureTransport));
    }
    Ok(())
}

fn crypto_permit() -> Result<OwnedSemaphorePermit, TransferError> {
    Arc::clone(CRYPTO_GATE.get_or_init(|| Arc::new(Semaphore::new(1))))
        .try_acquire_owned()
        .map_err(|_| TransferError::Busy)
}

fn map_transfer_error(state: &CoreState, error: TransferError) -> V3ApiError {
    match error {
        TransferError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        TransferError::InvalidBundle => V3ApiError::invalid_request_at(
            state,
            "migration password is incorrect or the backup file is damaged",
        ),
        TransferError::UnsupportedVersion(version) => V3ApiError::invalid_request_at(
            state,
            format!(
                "this backup uses payload version {version}; this node expects payload version {PAYLOAD_VERSION}"
            ),
        ),
        TransferError::Busy => V3ApiError::service_unavailable(
            state,
            "another account migration cryptographic operation is in progress",
        ),
        TransferError::InsecureTransport => {
            V3ApiError::forbidden_at(state, "account migration is limited to the local dashboard")
        }
        TransferError::Internal => V3ApiError::internal("account migration failed"),
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(super) async fn add_no_store(response: Response) -> Response {
    no_store(response)
}

#[cfg(test)]
mod tests;
