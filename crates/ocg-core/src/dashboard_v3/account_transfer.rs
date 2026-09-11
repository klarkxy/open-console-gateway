//! Password-encrypted node migration for Dashboard V3.
//!
//! Plaintext upstream Keys are decrypted and re-encrypted only inside the Host.
//! The dashboard receives a versioned Argon2id + AES-256-GCM envelope, plus
//! secret-free previews/results. Browser profiles, cookies, logs, usage, and
//! cooldowns remain host-local; portable configuration and credentials move.

use std::collections::HashSet;
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
use crate::db::{AccountImportRecord, NodeImportRecord};
use crate::models::{
    Account as ModelAccount, AccountCustomConfigInput, AccountModelCapabilityInput,
    AccountSetupStep as ModelSetupStep, AccountType as ModelAccountType, AppConfig, SubGatewayKey,
    normalize_account_notes, normalize_purchase_date,
};
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

const ENVELOPE_FORMAT: &str = "ocg-manager-account-backup";
const ENVELOPE_VERSION: u32 = 1;
#[cfg(test)]
const LEGACY_PAYLOAD_VERSION: u32 = 1;
#[cfg(test)]
const NODE_PAYLOAD_VERSION: u32 = 2;
const PAYLOAD_VERSION: u32 = 5;
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
    #[serde(default)]
    platform_accounts: Vec<crate::platform::PortablePlatformAccount>,
    #[serde(default)]
    platform_links: Vec<crate::platform::PortablePlatformLink>,
    version: u32,
    exported_at: String,
    accounts: Vec<PortableAccount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dynamic_providers: Vec<PortableProviderDefinition>,
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
}

#[derive(Debug)]
struct ValidatedMigration {
    platform_links_authoritative: bool,
    platform_accounts: Vec<crate::platform::PortablePlatformAccount>,
    platform_links: Vec<crate::platform::PortablePlatformLink>,
    exported_at: String,
    accounts: Vec<ValidatedAccount>,
    node: Option<Zeroizing<PortableNodeState>>,
    dynamic_providers: Vec<DynamicProviderRuntime>,
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
        let exported_accounts = payload.accounts.len() as u64;
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
    let is_node_migration = validated.node.is_some();
    if let Some(node) = validated.node.as_deref() {
        validate_node_merge_against_current(&state, node)?;
    }
    validate_platform_merge_against_current(&state, &validated.platform_accounts)?;
    let existing = current_logical_accounts(&state)?;
    let existing_ids = current_account_ids(&state)?;
    let mut records = Vec::new();
    let mut items = Vec::with_capacity(validated.accounts.len());
    let mut duplicate_accounts = 0_u64;
    for account in validated.accounts {
        let logical = logical_key(&account.provider_id, &account.name);
        if !is_node_migration && existing.contains(&logical) {
            duplicate_accounts += 1;
            items.push(preview_item(
                &account,
                AccountImportDisposition::Duplicate,
                Some("an account with the same Plan and name already exists".to_string()),
            ));
            continue;
        }
        let now = Utc::now();
        let id = account
            .id
            .clone()
            .filter(|_| is_node_migration)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let key_cipher = if account.key.is_empty() {
            String::new()
        } else {
            state
                .encrypt_key(account.key.as_str())
                .map_err(|_| V3ApiError::internal("failed to protect an imported credential"))?
        };
        let model = ModelAccount {
            id,
            provider_id: account.provider_id.clone(),

            credential_kind: account.credential_kind,
            quota_scope: account.quota_scope,
            name: account.name.clone(),
            username: account.username.clone(),
            password_cipher: None,
            key_cipher,
            enabled: account.enabled,
            account_type: account.account_type,
            setup_step: account.setup_step,
            referral_code: None,
            purchase_date: account.purchase_date.clone(),
            expires_on: account.expires_on.clone(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
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
            if is_node_migration && existing_ids.contains(account.id.as_deref().unwrap_or_default())
            {
                AccountImportDisposition::Merged
            } else {
                AccountImportDisposition::Imported
            },
            None,
        ));
    }

    let imported_accounts = records.len() as u64;
    let revision = if records.is_empty() && !is_node_migration {
        state.settings_revision()
    } else if let Some(mut node) = validated.node {
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
                        .map_err(|_| {
                            V3ApiError::invalid_request_at(&state, "invalid Access Key time")
                        })?
                        .with_timezone(&Utc),
                })
            })
            .collect::<Result<Vec<_>, V3ApiError>>()?;
        let node_record = NodeImportRecord {
            platform_links_authoritative: validated.platform_links_authoritative,
            platform_accounts: validated.platform_accounts,
            platform_links: validated.platform_links,
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
                    .map_err(|_| {
                        V3ApiError::invalid_request_at(&state, "invalid Zen catalog time")
                    })?
                    .map(|value| value.with_timezone(&Utc)),
                source_url: node.zen_free.source_url.clone(),
            },
            provider_contracts: persisted_contracts_from_portable(&node.provider_contracts, now)
                .map_err(|error| V3ApiError::invalid_request_at(&state, error))?,
            dynamic_providers: validated.dynamic_providers,
        };
        let runtime = state
            .db
            .lock()
            .import_node_state(&node_record, |db| state.prepare_imported_node_runtime(db))
            .map_err(|error| V3ApiError::conflict_at(&state, error.to_string()))?;
        state.install_imported_node_runtime(runtime);
        state.settings_revision()
    } else {
        state
            .db
            .lock()
            .import_accounts_with_contracts(&records)
            .map_err(|_| V3ApiError::internal("failed to import account migration package"))?;
        let revision = state.bump_settings_revision();
        state.reload_provider_contracts().map_err(|_| {
            V3ApiError::internal("accounts imported but runtime contracts could not reload")
        })?;
        revision
    };

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
    let (
        snapshots,
        sub_keys,
        persisted_contracts,
        dynamic_runtimes,
        platform_accounts,
        platform_links,
    ) = {
        let db = state.db.lock();
        let accounts = db.list_accounts().map_err(|_| TransferError::Internal)?;
        let snapshots = accounts
            .into_iter()
            .map(|account| {
                let contract = db
                    .load_account_contract(&account.id)
                    .map_err(|_| TransferError::Internal)?;
                let ollama_billing = if account.provider_id == crate::provider::OLLAMA_PROVIDER_ID {
                    db.ollama_cloud_billing_tier(&account.id)
                        .map_err(|_| TransferError::Internal)?
                } else {
                    None
                };
                Ok((account, contract, ollama_billing))
            })
            .collect::<Result<Vec<_>, TransferError>>()?;
        let sub_keys = db
            .list_active_sub_gateway_keys()
            .map_err(|_| TransferError::Internal)?;
        let persisted_contracts = db
            .load_persisted_contracts()
            .map_err(|_| TransferError::Internal)?;
        let dynamic_runtimes = db
            .list_dynamic_providers()
            .map_err(|_| TransferError::Internal)?;
        let parents = db
            .list_platform_accounts()
            .map_err(|_| TransferError::Internal)?
            .into_iter()
            .map(|p| crate::platform::PortablePlatformAccount {
                id: p.id,
                kind: p.kind,
                name: p.name,
                base_url: p.base_url,
            })
            .collect::<Vec<_>>();
        let links = db
            .list_platform_links()
            .map_err(|_| TransferError::Internal)?
            .into_iter()
            .map(|l| {
                let mut group = l.group;
                group.verified = false;
                group.subscription_type = None;
                crate::platform::PortablePlatformLink {
                    account_id: l.account_id,
                    platform_account_id: l.platform_account_id,
                    group,
                }
            })
            .collect::<Vec<_>>();
        (
            snapshots,
            sub_keys,
            persisted_contracts,
            dynamic_runtimes,
            parents,
            links,
        )
    };
    let mut accounts = Zeroizing::new(Vec::new());
    let mut account_order = Vec::new();
    let mut skipped = 0_u64;
    let mut zen_enabled = false;
    for (account, contract, ollama_billing) in snapshots {
        if account.id == crate::provider::CPA_ACCOUNT_ID {
            continue;
        }
        if account.is_zen_free() {
            zen_enabled = account.enabled;
            account_order.push(account.id);
            continue;
        }
        if account.account_type == ModelAccountType::Managed
            && account.setup_step != ModelSetupStep::Ready
        {
            skipped += 1;
            continue;
        }
        account_order.push(account.id.clone());
        let dynamic = dynamic_runtimes
            .iter()
            .find(|runtime| crate::dynamic::provider_ids_equal(&runtime.id, &account.provider_id));
        let plan = builtin_provider(&account.provider_id);
        if plan.is_none() && dynamic.is_none() {
            return Err(TransferError::Internal);
        }
        let portable_key_required =
            if dynamic.is_some_and(|runtime| !runtime.auth_kind.requires_key()) {
                false
            } else {
                migration_exports_key(account.account_type, account.setup_step)
            };
        let mut key = Zeroizing::new(if !portable_key_required || account.key_cipher.is_empty() {
            String::new()
        } else {
            state
                .decrypt_key(&account.key_cipher)
                .map_err(|_| TransferError::Internal)?
        });
        if portable_key_required && key.trim().is_empty() {
            return Err(TransferError::Internal);
        }
        let (custom_config, model_capabilities) =
            if plan.is_some_and(crate::provider::plan_requires_custom_config) {
                (
                    contract.custom_config.map(|config| PortableCustomConfig {
                        endpoint_url: config.endpoint_url,
                        upstream_protocol: config.upstream_protocol.as_str().to_string(),
                    }),
                    contract
                        .model_capabilities
                        .into_iter()
                        .map(|capability| {
                            PortableModelCapability::Canonical(PortableModelCapabilityCanonical {
                                public_model: capability.public_model,
                                upstream_model: capability.upstream_model,
                                protocol: capability.protocol.as_str().to_string(),
                            })
                        })
                        .collect(),
                )
            } else {
                // Non-Custom account capability rows are runtime/provider
                // evidence, not portable Custom declarations. Provider-level
                // catalogs and evidence are carried separately in node V2.
                (None, Vec::new())
            };
        accounts.push(PortableAccount {
            id: Some(account.id),
            provider_id: account.provider_id,

            name: account.name,
            username: account.username,
            key: std::mem::take(&mut *key),
            enabled: account.enabled,
            account_type: account.account_type.as_str().to_string(),
            setup_step: account.setup_step.as_str().to_string(),
            purchase_date: account.purchase_date,
            expires_on: account.expires_on,
            notes: account.notes,
            verification_status: Some(contract.verification.status.as_str().to_string()),
            connection_verified_at: contract
                .verification
                .connection_verified_at
                .map(|value| value.to_rfc3339()),
            custom_config,
            model_capabilities,
            ollama_billing_tier: ollama_billing.map(|tier| tier.as_str().to_string()),
        });
    }
    if accounts.len() > MAX_ACCOUNTS {
        return Err(TransferError::Invalid(format!(
            "at most {MAX_ACCOUNTS} accounts can be exported at once"
        )));
    }
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
    let mut portable_dynamics = dynamic_runtimes
        .into_iter()
        .map(|runtime| PortableProviderDefinition {
            preset_id: runtime.preset_id,
            id: runtime.id,
            name: runtime.name,
            endpoint_url: runtime.endpoint_url,
            upstream_protocol: runtime.upstream_protocol.as_str().to_string(),
            auth_kind: runtime.auth_kind.as_str().to_string(),
            models: runtime
                .mappings
                .into_iter()
                .map(|mapping| PortableProviderDefinitionModel {
                    public_model: mapping.public_model,
                    upstream_model: mapping.upstream_model,
                    upstream_override: mapping.upstream_override.map(|value| {
                        PortableProviderDefinitionModelOverride {
                            protocol: value.protocol.as_str().to_string(),
                            endpoint_url: value.endpoint_url,
                        }
                    }),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    portable_dynamics.sort_by(|left, right| left.id.cmp(&right.id));
    let zen_catalog = state.zen_free_model_catalog();
    Ok((
        PortablePayload {
            platform_accounts,
            platform_links: platform_links
                .into_iter()
                .filter(|l| {
                    accounts
                        .iter()
                        .any(|a| a.id.as_deref() == Some(l.account_id.as_str()))
                })
                .collect(),
            version: PAYLOAD_VERSION,
            exported_at: Utc::now().to_rfc3339(),
            accounts: std::mem::take(&mut *accounts),
            dynamic_providers: portable_dynamics,
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
        Some(version) if version == 4 || version == u64::from(PAYLOAD_VERSION) => {}
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
    if payload.version != 4 && payload.version != PAYLOAD_VERSION {
        return Err(TransferError::UnsupportedVersion(payload.version));
    }
    if payload.accounts.len() > MAX_ACCOUNTS || payload.node.is_none() {
        return Err(TransferError::InvalidBundle);
    }
    let is_node_migration = true;
    if payload.exported_at.chars().count() > 64
        || DateTime::parse_from_rfc3339(&payload.exported_at).is_err()
    {
        return Err(TransferError::InvalidBundle);
    }
    let exported_at = payload.exported_at.clone();
    let validated_dynamics = validate_portable_dynamic_providers(&payload.dynamic_providers)?;
    let mut logical = HashSet::new();
    let mut account_ids = HashSet::new();
    let mut validated = Vec::with_capacity(payload.accounts.len());
    for (index, account) in payload.accounts.iter_mut().enumerate() {
        let prefix = || format!("account {}", index + 1);
        let id = match account.id.as_deref().map(str::trim) {
            Some(id) => {
                if !is_node_migration {
                    None
                } else {
                    uuid::Uuid::parse_str(id).map_err(|_| {
                        TransferError::Invalid(format!("{} has an invalid account id", prefix()))
                    })?;
                    Some(id.to_string())
                }
            }
            None if is_node_migration => {
                return Err(TransferError::Invalid(format!(
                    "{} is missing its account id",
                    prefix()
                )));
            }
            None => None,
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
        if is_node_migration
            && enabled
            && verification_gates_enablement
            && !verification_status.allows_enablement()
        {
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
        let logical_key = logical_key(&account.provider_id, &account.name);
        let duplicate = if is_node_migration {
            !account_ids.insert(id.clone().expect("V2 account id was validated"))
        } else {
            !logical.insert(logical_key)
        };
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
    let node = payload
        .node
        .take()
        .map(|node| validate_node_state(node, &validated))
        .transpose()?
        .map(Zeroizing::new);
    Ok(ValidatedMigration {
        platform_links_authoritative: payload.version == PAYLOAD_VERSION,
        platform_accounts: payload.platform_accounts.clone(),
        platform_links: payload.platform_links.clone(),
        exported_at,
        accounts: validated,
        node,
        dynamic_providers: validated_dynamics,
    })
}

fn validate_portable_dynamic_providers(
    providers: &[PortableProviderDefinition],
) -> Result<Vec<DynamicProviderRuntime>, TransferError> {
    let now = Utc::now();
    let mut seen_ids = HashSet::new();
    let mut validated = Vec::with_capacity(providers.len());
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
        let definition = crate::dynamic::validate_definition(DynamicProviderDefinition {
            preset_id: provider.preset_id.clone(),
            id: id.to_string(),
            name: provider.name.clone(),
            endpoint_url,
            upstream_protocol: protocol,
            auth_kind,
            mappings,
        })
        .map_err(|error| TransferError::Invalid(format!("{} is invalid: {error}", prefix())))?;
        let preset_for_origin = definition.preset_id.as_deref();
        let origin = ocg_domain::provider::provider_origin_from_preset(preset_for_origin);
        let offering =
            ocg_domain::provider::preset_offering(preset_for_origin.unwrap_or("")).to_string();
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
    Ok(validated)
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
    if let Some(node) = validated.node.as_deref() {
        validate_node_merge_against_current(state, node)?;
    }
    validate_platform_merge_against_current(state, &validated.platform_accounts)?;
    let existing = current_logical_accounts(state)?;
    let existing_ids = current_account_ids(state)?;
    let mut importable = 0_u64;
    let mut duplicates = 0_u64;
    let items = validated
        .accounts
        .iter()
        .map(|account| {
            if validated.node.is_some() {
                importable += 1;
                return preview_item(
                    account,
                    if existing_ids.contains(account.id.as_deref().unwrap_or_default()) {
                        AccountImportDisposition::Merge
                    } else {
                        AccountImportDisposition::Import
                    },
                    None,
                );
            }
            let duplicate = existing.contains(&logical_key(&account.provider_id, &account.name));
            if duplicate {
                duplicates += 1;
                preview_item(
                    account,
                    AccountImportDisposition::Duplicate,
                    Some("an account with the same Plan and name already exists".to_string()),
                )
            } else {
                importable += 1;
                preview_item(account, AccountImportDisposition::Import, None)
            }
        })
        .collect();
    Ok((items, importable, duplicates, revision))
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

fn current_logical_accounts(state: &CoreState) -> Result<HashSet<(String, String)>, V3ApiError> {
    Ok(state
        .db
        .lock()
        .list_accounts()
        .map_err(|_| V3ApiError::internal("failed to inspect existing accounts"))?
        .into_iter()
        .filter(|account| !account.is_zen_free() && account.id != crate::provider::CPA_ACCOUNT_ID)
        .map(|account| logical_key(&account.provider_id, &account.name))
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

fn logical_key(provider_id: &str, name: &str) -> (String, String) {
    (provider_id.trim().to_string(), name.trim().to_string())
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
mod tests {
    use super::*;

    fn sample_account(name: impl Into<String>) -> PortableAccount {
        PortableAccount {
            id: None,
            provider_id: "opencode".to_string(),

            name: name.into(),
            username: Some("user@example.com".to_string()),
            key: "sk-ocg-test-secret".to_string(),
            enabled: true,
            account_type: "key".to_string(),
            setup_step: "ready".to_string(),
            purchase_date: "2026-08-01".to_string(),
            expires_on: String::new(),
            notes: Some("portable".to_string()),
            verification_status: None,
            connection_verified_at: None,
            custom_config: None,
            model_capabilities: Vec::new(),
            ollama_billing_tier: None,
        }
    }

    fn sample_payload() -> PortablePayload {
        let account_id = "00000000-0000-4000-8000-000000000041";
        let mut account = sample_account("Primary");
        account.id = Some(account_id.to_string());
        PortablePayload {
            platform_accounts: Vec::new(),
            platform_links: Vec::new(),
            version: PAYLOAD_VERSION,
            exported_at: "2026-08-29T00:00:00Z".to_string(),
            accounts: vec![account],
            dynamic_providers: Vec::new(),
            node: Some(sample_node(account_id)),
        }
    }

    fn sample_custom_account() -> PortableAccount {
        PortableAccount {
            id: None,
            provider_id: crate::kernel::ids::CUSTOM_PROVIDER_ID.to_string(),

            name: "Mapped Custom".to_string(),
            username: None,
            key: "sk-custom-test-secret".to_string(),
            enabled: true,
            account_type: "key".to_string(),
            setup_step: "ready".to_string(),
            purchase_date: String::new(),
            expires_on: String::new(),
            notes: None,
            verification_status: Some("pending".to_string()),
            connection_verified_at: None,
            custom_config: Some(PortableCustomConfig {
                endpoint_url: "https://api.example.com/v1/chat/completions".to_string(),
                upstream_protocol: "chat_completions".to_string(),
            }),
            model_capabilities: Vec::new(),
            ollama_billing_tier: None,
        }
    }

    fn sample_node(account_id: &str) -> PortableNodeState {
        let config = AppConfig {
            gateway_key: "ocg-transfer-primary-key".to_string(),
            ..AppConfig::default()
        };
        PortableNodeState {
            config,
            access_keys: Vec::new(),
            zen_free: PortableZenFree {
                enabled: false,
                models: Vec::new(),
                refreshed_at: None,
                source_url: String::new(),
            },
            account_order: vec![
                crate::kernel::ids::ZEN_FREE_ACCOUNT_ID.to_string(),
                account_id.to_string(),
            ],
            provider_contracts: Vec::new(),
        }
    }

    #[test]
    fn encrypted_bundle_round_trips_without_plaintext_secret() {
        let payload = sample_payload();
        let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
        let second = encrypt_payload(&payload, "correct horse battery").unwrap();
        assert_ne!(
            bundle, second,
            "OS randomness must produce a fresh envelope"
        );
        assert!(!bundle.contains("sk-ocg-test-secret"));
        let migration = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
        assert_eq!(migration.accounts.len(), 1);
        assert_eq!(migration.accounts[0].key.as_str(), "sk-ocg-test-secret");
    }

    #[test]
    fn payload_v1_v2_and_v3_are_rejected_without_a_legacy_offering_parser() {
        for version in [LEGACY_PAYLOAD_VERSION, NODE_PAYLOAD_VERSION, 3] {
            let mut account = sample_custom_account();
            let node = if version >= NODE_PAYLOAD_VERSION {
                let account_id = "00000000-0000-4000-8000-000000000042";
                account.id = Some(account_id.to_string());
                Some(sample_node(account_id))
            } else {
                None
            };
            let error = validate_payload(PortablePayload {
                platform_accounts: Vec::new(),
                platform_links: Vec::new(),
                version,
                exported_at: "2026-08-29T00:00:00Z".to_string(),
                accounts: vec![account],
                dynamic_providers: Vec::new(),
                node,
            })
            .unwrap_err();
            assert!(
                matches!(error, TransferError::UnsupportedVersion(found) if found == version),
                "{error:?}"
            );
        }
    }

    #[test]
    fn unsupported_payload_version_is_not_a_password_or_damage_error() {
        let mut v4 = sample_payload();
        v4.version = 4;
        let bundle = encrypt_payload(&v4, "correct horse battery").unwrap();
        let imported = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
        assert!(imported.platform_accounts.is_empty());
        assert!(imported.platform_links.is_empty());
        let mut payload = sample_payload();
        payload.version = 3;
        let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
        let error = decrypt_and_validate(&bundle, "correct horse battery").unwrap_err();
        assert!(
            matches!(error, TransferError::UnsupportedVersion(3)),
            "{error:?}"
        );
        let message = format!(
            "this backup uses payload version 3; this node expects payload version {PAYLOAD_VERSION}"
        );
        assert!(!message.to_ascii_lowercase().contains("password"));
        assert!(!message.to_ascii_lowercase().contains("damaged"));
        assert!(message.contains(&PAYLOAD_VERSION.to_string()));
    }

    #[test]
    fn future_payload_version_is_rejected_as_unsupported_not_wrong_password() {
        use crate::crypto::{KeyCipher, StaticKeyCipher};
        use crate::db::Database;
        use crate::state::CoreStateInner;
        use axum::http::StatusCode;
        use std::fs;
        use std::sync::Arc;

        assert_eq!(PAYLOAD_VERSION, 5);
        let mut payload = sample_payload();
        payload.version = 6;
        let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
        let error = decrypt_and_validate(&bundle, "correct horse battery").unwrap_err();
        assert!(
            matches!(error, TransferError::UnsupportedVersion(6)),
            "{error:?}"
        );
        assert!(!matches!(error, TransferError::InvalidBundle));

        let dir = std::env::temp_dir().join(format!(
            "ocg-transfer-future-payload-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("v3-transfer-future-payload"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        let mapped = map_transfer_error(&state, error);
        assert_eq!(mapped.status, StatusCode::BAD_REQUEST);
        assert_eq!(mapped.body.code, super::super::ERROR_INVALID_REQUEST);
        assert!(
            mapped.body.message.contains("payload version 6"),
            "{}",
            mapped.body.message
        );
        assert!(
            mapped
                .body
                .message
                .contains(&format!("payload version {PAYLOAD_VERSION}")),
            "{}",
            mapped.body.message
        );
        let lower = mapped.body.message.to_ascii_lowercase();
        assert!(!lower.contains("password"), "{}", mapped.body.message);
        assert!(!lower.contains("damaged"), "{}", mapped.body.message);
        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn preview_rejects_same_platform_id_with_a_different_site() {
        use crate::crypto::{KeyCipher, StaticKeyCipher};
        use crate::db::Database;
        use crate::platform::{PlatformKind, PortablePlatformAccount};
        use crate::state::CoreStateInner;
        use axum::http::StatusCode;
        use std::fs;
        use std::sync::Arc;

        let parent_id = "00000000-0000-4000-8000-0000000000aa";
        let dir = std::env::temp_dir().join(format!(
            "ocg-transfer-platform-site-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("v3-transfer-platform-site"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        state
            .db
            .lock()
            .create_platform_account(
                parent_id,
                PlatformKind::NewApi,
                "Destination site",
                "https://dest.example",
                None,
            )
            .unwrap();

        let mut payload = sample_payload();
        payload.platform_accounts = vec![PortablePlatformAccount {
            id: parent_id.to_string(),
            kind: PlatformKind::NewApi,
            name: "Source site".into(),
            base_url: "https://other.example".into(),
        }];
        let validated = validate_payload(payload).unwrap();
        let error = preview_against_current(&state, &validated).unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(
            error
                .body
                .message
                .contains("imported platform identity conflicts with immutable origin"),
            "{}",
            error.body.message
        );
        assert_eq!(
            state
                .db
                .lock()
                .platform_account(parent_id)
                .unwrap()
                .unwrap()
                .base_url,
            "https://dest.example"
        );
        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    fn sample_dynamic_provider(id: &str, name: &str) -> PortableProviderDefinition {
        PortableProviderDefinition {
            preset_id: None,
            id: id.to_string(),
            name: name.to_string(),
            endpoint_url: "http://127.0.0.1:9/v1".to_string(),
            upstream_protocol: "chat_completions".to_string(),
            auth_kind: "bearer".to_string(),
            models: vec![PortableProviderDefinitionModel {
                public_model: "lab-opus".to_string(),
                upstream_model: "vendor/opus".to_string(),
                upstream_override: None,
            }],
        }
    }

    #[test]
    fn portable_preset_provenance_is_optional_and_survives_validation() {
        let mut provider =
            sample_dynamic_provider("dc7f6bbf-18a1-458b-845b-54c219c19dba", "Renamed");
        provider.preset_id = Some("azure-openai".into());
        let encoded = serde_json::to_value(&provider).unwrap();
        assert_eq!(encoded["presetId"], "azure-openai");
        let decoded: PortableProviderDefinition = serde_json::from_value(encoded.clone()).unwrap();
        let validated = validate_portable_dynamic_providers(&[decoded]).unwrap();
        assert_eq!(validated[0].preset_id.as_deref(), Some("azure-openai"));
        let mut legacy = encoded;
        legacy.as_object_mut().unwrap().remove("presetId");
        let decoded: PortableProviderDefinition = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            validate_portable_dynamic_providers(&[decoded]).unwrap()[0].preset_id,
            None
        );
    }

    #[test]
    fn portable_model_override_roundtrips_and_missing_override_inherits() {
        let mut provider = sample_dynamic_provider("dc7f6bbf-18a1-458b-845b-54c219c19dba", "Lab");
        let legacy = serde_json::to_value(&provider).unwrap();
        assert!(legacy["models"][0].get("upstreamOverride").is_none());
        provider.models[0].upstream_override = Some(PortableProviderDefinitionModelOverride {
            protocol: "messages".into(),
            endpoint_url: "https://example.test/anthropic/v1/messages".into(),
        });
        let encoded = serde_json::to_value(&provider).unwrap();
        assert_eq!(
            encoded["models"][0]["upstreamOverride"]["protocol"],
            "messages"
        );
        let decoded = serde_json::from_value(encoded).unwrap();
        let validated = validate_portable_dynamic_providers(&[decoded]).unwrap();
        let route = validated[0].mappings[0].upstream_override.as_ref().unwrap();
        assert_eq!(route.protocol, UpstreamProtocolKind::Messages);
        assert_eq!(
            route.endpoint_url,
            "https://example.test/anthropic/v1/messages"
        );
        let old = validate_portable_dynamic_providers(&[serde_json::from_value(legacy).unwrap()])
            .unwrap();
        assert!(old[0].mappings[0].upstream_override.is_none());
        provider.models[0]
            .upstream_override
            .as_mut()
            .unwrap()
            .protocol = "unknown".into();
        assert!(validate_portable_dynamic_providers(&[provider]).is_err());
    }

    #[test]
    fn dynamic_provider_definitions_are_validated_and_dangling_ids_fail() {
        let provider_id = "00000000-0000-4000-8000-0000000000aa";
        let account_id = "00000000-0000-4000-8000-0000000000ab";
        let mut account = sample_account("Lab");
        account.id = Some(account_id.to_string());
        account.provider_id = provider_id.to_string();
        let payload = PortablePayload {
            platform_accounts: Vec::new(),
            platform_links: Vec::new(),
            version: PAYLOAD_VERSION,
            exported_at: "2026-08-29T00:00:00Z".to_string(),
            accounts: vec![account],
            dynamic_providers: vec![sample_dynamic_provider(provider_id, "Lab")],
            node: Some(sample_node(account_id)),
        };
        let validated = validate_payload(payload).unwrap();
        assert_eq!(validated.dynamic_providers.len(), 1);
        assert_eq!(validated.dynamic_providers[0].name, "Lab");
        assert_eq!(
            validated.accounts[0].credential_kind,
            crate::provider::CredentialKind::ApiKey
        );

        let mut dangling_account = sample_account("Lab");
        dangling_account.id = Some(account_id.to_string());
        dangling_account.provider_id = provider_id.to_string();
        let dangling = PortablePayload {
            platform_accounts: Vec::new(),
            platform_links: Vec::new(),
            version: PAYLOAD_VERSION,
            exported_at: "2026-08-29T00:00:00Z".to_string(),
            accounts: vec![dangling_account],
            dynamic_providers: Vec::new(),
            node: Some(sample_node(account_id)),
        };
        let error = validate_payload(dangling).unwrap_err();
        assert!(
            matches!(error, TransferError::Invalid(ref message) if message.contains("unknown provider")),
            "{error:?}"
        );
    }

    #[test]
    fn v3_exports_canonical_model_mapping_inside_the_v1_envelope() {
        let account_id = "00000000-0000-4000-8000-000000000043";
        let mut account = sample_custom_account();
        account.id = Some(account_id.to_string());
        account.model_capabilities = vec![PortableModelCapability::Canonical(
            PortableModelCapabilityCanonical {
                public_model: "deepseek-v4-flash".to_string(),
                upstream_model: "deepseek-v4-flash:0731".to_string(),
                protocol: "chat_completions".to_string(),
            },
        )];
        let payload = PortablePayload {
            platform_accounts: Vec::new(),
            platform_links: Vec::new(),
            version: PAYLOAD_VERSION,
            exported_at: "2026-08-29T00:00:00Z".to_string(),
            accounts: vec![account],
            dynamic_providers: Vec::new(),
            node: Some(sample_node(account_id)),
        };
        let json = serde_json::to_value(&payload).unwrap();
        let capability = &json["accounts"][0]["modelCapabilities"][0];
        assert_eq!(capability["publicModel"], "deepseek-v4-flash");
        assert_eq!(capability["upstreamModel"], "deepseek-v4-flash:0731");
        assert!(capability.get("modelId").is_none());

        let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
        let envelope: EncryptedEnvelope = serde_json::from_str(&bundle).unwrap();
        assert_eq!(envelope.version, ENVELOPE_VERSION);
        let validated = decrypt_and_validate(&bundle, "correct horse battery").unwrap();
        let capability = &validated.accounts[0].capabilities[0];
        assert_eq!(capability.public_model, "deepseek-v4-flash");
        assert_eq!(capability.upstream_model, "deepseek-v4-flash:0731");
    }

    #[test]
    fn wrong_password_and_tampering_share_invalid_bundle_result() {
        let bundle = encrypt_payload(&sample_payload(), "correct horse battery").unwrap();
        assert!(matches!(
            decrypt_and_validate(&bundle, "wrong password value"),
            Err(TransferError::InvalidBundle)
        ));
        let mut envelope: EncryptedEnvelope = serde_json::from_str(&bundle).unwrap();
        let mut ciphertext = STANDARD.decode(&envelope.ciphertext).unwrap();
        ciphertext[0] ^= 1;
        envelope.ciphertext = STANDARD.encode(ciphertext);
        assert!(matches!(
            decrypt_and_validate(
                &serde_json::to_string(&envelope).unwrap(),
                "correct horse battery"
            ),
            Err(TransferError::InvalidBundle)
        ));

        let mut wrong_version: EncryptedEnvelope = serde_json::from_str(&bundle).unwrap();
        wrong_version.version += 1;
        assert!(matches!(
            decrypt_and_validate(
                &serde_json::to_string(&wrong_version).unwrap(),
                "correct horse battery"
            ),
            Err(TransferError::InvalidBundle)
        ));

        let mut wrong_nonce: EncryptedEnvelope = serde_json::from_str(&bundle).unwrap();
        wrong_nonce.nonce = STANDARD.encode([0_u8; NONCE_LEN - 1]);
        assert!(matches!(
            decrypt_and_validate(
                &serde_json::to_string(&wrong_nonce).unwrap(),
                "correct horse battery"
            ),
            Err(TransferError::InvalidBundle)
        ));
    }

    #[test]
    fn duplicate_rows_inside_bundle_fail_closed() {
        let mut payload = sample_payload();
        payload.accounts.push(PortableAccount {
            id: None,
            provider_id: "opencode".to_string(),

            name: "Primary".to_string(),
            username: None,
            key: "sk-ocg-another-secret".to_string(),
            enabled: false,
            account_type: "key".to_string(),
            setup_step: "ready".to_string(),
            purchase_date: String::new(),
            expires_on: String::new(),
            notes: None,
            verification_status: None,
            connection_verified_at: None,
            custom_config: None,
            model_capabilities: Vec::new(),
            ollama_billing_tier: None,
        });
        let bundle = encrypt_payload(&payload, "correct horse battery").unwrap();
        assert!(matches!(
            decrypt_and_validate(&bundle, "correct horse battery"),
            Err(TransferError::Invalid(_))
        ));
    }

    #[test]
    fn managed_lifecycle_is_normalized_without_browser_identity() {
        assert!(!migration_exports_key(
            ModelAccountType::Managed,
            ModelSetupStep::Payment
        ));
        assert!(migration_exports_key(
            ModelAccountType::Managed,
            ModelSetupStep::Ready
        ));
        let mut draft = sample_payload();
        draft.accounts[0].account_type = "managed".to_string();
        draft.accounts[0].setup_step = "payment".to_string();
        draft.accounts[0].enabled = true;
        let draft = validate_payload(draft).unwrap();
        assert_eq!(draft.accounts[0].setup_step, ModelSetupStep::GoogleAccount);
        assert!(!draft.accounts[0].enabled);
        assert!(draft.accounts[0].key.is_empty());

        let mut ready = sample_payload();
        ready.accounts[0].account_type = "managed".to_string();
        let ready = validate_payload(ready).unwrap();
        assert_eq!(ready.accounts[0].setup_step, ModelSetupStep::Ready);
        assert!(ready.accounts[0].enabled);
        assert_eq!(ready.accounts[0].key.as_str(), "sk-ocg-test-secret");
    }

    #[test]
    fn account_count_and_decoded_ciphertext_limits_fail_closed() {
        let mut payload = sample_payload();
        let node = payload
            .node
            .as_mut()
            .expect("v4 payload includes node state");
        for index in 1..MAX_ACCOUNTS {
            let id = format!("00000000-0000-4000-8000-{:012x}", 0x41 + index);
            let mut extra = sample_account(format!("Account {index}"));
            extra.id = Some(id.clone());
            payload.accounts.push(extra);
            node.account_order.push(id);
        }
        assert_eq!(
            validate_payload(payload).unwrap().accounts.len(),
            MAX_ACCOUNTS
        );

        let mut oversized = sample_payload();
        for index in 1..=MAX_ACCOUNTS {
            oversized
                .accounts
                .push(sample_account(format!("Account {index}")));
        }
        assert!(matches!(
            validate_payload(oversized),
            Err(TransferError::InvalidBundle)
        ));

        let envelope = EncryptedEnvelope {
            format: ENVELOPE_FORMAT.to_string(),
            version: ENVELOPE_VERSION,
            salt: STANDARD.encode([0_u8; SALT_LEN]),
            nonce: STANDARD.encode([0_u8; NONCE_LEN]),
            ciphertext: STANDARD.encode(vec![0_u8; MAX_PLAINTEXT_BYTES + 33]),
        };
        assert!(matches!(
            decrypt_and_validate(
                &serde_json::to_string(&envelope).unwrap(),
                "correct horse battery"
            ),
            Err(TransferError::InvalidBundle)
        ));
    }

    #[test]
    fn v1_encryption_vector_is_stable() {
        use sha2::{Digest, Sha256};

        let bundle = encrypt_payload_with_material(
            &sample_payload(),
            "correct horse battery",
            [7_u8; SALT_LEN],
            [9_u8; NONCE_LEN],
        )
        .unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(bundle.as_bytes())),
            // Envelope V1 with the portable V5 sample payload.
            "0a0111257ed3fb7a7071a65b06c79a9bc4ef1003af97ad728bdee678b18d6cc4"
        );
    }
}
