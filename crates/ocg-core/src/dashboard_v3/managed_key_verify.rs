//! POST `/accounts/{id}/setup/verify-key` — managed onboarding Key verification.
//!
//! Preserves V2 eligibility, Go protocol-correct non-stream ping, proxy /
//! no-redirect / auth-isolation / timeout / body-bound behavior, and the
//! ready+enabled vs pending transitions. Locks are not held across the
//! network: CAS and the account contract are captured, then rechecked before
//! any persist so a stale in-flight request has no DB/session/runtime effect.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
#[cfg(debug_assertions)]
use parking_lot::Mutex;
use std::time::Duration;

use crate::db::{
    ManagedKeyVerificationCas, ManagedKeyVerificationCommit, ManagedKeyVerificationRateLimit,
    ManagedKeyVerificationWrite,
};
use crate::gateway::failure::decode::temporary_429_until;
use crate::http_client;
use crate::models::{
    Account as ModelAccount, AccountSetupStep as ModelSetupStep, AccountType as ModelAccountType,
    AppConfig,
};
use crate::provider::{self, ProviderBindingError};
use crate::provider_contracts::{EffectiveScopeContract, PersistedContracts};
use crate::redaction::{
    redact_known_secret, redact_text, sanitize_upstream_error_value_with_known_secret,
};
use crate::state::CoreState;
use crate::verification_models::select_verification_model;

use super::types::{
    Account, AccountCustomConfig, AccountManagedKeyVerify, AccountModelCapability, AccountMutation,
    MutationExpectation,
};
use super::{V3ApiError, check_expectation, parse_mutation_json};

const MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES: usize = 64 * 1024;
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

/// Bind a loopback verification base URL to one `CoreState` process generation.
///
/// Compiled out of release production. Non-loopback, credentialed, query, or
/// fragment URLs are rejected and do not install an override.
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

fn verification_base_url(process_generation: u64, configured: &str) -> String {
    #[cfg(debug_assertions)]
    if let Some(url) = debug_managed_key_verify_target(process_generation) {
        return url;
    }
    #[cfg(not(debug_assertions))]
    let _ = process_generation;
    configured.to_string()
}

pub(super) async fn verify_managed_account_key(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let mut op =
        super::settings::open_dashboard(&state, "account.key.verify", "account", Some(id.clone()));
    let input = match parse_mutation_json::<AccountManagedKeyVerify>(&body) {
        Ok(input) => input,
        Err(error) => {
            return super::settings::record_after(
                op,
                &state,
                &[],
                (None, None, None),
                None,
                Err(error),
            );
        }
    };
    let key = input.key.trim().to_string();
    if key.is_empty() {
        return super::settings::record_after(
            op,
            &state,
            &[],
            (None, None, None),
            None,
            Err(V3ApiError::invalid_request_at(&state, "key is required")),
        );
    }
    if key.len() > MAX_KEY_CHARS {
        return super::settings::record_after(
            op,
            &state,
            &[],
            (None, None, None),
            None,
            Err(V3ApiError::invalid_request_at(&state, "key is too long")),
        );
    }
    let key_cipher = match state.encrypt_key(&key) {
        Ok(cipher) => cipher,
        Err(error) => {
            return super::settings::record_after(
                op,
                &state,
                &[],
                (None, None, None),
                None,
                Err(V3ApiError::internal(error)),
            );
        }
    };

    let mut prepared = {
        let _settings_update = state.settings_update.lock();
        match check_expectation(&state, &input.expectation)
            .and_then(|()| prepare_managed_key_verify(&state, &id, key, key_cipher))
        {
            Ok(prepared) => prepared,
            Err(error) => {
                drop(_settings_update);
                return super::settings::record_after(
                    op,
                    &state,
                    &[],
                    (None, None, None),
                    None,
                    Err(error),
                );
            }
        }
    };

    if let Err(error) = resolve_verification_request(&state, &mut prepared)
        .await
        .and_then(|()| recheck_prepared(&state, &id, &input.expectation, &prepared))
    {
        return super::settings::record_after(
            op,
            &state,
            &[],
            (None, None, None),
            None,
            Err(error),
        );
    }

    op.accepted(super::settings::metadata_for(
        &state,
        &[],
        Some(1),
        Some(0),
        None,
        None,
    ));
    let outcome = execute_managed_key_verify(&prepared).await;
    let verified = matches!(
        outcome,
        VerifyOutcome::Success | VerifyOutcome::RateLimited { .. }
    );
    let mut effect = super::settings::CommittedEffect::Atomic;
    let result = commit_managed_key_verify(
        &state,
        &id,
        &input.expectation,
        &prepared,
        outcome,
        &mut effect,
    );
    let ok_outcome = (!verified).then_some((
        crate::log_types::OperationOutcome::Failed,
        "verificationFailed",
    ));
    super::settings::record_effect(
        op,
        &state,
        &[],
        (Some(1), verified.then_some(1), (!verified).then_some(1)),
        ok_outcome,
        effect,
        result,
    )
}

struct PreparedVerify {
    account_name: String,
    account_cas: ManagedKeyVerificationCas,
    existing_generic_cooldown_until: Option<DateTime<Utc>>,
    key: String,
    key_cipher: String,
    config: AppConfig,
    target_url: String,
    body: Vec<u8>,
    send_headers: reqwest::header::HeaderMap,
    saved_scope: EffectiveScopeContract,
    persisted: PersistedContracts,
    projection: crate::destination_projection::DestinationProjection,
}

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
    let config = state.config();
    let base = verification_base_url(
        state.process_generation(),
        &crate::gateway::free_models::opencode_go_base_url(&config.upstream_base_url),
    );
    validate_upstream_url(&base)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    let saved_scope = state
        .provider_contracts()
        .provider_offering(provider::OPENCODE_PROVIDER_ID)
        .cloned()
        .ok_or_else(|| V3ApiError::internal("OpenCode Go contract is unavailable"))?;
    let (persisted, projection) = {
        let db = state.db.lock();
        (
            db.load_persisted_contracts()
                .map_err(V3ApiError::internal)?,
            crate::destination_projection::load_runtime(&db).map_err(V3ApiError::internal)?,
        )
    };
    Ok(PreparedVerify {
        account_name: account.name.clone(),
        account_cas: ManagedKeyVerificationCas::from_account(&account),
        existing_generic_cooldown_until: account.cooldown_generic_until,
        key,
        key_cipher,
        config,
        target_url: base,
        body: Vec::new(),
        send_headers: reqwest::header::HeaderMap::new(),
        saved_scope,
        persisted,
        projection,
    })
}

/// Discovery is public and transient: verification never changes model controls.
async fn resolve_verification_request(
    state: &CoreState,
    prepared: &mut PreparedVerify,
) -> Result<(), V3ApiError> {
    let scope = if prepared.saved_scope.catalog.models.is_empty() {
        let discovery = crate::goat::refresh_opencode_go_catalog_discovery(&prepared.config, &prepared.target_url)
            .await.map_err(|_| V3ApiError::outbound_failed(state, "OpenCode Go directory discovery failed; refresh its model directory and retry Key verification"))?;
        let docs = if matches!(
            discovery.protocol_baseline,
            crate::goat::OfficialProtocolBaseline::Unavailable
        ) {
            crate::official_protocols::fetch_official_protocol_baseline(
                &prepared.config,
                provider::OPENCODE_PROVIDER_ID,
                state.process_generation(),
            )
            .await
        } else {
            crate::goat::OfficialProtocolBaseline::Unavailable
        };
        let baseline = discovery.protocol_baseline.prefer_catalog(docs);
        crate::verification_models::discovered_go_contract(
            discovery.models,
            &baseline,
            &prepared.saved_scope,
            &prepared.persisted,
            &prepared.projection,
            &prepared.target_url,
        )
        .map_err(|message| V3ApiError::outbound_failed(state, message))?
    } else {
        prepared.saved_scope.clone()
    };
    let selected = select_verification_model(&scope, None)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    prepared.body = selected.body("ping", 1).map_err(V3ApiError::internal)?;
    prepared.send_headers = selected
        .go_headers(&prepared.key)
        .map_err(V3ApiError::internal)?;
    prepared.target_url = join_upstream(&prepared.target_url, selected.path());
    Ok(())
}

fn validate_prepared(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    prepared: &PreparedVerify,
) -> Result<(), V3ApiError> {
    check_expectation(state, expectation)?;
    let current = load_waiting_managed_account(state, id)?;
    ensure_plan_can_enable(state, &current)?;
    ensure_managed_registration(state, &current)?;
    if ManagedKeyVerificationCas::from_account(&current) != prepared.account_cas {
        return Err(key_changed_conflict(state));
    }
    let config_changed = serde_json::to_value(state.config()).map_err(V3ApiError::internal)?
        != serde_json::to_value(&prepared.config).map_err(V3ApiError::internal)?;
    if config_changed
        || state
            .provider_contracts()
            .provider_offering(provider::OPENCODE_PROVIDER_ID)
            != Some(&prepared.saved_scope)
    {
        return Err(V3ApiError::conflict_at(
            state,
            "configuration changed while preparing Key verification; retry verification",
        ));
    }
    Ok(())
}

fn recheck_prepared(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    prepared: &PreparedVerify,
) -> Result<(), V3ApiError> {
    let _settings_update = state.settings_update.lock();
    validate_prepared(state, id, expectation, prepared)
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

fn join_upstream(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim_end_matches('/'))
}

fn validate_upstream_url(url: &str) -> Result<(), String> {
    let parsed =
        reqwest::Url::parse(url).map_err(|error| format!("invalid upstream URL: {error}"))?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(&parsed) => Ok(()),
        _ => Err("upstream must use https, except loopback http".to_string()),
    }
}

fn is_loopback(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    )
}

async fn execute_managed_key_verify(prepared: &PreparedVerify) -> VerifyOutcome {
    let client = match http_client::configured_builder(&prepared.config).and_then(|builder| {
        builder
            .connect_timeout(Duration::from_secs(prepared.config.connect_timeout_secs))
            .redirect(http_client::no_redirect_policy())
            .build()
            .map_err(Into::into)
    }) {
        Ok(client) => client,
        Err(error) => {
            return VerifyOutcome::UpstreamFailed {
                message: redact_verify_detail(
                    &format!(
                        "key verification request failed; the account remains pending: {error}"
                    ),
                    &prepared.key,
                    &prepared.config,
                ),
            };
        }
    };

    let response = match client
        .post(&prepared.target_url)
        .headers(prepared.send_headers.clone())
        .body(prepared.body.clone())
        .timeout(Duration::from_secs(prepared.config.non_stream_timeout_secs))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return VerifyOutcome::UpstreamFailed {
                message: network_error_message(&error, &prepared.key, &prepared.config, true),
            };
        }
    };

    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = match read_managed_key_verification_response(response).await {
        Ok(body) => body,
        Err(error) => {
            return VerifyOutcome::UpstreamFailed {
                message: network_error_message(&error, &prepared.key, &prepared.config, false),
            };
        }
    };

    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        VerifyOutcome::AuthFailed { status, body }
    } else if status.is_server_error() {
        VerifyOutcome::UpstreamFailed {
            message: format!(
                "key verification upstream returned {status}; the account remains pending"
            ),
        }
    } else if status == StatusCode::TOO_MANY_REQUESTS {
        VerifyOutcome::RateLimited { body, retry_after }
    } else if status.is_success() {
        VerifyOutcome::Success
    } else {
        VerifyOutcome::ClientFailed { status, body }
    }
}

fn network_error_message(
    error: &reqwest::Error,
    key: &str,
    config: &AppConfig,
    request_phase: bool,
) -> String {
    if error.is_timeout() {
        if request_phase {
            "key verification timed out; the account remains pending".to_string()
        } else {
            "key verification response timed out; the account remains pending".to_string()
        }
    } else if request_phase {
        redact_verify_detail(
            &format!(
                "key verification request failed; the account remains pending: {}",
                format_error_chain(error)
            ),
            key,
            config,
        )
    } else {
        redact_verify_detail(
            &format!(
                "failed to read key verification response: {}",
                format_error_chain(error)
            ),
            key,
            config,
        )
    }
}

async fn read_managed_key_verification_response(
    response: reqwest::Response,
) -> Result<String, reqwest::Error> {
    let read_limit = MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES.saturating_add(1);
    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .map_or(read_limit, |length| length.min(read_limit));
    let mut body = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = read_limit.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if body.len() == read_limit {
            break;
        }
    }

    let truncated = body.len() > MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES;
    body.truncate(MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES);
    let mut text = String::from_utf8_lossy(&body).into_owned();
    if truncated {
        text.push_str("\n<key verification response truncated>");
    }
    Ok(text)
}

fn commit_managed_key_verify(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    prepared: &PreparedVerify,
    outcome: VerifyOutcome,
    effect: &mut super::settings::CommittedEffect,
) -> Result<Json<AccountMutation>, V3ApiError> {
    enum ResponseKind {
        Verified,
        InvalidRequest(String),
        OutboundFailed(String),
    }

    let (result, refresh_usage) = {
        let _settings_update = state.settings_update.lock();
        validate_prepared(state, id, expectation, prepared)?;
        let account = load_waiting_managed_account(state, id)?;
        ensure_plan_can_enable(state, &account)?;

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
                // A 429 without a declared window must only update the generic
                // slot. Keep a longer generic wait captured with the account;
                // named quota-window slots remain untouched by the DB commit.
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
                let account = effect.note_follow_up(load_model_account(state, id))?;
                (
                    effect.note_follow_up(account_mutation_at(state, account, revision).map(Json)),
                    rate_limited,
                )
            }
            ResponseKind::InvalidRequest(message) => {
                *effect = super::settings::CommittedEffect::Failed {
                    reason: "verificationFailed",
                };
                (Err(V3ApiError::invalid_request_at(state, message)), false)
            }
            ResponseKind::OutboundFailed(message) => {
                *effect = super::settings::CommittedEffect::Failed {
                    reason: "verificationFailed",
                };
                (Err(V3ApiError::outbound_failed(state, message)), false)
            }
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
    let ((usage_sync_last_success_at, usage_sync_next_allowed_at), contract, goat_plan) = {
        let db = state.db.lock();
        let sync = db
            .account_usage_sync_state(&account.id)
            .map_err(V3ApiError::internal)?;
        let contract = db
            .load_account_contract(&account.id)
            .map_err(V3ApiError::internal)?;
        let goat_plan = crate::goat_plan_cooldowns::load_for_legacy_on(&db.conn, &account.id)
            .map_err(V3ApiError::internal)?;
        (
            crate::usage_sync::dashboard_sync_fields(sync.as_ref(), state.usage_sync.now()),
            contract,
            goat_plan,
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
    let (cooldown_until, cooldown_5h_until, cooldown_week_until, cooldown_month_until) =
        crate::goat_plan_cooldowns::overlay_account_deadlines(
            account.cooldown_until,
            account.cooldown_5h_until,
            account.cooldown_week_until,
            account.cooldown_month_until,
            goat_plan.as_ref(),
        );
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
        cooldown_until,
        cooldown_generic_until: account.cooldown_generic_until.map(|t| t.to_rfc3339()),
        cooldown_5h_until,
        cooldown_week_until,
        cooldown_month_until,
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

fn format_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
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
mod tests;
