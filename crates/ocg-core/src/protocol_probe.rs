//! One private CPA hop for management-authorized validation sends.
//!
//! Dashboard V3 owns the live entrypoint, CAS, and persistence. CPA owns
//! provider selection, translation, and retries. This module pins one applied
//! credential, version, model, and protocol, then posts once to the owned
//! child. It does not choose another account or protocol, and it does not
//! write hop attempt logs.

use crate::custom;
use crate::models::{Account, AppConfig};
use crate::provider::UpstreamProtocolKind;
use crate::provider_contracts::{self, ContractScope, PersistedModelProtocol};
use crate::state::CoreState;
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;
use uuid::Uuid;
use zeroize::Zeroize;

const HOP_BODY_LIMIT: usize = custom::MAX_CUSTOM_VERIFICATION_BODY_BYTES;

#[derive(Debug, Clone)]
pub(crate) struct ProtocolProbeOutcome {
    pub protocol: UpstreamProtocolKind,
    pub success: bool,
    pub skipped: bool,
    pub error: Option<String>,
    pub observation: Option<PersistedModelProtocol>,
}

#[derive(Debug)]
pub(crate) enum ProtocolProbeRunError {
    Evidence(String),
    Apply(String),
    /// The owned hop was not sent. Callers must not record a provider failure.
    NotSent(String),
}

pub(crate) struct ProtocolProbeContext<'a> {
    pub state: &'a CoreState,
    pub config: &'a AppConfig,
    pub accounts: &'a [Account],
    pub model_id: &'a str,
    pub now: DateTime<Utc>,
}

pub(crate) fn require_unique_probe_protocols(
    protocols: &[UpstreamProtocolKind],
) -> Result<(), String> {
    let mut seen = HashSet::new();
    for protocol in protocols {
        if !seen.insert(*protocol) {
            return Err("duplicate upstream protocol".to_string());
        }
    }
    Ok(())
}

/// Provider-wide Custom probes stay rejected. Account verification owns that send.
pub(crate) fn reject_provider_wide_custom_probe(provider_id: &str) -> Result<(), &'static str> {
    if provider_id == crate::provider::CUSTOM_PROVIDER_ID {
        Err("protocol probes for Custom API are account-owned")
    } else {
        Ok(())
    }
}

/// One eligible account. A provided id that is not in `eligible` is refused
/// rather than replaced. An omitted id uses the first eligible account and
/// does not fail over to the next.
pub(crate) fn select_single_probe_account<'a>(
    eligible: &'a [Account],
    requested_account_id: Option<&str>,
) -> Result<&'a Account, String> {
    let requested = requested_account_id
        .map(str::trim)
        .filter(|id| !id.is_empty());
    if let Some(requested) = requested {
        return eligible
            .iter()
            .find(|account| account.id == requested)
            .ok_or_else(|| {
                "selected account is not eligible for this provider protocol probe".to_string()
            });
    }
    eligible.first().ok_or_else(|| {
        "no eligible provider accounts are available for protocol probes".to_string()
    })
}

pub(crate) async fn run_protocol_probes<L>(
    ctx: &ProtocolProbeContext<'_>,
    scope: &ContractScope,
    protocols: &[UpstreamProtocolKind],
    mut load_existing: L,
) -> Result<Vec<ProtocolProbeOutcome>, ProtocolProbeRunError>
where
    L: FnMut(UpstreamProtocolKind) -> Result<Option<PersistedModelProtocol>, String>,
{
    if ctx.accounts.len() != 1 {
        return Err(ProtocolProbeRunError::NotSent(
            "protocol probe sends to one selected account".to_string(),
        ));
    }
    let account = &ctx.accounts[0];
    let mut results = Vec::with_capacity(protocols.len());
    for protocol in protocols {
        let existing = load_existing(*protocol).map_err(ProtocolProbeRunError::Evidence)?;
        let (success, error) = match execute_protocol_probe(ctx, account, *protocol).await {
            Ok(_) => (true, None),
            Err(ProtocolRequestError::NotSent(message)) => {
                return Err(ProtocolProbeRunError::NotSent(message));
            }
            Err(ProtocolRequestError::Provider { message, .. }) => (false, Some(message)),
        };
        let persisted = provider_contracts::apply_probe_observation(
            existing.as_ref(),
            scope.clone(),
            ctx.model_id,
            *protocol,
            success,
            error.clone(),
            ctx.now,
            true,
        )
        .map_err(ProtocolProbeRunError::Apply)?;
        results.push(ProtocolProbeOutcome {
            protocol: *protocol,
            success,
            skipped: false,
            error,
            observation: Some(persisted),
        });
    }
    Ok(results)
}

pub(crate) async fn execute_protocol_probe(
    ctx: &ProtocolProbeContext<'_>,
    account: &Account,
    protocol: UpstreamProtocolKind,
) -> Result<u16, ProtocolRequestError> {
    execute_protocol_request(
        ctx,
        account,
        protocol,
        false,
        ctx.model_id,
        VerificationOverrides::default(),
    )
    .await
}

/// Send the minimal public protocol body for one already selected account.
/// The provider URL prepared by the caller is not the hop target.
/// Supplied message and max_tokens replace only those fields.
pub(crate) struct AccountModelTestInput<'a> {
    pub state: &'a CoreState,
    pub config: &'a AppConfig,
    pub account: &'a Account,
    pub public_model: &'a str,
    pub protocol: UpstreamProtocolKind,
    pub message: Option<&'a str>,
    pub max_tokens: Option<u32>,
}

/// Fields that replace the minimal verification body. Absent keeps that body.
#[derive(Clone, Debug, Default)]
pub(crate) struct VerificationOverrides {
    pub message: Option<String>,
    pub max_tokens: Option<u32>,
}

pub(crate) async fn execute_account_model_test(
    input: AccountModelTestInput<'_>,
) -> Result<u16, ProtocolRequestError> {
    let ctx = ProtocolProbeContext {
        state: input.state,
        config: input.config,
        accounts: std::slice::from_ref(input.account),
        model_id: input.public_model,
        now: Utc::now(),
    };
    execute_protocol_request(
        &ctx,
        input.account,
        input.protocol,
        true,
        input.public_model,
        VerificationOverrides {
            message: input.message.map(str::to_string),
            max_tokens: input.max_tokens,
        },
    )
    .await
}

#[derive(Debug)]
pub(crate) enum ProtocolRequestError {
    NotSent(String),
    Provider {
        status: Option<u16>,
        message: String,
    },
}

#[derive(Clone)]
pub(crate) struct PersistedCredential {
    pub credential_id: String,
    pub credential_version: u64,
    pub binding_id: String,
    pub binding_enabled: bool,
}

pub(crate) fn load_persisted_credential(
    state: &CoreState,
    account_id: &str,
) -> Result<PersistedCredential, String> {
    let snapshot = state
        .db
        .lock()
        .list_identity_model()
        .map_err(|error| error.to_string())?;
    let record = snapshot
        .accounts
        .into_iter()
        .find(|row| row.account.id == account_id)
        .ok_or_else(|| "selected account has no persisted credential".to_string())?;
    if record.credential_id.trim().is_empty()
        || record.credential_version == 0
        || record.binding_id.trim().is_empty()
    {
        return Err("selected account has no complete credential version or binding".to_string());
    }
    Ok(PersistedCredential {
        credential_id: record.credential_id,
        credential_version: record.credential_version,
        binding_id: record.binding_id,
        binding_enabled: record.binding_enabled,
    })
}

pub(crate) struct ValidatedGeneration {
    pub credential_id: String,
    pub credential_version: u64,
    pub binding_id: String,
    pub public_model: String,
    pub protocol: UpstreamProtocolKind,
}

#[derive(Debug)]
pub(crate) struct ValidatedHttpResult {
    pub status: u16,
    pub body: Vec<u8>,
    pub retry_after: Option<String>,
}

#[derive(Debug)]
pub(crate) enum ValidatedSendError {
    NotSent(String),
}

/// Owned-child target for one validated post. The hop secret is not part of `Debug`.
pub(crate) struct ValidatedHop {
    pub base_url: String,
    pub hop_secret: String,
    pub child_generation: u64,
    pub applied_revision: u64,
    pub auth_id: String,
    pub credential_id: String,
    pub credential_version: u64,
    pub binding_id: String,
    pub material_revision: String,
    pub public_model: String,
    pub protocol: String,
}

impl Drop for ValidatedHop {
    fn drop(&mut self) {
        self.hop_secret.zeroize();
    }
}

impl std::fmt::Debug for ValidatedHop {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ValidatedHop")
            .field("base_url", &self.base_url)
            .field("hop_secret", &"[redacted]")
            .field("child_generation", &self.child_generation)
            .field("applied_revision", &self.applied_revision)
            .field("auth_id", &self.auth_id)
            .field("credential_id", &self.credential_id)
            .field("credential_version", &self.credential_version)
            .field("binding_id", &self.binding_id)
            .field("material_revision", &self.material_revision)
            .field("public_model", &self.public_model)
            .field("protocol", &self.protocol)
            .finish()
    }
}

pub(crate) async fn send_validated_generation(
    state: &CoreState,
    request: &ValidatedGeneration,
    timeout: Duration,
) -> Result<ValidatedHttpResult, ValidatedSendError> {
    send_validated_generation_with(state, request, timeout, VerificationOverrides::default()).await
}

async fn send_validated_generation_with(
    state: &CoreState,
    request: &ValidatedGeneration,
    timeout: Duration,
    payload: VerificationOverrides,
) -> Result<ValidatedHttpResult, ValidatedSendError> {
    if request.binding_id.trim().is_empty() || request.credential_version == 0 {
        return Err(ValidatedSendError::NotSent(
            "selected credential binding or version is incomplete".to_string(),
        ));
    }
    let pin = crate::cpa_execution::validated_inference_pin(
        state,
        &request.credential_id,
        request.credential_version,
        &request.public_model,
        request.protocol.as_str(),
    )
    .map_err(|error| ValidatedSendError::NotSent(error.to_string()))?;
    if pin.credential_id != request.credential_id
        || pin.credential_version != request.credential_version
        || pin.binding_id != request.binding_id
        || pin.public_model != request.public_model
        || pin.protocol != request.protocol.as_str()
        || pin.auth_id.trim().is_empty()
        || pin.material_revision.trim().is_empty()
    {
        return Err(ValidatedSendError::NotSent(
            "validated pin does not match the selected credential".to_string(),
        ));
    }
    let hop = ValidatedHop {
        base_url: pin.connection.base_url,
        hop_secret: pin.connection.hop.expose().to_string(),
        child_generation: pin.connection.child_generation,
        applied_revision: pin.connection.applied_revision,
        auth_id: pin.auth_id,
        credential_id: pin.credential_id,
        credential_version: pin.credential_version,
        binding_id: pin.binding_id,
        material_revision: pin.material_revision,
        public_model: pin.public_model,
        protocol: pin.protocol,
    };
    post_validated_hop(state, &hop, timeout, payload).await
}

pub(crate) async fn post_validated_hop(
    state: &CoreState,
    hop: &ValidatedHop,
    timeout: Duration,
    payload: VerificationOverrides,
) -> Result<ValidatedHttpResult, ValidatedSendError> {
    if timeout.is_zero() {
        return Err(ValidatedSendError::NotSent(
            "validated hop deadline already elapsed".to_string(),
        ));
    }
    let path = owned_protocol_path(&hop.protocol).ok_or_else(|| {
        ValidatedSendError::NotSent(
            "validated protocol pin is not a private bridge protocol".into(),
        )
    })?;
    let url = hop_url(&hop.base_url, path)?;
    let protocol = UpstreamProtocolKind::try_from(hop.protocol.as_str()).map_err(|_| {
        ValidatedSendError::NotSent(
            "validated protocol pin is not a private bridge protocol".into(),
        )
    })?;
    let body = validated_send_body(protocol, &hop.public_model, &payload)?;
    let span = chrono::Duration::from_std(timeout).map_err(|_| {
        ValidatedSendError::NotSent("validated hop deadline already elapsed".to_string())
    })?;
    let sampled = Utc::now();
    let mono = tokio::time::Instant::now() + timeout;
    let deadline = sampled.checked_add_signed(span).ok_or_else(|| {
        ValidatedSendError::NotSent("validated hop deadline already elapsed".to_string())
    })?;
    if Utc::now() >= deadline {
        return Err(ValidatedSendError::NotSent(
            "validated hop deadline already elapsed".to_string(),
        ));
    }
    let request_id = Uuid::new_v4();
    crate::cpa_execution::register_correlation_intent(
        state,
        crate::cpa_execution::CorrelationIntent {
            request_id,
            deadline,
            requested_model: hop.public_model.clone(),
            authorization: crate::cpa_execution::CorrelationAuthorization::Validated {
                credential_id: hop.credential_id.clone(),
                credential_version: hop.credential_version,
                requested_protocol: hop.protocol.clone(),
            },
        },
    )
    .map_err(|error| ValidatedSendError::NotSent(error.to_string()))?;
    #[cfg(test)]
    note_registered_deadline(request_id, deadline);
    let mut guard = CorrelationGuard {
        state: state.clone(),
        request_id,
        disarmed: false,
    };
    let mut headers = validated_headers(hop, request_id).map_err(|_| {
        ValidatedSendError::NotSent("validated hop headers were rejected".to_string())
    })?;
    // Insert after validated headers so a caller deadline cannot extend the capture.
    seal_captured_deadline(&mut headers, deadline).map_err(|_| {
        ValidatedSendError::NotSent("validated hop headers were rejected".to_string())
    })?;
    let remaining = mono
        .checked_duration_since(tokio::time::Instant::now())
        .unwrap_or_default();
    if remaining.is_zero() {
        return Err(ValidatedSendError::NotSent(
            "validated hop deadline already elapsed".to_string(),
        ));
    }
    let pending = validated_hop_client()
        .post(url)
        .timeout(remaining)
        .headers(headers)
        .body(body)
        .send();
    let sent = tokio::select! {
        biased;
        _ = tokio::time::sleep_until(mono) => None,
        result = pending => Some(result),
    };
    let response = match sent {
        Some(Ok(response)) => response,
        Some(Err(error)) => {
            return Err(ValidatedSendError::NotSent(redact_hop(
                &format!("validated hop failed before a provider response: {error}"),
                &hop.hop_secret,
            )));
        }
        None => {
            return Err(ValidatedSendError::NotSent(
                "validated hop deadline already elapsed".to_string(),
            ));
        }
    };
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = match crate::custom_http::HttpInferenceTransport::read_body_limited(
        response,
        HOP_BODY_LIMIT,
    )
    .await
    {
        Ok(body) => body,
        Err(error) => {
            return Err(ValidatedSendError::NotSent(redact_hop(
                &format!("validated hop response was not readable: {error}"),
                &hop.hop_secret,
            )));
        }
    };
    guard.disarm();
    Ok(ValidatedHttpResult {
        status,
        body,
        retry_after,
    })
}

pub(crate) fn protocol_shaped_success(
    status: u16,
    body: &[u8],
    protocol: UpstreamProtocolKind,
) -> Result<(), String> {
    let code = reqwest::StatusCode::from_u16(status).unwrap_or(reqwest::StatusCode::BAD_GATEWAY);
    custom::prove_verified_protocol_response(code, body, protocol)
        .map_err(|failure| failure.message)
}

fn execute_protocol_request(
    ctx: &ProtocolProbeContext<'_>,
    account: &Account,
    protocol: UpstreamProtocolKind,
    account_test: bool,
    public_model: &str,
    payload: VerificationOverrides,
) -> impl std::future::Future<Output = Result<u16, ProtocolRequestError>> + Send {
    let state = ctx.state.clone();
    let account_id = account.id.clone();
    let public_model = public_model.to_string();
    let timeout = Duration::from_secs(ctx.config.non_stream_timeout_secs.clamp(5, 30));
    async move {
        let persisted = load_persisted_credential(&state, &account_id)
            .map_err(ProtocolRequestError::NotSent)?;
        if !persisted.binding_enabled {
            return Err(ProtocolRequestError::NotSent(
                "credential binding is disabled".to_string(),
            ));
        }
        let result = send_validated_generation_with(
            &state,
            &ValidatedGeneration {
                credential_id: persisted.credential_id,
                credential_version: persisted.credential_version,
                binding_id: persisted.binding_id,
                public_model,
                protocol,
            },
            timeout,
            payload,
        )
        .await
        .map_err(|ValidatedSendError::NotSent(message)| ProtocolRequestError::NotSent(message))?;
        classify_provider_response(result.status, &result.body, protocol, account_test)
    }
}

fn classify_provider_response(
    status: u16,
    body: &[u8],
    protocol: UpstreamProtocolKind,
    account_test: bool,
) -> Result<u16, ProtocolRequestError> {
    let code = reqwest::StatusCode::from_u16(status).unwrap_or(reqwest::StatusCode::BAD_GATEWAY);
    if !code.is_success() {
        let message = if account_test {
            format!("upstream returned HTTP {status}")
        } else {
            provider_contracts::sanitize_probe_error(&format!("upstream returned {status}"), None)
        };
        return Err(ProtocolRequestError::Provider {
            status: Some(status),
            message,
        });
    }
    if let Ok(parsed) = serde_json::from_slice::<serde_json::Value>(body)
        && non_null_probe_error(&parsed).is_some()
    {
        return Err(ProtocolRequestError::Provider {
            status: Some(status),
            message: if account_test {
                "upstream returned a protocol error".to_string()
            } else {
                provider_contracts::sanitize_probe_error(
                    "protocol probe returned an error object",
                    None,
                )
            },
        });
    }
    protocol_shaped_success(status, body, protocol).map_err(|message| {
        ProtocolRequestError::Provider {
            status: Some(status),
            message,
        }
    })?;
    Ok(status)
}

fn non_null_probe_error(value: &serde_json::Value) -> Option<&serde_json::Value> {
    value.get("error").filter(|error| !error.is_null())
}

fn owned_protocol_path(protocol: &str) -> Option<&'static str> {
    match protocol {
        "chat_completions" => Some("/v1/chat/completions"),
        "responses" => Some("/v1/responses"),
        "messages" => Some("/v1/messages"),
        _ => None,
    }
}

fn hop_url(base: &str, path: &str) -> Result<reqwest::Url, ValidatedSendError> {
    let base = base.trim().trim_end_matches('/');
    let url = reqwest::Url::parse(&format!("{base}{path}")).map_err(|_| {
        ValidatedSendError::NotSent("validated hop origin is not a loopback URL".to_string())
    })?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !hop_host_is_loopback(&url)
    {
        return Err(ValidatedSendError::NotSent(
            "validated hop origin is not a loopback URL".to_string(),
        ));
    }
    Ok(url)
}

fn hop_host_is_loopback(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    )
}

fn validated_send_body(
    protocol: UpstreamProtocolKind,
    model: &str,
    payload: &VerificationOverrides,
) -> Result<Vec<u8>, ValidatedSendError> {
    if payload.max_tokens == Some(0) {
        return Err(ValidatedSendError::NotSent(
            "maxTokens must be positive".to_string(),
        ));
    }
    let minimal = custom::minimal_verification_body(protocol, model)
        .map_err(|error| ValidatedSendError::NotSent(error.message))?;
    if payload.message.is_none() && payload.max_tokens.is_none() {
        return Ok(minimal);
    }
    let mut body: serde_json::Value = serde_json::from_slice(&minimal)
        .map_err(|_| ValidatedSendError::NotSent("validated hop body was rejected".to_string()))?;
    if let Some(message) = payload.message.as_deref() {
        let slot = match protocol {
            UpstreamProtocolKind::ChatCompletions | UpstreamProtocolKind::Messages => {
                body.pointer_mut("/messages/0/content")
            }
            UpstreamProtocolKind::Responses => body.get_mut("input"),
        };
        let Some(slot) = slot else {
            return Err(ValidatedSendError::NotSent(
                "validated hop body was rejected".to_string(),
            ));
        };
        *slot = serde_json::Value::String(message.to_string());
    }
    if let Some(max_tokens) = payload.max_tokens {
        let key = match protocol {
            UpstreamProtocolKind::ChatCompletions | UpstreamProtocolKind::Messages => "max_tokens",
            UpstreamProtocolKind::Responses => "max_output_tokens",
        };
        let Some(slot) = body.get_mut(key) else {
            return Err(ValidatedSendError::NotSent(
                "validated hop body was rejected".to_string(),
            ));
        };
        *slot = serde_json::json!(max_tokens);
    }
    serde_json::to_vec(&body)
        .map_err(|_| ValidatedSendError::NotSent("validated hop body was rejected".to_string()))
}

/// UTC RFC3339 with nanoseconds. Replaces any deadline already on the map.
fn seal_captured_deadline(
    headers: &mut reqwest::header::HeaderMap,
    deadline: DateTime<Utc>,
) -> Result<(), ()> {
    let value = deadline.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    insert_header(headers, "x-ocg-request-deadline", &value)
}

#[cfg(test)]
fn note_registered_deadline(request_id: Uuid, deadline: DateTime<Utc>) {
    registered_deadlines()
        .lock()
        .expect("registered deadlines")
        .insert(request_id, deadline);
}

#[cfg(test)]
fn registered_deadline(request_id: Uuid) -> Option<DateTime<Utc>> {
    registered_deadlines()
        .lock()
        .expect("registered deadlines")
        .get(&request_id)
        .copied()
}

#[cfg(test)]
fn registered_deadlines()
-> &'static std::sync::Mutex<std::collections::HashMap<Uuid, DateTime<Utc>>> {
    static REGISTERED: OnceLock<std::sync::Mutex<std::collections::HashMap<Uuid, DateTime<Utc>>>> =
        OnceLock::new();
    REGISTERED.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn validated_headers(
    hop: &ValidatedHop,
    request_id: Uuid,
) -> Result<reqwest::header::HeaderMap, ()> {
    let mut headers = reqwest::header::HeaderMap::new();
    let mut bearer = format!("Bearer {}", hop.hop_secret);
    let authorization = match reqwest::header::HeaderValue::from_str(&bearer) {
        Ok(value) => value,
        Err(_) => {
            bearer.zeroize();
            return Err(());
        }
    };
    bearer.zeroize();
    headers.insert(reqwest::header::AUTHORIZATION, authorization);
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    insert_header(&mut headers, "x-ocg-request-kind", "validated")?;
    insert_header(&mut headers, "x-ocg-pinned-auth-id", &hop.auth_id)?;
    insert_header(&mut headers, "x-ocg-validated-protocol", &hop.protocol)?;
    insert_header(&mut headers, "x-ocg-request-id", &request_id.to_string())?;
    insert_header(
        &mut headers,
        "x-ocg-process-generation",
        &hop.child_generation.to_string(),
    )?;
    insert_header(
        &mut headers,
        "x-ocg-projection-revision",
        &hop.applied_revision.to_string(),
    )?;
    Ok(headers)
}

fn insert_header(
    headers: &mut reqwest::header::HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), ()> {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || !byte.is_ascii())
    {
        return Err(());
    }
    headers.insert(
        reqwest::header::HeaderName::from_static(name),
        reqwest::header::HeaderValue::from_str(value).map_err(|_| ())?,
    );
    Ok(())
}

fn validated_hop_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(crate::http_client::no_redirect_policy())
            .no_proxy()
            .build()
            .expect("loopback validated CPA hop client")
    })
}

fn redact_hop(text: &str, hop_secret: &str) -> String {
    let redacted = crate::redaction::redact_text(text);
    if hop_secret.is_empty() {
        redacted
    } else {
        crate::redaction::redact_known_secret(&redacted, hop_secret)
    }
}

struct CorrelationGuard {
    state: CoreState,
    request_id: Uuid,
    disarmed: bool,
}

impl CorrelationGuard {
    fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for CorrelationGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        crate::cpa_execution::cancel_correlation(&self.state, self.request_id);
    }
}

#[cfg(test)]
mod tests;
