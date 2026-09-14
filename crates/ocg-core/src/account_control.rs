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
mod tests {
    use super::*;
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::models::{AccountSetupStep, AccountType};
    use crate::provider::CUSTOM_PROVIDER_ID;
    use crate::state::CoreStateInner;
    use std::sync::Arc;

    fn temp_state(label: &str) -> (Arc<CoreStateInner>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "ocg-account-control-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(dir.clone()).unwrap();
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("account-control"));
        (
            Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap()),
            dir,
        )
    }

    fn custom_pending(state: &CoreStateInner, id: &str) -> Account {
        let now = Utc::now();
        Account {
            id: id.to_string(),
            provider_id: CUSTOM_PROVIDER_ID.to_string(),

            credential_kind: crate::provider::CredentialKind::ApiKey,
            quota_scope: crate::provider::QuotaScope::Key,
            name: id.to_string(),
            username: None,
            password_cipher: None,
            key_cipher: state.encrypt_key("custom-key").unwrap(),
            enabled: false,
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
            notes: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn go_create_and_toggle_bump_revision_and_allow_pending_custom() {
        let (state, dir) = temp_state("go-toggle");
        let before = state.settings_revision();
        let created = create_go_api_key(
            &state,
            "go-main".into(),
            "sk-go".into(),
            Some("  alice  ".into()),
            Some("  secret  ".into()),
        )
        .unwrap();
        assert!(!created.enabled);
        assert_eq!(created.provider_id, OPENCODE_PROVIDER_ID);
        assert_eq!(created.username.as_deref(), Some("alice"));
        assert_eq!(state.settings_revision(), before + 1);

        let enabled = set_account_enabled(&state, &created.id, true).unwrap();
        assert!(enabled.enabled);
        assert_eq!(state.settings_revision(), before + 2);

        let disabled = set_account_enabled(&state, &created.id, false).unwrap();
        assert!(!disabled.enabled);
        assert_eq!(state.settings_revision(), before + 3);

        // Custom verification is an optional tool: a pending Custom account
        // enables without verifying first.
        state
            .db
            .lock()
            .create_account(&custom_pending(&state, "cli-custom"))
            .unwrap();
        let enabled = set_account_enabled(&state, "cli-custom", true)
            .expect("pending Custom may enable; verification is optional");
        assert!(enabled.enabled);
        let verification = state
            .db
            .lock()
            .account_verification_state("cli-custom")
            .unwrap()
            .unwrap();
        assert_eq!(verification.status, ConnectionVerificationStatus::Pending);
        assert_eq!(state.settings_revision(), before + 4);

        let zen = set_account_enabled(&state, ZEN_FREE_ACCOUNT_ID, false).unwrap_err();
        assert!(
            matches!(zen, AccountControlError::Invalid(message) if message == ZEN_FREE_MUTATION_MESSAGE)
        );

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn provider_enable_validation_accepts_dynamic_snapshot_and_rejects_unknown() {
        let (state, dir) = temp_state("dyn-enable");
        let unknown = state.ensure_provider_can_enable("no-such-provider");
        assert!(matches!(
            unknown,
            Err(crate::provider::ProviderBindingError::UnknownProvider { .. })
        ));

        let now = Utc::now();
        let provider_id = uuid::Uuid::new_v4().to_string();
        let runtime = crate::dynamic::DynamicProviderRuntime {
            preset_id: None,
            id: provider_id.clone(),
            name: "Lab".into(),
            endpoint_url: "http://127.0.0.1:9".into(),
            upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
            auth_kind: ocg_domain::dynamic::DynamicAuthKind::None,
            mappings: vec![ocg_domain::dynamic::DynamicModelMapping {
                upstream_override: None,
                public_model: "lab-opus".into(),
                upstream_model: "vendor/opus".into(),
            }],
            created_at: now,
            updated_at: now,
            origin: ocg_domain::provider::ProviderOrigin::Custom,
            offering: "api".to_string(),
        };
        let mut account = custom_pending(&state, "dyn-none");
        account.provider_id = provider_id.clone();
        account.credential_kind = crate::provider::CredentialKind::None;
        account.key_cipher = String::new();
        account.enabled = true;
        state
            .db
            .lock()
            .create_dynamic_provider(&runtime, &account)
            .unwrap();
        state.reload_dynamic_providers().unwrap();
        state
            .ensure_provider_can_enable(&provider_id)
            .expect("current dynamic snapshot members may be enabled");

        let disabled = set_account_enabled(&state, "dyn-none", false).unwrap();
        assert!(!disabled.enabled);
        let enabled = set_account_enabled(&state, "dyn-none", true).unwrap();
        assert!(enabled.enabled);

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn verification_enablement_gate_reads_the_composed_card_descriptor() {
        // Custom keeps required verification for status tracking while its
        // card does not gate enablement. Command Code's public model catalog
        // is not Key verification, so its Plan and card are both ungated.
        let custom_plan = crate::provider::builtin_provider(CUSTOM_PROVIDER_ID).unwrap();
        let goat_plan =
            crate::provider::builtin_provider(crate::provider::COMMAND_CODE_PROVIDER_ID).unwrap();
        assert_eq!(
            custom_plan.verification_policy,
            crate::provider::VerificationPolicy::Required
        );
        assert_eq!(
            goat_plan.verification_policy,
            crate::provider::VerificationPolicy::NotRequired
        );
        let custom_card = crate::provider::ProviderRegistry::get(CUSTOM_PROVIDER_ID).unwrap();
        assert!(!custom_card.card_actions.enable_requires_verification);
        let goat_card =
            crate::provider::ProviderRegistry::get(crate::provider::COMMAND_CODE_PROVIDER_ID)
                .unwrap();
        assert!(!goat_card.card_actions.enable_requires_verification);
    }

    #[tokio::test]
    async fn delete_account_bumps_revision_and_rejects_zen() {
        let (state, dir) = temp_state("delete");
        let created =
            create_go_api_key(&state, "gone".into(), "sk-gone".into(), None, None).unwrap();
        let before = state.settings_revision();
        delete_account(&state, &created.id, None).await.unwrap();
        assert!(state.db.lock().get_account(&created.id).unwrap().is_none());
        assert_eq!(state.settings_revision(), before + 1);

        let zen = delete_account(&state, ZEN_FREE_ACCOUNT_ID, None)
            .await
            .unwrap_err();
        assert!(
            matches!(zen, AccountControlError::Invalid(message) if message == ZEN_FREE_DELETE_MESSAGE)
        );
        assert!(
            state
                .db
                .lock()
                .get_account(ZEN_FREE_ACCOUNT_ID)
                .unwrap()
                .is_some()
        );

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }
}
