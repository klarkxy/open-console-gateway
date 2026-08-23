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
use chrono::Utc;
use futures_util::StreamExt;
#[cfg(debug_assertions)]
use parking_lot::Mutex;
use std::time::Duration;

use crate::custom;
use crate::db::{
    ManagedKeyVerificationCas, ManagedKeyVerificationCommit, ManagedKeyVerificationRateLimit,
    ManagedKeyVerificationWrite,
};
use crate::http_client;
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
use crate::upstream_limit::{parse_reset, parse_usage_limit_window};

use super::types::{
    Account, AccountAcknowledgement, AccountCustomConfig, AccountManagedKeyVerify,
    AccountModelCapability, AccountMutation, MutationExpectation,
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
    let input = parse_mutation_json::<AccountManagedKeyVerify>(&body)?;
    let key = input.key.trim().to_string();
    if key.is_empty() {
        return Err(V3ApiError::invalid_request_at(&state, "key is required"));
    }
    if key.len() > MAX_KEY_CHARS {
        return Err(V3ApiError::invalid_request_at(&state, "key is too long"));
    }
    let key_cipher = state.encrypt_key(&key).map_err(V3ApiError::internal)?;

    let prepared = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        prepare_managed_key_verify(&state, &id, key, key_cipher)?
    };

    let outcome = execute_managed_key_verify(&prepared).await;
    commit_managed_key_verify(&state, &id, &input.expectation, &prepared, outcome)
}

struct PreparedVerify {
    account_name: String,
    account_cas: ManagedKeyVerificationCas,
    key: String,
    key_cipher: String,
    config: AppConfig,
    target_url: String,
    body: Vec<u8>,
}

enum VerifyOutcome {
    Success,
    RateLimited { body: String },
    AuthFailed { status: StatusCode, body: String },
    ClientFailed { status: StatusCode, body: String },
    UpstreamFailed { message: String },
}

fn prepare_managed_key_verify(
    state: &CoreState,
    id: &str,
    key: String,
    key_cipher: String,
) -> Result<PreparedVerify, V3ApiError> {
    let account = load_waiting_managed_account(state, id)?;
    ensure_plan_can_enable(state, &account)?;
    let (_protocol, path, body) = go_verification_request()?;
    let config = state.config();
    let base = verification_base_url(state.process_generation(), &config.upstream_base_url);
    validate_upstream_url(&base)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    Ok(PreparedVerify {
        account_name: account.name.clone(),
        account_cas: ManagedKeyVerificationCas::from_account(&account),
        key,
        key_cipher,
        config,
        target_url: join_upstream(&base, path),
        body,
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
    provider::ensure_offering_can_enable(&account.provider_id, &account.offering_id)
        .map_err(|error| map_enablement_error(state, error))
}

fn map_enablement_error(state: &CoreState, error: ProviderBindingError) -> V3ApiError {
    match error {
        ProviderBindingError::EnablementNotRoutable { .. } => {
            V3ApiError::conflict_at(state, error.to_string())
        }
        other => V3ApiError::invalid_request_at(state, other.to_string()),
    }
}

fn go_verification_request() -> Result<(UpstreamProtocolKind, &'static str, Vec<u8>), V3ApiError> {
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
    let path = preferred.upstream_path().ok_or_else(|| {
        V3ApiError::internal("default OpenCode Go verification model has no upstream path")
    })?;
    let body = custom::minimal_verification_body(protocol, canonical)
        .map_err(|error| V3ApiError::internal(error.message))?;
    Ok((protocol, path, body))
}

fn upstream_protocol_for_api(format: ApiFormat) -> Option<UpstreamProtocolKind> {
    match format {
        ApiFormat::ChatCompletions => Some(UpstreamProtocolKind::ChatCompletions),
        ApiFormat::Responses => Some(UpstreamProtocolKind::Responses),
        ApiFormat::Messages => Some(UpstreamProtocolKind::Messages),
        ApiFormat::Gemini => None,
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
        .bearer_auth(&prepared.key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
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
        VerifyOutcome::RateLimited { body }
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
) -> Result<Json<AccountMutation>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)?;
    let account = load_waiting_managed_account(state, id)?;
    ensure_plan_can_enable(state, &account)?;

    enum ResponseKind {
        Verified,
        InvalidRequest(String),
        OutboundFailed(String),
    }

    let (write, response_kind) = match outcome {
        VerifyOutcome::Success => (
            ManagedKeyVerificationWrite::Verified {
                rate_limit: None,
                account_name: prepared.account_name.clone(),
            },
            ResponseKind::Verified,
        ),
        VerifyOutcome::RateLimited { body } => {
            let cooldown = parse_reset(&body).unwrap_or_else(|| chrono::Duration::minutes(5));
            let sanitized =
                sanitize_upstream_error_value_with_known_secret(&body, &prepared.key).to_string();
            (
                ManagedKeyVerificationWrite::Verified {
                    rate_limit: Some(ManagedKeyVerificationRateLimit {
                        until: Utc::now() + cooldown,
                        error: sanitized,
                        window: parse_usage_limit_window(&body),
                    }),
                    account_name: prepared.account_name.clone(),
                },
                ResponseKind::Verified,
            )
        }
        VerifyOutcome::AuthFailed { status, body } => {
            let sanitized =
                sanitize_upstream_error_value_with_known_secret(&body, &prepared.key).to_string();
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
            )
        }
        VerifyOutcome::ClientFailed { status, body } => {
            let sanitized =
                sanitize_upstream_error_value_with_known_secret(&body, &prepared.key).to_string();
            (
                ManagedKeyVerificationWrite::Pending,
                ResponseKind::InvalidRequest(format!(
                    "Key verification failed: upstream returned {}: {}",
                    status,
                    short_body(&sanitized)
                )),
            )
        }
        VerifyOutcome::UpstreamFailed { message } => (
            ManagedKeyVerificationWrite::Pending,
            ResponseKind::OutboundFailed(message),
        ),
    };

    let committed = state
        .db
        .lock()
        .commit_managed_key_verification(id, &prepared.account_cas, &prepared.key_cipher, &write)
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
            Ok(Json(account_mutation_at(state, account, revision)?))
        }
        ResponseKind::InvalidRequest(message) => {
            Err(V3ApiError::invalid_request_at(state, message))
        }
        ResponseKind::OutboundFailed(message) => Err(V3ApiError::outbound_failed(state, message)),
    }
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
    let plan = provider::builtin_plan(&account.provider_id, &account.offering_id);
    Ok(Account {
        id: account.id.clone(),
        provider_id: account.provider_id.clone(),
        offering_id: account.offering_id.clone(),
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
        acknowledgements: contract
            .acknowledgements
            .into_iter()
            .map(acknowledgement_from_model)
            .collect(),
    })
}

fn custom_config_from_model(config: crate::models::AccountCustomConfig) -> AccountCustomConfig {
    AccountCustomConfig {
        account_id: config.account_id,
        base_url: config.base_url,
        upstream_protocol: config.upstream_protocol.into(),
        auth_scheme: config.auth_scheme.into(),
        created_at: config.created_at.to_rfc3339(),
        updated_at: config.updated_at.to_rfc3339(),
    }
}

fn capability_from_model(
    capability: crate::models::AccountModelCapability,
) -> AccountModelCapability {
    AccountModelCapability {
        account_id: capability.account_id,
        model_id: capability.model_id,
        protocol: capability.protocol.into(),
        verified_at: capability.verified_at.map(|value| value.to_rfc3339()),
        source: capability.source,
    }
}

fn acknowledgement_from_model(
    acknowledgement: crate::models::AccountAcknowledgement,
) -> AccountAcknowledgement {
    AccountAcknowledgement {
        account_id: acknowledgement.account_id,
        acknowledgement_id: acknowledgement.acknowledgement_id,
        version: acknowledgement.version,
        content_hash: acknowledgement.content_hash,
        accepted_at: acknowledgement.accepted_at.to_rfc3339(),
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

#[cfg(test)]
mod tests {
    #[test]
    fn production_source_gates_the_managed_key_verify_seam() {
        let production = include_str!("managed_key_verify.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes tests");
        assert!(production.contains("configured_builder(&prepared.config)"));
        assert!(production.contains("no_redirect_policy"));
        assert!(production.contains("minimal_verification_body"));
        assert!(production.contains("DEFAULT_ACCOUNT_TEST_MODEL"));
        assert!(production.contains("commit_managed_key_verification"));
        assert!(production.contains("ManagedKeyVerificationCas::from_account"));
        assert!(!production.contains("save_managed_key_for_verification"));
        assert!(!production.contains("complete_managed_setup_if_key_matches"));
        assert!(production.contains("routing.reset()"));
        assert!(production.contains("bump_settings_revision"));
        assert!(
            !production.contains("upstream_context"),
            "verify-key must not reuse the redirect-capable shared client"
        );
        assert!(production.contains("#[cfg(debug_assertions)]"));
        assert!(production.contains("#[cfg(not(debug_assertions))]"));
        for needle in [
            "ManagedKeyVerifyTargetGuard",
            "install_managed_key_verify_target_for_tests",
            "MANAGED_KEY_VERIFY_TARGET_OVERRIDES",
            "debug_managed_key_verify_target",
            "parse_loopback_http_url",
        ] {
            let idx = production
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} must exist behind debug_assertions"));
            let before = &production[..idx];
            let cfg_idx = before
                .rfind("#[cfg(debug_assertions)]")
                .unwrap_or_else(|| panic!("{needle} must be gated by debug_assertions"));
            assert!(
                !before[cfg_idx..].contains("#[cfg(not(debug_assertions))]"),
                "{needle} compiled into release"
            );
        }
        let release_idx = production
            .find("#[cfg(not(debug_assertions))]")
            .expect("release verification path");
        let release = &production[release_idx..];
        assert!(!release.contains("MANAGED_KEY_VERIFY_TARGET_OVERRIDES"));
        assert!(!release.contains("install_managed_key_verify_target_for_tests"));
        assert!(!release.contains("debug_managed_key_verify_target"));
    }
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
