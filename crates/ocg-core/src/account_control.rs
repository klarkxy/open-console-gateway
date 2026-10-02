//! HTTP-neutral account control-plane mutations.
//!
//! Dashboard V3 adapters wrap these functions with CAS. The CLI calls the
//! same functions without a Dashboard CAS token; both paths bump
//! `settings_revision` after a successful persist. This module does not
//! serialize HTTP envelopes or import `dashboard` / `dashboard_v3` /
//! `gateway` / `state`. Concrete hosts implement [`AccountControlHost`].

use crate::browser::{BrowserProfileOperationKind, StagedBrowserProfiles};
use crate::models::{
    Account, AccountSetupStep, AccountType, AccountUpdate, NEW_READY_KEY_ACCOUNT_ENABLED,
    normalize_account_notes,
};
use crate::provider::{
    CPA_ACCOUNT_ID, ConnectionVerificationStatus, OPENCODE_PROVIDER_ID, VerificationPolicy,
    ZEN_FREE_ACCOUNT_ID,
};
use chrono::Utc;
use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::path::PathBuf;

const ZEN_FREE_MUTATION_MESSAGE: &str =
    "Zen Free settings must use the dedicated provider-settings endpoint";
const ZEN_FREE_DELETE_MESSAGE: &str = "Zen Free is a built-in singleton and cannot be deleted";
const CPA_DELETE_MESSAGE: &str = "CPA Subscription Pool is an external-integration singleton and cannot be deleted as an account";
const SETUP_INCOMPLETE_MESSAGE: &str = "account setup is not complete and cannot be enabled";
const VERIFY_BEFORE_ENABLE_MESSAGE: &str = "verify the account connection before enabling it";

/// Process-level account mutation host. Concrete adapters live in `state`;
/// this module never names the process-level owner.
pub(crate) trait CatalogRefreshHost {
    fn load_routing_snapshot(&self) -> anyhow::Result<crate::routing_snapshot::RoutingSnapshot>;
}

impl<T> CatalogRefreshHost for T
where
    T: Deref + Sync,
    T::Target: CatalogRefreshHost,
{
    fn load_routing_snapshot(&self) -> anyhow::Result<crate::routing_snapshot::RoutingSnapshot> {
        self.deref().load_routing_snapshot()
    }
}

pub trait AccountControlHost: Sync {
    fn with_settings_update<R>(&self, f: impl FnOnce() -> R) -> R;
    fn encrypt_key(&self, plaintext: &str) -> anyhow::Result<String>;
    fn bump_settings_revision(&self) -> u64;
    fn settings_revision(&self) -> u64;
    fn process_generation(&self) -> u64;
    fn recover_browser_profiles_for_account(&self, account_id: &str) -> anyhow::Result<()>;
    fn data_dir(&self) -> PathBuf;
    fn reload_provider_contracts(&self) -> anyhow::Result<()>;
    /// Builtin enablement first; otherwise current dynamic snapshot membership.
    fn ensure_provider_can_enable(
        &self,
        provider_id: &str,
    ) -> Result<(), crate::provider::ProviderBindingError>;
    fn create_account_with_contract(&self, account: &Account) -> anyhow::Result<()>;
    fn update_account(&self, id: &str, update: &AccountUpdate) -> anyhow::Result<()>;
    fn get_account(&self, id: &str) -> anyhow::Result<Option<Account>>;
    fn account_verification_status(
        &self,
        account_id: &str,
    ) -> anyhow::Result<Option<ConnectionVerificationStatus>>;
    fn delete_account_row(&self, id: &str) -> anyhow::Result<()>;
    fn list_identity_model(&self) -> anyhow::Result<crate::db::identity::IdentityModelSnapshot>;
    fn dynamic_auth_kind(&self, provider_id: &str) -> Option<ocg_domain::dynamic::DynamicAuthKind>;
    fn rotate_account_credential(
        &self,
        account_id: &str,
        key_cipher: &str,
    ) -> anyhow::Result<crate::db::identity::RotatedCredential>;
    fn commit_configuration<T>(
        &self,
        mutation: impl FnOnce(&crate::db::Database) -> anyhow::Result<T>,
    ) -> anyhow::Result<T>;
    fn set_public_model_published(&self, key: &str, published: bool)
    -> anyhow::Result<Vec<String>>;
    fn commit_database<T>(
        &self,
        mutation: impl FnOnce(&crate::db::Database) -> anyhow::Result<T>,
    ) -> anyhow::Result<T>;
    fn load_destination_runtime(
        &self,
    ) -> anyhow::Result<crate::destination_projection::DestinationProjection>;
    fn decrypt_key(&self, ciphertext: &str) -> anyhow::Result<String>;
    fn app_config(&self) -> crate::models::AppConfig;
    fn replace_http_catalog(
        &self,
        destination: &ocg_domain::destination::Destination,
        catalog: &[ocg_domain::destination::CatalogModel],
        metadata: Option<&std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>>,
    ) -> anyhow::Result<()>;
    fn log_gateway(&self, level: &str, category: &str, message: &str) -> anyhow::Result<()>;
    fn stop_browser_account(
        &self,
        account_id: &str,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

impl<T> AccountControlHost for T
where
    T: Deref + Sync,
    T::Target: AccountControlHost,
{
    fn with_settings_update<R>(&self, f: impl FnOnce() -> R) -> R {
        self.deref().with_settings_update(f)
    }
    fn encrypt_key(&self, plaintext: &str) -> anyhow::Result<String> {
        self.deref().encrypt_key(plaintext)
    }
    fn bump_settings_revision(&self) -> u64 {
        self.deref().bump_settings_revision()
    }
    fn settings_revision(&self) -> u64 {
        self.deref().settings_revision()
    }
    fn process_generation(&self) -> u64 {
        self.deref().process_generation()
    }
    fn recover_browser_profiles_for_account(&self, account_id: &str) -> anyhow::Result<()> {
        self.deref()
            .recover_browser_profiles_for_account(account_id)
    }
    fn data_dir(&self) -> PathBuf {
        self.deref().data_dir()
    }
    fn reload_provider_contracts(&self) -> anyhow::Result<()> {
        self.deref().reload_provider_contracts()
    }
    fn ensure_provider_can_enable(
        &self,
        provider_id: &str,
    ) -> Result<(), crate::provider::ProviderBindingError> {
        self.deref().ensure_provider_can_enable(provider_id)
    }
    fn create_account_with_contract(&self, account: &Account) -> anyhow::Result<()> {
        self.deref().create_account_with_contract(account)
    }
    fn update_account(&self, id: &str, update: &AccountUpdate) -> anyhow::Result<()> {
        self.deref().update_account(id, update)
    }
    fn get_account(&self, id: &str) -> anyhow::Result<Option<Account>> {
        self.deref().get_account(id)
    }
    fn account_verification_status(
        &self,
        account_id: &str,
    ) -> anyhow::Result<Option<ConnectionVerificationStatus>> {
        self.deref().account_verification_status(account_id)
    }
    fn delete_account_row(&self, id: &str) -> anyhow::Result<()> {
        self.deref().delete_account_row(id)
    }
    fn list_identity_model(&self) -> anyhow::Result<crate::db::identity::IdentityModelSnapshot> {
        self.deref().list_identity_model()
    }
    fn dynamic_auth_kind(&self, provider_id: &str) -> Option<ocg_domain::dynamic::DynamicAuthKind> {
        self.deref().dynamic_auth_kind(provider_id)
    }
    fn rotate_account_credential(
        &self,
        account_id: &str,
        key_cipher: &str,
    ) -> anyhow::Result<crate::db::identity::RotatedCredential> {
        self.deref()
            .rotate_account_credential(account_id, key_cipher)
    }
    fn set_public_model_published(
        &self,
        key: &str,
        published: bool,
    ) -> anyhow::Result<Vec<String>> {
        self.deref().set_public_model_published(key, published)
    }
    fn commit_database<R>(
        &self,
        mutation: impl FnOnce(&crate::db::Database) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        self.deref().commit_database(mutation)
    }
    fn commit_configuration<R>(
        &self,
        mutation: impl FnOnce(&crate::db::Database) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        self.deref().commit_configuration(mutation)
    }
    fn load_destination_runtime(
        &self,
    ) -> anyhow::Result<crate::destination_projection::DestinationProjection> {
        self.deref().load_destination_runtime()
    }
    fn decrypt_key(&self, ciphertext: &str) -> anyhow::Result<String> {
        self.deref().decrypt_key(ciphertext)
    }
    fn app_config(&self) -> crate::models::AppConfig {
        self.deref().app_config()
    }
    fn replace_http_catalog(
        &self,
        destination: &ocg_domain::destination::Destination,
        catalog: &[ocg_domain::destination::CatalogModel],
        metadata: Option<&std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>>,
    ) -> anyhow::Result<()> {
        self.deref()
            .replace_http_catalog(destination, catalog, metadata)
    }
    fn log_gateway(&self, level: &str, category: &str, message: &str) -> anyhow::Result<()> {
        self.deref().log_gateway(level, category, message)
    }
    fn stop_browser_account(
        &self,
        account_id: &str,
    ) -> impl Future<Output = anyhow::Result<()>> + Send {
        self.deref().stop_browser_account(account_id)
    }
}

#[derive(Debug)]
pub enum AccountControlError {
    NotFound,
    RevisionConflict,
    Invalid(String),
    Conflict(String),
    Unavailable(String),
    Internal(anyhow::Error),
}

impl fmt::Display for AccountControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("account not found"),
            Self::RevisionConflict => f.write_str("control-plane revision conflict"),
            Self::Invalid(message) | Self::Conflict(message) | Self::Unavailable(message) => {
                f.write_str(message)
            }
            Self::Internal(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for AccountControlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Internal(error) => Some(error.as_ref()),
            Self::NotFound
            | Self::RevisionConflict
            | Self::Invalid(_)
            | Self::Conflict(_)
            | Self::Unavailable(_) => None,
        }
    }
}

const OBSERVER_ROTATION_MESSAGE: &str = "platform observer credentials cannot be rotated here";
const CPA_ROTATION_MESSAGE: &str =
    "CPA Subscription Pool settings must use the external-integration endpoint";
const NO_AUTH_ROTATION_MESSAGE: &str = "anonymous and no-auth credentials cannot be rotated";

/// Process-local result of replacing one upstream credential.
///
/// This is not a Dashboard DTO. The HTTP adapter maps it after the use case
/// returns; the secret supplied by the caller is never retained here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotatedUpstreamCredential {
    pub credential_id: String,
    pub version: u64,
    pub auth_state_version: u64,
}

/// Process-scoped CAS tokens carried by a Dashboard mutation.
///
/// HTTP adapters parse these from the request body. The use case compares
/// them with the host; it does not know about JSON field names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MutationCas {
    pub expected_revision: u64,
    pub process_generation: u64,
}

/// Replace one upstream credential, including the settings lock and CAS.
///
/// Excludes observer, CPA, Zen Free, and no-auth credentials, then reuses the
/// existing transactional rotation. A matching CAS advances the settings
/// revision exactly once. A stale CAS writes nothing.
#[allow(dead_code)]
pub(crate) fn rotate_upstream_credential(
    host: &impl AccountControlHost,
    credential_id: &str,
    secret: &str,
    cas: MutationCas,
) -> Result<RotatedUpstreamCredential, AccountControlError> {
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(AccountControlError::RevisionConflict);
        }
        rotate_upstream_credential_locked(host, credential_id, secret)
    })
}

/// Replace one upstream credential under the caller's settings lock.
///
/// The caller owns CAS. This operation excludes observer, CPA, Zen Free, and
/// no-auth credentials, then reuses the existing transactional rotation. A
/// successful call advances the settings revision exactly once.
pub(crate) fn rotate_upstream_credential_locked(
    host: &impl AccountControlHost,
    credential_id: &str,
    secret: &str,
) -> Result<RotatedUpstreamCredential, AccountControlError> {
    let secret = secret.trim();
    if secret.is_empty() {
        return Err(AccountControlError::Invalid(
            "secretInput is required".into(),
        ));
    }

    let snapshot = host
        .list_identity_model()
        .map_err(AccountControlError::Internal)?;
    if snapshot.platform_parents.iter().any(|parent| {
        ocg_domain::credential::observer_credential_id_for_platform_account(&parent.platform_id)
            .as_str()
            == credential_id
    }) {
        return Err(AccountControlError::Invalid(
            OBSERVER_ROTATION_MESSAGE.into(),
        ));
    }
    let record = snapshot
        .accounts
        .iter()
        .find(|record| record.credential_id == credential_id)
        .ok_or(AccountControlError::NotFound)?;
    let account = &record.account;
    if account.id == CPA_ACCOUNT_ID {
        return Err(AccountControlError::Invalid(CPA_ROTATION_MESSAGE.into()));
    }
    if account.is_zen_free() {
        return Err(AccountControlError::Invalid(
            ZEN_FREE_MUTATION_MESSAGE.into(),
        ));
    }
    if account.credential_kind == crate::provider::CredentialKind::None {
        return Err(AccountControlError::Invalid(
            NO_AUTH_ROTATION_MESSAGE.into(),
        ));
    }

    let key_cipher = match host.dynamic_auth_kind(&account.provider_id) {
        Some(auth_kind) => encrypted_dynamic_key(host, auth_kind, secret)?,
        None => {
            if let Some(plan) = crate::provider::builtin_provider(&account.provider_id) {
                crate::provider::validate_plan_key(plan, secret)
                    .map_err(|error| AccountControlError::Invalid(error.to_string()))?;
            }
            host.encrypt_key(secret)
                .map_err(AccountControlError::Internal)?
        }
    };
    let rotated = host
        .rotate_account_credential(&account.id, &key_cipher)
        .map_err(|error| AccountControlError::Invalid(error.to_string()))?;
    let _revision = host.bump_settings_revision();
    Ok(RotatedUpstreamCredential {
        credential_id: rotated.credential_id,
        version: rotated.version,
        auth_state_version: rotated.auth_state_version,
    })
}

/// One catalog-row edit. Protocols are domain values; the HTTP adapter converts
/// its wire enum before calling the use case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CatalogModelEdit {
    pub public_model: String,
    pub enabled: Option<bool>,
    pub protocols: Option<Vec<ocg_domain::catalog::UpstreamProtocolKind>>,
    pub preferred: Option<ocg_domain::catalog::UpstreamProtocolKind>,
}

pub(crate) struct HttpDestinationUpdate {
    pub name: String,
    pub endpoint_url: String,
    pub upstream_protocol: ocg_domain::catalog::UpstreamProtocolKind,
    pub auth_kind: ocg_domain::dynamic::DynamicAuthKind,
    pub mappings: Vec<ocg_domain::dynamic::DynamicModelMapping>,
    pub authorize_credential_ids: Vec<String>,
    pub protocol_routes: Option<Vec<ocg_domain::destination::HttpProtocolRoute>>,
    pub enabled: Option<bool>,
    pub catalog_updates: Vec<CatalogModelEdit>,
}

/// Apply catalog edits and removals. Only configurable HTTP catalogs are
/// editable; observer and sealed adapters are refused before any row changes.
pub(crate) fn apply_catalog_model_edits(
    destination: &ocg_domain::destination::Destination,
    updates: &[CatalogModelEdit],
    remove: &[String],
) -> Result<Vec<ocg_domain::destination::CatalogModel>, String> {
    use ocg_domain::destination::{AdapterKind, CatalogModel};
    if destination.adapter != AdapterKind::Http || destination.capabilities.observer {
        return Err("only configurable HTTP catalogs are editable here".into());
    }
    if updates.len() + remove.len() > 2000 {
        return Err("catalog update is too large".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut catalog: Vec<CatalogModel> = destination.catalog.clone();
    for update in updates {
        let key = update.public_model.trim().to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err("duplicate catalog model".into());
        }
        let model = catalog
            .iter_mut()
            .find(|row| row.public_model.to_ascii_lowercase() == key)
            .ok_or_else(|| "catalog model not found".to_string())?;
        let available = ocg_domain::destination::http_model_protocols(destination, model);
        if let Some(protocols) = &update.protocols {
            let unique: std::collections::HashSet<_> = protocols.iter().collect();
            if unique.len() != protocols.len() || protocols.iter().any(|p| !available.contains(p)) {
                return Err("model protocols must be distinct configured routes".into());
            }
            model.protocols.clone_from(protocols);
        }
        if let Some(preferred) = update.preferred {
            if !available.contains(&preferred) {
                return Err("preferred protocol has no configured route".into());
            }
            model.preferred = Some(preferred);
        }
        if let Some(enabled) = update.enabled {
            model.enabled = enabled;
            if enabled && model.protocols.is_empty() && update.protocols.is_none() {
                model.protocols.clone_from(&available);
            }
        }
        if model.enabled && model.protocols.is_empty() {
            return Err("enabled model requires an enabled protocol".into());
        }
        if model.enabled
            && model
                .preferred
                .is_none_or(|preferred| !model.protocols.contains(&preferred))
        {
            if update.preferred.is_some() {
                return Err("preferred protocol must be enabled".into());
            }
            model.preferred = model.protocols.first().copied();
        }
    }
    for name in remove {
        let key = name.trim().to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err("duplicate or conflicting catalog model".into());
        }
        let index = catalog
            .iter()
            .position(|row| row.public_model.to_ascii_lowercase() == key)
            .ok_or_else(|| "catalog model not found".to_string())?;
        catalog.remove(index);
    }
    Ok(catalog)
}

pub(crate) struct PreparedCatalogRefresh {
    destination: ocg_domain::destination::Destination,
    credential: Option<crate::routing_snapshot::ExecutionCredential>,
    input: crate::models::AccountCustomConfigInput,
    config: crate::models::AppConfig,
    key: String,
    auth: Option<crate::provider::UpstreamAuthScheme>,
}

#[derive(Debug)]
pub(crate) enum CatalogRefreshError {
    Invalid(String),
    NotFound(String),
    Conflict(String),
    Outbound(String),
    Internal(anyhow::Error),
}

/// Prepare one configurable-HTTP catalog refresh under the settings lock.
///
/// The returned value carries the decrypted discovery key. The caller must
/// drop the settings lock before the bounded network discovery, then pass the
/// same value back to [`commit_catalog_refresh`].
pub(crate) fn prepare_catalog_refresh(
    host: &(impl AccountControlHost + CatalogRefreshHost),
    id: &str,
    cas: MutationCas,
) -> Result<PreparedCatalogRefresh, CatalogRefreshError> {
    use ocg_domain::destination::{AdapterKind, AuthScheme};
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(CatalogRefreshError::Conflict("revision conflict".into()));
        }
        let snapshot = host
            .load_routing_snapshot()
            .map_err(CatalogRefreshError::Internal)?;
        let destination = snapshot
            .projection
            .destinations
            .into_iter()
            .find(|destination| destination.id == id)
            .ok_or_else(|| CatalogRefreshError::NotFound("destination not found".into()))?;
        if destination.adapter != AdapterKind::Http
            || !destination.capabilities.discoverable_models
            || destination.capabilities.observer
        {
            return Err(CatalogRefreshError::Invalid(
                "destination does not support HTTP model discovery".into(),
            ));
        }
        let protocol = destination.protocols.first().copied().ok_or_else(|| {
            CatalogRefreshError::Invalid("destination has no upstream protocol".into())
        })?;
        let input = crate::models::AccountCustomConfigInput {
            endpoint_url: destination.base_url.clone().unwrap_or_default(),
            upstream_protocol: protocol,
        };
        let models_url =
            crate::custom::derive_custom_models_endpoint(&input.endpoint_url, protocol)
                .map_err(|error| CatalogRefreshError::Invalid(error.message))?;
        let credential = if destination.auth_scheme == AuthScheme::None {
            None
        } else {
            let mut candidates: Vec<_> = snapshot
                .credentials
                .into_iter()
                .filter(|credential| {
                    discovery_credential_allowed(
                        credential,
                        &destination,
                        protocol,
                        models_url.as_str(),
                    )
                })
                .collect();
            candidates.sort_by_key(|credential| !credential.enabled);
            Some(candidates.into_iter().next().ok_or_else(|| {
                CatalogRefreshError::Invalid(
                    "model discovery requires a ready Key authorized for this destination".into(),
                )
            })?)
        };
        let key = credential
            .as_ref()
            .map(|credential| host.decrypt_key(&credential.key_cipher))
            .transpose()
            .map_err(CatalogRefreshError::Internal)?
            .unwrap_or_default();
        let auth = match destination.auth_scheme {
            AuthScheme::Bearer => Some(crate::provider::UpstreamAuthScheme::Bearer),
            AuthScheme::XApiKey => Some(crate::provider::UpstreamAuthScheme::XApiKey),
            AuthScheme::ApiKey => Some(crate::provider::UpstreamAuthScheme::ApiKey),
            AuthScheme::None => None,
        };
        Ok(PreparedCatalogRefresh {
            destination,
            credential,
            input,
            config: host.app_config(),
            key,
            auth,
        })
    })
}

fn discovery_credential_allowed(
    credential: &crate::routing_snapshot::ExecutionCredential,
    destination: &ocg_domain::destination::Destination,
    protocol: ocg_domain::destination::Protocol,
    models_url: &str,
) -> bool {
    use ocg_domain::connection::{ConnectionId, EndpointOperation, endpoint_id_for};
    if credential.destination_id != destination.id
        || !credential.ready
        || !credential.binding_enabled
        || credential.key_cipher.is_empty()
        || credential.authorization_connection_id.is_empty()
    {
        return false;
    }
    let Ok(connection): Result<ConnectionId, _> = serde_json::from_value(
        serde_json::Value::String(credential.authorization_connection_id.clone()),
    ) else {
        return false;
    };
    let endpoint = endpoint_id_for(&connection, EndpointOperation::from(protocol)).to_string();
    credential.grants.allowed_endpoint_ids.contains(&endpoint)
        && crate::custom_http::ensure_secret_origin_granted(
            models_url,
            &credential.grants.allowed_origins,
        )
        .is_ok()
}

/// Re-check the destination and credential, then merge and publish the catalog.
///
/// An empty discovery writes nothing. A destination, credential, grant, or CAS
/// change during the lock-free discovery rejects the stale result. Callers that
/// already hold `settings_update` use [`commit_catalog_refresh_locked`] so the
/// receipt can be projected before that lock is released.
#[allow(dead_code)]
#[cfg(test)]
pub(crate) fn commit_catalog_refresh(
    host: &(impl AccountControlHost + CatalogRefreshHost),
    id: &str,
    cas: MutationCas,
    prepared: PreparedCatalogRefresh,
    models: &[String],
    metadata: &std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>,
) -> Result<(ocg_domain::destination::Destination, usize), CatalogRefreshError> {
    host.with_settings_update(|| {
        commit_catalog_refresh_locked(host, id, cas, prepared, models, metadata)
    })
}

/// Caller holds `settings_update`. Does not take the lock and does not await.
pub(crate) fn commit_catalog_refresh_locked(
    host: &(impl AccountControlHost + CatalogRefreshHost),
    id: &str,
    cas: MutationCas,
    prepared: PreparedCatalogRefresh,
    models: &[String],
    metadata: &std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>,
) -> Result<(ocg_domain::destination::Destination, usize), CatalogRefreshError> {
    if models.is_empty() {
        return Err(CatalogRefreshError::Outbound(
            "model discovery returned no usable models; saved catalog retained".into(),
        ));
    }
    let PreparedCatalogRefresh {
        destination,
        credential,
        ..
    } = prepared;
    if cas.expected_revision != host.settings_revision()
        || cas.process_generation != host.process_generation()
    {
        return Err(CatalogRefreshError::Conflict("revision conflict".into()));
    }
    let current = host
        .load_routing_snapshot()
        .map_err(CatalogRefreshError::Internal)?;
    if current
        .projection
        .destinations
        .iter()
        .find(|row| row.id == id)
        != Some(&destination)
        || credential.as_ref().is_some_and(|before| {
            !current.credentials.iter().any(|after| {
                after.credential_id == before.credential_id
                    && after.destination_id == before.destination_id
                    && after.credential_version == before.credential_version
                    && after.key_cipher == before.key_cipher
                    && after.ready == before.ready
                    && after.binding_enabled == before.binding_enabled
                    && after.binding_id == before.binding_id
                    && after.authorization_connection_id == before.authorization_connection_id
                    && after.grants == before.grants
            })
        })
    {
        return Err(CatalogRefreshError::Conflict(
            "destination or credential changed during model discovery".into(),
        ));
    }
    let available: Vec<_> = ocg_domain::destination::http_protocol_routes(&destination)
        .iter()
        .map(|route| route.protocol)
        .collect();
    let catalog = merge_discovered_models(&destination.catalog, models, &available);
    let added_count = catalog.len() - destination.catalog.len();
    host.replace_http_catalog(&destination, &catalog, Some(metadata))
        .map_err(CatalogRefreshError::Internal)?;
    let mut updated = destination;
    updated.catalog = catalog;
    Ok((updated, added_count))
}

fn merge_discovered_models(
    existing: &[ocg_domain::destination::CatalogModel],
    discovered: &[String],
    protocols: &[ocg_domain::destination::Protocol],
) -> Vec<ocg_domain::destination::CatalogModel> {
    let mut catalog = existing.to_vec();
    let mut known: std::collections::HashSet<_> = existing
        .iter()
        .flat_map(|model| {
            [
                model.public_model.to_ascii_lowercase(),
                model.upstream_model.to_ascii_lowercase(),
            ]
        })
        .collect();
    for model in discovered {
        if known.insert(model.to_ascii_lowercase()) {
            catalog.push(ocg_domain::destination::CatalogModel {
                public_model: model.clone(),
                upstream_model: model.clone(),
                protocols: protocols.to_vec(),
                preferred: protocols.first().copied(),
                enabled: !protocols.is_empty(),
                upstream_override: None,
            });
        }
    }
    catalog
}

impl PreparedCatalogRefresh {
    pub(crate) fn discovery_input(
        &self,
    ) -> (
        &crate::models::AppConfig,
        &crate::models::AccountCustomConfigInput,
        Option<crate::provider::UpstreamAuthScheme>,
        &str,
    ) {
        (&self.config, &self.input, self.auth, &self.key)
    }

    pub(crate) fn redact(&self, message: &str) -> String {
        crate::redaction::redact_known_secret(message, &self.key)
    }
}

/// Replace one configurable HTTP catalog under the settings lock and CAS.
///
/// The caller supplies already converted domain edits. Empty input, unknown
/// destinations, invalid protocol selections, and stale CAS write nothing.
#[expect(dead_code)]
pub(crate) fn update_http_catalog(
    host: &(impl AccountControlHost + CatalogRefreshHost),
    destination_id: &str,
    cas: MutationCas,
    updates: &[CatalogModelEdit],
    remove: &[String],
) -> Result<(), AccountControlError> {
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(AccountControlError::RevisionConflict);
        }
        update_http_catalog_locked(host, destination_id, updates, remove)
    })
}

/// Caller holds `settings_update` and has already checked CAS.
pub(crate) fn update_http_catalog_locked(
    host: &(impl AccountControlHost + CatalogRefreshHost),
    destination_id: &str,
    updates: &[CatalogModelEdit],
    remove: &[String],
) -> Result<(), AccountControlError> {
    if updates.is_empty() && remove.is_empty() {
        return Err(AccountControlError::Invalid(
            "catalog update is empty".into(),
        ));
    }
    let snapshot = host
        .load_routing_snapshot()
        .map_err(AccountControlError::Internal)?;
    let destination = snapshot
        .projection
        .destinations
        .into_iter()
        .find(|row| row.id == destination_id)
        .ok_or(AccountControlError::NotFound)?;
    let catalog = apply_catalog_model_edits(&destination, updates, remove)
        .map_err(AccountControlError::Invalid)?;
    host.replace_http_catalog(&destination, &catalog, None)
        .map_err(AccountControlError::Internal)
}

#[expect(dead_code)]
pub(crate) fn update_http_destination(
    host: &impl AccountControlHost,
    destination_id: &str,
    cas: MutationCas,
    update: HttpDestinationUpdate,
) -> Result<(), AccountControlError> {
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(AccountControlError::RevisionConflict);
        }
        update_http_destination_locked(host, destination_id, update)
    })
}

pub(crate) fn update_http_destination_locked(
    host: &impl AccountControlHost,
    destination_id: &str,
    update: HttpDestinationUpdate,
) -> Result<(), AccountControlError> {
    let current = load_http_destination(host, destination_id)?;
    if current.adapter != ocg_domain::destination::AdapterKind::Http {
        return Err(AccountControlError::Invalid(
            "sealed destination adapters are immutable".into(),
        ));
    }
    if current.capabilities.observer {
        return Err(AccountControlError::Invalid(
            "platform-managed destinations are immutable".into(),
        ));
    }
    let endpoint_url = crate::custom::validate_custom_endpoint_url(&update.endpoint_url)
        .map_err(|error| AccountControlError::Invalid(error.to_string()))?;
    let definition =
        crate::dynamic::validate_definition(ocg_domain::dynamic::DynamicProviderDefinition {
            preset_id: None,
            id: current.id,
            name: update.name,
            endpoint_url,
            upstream_protocol: update.upstream_protocol,
            auth_kind: update.auth_kind,
            mappings: update.mappings,
        })
        .map_err(|error| AccountControlError::Invalid(error.to_string()))?;
    host.commit_configuration(|db| {
        crate::db::destination_commands::replace_http_destination_with_routes_on(
            db,
            destination_id,
            &definition,
            &update.authorize_credential_ids,
            update.protocol_routes.as_deref(),
        )?;
        if !update.catalog_updates.is_empty() {
            let current =
                crate::db::destination_commands::load_http_destination(db, destination_id)?;
            let catalog = apply_catalog_model_edits(&current, &update.catalog_updates, &[])
                .map_err(anyhow::Error::msg)?;
            crate::db::destination_commands::replace_http_catalog_on(&db.conn, &current, &catalog)?;
        }
        if let Some(enabled) = update.enabled {
            db.conn.execute(
                "UPDATE destinations SET enabled = ?2 WHERE id = ?1",
                rusqlite::params![destination_id, enabled],
            )?;
        }
        Ok(())
    })
    .map_err(|error| AccountControlError::Invalid(error.to_string()))
}

#[expect(dead_code)]
pub(crate) fn replace_routing_cards(
    host: &impl AccountControlHost,
    cas: MutationCas,
    cards: &[crate::db::routing_cards::RoutingCard],
) -> Result<(), AccountControlError> {
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(AccountControlError::RevisionConflict);
        }
        replace_routing_cards_locked(host, cards)
    })
}

pub(crate) fn replace_routing_cards_locked(
    host: &impl AccountControlHost,
    cards: &[crate::db::routing_cards::RoutingCard],
) -> Result<(), AccountControlError> {
    host.commit_database(|db| {
        crate::db::routing_cards::save_on(&db.conn, cards)?;
        crate::db::routing_cards::reconcile_on(&db.conn)?;
        Ok(())
    })
    .map_err(|error| AccountControlError::Invalid(error.to_string()))?;
    let _revision = host.bump_settings_revision();
    Ok(())
}

#[expect(dead_code)]
pub(crate) fn add_builtin_catalog_models(
    host: &impl AccountControlHost,
    cas: MutationCas,
    provider_id: &str,
    model_ids: &[String],
) -> Result<(), AccountControlError> {
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(AccountControlError::RevisionConflict);
        }
        add_builtin_catalog_models_locked(host, provider_id, model_ids)
    })
}

pub(crate) fn add_builtin_catalog_models_locked(
    host: &impl AccountControlHost,
    provider_id: &str,
    model_ids: &[String],
) -> Result<(), AccountControlError> {
    let scope = crate::provider_contracts::ContractScope::provider(provider_id);
    host.commit_configuration(|db| db.add_contract_catalog_models(&scope, model_ids, Utc::now()))
        .map_err(AccountControlError::Internal)
        .map(|_| ())
}

#[expect(dead_code)]
pub(crate) fn edit_builtin_catalog_model(
    host: &impl AccountControlHost,
    cas: MutationCas,
    provider_id: &str,
    original_model_id: Option<&str>,
    model: ocg_domain::destination::CatalogModel,
) -> Result<(), AccountControlError> {
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(AccountControlError::RevisionConflict);
        }
        edit_builtin_catalog_model_locked(host, provider_id, original_model_id, model)
    })
}

pub(crate) fn edit_builtin_catalog_model_locked(
    host: &impl AccountControlHost,
    provider_id: &str,
    original_model_id: Option<&str>,
    model: ocg_domain::destination::CatalogModel,
) -> Result<(), AccountControlError> {
    let scope = crate::provider_contracts::ContractScope::provider(provider_id);
    host.commit_configuration(|db| {
        db.edit_contract_catalog_model(&scope, original_model_id, model, Utc::now())
    })
    .map_err(|error| AccountControlError::Invalid(error.to_string()))
}

pub(crate) fn declare_model_metadata_locked(
    host: &impl AccountControlHost,
    destination: &ocg_domain::destination::Destination,
    model: &ocg_domain::destination::CatalogModel,
    metadata: Option<crate::model_metadata::ModelMetadata>,
) -> Result<(), AccountControlError> {
    host.commit_configuration(|db| crate::model_metadata::declare(db, destination, model, metadata))
        .map_err(AccountControlError::Internal)
}

#[allow(dead_code)]
pub(crate) fn set_public_model_publication(
    host: &impl AccountControlHost,
    cas: MutationCas,
    key: &str,
    published: bool,
) -> Result<Vec<String>, AccountControlError> {
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(AccountControlError::RevisionConflict);
        }
        set_public_model_publication_locked(host, key, published)
    })
}

pub(crate) fn set_public_model_publication_locked(
    host: &impl AccountControlHost,
    key: &str,
    published: bool,
) -> Result<Vec<String>, AccountControlError> {
    let unpublished = host
        .set_public_model_published(key, published)
        .map_err(AccountControlError::Internal)?;
    let _revision = host.bump_settings_revision();
    Ok(unpublished)
}

#[expect(dead_code)]
pub(crate) fn delete_http_destination(
    host: &impl AccountControlHost,
    destination_id: &str,
    cas: MutationCas,
) -> Result<(), AccountControlError> {
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(AccountControlError::RevisionConflict);
        }
        delete_http_destination_locked(host, destination_id)
    })
}

/// One personal-credit mutation. The HTTP adapter parses the body into this
/// value; it never supplies a database connection.
pub(crate) enum CreditMutation<'a> {
    Configure {
        configuration: crate::billing_types::CreditConfiguration,
        initial_buckets: Option<Vec<crate::billing_types::CreditBucket>>,
    },
    Calibrate {
        balances: &'a [crate::billing_types::CreditBalanceCorrection],
    },
    Grant {
        label: String,
        amount: f64,
        expires_at: Option<chrono::DateTime<Utc>>,
    },
    Disable,
    /// Test-only: persist a meter the following view read cannot decode, so
    /// the uncommitted transaction rolls back.
    #[cfg(test)]
    Corrupt,
}

/// Facts the HTTP adapter projects into the billing receipt. The use case
/// does not build the Dashboard DTO.
pub(crate) struct AppliedCreditMutation {
    pub adapter: ocg_domain::destination::AdapterKind,
    pub endpoint: String,
    pub provider_id: String,
    pub usage: crate::dashboard_v3::ProviderUsage,
    pub credits: Option<crate::billing_types::CreditMeterView>,
    pub cash: Option<crate::official_api::OfficialApiStatus>,
    pub revision: u64,
}

/// Apply one credit mutation under the settings lock and CAS.
///
/// Usage is read before the write, so a usage failure persists nothing. The
/// credit view and the official-cash read share the uncommitted transaction,
/// so a cash failure rolls the credit write back. The view clock is sampled
/// after the write so a grant bucket that starts at `now` is already active.
/// The revision advances once, after the transaction commits.
pub(crate) fn apply_credit_mutation(
    state: &crate::state::CoreState,
    account_id: &str,
    cas: MutationCas,
    mutation: CreditMutation<'_>,
) -> Result<AppliedCreditMutation, AccountControlError> {
    use ocg_domain::destination::AdapterKind;
    // The credit use case operates on the unlocked inner state; the Arc
    // wrapper is only how callers pass it in.
    let host: &crate::state::CoreStateInner = state;
    host.with_settings_update(|| {
        if cas.expected_revision != host.settings_revision()
            || cas.process_generation != host.process_generation()
        {
            return Err(AccountControlError::RevisionConflict);
        }
        let db = host.db.lock();
        let (adapter, endpoint, legacy) = billing_destination(&db.conn, account_id)
            .map_err(AccountControlError::Internal)?
            .ok_or_else(|| AccountControlError::NotFound)?;
        if adapter != AdapterKind::Http || !matches!(legacy.as_str(), "dynamic" | "custom_account")
        {
            return Err(AccountControlError::Invalid(
                "this account uses its provider billing contract".into(),
            ));
        }
        let account = db
            .get_account(account_id)
            .map_err(AccountControlError::Internal)?
            .ok_or(AccountControlError::NotFound)?;
        let official_cash = db
            .get_dynamic_provider(&account.provider_id)
            .map_err(AccountControlError::Internal)?
            .as_ref()
            .and_then(crate::official_api::kind_for_runtime)
            .is_some();
        let usage = crate::dashboard_v3::usage::provider_usage_from_db(state, &db, account_id)
            .map_err(|error| AccountControlError::Internal(anyhow::anyhow!("{error:?}")))?;
        let transaction = db
            .conn
            .unchecked_transaction()
            .map_err(|error| AccountControlError::Internal(error.into()))?;
        let written_at = Utc::now();
        let write = match mutation {
            CreditMutation::Configure {
                configuration,
                initial_buckets,
            } => crate::db::billing::configure_on(
                &transaction,
                account_id,
                configuration.clone(),
                initial_buckets.clone(),
                written_at,
            ),
            CreditMutation::Calibrate { balances } => {
                crate::db::billing::calibrate_on(&transaction, account_id, balances, written_at)
            }
            CreditMutation::Grant {
                label,
                amount,
                expires_at,
            } => crate::db::billing::grant_on(
                &transaction,
                account_id,
                label.clone(),
                amount,
                expires_at,
                written_at,
            ),
            CreditMutation::Disable => {
                crate::db::billing::disable_on(&transaction, account_id, written_at)
            }
            #[cfg(test)]
            CreditMutation::Corrupt => transaction
                .execute(
                    "UPDATE credentials SET credit_meter_json = '{' WHERE legacy_account_id = ?1",
                    [account_id],
                )
                .map(|_| ())
                .map_err(Into::into),
        };
        write.map_err(billing_write_error)?;
        let credits = crate::db::billing::read_view_on(&transaction, account_id, Utc::now())
            .map_err(AccountControlError::Internal)?;
        let model = if credits.is_some() {
            crate::billing_types::BillingModel::Credits
        } else {
            crate::billing::billing_model_for_destination(adapter, &endpoint)
        };
        let cash = if official_cash && model == crate::billing_types::BillingModel::Cash {
            Some(
                crate::dashboard_v4::official_api::status_locked(state, &db, account_id)
                    .map_err(|error| AccountControlError::Internal(anyhow::anyhow!("{error:?}")))?,
            )
        } else {
            None
        };
        transaction
            .commit()
            .map_err(|error| AccountControlError::Internal(error.into()))?;
        let revision = host.bump_settings_revision();
        Ok(AppliedCreditMutation {
            adapter,
            endpoint,
            provider_id: account.provider_id,
            usage,
            credits,
            cash,
            revision,
        })
    })
}

fn billing_write_error(error: anyhow::Error) -> AccountControlError {
    if error.downcast_ref::<rusqlite::Error>().is_some() {
        AccountControlError::Internal(error)
    } else {
        AccountControlError::Invalid(error.to_string())
    }
}

fn billing_destination(
    conn: &rusqlite::Connection,
    account_id: &str,
) -> anyhow::Result<Option<(ocg_domain::destination::AdapterKind, String, String)>> {
    use rusqlite::OptionalExtension;
    let row: Option<(String, Option<String>, String)> = conn
        .query_row(
            "SELECT d.adapter, d.base_url, d.legacy_kind FROM credentials c
         JOIN destinations d ON d.id = c.destination_id
         WHERE c.legacy_account_id = ?1
           AND COALESCE(c.credential_purpose, 'inference') = 'inference'",
            [account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(adapter, endpoint, legacy)| {
        let kind = ocg_domain::destination::AdapterKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == adapter)
            .ok_or_else(|| anyhow::anyhow!("unknown billing adapter"))?;
        Ok((kind, endpoint.unwrap_or_default(), legacy))
    })
    .transpose()
}

pub(crate) fn delete_http_destination_locked(
    host: &impl AccountControlHost,
    destination_id: &str,
) -> Result<(), AccountControlError> {
    host.commit_configuration(|db| {
        crate::db::destination_commands::delete_http_destination_on(db, destination_id)
    })
    .map_err(|error| AccountControlError::Invalid(error.to_string()))
}

fn load_http_destination(
    host: &impl AccountControlHost,
    destination_id: &str,
) -> Result<ocg_domain::destination::Destination, AccountControlError> {
    host.load_destination_runtime()
        .map_err(AccountControlError::Internal)?
        .destinations
        .into_iter()
        .find(|destination| destination.id == destination_id)
        .ok_or(AccountControlError::NotFound)
}

fn encrypted_dynamic_key(
    host: &impl AccountControlHost,
    auth_kind: ocg_domain::dynamic::DynamicAuthKind,
    secret: &str,
) -> Result<String, AccountControlError> {
    if !auth_kind.requires_key() {
        return Ok(String::new());
    }
    if secret.is_empty() {
        return Err(AccountControlError::Invalid("key is required".into()));
    }
    host.encrypt_key(secret)
        .map_err(AccountControlError::Internal)
}

/// Create an enabled ready OpenCode Go API-key account.
///
/// Holds `settings_update` and bumps `settings_revision` on success. Custom
/// and other catalog plans are not accepted here; the CLI surface stays
/// Go-only.
pub fn create_go_api_key(
    host: &impl AccountControlHost,
    name: String,
    key: String,
    username: Option<String>,
    password: Option<String>,
) -> Result<Account, AccountControlError> {
    host.with_settings_update(|| create_go_api_key_locked(host, name, key, username, password))
}

fn create_go_api_key_locked(
    host: &impl AccountControlHost,
    name: String,
    key: String,
    username: Option<String>,
    password: Option<String>,
) -> Result<Account, AccountControlError> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(AccountControlError::Invalid("name is required".into()));
    }
    if name.chars().count() > 200 {
        return Err(AccountControlError::Invalid(
            "name must be at most 200 characters".into(),
        ));
    }
    let plan = crate::provider::builtin_provider(OPENCODE_PROVIDER_ID)
        .ok_or_else(|| AccountControlError::Invalid("unknown provider offering".into()))?;
    crate::provider::validate_plan_key(plan, &key)
        .map_err(|error| AccountControlError::Invalid(error.to_string()))?;
    let now = Utc::now();
    let id = uuid::Uuid::new_v4().to_string();
    let account = Account {
        id: id.clone(),
        provider_id: OPENCODE_PROVIDER_ID.to_string(),

        credential_kind: crate::provider::CredentialKind::ApiKey,
        quota_scope: crate::provider::QuotaScope::Key,
        name,
        username: clean_optional(username),
        password_cipher: encrypted_optional(host, password)?,
        key_cipher: host
            .encrypt_key(key.trim())
            .map_err(AccountControlError::Internal)?,
        enabled: NEW_READY_KEY_ACCOUNT_ENABLED,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: normalize_account_notes("")
            .map_err(|error| AccountControlError::Invalid(error.to_string()))?,
        created_at: now,
        updated_at: now,
    };
    crate::provider::ensure_enabled_provider_is_routable(&account.provider_id, account.enabled)
        .map_err(|error| AccountControlError::Conflict(error.to_string()))?;
    host.create_account_with_contract(&account)
        .map_err(map_write_error)?;
    let _ = host.log_gateway(
        "info",
        "account",
        &format!("created account {}", account.name),
    );
    commit_account(host, &id, true)
}

/// Enable or disable an account using Dashboard enablement policy.
///
/// Holds `settings_update` and bumps `settings_revision` on success. Pending
/// Custom accounts cannot be enabled; Zen Free is rejected.
pub fn set_account_enabled(
    host: &impl AccountControlHost,
    id: &str,
    enabled: bool,
) -> Result<Account, AccountControlError> {
    host.with_settings_update(|| set_account_enabled_locked(host, id, enabled))
}

/// Same persist + revision bump as [`set_account_enabled`], for callers that
/// already hold `settings_update` (Dashboard CAS).
pub(crate) fn set_account_enabled_locked(
    host: &impl AccountControlHost,
    id: &str,
    enabled: bool,
) -> Result<Account, AccountControlError> {
    let account = load_account(host, id)?;
    if account.is_zen_free() {
        return Err(AccountControlError::Invalid(
            ZEN_FREE_MUTATION_MESSAGE.into(),
        ));
    }
    if enabled && !account.setup_step.is_ready() {
        return Err(AccountControlError::Conflict(
            SETUP_INCOMPLETE_MESSAGE.into(),
        ));
    }
    if enabled
        && account.credential_kind == crate::provider::CredentialKind::ApiKey
        && account.key_cipher.is_empty()
    {
        return Err(AccountControlError::Conflict(
            SETUP_INCOMPLETE_MESSAGE.into(),
        ));
    }
    if enabled {
        ensure_account_can_enable(host, &account)?;
    }
    let update = AccountUpdate {
        name: None,
        username: None,
        password: None,
        key: None,
        enabled: Some(enabled),
        referral_code: None,
        purchase_date: None,
        notes: None,
    };
    host.update_account(id, &update).map_err(map_write_error)?;
    let _ = host.log_gateway(
        "info",
        "account",
        &format!(
            "{} account {}",
            if enabled { "enabled" } else { "disabled" },
            account.name
        ),
    );
    commit_account(host, id, false)
}

/// Delete an account, staging and purging its browser profiles.
///
/// Stops the native/remote browser without holding `settings_update`, then
/// re-locks for the persist + revision bump. `cas` is `(settings_revision,
/// process_generation)` rechecked after the await so Dashboard can keep
/// strong CAS; the CLI passes `None`. Does not cancel process-level workers.
pub async fn delete_account(
    host: &impl AccountControlHost,
    id: &str,
    cas: Option<(u64, u64)>,
) -> Result<u64, AccountControlError> {
    host.with_settings_update(|| {
        check_cas(host, cas)?;
        reject_singleton_delete(id)?;
        host.recover_browser_profiles_for_account(id)
            .map_err(AccountControlError::Internal)?;
        load_account(host, id)?;
        Ok(())
    })?;

    host.stop_browser_account(id)
        .await
        .map_err(|error| AccountControlError::Unavailable(error.to_string()))?;

    host.with_settings_update(|| delete_account_persist(host, id, cas))
}

fn delete_account_persist(
    host: &impl AccountControlHost,
    id: &str,
    cas: Option<(u64, u64)>,
) -> Result<u64, AccountControlError> {
    check_cas(host, cas)?;
    reject_singleton_delete(id)?;
    let account = load_account(host, id)?;
    let staged = StagedBrowserProfiles::stage(
        &host.data_dir(),
        id,
        BrowserProfileOperationKind::DeleteAccount,
    )
    .map_err(AccountControlError::Internal)?;
    let delete_result = host.delete_account_row(id);
    if delete_result.is_ok() {
        let _ = host.log_gateway(
            "info",
            "account",
            &format!("deleted account {} ({})", id, account.name),
        );
    }
    if let Err(error) = delete_result {
        let restore_error = staged.restore().err();
        return Err(AccountControlError::Internal(match restore_error {
            Some(restore) => anyhow::anyhow!(
                "failed to delete account: {error}; failed to restore browser profile: {restore}"
            ),
            None => anyhow::anyhow!("failed to delete account: {error}"),
        }));
    }
    let revision = host.bump_settings_revision();
    staged.purge().map_err(AccountControlError::Internal)?;
    host.reload_provider_contracts()
        .map_err(AccountControlError::Internal)?;
    Ok(revision)
}

pub(crate) fn ensure_account_can_enable(
    host: &impl AccountControlHost,
    account: &Account,
) -> Result<(), AccountControlError> {
    host.ensure_provider_can_enable(&account.provider_id)
        .map_err(|error| AccountControlError::Conflict(error.to_string()))?;
    let Some(plan) = crate::provider::builtin_provider(&account.provider_id) else {
        return Ok(());
    };
    // Verification blocks enablement only for Plans whose composed card
    // descriptor gates on it (GOAT). Custom keeps `VerificationPolicy::Required`
    // for status tracking, but its card flips the gate off, so a pending Custom
    // account may be enabled without verifying first. Dynamic snapshot members
    // have no builtin card and skip this gate.
    let verification_gates_enablement = plan.verification_policy == VerificationPolicy::Required
        && crate::provider::ProviderRegistry::get(&account.provider_id)
            .is_some_and(|descriptor| descriptor.card_actions.enable_requires_verification);
    if verification_gates_enablement {
        let status = host
            .account_verification_status(&account.id)
            .map_err(AccountControlError::Internal)?
            .unwrap_or(ConnectionVerificationStatus::Pending);
        if !status.allows_enablement() {
            return Err(AccountControlError::Conflict(
                VERIFY_BEFORE_ENABLE_MESSAGE.into(),
            ));
        }
    }
    Ok(())
}

fn commit_account(
    host: &impl AccountControlHost,
    id: &str,
    reload_contracts: bool,
) -> Result<Account, AccountControlError> {
    let _revision = host.bump_settings_revision();
    if reload_contracts {
        host.reload_provider_contracts()
            .map_err(AccountControlError::Internal)?;
    }
    load_account(host, id)
}

fn load_account(host: &impl AccountControlHost, id: &str) -> Result<Account, AccountControlError> {
    host.get_account(id)
        .map_err(AccountControlError::Internal)?
        .ok_or(AccountControlError::NotFound)
}

fn check_cas(
    host: &impl AccountControlHost,
    cas: Option<(u64, u64)>,
) -> Result<(), AccountControlError> {
    let Some((revision, generation)) = cas else {
        return Ok(());
    };
    if revision != host.settings_revision() || generation != host.process_generation() {
        Err(AccountControlError::RevisionConflict)
    } else {
        Ok(())
    }
}

fn reject_singleton_delete(id: &str) -> Result<(), AccountControlError> {
    if id == ZEN_FREE_ACCOUNT_ID {
        Err(AccountControlError::Invalid(ZEN_FREE_DELETE_MESSAGE.into()))
    } else if id == CPA_ACCOUNT_ID {
        Err(AccountControlError::Invalid(CPA_DELETE_MESSAGE.into()))
    } else {
        Ok(())
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn encrypted_optional(
    host: &impl AccountControlHost,
    value: Option<String>,
) -> Result<Option<String>, AccountControlError> {
    match value.as_deref().map(str::trim) {
        Some("") | None => Ok(None),
        Some(v) => host
            .encrypt_key(v)
            .map(Some)
            .map_err(AccountControlError::Internal),
    }
}

fn map_write_error(error: anyhow::Error) -> AccountControlError {
    if let Some(binding) = error.downcast_ref::<crate::provider::ProviderBindingError>() {
        return AccountControlError::Conflict(binding.to_string());
    }
    let message = error.to_string();
    if message.contains("not routable") {
        AccountControlError::Conflict(message)
    } else {
        AccountControlError::Internal(error)
    }
}

#[cfg(test)]
mod tests;
