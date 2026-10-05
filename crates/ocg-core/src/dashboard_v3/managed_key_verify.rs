//! POST `/accounts/{id}/setup/verify-key` — managed onboarding Key verification.
//!
//! A changed candidate advances credential identity and the product settings
//! revision in the stage write. Completion accepts that post-stage receipt and
//! still conflicts when a later writer moves revision, generation, ciphertext,
//! update time, credential id, version, binding, or setup. Locks are released
//! before the owned apply await and before the validated pin. Apply or pin
//! failure performs no provider send and leaves the encrypted candidate pending.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
#[cfg(debug_assertions)]
use parking_lot::Mutex;
use std::time::Duration;

use crate::db::{
    ManagedKeyVerificationCas, ManagedKeyVerificationCommit, ManagedKeyVerificationRateLimit,
    ManagedKeyVerificationWrite,
};
use crate::gateway::failure::decode::temporary_429_until;
use crate::kernel::protocol::{ApiFormat, supported_model_protocol_profiles};
use crate::models::{
    Account as ModelAccount, AccountSetupStep as ModelSetupStep, AccountType as ModelAccountType,
    AppConfig, DEFAULT_ACCOUNT_TEST_MODEL,
};
use crate::provider::{self, ProviderBindingError, UpstreamProtocolKind};
use crate::redaction::{
    redact_known_secret, redact_text, sanitize_upstream_error_value_with_known_secret,
};
use crate::state::CoreState;

use super::types::{
    Account, AccountCustomConfig, AccountManagedKeyVerify, AccountModelCapability, AccountMutation,
};
use super::{V3ApiError, check_expectation, parse_mutation_json};

const MAX_KEY_CHARS: usize = 4096;

#[cfg(debug_assertions)]
static MANAGED_KEY_VERIFY_TARGET_OVERRIDES: Mutex<std::collections::BTreeMap<u64, String>> =
    Mutex::new(std::collections::BTreeMap::new());

/// Test-only guard that restores the production upstream base when dropped.
#[cfg(debug_assertions)]
pub struct ManagedKeyVerifyTargetGuard {
    process_generation: u64,
}

#[cfg(debug_assertions)]
impl Drop for ManagedKeyVerifyTargetGuard {
    fn drop(&mut self) {
        MANAGED_KEY_VERIFY_TARGET_OVERRIDES
            .lock()
            .remove(&self.process_generation);
    }
}

/// Retain the installer so existing debug builds still link.
///
/// The production send no longer uses this URL. Verification goes to the owned
/// CPA hop. Non-loopback, credentialed, query, or fragment URLs are rejected
/// and do not install an override.
#[cfg(debug_assertions)]
#[must_use]
pub fn install_managed_key_verify_target_for_tests(
    process_generation: u64,
    url: impl Into<String>,
) -> ManagedKeyVerifyTargetGuard {
    let mut overrides = MANAGED_KEY_VERIFY_TARGET_OVERRIDES.lock();
    match parse_loopback_http_url(&url.into()) {
        Some(canonical) => {
            overrides.insert(process_generation, canonical);
        }
        None => {
            overrides.remove(&process_generation);
        }
    }
    ManagedKeyVerifyTargetGuard { process_generation }
}

#[cfg(debug_assertions)]
#[cfg_attr(not(test), allow(dead_code))]
fn debug_managed_key_verify_target(process_generation: u64) -> Option<String> {
    MANAGED_KEY_VERIFY_TARGET_OVERRIDES
        .lock()
        .get(&process_generation)
        .cloned()
}

/// Accept only an unambiguous loopback HTTP(S) origin: parsed host must be
/// exactly `127.0.0.1`, `localhost`, or `::1`, with no userinfo, query, or
/// fragment.
#[cfg(debug_assertions)]
fn parse_loopback_http_url(url: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return None;
    }
    if !host_is_exact_loopback(&parsed) {
        return None;
    }
    Some(parsed.as_str().to_string())
}

#[cfg(debug_assertions)]
fn host_is_exact_loopback(parsed: &reqwest::Url) -> bool {
    use std::net::{Ipv4Addr, Ipv6Addr};

    let Some(host) = parsed.host() else {
        return false;
    };
    let rendered = host.to_string();
    if let Some(inside) = rendered
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        return inside
            .parse::<Ipv6Addr>()
            .is_ok_and(|ip| ip == Ipv6Addr::LOCALHOST);
    }
    if let Ok(ip) = rendered.parse::<Ipv4Addr>() {
        return ip == Ipv4Addr::LOCALHOST;
    }
    rendered.eq_ignore_ascii_case("localhost")
}

pub(super) async fn verify_managed_account_key(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let input = parse_mutation_json::<AccountManagedKeyVerify>(&body)?;
    let key = input.key.trim().to_string();
    if key.is_empty() {
        return Err(V3ApiError::invalid_request_at(&state, "key is required"));
    }
    if key.len() > MAX_KEY_CHARS {
        return Err(V3ApiError::invalid_request_at(&state, "key is too long"));
    }
    let key_cipher = state.encrypt_key(&key).map_err(V3ApiError::internal)?;

    let staged = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        let prepared = prepare_managed_key_verify(&state, &id, key, key_cipher)?;
        stage_managed_candidate(&state, &id, prepared)?
    };

    let outcome = match await_owned_projection(&state, &staged).await {
        Ok(()) => execute_managed_key_verify(&state, &staged).await,
        Err(message) => VerifyOutcome::UpstreamFailed { message },
    };
    let revision_before_completion = state.settings_revision();
    let result = commit_managed_key_verify(&state, &id, &staged, outcome);
    if state.settings_revision() != revision_before_completion {
        #[cfg(test)]
        assert_product_locks_released(&state);
        // The completion row is already committed. A later apply failure stays
        // in the runtime log and does not replace this result.
        crate::cpa_execution::note_product_apply(&state).await;
    }
    result
}

#[cfg(test)]
fn assert_product_locks_released(state: &CoreState) {
    assert!(
        state.settings_update.try_lock().is_some(),
        "settings revision lock held across await"
    );
    assert!(
        state.db.try_lock().is_some(),
        "database lock held across await"
    );
}

async fn await_owned_projection(
    state: &CoreState,
    staged: &StagedManagedKey,
) -> Result<(), String> {
    #[cfg(test)]
    assert_product_locks_released(state);
    match crate::cpa_execution::schedule_owned_apply(state).await {
        Ok(_) => Ok(()),
        Err(error) => Err(redact_verify_detail(
            &format!("owned apply is unavailable: {error}"),
            &staged.key,
            &staged.config,
        )),
    }
}

struct PreparedVerify {
    account_name: String,
    pre_stage_cas: ManagedKeyVerificationCas,
    existing_generic_cooldown_until: Option<DateTime<Utc>>,
    key: String,
    key_cipher: String,
    config: AppConfig,
    protocol: UpstreamProtocolKind,
    public_model: String,
}

impl std::fmt::Debug for PreparedVerify {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedVerify")
            .field("account_name", &self.account_name)
            .field("public_model", &self.public_model)
            .field("protocol", &self.protocol)
            .field("key", &"[redacted]")
            .field("key_cipher", &"[redacted]")
            .finish_non_exhaustive()
    }
}

struct StagedManagedKey {
    account_name: String,
    account_cas: ManagedKeyVerificationCas,
    existing_generic_cooldown_until: Option<DateTime<Utc>>,
    key: String,
    key_cipher: String,
    config: AppConfig,
    protocol: UpstreamProtocolKind,
    public_model: String,
    credential_id: String,
    credential_version: u64,
    binding_id: String,
    /// Settings revision after the stage bump. Completion accepts this
    /// self-change and conflicts if another writer advanced it.
    product_revision: u64,
    process_generation: u64,
}

#[derive(Debug)]
enum VerifyOutcome {
    Success,
    RateLimited {
        body: String,
        retry_after: Option<String>,
    },
    AuthFailed {
        status: StatusCode,
        body: String,
    },
    ClientFailed {
        status: StatusCode,
        body: String,
    },
    UpstreamFailed {
        message: String,
    },
}

fn prepare_managed_key_verify(
    state: &CoreState,
    id: &str,
    key: String,
    key_cipher: String,
) -> Result<PreparedVerify, V3ApiError> {
    let account = load_waiting_managed_account(state, id)?;
    ensure_managed_registration(state, &account)?;
    ensure_plan_can_enable(state, &account)?;
    let persisted = crate::protocol_probe::load_persisted_credential(state, id)
        .map_err(|message| V3ApiError::conflict_at(state, message))?;
    if !persisted.binding_enabled {
        return Err(V3ApiError::conflict_at(
            state,
            "credential binding is disabled",
        ));
    }
    let (protocol, public_model) = managed_verification_target()?;
    Ok(PreparedVerify {
        account_name: account.name.clone(),
        pre_stage_cas: ManagedKeyVerificationCas::from_account(&account),
        existing_generic_cooldown_until: account.cooldown_generic_until,
        key,
        key_cipher,
        config: state.config(),
        protocol,
        public_model,
    })
}

fn stage_managed_candidate(
    state: &CoreState,
    id: &str,
    prepared: PreparedVerify,
) -> Result<StagedManagedKey, V3ApiError> {
    let committed = state
        .db
        .lock()
        .commit_managed_key_verification(
            id,
            &prepared.pre_stage_cas,
            &prepared.key_cipher,
            &ManagedKeyVerificationWrite::Pending,
        )
        .map_err(|error| map_complete_error(state, error))?;
    if committed == ManagedKeyVerificationCommit::Conflict {
        return Err(key_changed_conflict(state));
    }
    let account = load_waiting_managed_account(state, id)?;
    if account.key_cipher != prepared.key_cipher {
        return Err(key_changed_conflict(state));
    }
    let persisted = crate::protocol_probe::load_persisted_credential(state, id)
        .map_err(|message| V3ApiError::conflict_at(state, message))?;
    if !persisted.binding_enabled {
        return Err(V3ApiError::conflict_at(
            state,
            "credential binding is disabled",
        ));
    }
    let product_revision = state.bump_settings_revision();
    Ok(StagedManagedKey {
        account_name: prepared.account_name,
        account_cas: ManagedKeyVerificationCas::from_account(&account),
        existing_generic_cooldown_until: prepared.existing_generic_cooldown_until,
        key: prepared.key,
        key_cipher: prepared.key_cipher,
        config: prepared.config,
        protocol: prepared.protocol,
        public_model: prepared.public_model,
        credential_id: persisted.credential_id,
        credential_version: persisted.credential_version,
        binding_id: persisted.binding_id,
        product_revision,
        process_generation: state.process_generation(),
    })
}

fn load_waiting_managed_account(state: &CoreState, id: &str) -> Result<ModelAccount, V3ApiError> {
    let account = state
        .db
        .lock()
        .get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found(state))?;
    require_waiting_managed(state, &account)?;
    Ok(account)
}

fn require_waiting_managed(state: &CoreState, account: &ModelAccount) -> Result<(), V3ApiError> {
    if account.account_type != ModelAccountType::Managed
        || account.setup_step != ModelSetupStep::KeyVerification
    {
        return Err(V3ApiError::conflict_at(
            state,
            "managed account is not waiting for key verification",
        ));
    }
    Ok(())
}

fn ensure_plan_can_enable(state: &CoreState, account: &ModelAccount) -> Result<(), V3ApiError> {
    provider::ensure_provider_can_enable(&account.provider_id)
        .map_err(|error| map_enablement_error(state, error))
}

fn ensure_managed_registration(
    state: &CoreState,
    account: &ModelAccount,
) -> Result<(), V3ApiError> {
    let is_managed = provider::builtin_provider(&account.provider_id)
        .is_some_and(|plan| plan.managed_registration);
    if !is_managed {
        return Err(V3ApiError::conflict_at(
            state,
            "managed key verification is only available for managed-registration offerings",
        ));
    }
    Ok(())
}

fn map_enablement_error(state: &CoreState, error: ProviderBindingError) -> V3ApiError {
    match error {
        ProviderBindingError::EnablementNotRoutable { .. } => {
            V3ApiError::conflict_at(state, error.to_string())
        }
        other => V3ApiError::invalid_request_at(state, other.to_string()),
    }
}

fn managed_verification_target() -> Result<(UpstreamProtocolKind, String), V3ApiError> {
    let Some((canonical, preferred, _)) = supported_model_protocol_profiles()
        .find(|(model_id, _, _)| *model_id == DEFAULT_ACCOUNT_TEST_MODEL)
    else {
        return Err(V3ApiError::internal(
            "default OpenCode Go verification model is missing from the protocol catalog",
        ));
    };
    let protocol = upstream_protocol_for_api(preferred).ok_or_else(|| {
        V3ApiError::internal("default OpenCode Go verification model has no upstream protocol")
    })?;
    Ok((protocol, canonical.to_string()))
}

fn upstream_protocol_for_api(format: ApiFormat) -> Option<UpstreamProtocolKind> {
    match format {
        ApiFormat::ChatCompletions => Some(UpstreamProtocolKind::ChatCompletions),
        ApiFormat::Responses => Some(UpstreamProtocolKind::Responses),
        ApiFormat::Messages => Some(UpstreamProtocolKind::Messages),
        ApiFormat::Gemini => None,
    }
}

async fn execute_managed_key_verify(state: &CoreState, staged: &StagedManagedKey) -> VerifyOutcome {
    #[cfg(test)]
    assert_product_locks_released(state);
    let timeout = Duration::from_secs(staged.config.non_stream_timeout_secs);
    let response = match crate::protocol_probe::send_validated_generation(
        state,
        &crate::protocol_probe::ValidatedGeneration {
            credential_id: staged.credential_id.clone(),
            credential_version: staged.credential_version,
            binding_id: staged.binding_id.clone(),
            public_model: staged.public_model.clone(),
            protocol: staged.protocol,
        },
        timeout,
    )
    .await
    {
        Ok(response) => response,
        Err(crate::protocol_probe::ValidatedSendError::NotSent(message)) => {
            return VerifyOutcome::UpstreamFailed {
                message: redact_verify_detail(&message, &staged.key, &staged.config),
            };
        }
    };
    classify_managed_response(staged, response)
}

fn classify_managed_response(
    staged: &StagedManagedKey,
    response: crate::protocol_probe::ValidatedHttpResult,
) -> VerifyOutcome {
    let Ok(status) = StatusCode::from_u16(response.status) else {
        return VerifyOutcome::UpstreamFailed {
            message: format!(
                "key verification upstream returned {}; the account remains pending",
                response.status
            ),
        };
    };
    let body = String::from_utf8_lossy(&response.body).into_owned();
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        VerifyOutcome::AuthFailed { status, body }
    } else if status.is_server_error() {
        VerifyOutcome::UpstreamFailed {
            message: format!(
                "key verification upstream returned {status}; the account remains pending"
            ),
        }
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        VerifyOutcome::RateLimited {
            body,
            retry_after: response.retry_after,
        }
    } else if status.is_success() {
        match crate::protocol_probe::protocol_shaped_success(
            response.status,
            &response.body,
            staged.protocol,
        ) {
            Ok(()) => VerifyOutcome::Success,
            Err(message) => VerifyOutcome::ClientFailed {
                status,
                body: message,
            },
        }
    } else {
        VerifyOutcome::ClientFailed { status, body }
    }
}

fn saved_pending(detail: impl Into<String>) -> String {
    format!("saved pending candidate; {}", detail.into())
}

fn commit_managed_key_verify(
    state: &CoreState,
    id: &str,
    prepared: &StagedManagedKey,
    outcome: VerifyOutcome,
) -> Result<Json<AccountMutation>, V3ApiError> {
    enum ResponseKind {
        Verified,
        InvalidRequest(String),
        OutboundFailed(String),
    }

    let (result, refresh_usage) = {
        let _settings_update = state.settings_update.lock();
        if state.settings_revision() != prepared.product_revision
            || state.process_generation() != prepared.process_generation
        {
            return Err(V3ApiError::revision_conflict(state));
        }
        let account = load_waiting_managed_account(state, id)?;
        ensure_plan_can_enable(state, &account)?;
        let persisted = crate::protocol_probe::load_persisted_credential(state, id)
            .map_err(|message| V3ApiError::conflict_at(state, message))?;
        if persisted.credential_id != prepared.credential_id
            || persisted.credential_version != prepared.credential_version
            || persisted.binding_id != prepared.binding_id
            || !persisted.binding_enabled
        {
            return Err(key_changed_conflict(state));
        }

        let (write, response_kind, rate_limited) = match outcome {
            VerifyOutcome::Success => (
                ManagedKeyVerificationWrite::Verified {
                    rate_limit: None,
                    account_name: prepared.account_name.clone(),
                },
                ResponseKind::Verified,
                false,
            ),
            VerifyOutcome::RateLimited { body, retry_after } => {
                let sanitized =
                    sanitize_upstream_error_value_with_known_secret(&body, &prepared.key)
                        .to_string();
                let retry_until = temporary_429_until(retry_after.as_deref(), Utc::now());
                // HTTP 429 is the existing temporary cooldown, not a trusted
                // plan parser. Named quota windows and quota-recovery JSON stay
                // empty so a generic 429 cannot become durable Plan authority.
                // Keep a longer generic wait captured with the account.
                let until = prepared
                    .existing_generic_cooldown_until
                    .filter(|existing| existing > &retry_until)
                    .unwrap_or(retry_until);
                (
                    ManagedKeyVerificationWrite::Verified {
                        rate_limit: Some(ManagedKeyVerificationRateLimit {
                            until,
                            error: sanitized,
                            window: None,
                        }),
                        account_name: prepared.account_name.clone(),
                    },
                    ResponseKind::Verified,
                    true,
                )
            }
            VerifyOutcome::AuthFailed { status, body } => {
                let sanitized =
                    sanitize_upstream_error_value_with_known_secret(&body, &prepared.key)
                        .to_string();
                let auth_error = format!(
                    "upstream auth error {}: {}",
                    status.as_u16(),
                    short_body(&sanitized)
                );
                (
                    ManagedKeyVerificationWrite::AuthFailed {
                        auth_error: auth_error.clone(),
                    },
                    ResponseKind::InvalidRequest(format!("Key verification failed: {auth_error}")),
                    false,
                )
            }
            VerifyOutcome::ClientFailed { status, body } => {
                let sanitized =
                    sanitize_upstream_error_value_with_known_secret(&body, &prepared.key)
                        .to_string();
                (
                    ManagedKeyVerificationWrite::Pending,
                    ResponseKind::InvalidRequest(format!(
                        "Key verification failed: upstream returned {}: {}",
                        status,
                        short_body(&sanitized)
                    )),
                    false,
                )
            }
            VerifyOutcome::UpstreamFailed { message } => (
                ManagedKeyVerificationWrite::Pending,
                ResponseKind::OutboundFailed(message),
                false,
            ),
        };

        let committed = state
            .db
            .lock()
            .commit_managed_key_verification(
                id,
                &prepared.account_cas,
                &prepared.key_cipher,
                &write,
            )
            .map_err(|error| map_complete_error(state, error))?;
        if committed == ManagedKeyVerificationCommit::Conflict {
            return Err(key_changed_conflict(state));
        }

        if matches!(&response_kind, ResponseKind::Verified) {
            state.routing.reset();
        }
        let revision = state.bump_settings_revision();
        match response_kind {
            ResponseKind::Verified => {
                let account = load_model_account(state, id)?;
                (
                    Ok(Json(account_mutation_at(state, account, revision)?)),
                    rate_limited,
                )
            }
            ResponseKind::InvalidRequest(message) => (
                Err(V3ApiError::invalid_request_at(
                    state,
                    saved_pending(message),
                )),
                false,
            ),
            ResponseKind::OutboundFailed(message) => (
                Err(V3ApiError::outbound_failed(state, saved_pending(message))),
                false,
            ),
        }
    };
    if refresh_usage {
        crate::usage_sync::spawn_reactive_usage_refresh(state, id);
    }
    result
}

fn key_changed_conflict(state: &CoreState) -> V3ApiError {
    V3ApiError::conflict_at(
        state,
        "the key changed while it was being verified; retry verification",
    )
}

fn map_complete_error(state: &CoreState, error: anyhow::Error) -> V3ApiError {
    if let Some(binding) = error.downcast_ref::<ProviderBindingError>() {
        return map_enablement_error(state, binding.clone());
    }
    let message = error.to_string();
    if message.contains("not routable") {
        V3ApiError::conflict_at(state, message)
    } else {
        V3ApiError::internal(error)
    }
}

fn load_model_account(state: &CoreState, id: &str) -> Result<ModelAccount, V3ApiError> {
    state
        .db
        .lock()
        .get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found(state))
}

fn account_mutation_at(
    state: &CoreState,
    account: ModelAccount,
    revision: u64,
) -> Result<AccountMutation, V3ApiError> {
    let mut account = account_from_state(state, account)?;
    account.revision = revision;
    Ok(AccountMutation {
        account: Some(account),
        revision,
        process_generation: state.process_generation(),
    })
}

fn account_from_state(state: &CoreState, account: ModelAccount) -> Result<Account, V3ApiError> {
    let ((usage_sync_last_success_at, usage_sync_next_allowed_at), contract) = {
        let db = state.db.lock();
        let sync = db
            .account_usage_sync_state(&account.id)
            .map_err(V3ApiError::internal)?;
        let contract = db
            .load_account_contract(&account.id)
            .map_err(V3ApiError::internal)?;
        (
            crate::usage_sync::dashboard_sync_fields(sync.as_ref(), state.usage_sync.now()),
            contract,
        )
    };
    let known_secret = if account.last_error.is_some()
        || account.auth_error.is_some()
        || contract.verification.verification_error.is_some()
    {
        if account.key_cipher.is_empty() {
            Some(String::new())
        } else {
            state.decrypt_key(&account.key_cipher).ok()
        }
    } else {
        None
    };
    let sanitize_persisted_error = |error: Option<String>| {
        error.and_then(|error| {
            known_secret
                .as_deref()
                .map(|secret| redact_known_secret(&error, secret))
        })
    };
    let plan = provider::builtin_provider(&account.provider_id);
    Ok(Account {
        id: account.id.clone(),
        provider_id: account.provider_id.clone(),

        credential_kind: account.credential_kind.into(),
        quota_scope: account.quota_scope.into(),
        name: account.name,
        username: account.username,
        enabled: account.enabled,
        account_type: account.account_type.into(),
        setup_step: account.setup_step.into(),
        purchase_date: account.purchase_date,
        expires_on: account.expires_on,
        cooldown_until: account.cooldown_until.map(|t| t.to_rfc3339()),
        cooldown_generic_until: account.cooldown_generic_until.map(|t| t.to_rfc3339()),
        cooldown_5h_until: account.cooldown_5h_until.map(|t| t.to_rfc3339()),
        cooldown_week_until: account.cooldown_week_until.map(|t| t.to_rfc3339()),
        cooldown_month_until: account.cooldown_month_until.map(|t| t.to_rfc3339()),
        cooldown_free_until: account.cooldown_free_until.map(|t| t.to_rfc3339()),
        last_error: sanitize_persisted_error(account.last_error),
        auth_error: sanitize_persisted_error(account.auth_error),
        notes: account.notes,
        usage_sync_last_success_at,
        usage_sync_next_allowed_at,
        created_at: account.created_at.to_rfc3339(),
        updated_at: account.updated_at.to_rfc3339(),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        verification_status: contract.verification.status.into(),
        connection_verified_at: contract
            .verification
            .connection_verified_at
            .map(|value| value.to_rfc3339()),
        verification_error: sanitize_persisted_error(contract.verification.verification_error),
        plan_routable: plan.is_some_and(|plan| plan.routable),
        custom_config: contract.custom_config.map(custom_config_from_model),
        model_capabilities: contract
            .model_capabilities
            .into_iter()
            .map(capability_from_model)
            .collect(),
        ollama_billing_tier: None,
    })
}

fn custom_config_from_model(config: crate::models::AccountCustomConfig) -> AccountCustomConfig {
    AccountCustomConfig {
        account_id: config.account_id,
        endpoint_url: config.endpoint_url,
        upstream_protocol: config.upstream_protocol.into(),
        created_at: config.created_at.to_rfc3339(),
        updated_at: config.updated_at.to_rfc3339(),
    }
}

fn capability_from_model(
    capability: crate::models::AccountModelCapability,
) -> AccountModelCapability {
    AccountModelCapability {
        public_model: capability.public_model,
        upstream_model: capability.upstream_model,
        protocol: capability.protocol.into(),
        verified_at: capability.verified_at.map(|value| value.to_rfc3339()),
        source: capability.source,
    }
}

fn short_body(body: &str) -> String {
    body.split_whitespace()
        .take(40)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect()
}

fn redact_verify_detail(text: &str, key: &str, config: &AppConfig) -> String {
    let mut redacted = redact_text(text);
    redacted = redact_known_secret(&redacted, key);
    if !config.gateway_key.is_empty() {
        redacted = redact_known_secret(&redacted, &config.gateway_key);
    }
    if !config.proxy_url.is_empty() {
        redacted = redact_known_secret(&redacted, &config.proxy_url);
    }
    redacted
}

#[cfg(all(test, debug_assertions))]
mod target_override_tests {
    use super::{
        debug_managed_key_verify_target, install_managed_key_verify_target_for_tests,
        parse_loopback_http_url,
    };

    fn unique_generation() -> u64 {
        uuid::Uuid::new_v4().as_u128() as u64
    }

    #[test]
    fn parse_loopback_http_url_requires_exact_host_without_userinfo_query_or_fragment() {
        assert_eq!(
            parse_loopback_http_url("http://127.0.0.1:9/").as_deref(),
            Some("http://127.0.0.1:9/")
        );
        assert_eq!(
            parse_loopback_http_url("http://localhost:9/").as_deref(),
            Some("http://localhost:9/")
        );
        assert_eq!(
            parse_loopback_http_url("http://[::1]:9/").as_deref(),
            Some("http://[::1]:9/")
        );
        assert!(parse_loopback_http_url("http://127.0.0.1:9/?x=1").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.1:9/#frag").is_none());
        assert!(parse_loopback_http_url("http://user@127.0.0.1:9/").is_none());
        assert!(parse_loopback_http_url("https://opencode.ai/zen/go").is_none());
        assert!(parse_loopback_http_url("http://127.0.0.2:9/").is_none());
    }

    #[test]
    fn overrides_are_isolated_by_process_generation_and_reject_ambiguous_urls() {
        let first = unique_generation();
        let second = unique_generation();
        let _guard_a = install_managed_key_verify_target_for_tests(first, "http://127.0.0.1:11/");
        let _guard_b = install_managed_key_verify_target_for_tests(second, "http://127.0.0.1:12/");
        assert_eq!(
            debug_managed_key_verify_target(first).as_deref(),
            Some("http://127.0.0.1:11/")
        );
        assert_eq!(
            debug_managed_key_verify_target(second).as_deref(),
            Some("http://127.0.0.1:12/")
        );

        drop(_guard_a);
        let _cleared =
            install_managed_key_verify_target_for_tests(first, "http://127.0.0.1:11@example.com/");
        assert!(debug_managed_key_verify_target(first).is_none());
        assert_eq!(
            debug_managed_key_verify_target(second).as_deref(),
            Some("http://127.0.0.1:12/")
        );
    }
}

#[cfg(test)]
mod tests;
