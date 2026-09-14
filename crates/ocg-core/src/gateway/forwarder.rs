use crate::custom_http::{build_custom_http_client, json_content_headers};
use crate::db::{Database, ForwardLogDiagnosticUpdate};
use crate::gateway::attempt::{
    AttemptSpec, AttemptTimeouts, AttemptTransportError, CredentialHandle, CredentialResolveError,
    CredentialResolver, ProxyRoutingModel, TransportFailureKind, TransportSendFailure,
    UpstreamAuth,
};
use crate::gateway::classify::{
    PreflightKind, ProviderErrorClass, RateLimitFallback, StreamClassifyInput,
    TransportClassifyInput, classify_http, classify_preflight, classify_stream, classify_transport,
    rate_limit_fallback, rate_limit_window_and_cooldown, schedule_go_usage_sync,
};
use crate::gateway::diagnostics::{
    ErrorDiagnostic, RequestTrace, api_format_name, emit_failure, emit_legacy_tool_compat,
    redact_known_secret, redact_known_secret_values, safe_upstream_headers,
    sanitize_upstream_error_value_with_known_secret, serialize_diagnostic,
};
use crate::gateway::materialize::native_log_identity;
use crate::gateway::protocol::{
    RequestPlan, UsageCounts, error_body, extract_usage, format_error, has_complete_usage,
    has_usage, merge_stream_usage, transform_response,
};
use crate::gateway::protocol_stream::StreamConverter;
use crate::gateway::provider_adapter;
use crate::gateway::routing::resolve_conversation_key;
use crate::http_client::RouteLabel;
use crate::kernel::pricing::PricingSnapshot;
use crate::kernel::protocol::ApiFormat;
use crate::models::{
    Account, AppConfig, ForwardLog, ForwardLogNativeAttribution, ForwardMetrics, UsageWindowKind,
};
use crate::platform::{PlatformAccount, PlatformLink};
use crate::pricing::{
    ProviderPricingEvidence, ProviderScopedPricingSnapshot, latest_provider_pricing_snapshot,
};
use crate::state::CoreState;
use anyhow::Result;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::BytesMut;
use chrono::Utc;
use futures_util::StreamExt;
use parking_lot::Mutex;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

const MAX_UPSTREAM_ERROR_BODY_BYTES: usize = 64 * 1024;

mod live_send;

pub(crate) use live_send::{
    LiveSendAccountGate, LiveSendAuthError, LiveSendSelection, authorize_live_send_secret,
    confirm_live_send_secret,
};

/// Host secret resolution for the account the outer loop selected. Live
/// account/binding/grant checks and decrypt share one DB read lock. Same-account
/// retry reuses the original captured selection.
pub(crate) struct HostCredentialResolver<'a> {
    state: &'a CoreState,
    account: &'a Account,
    selection: &'a LiveSendSelection,
    plan: &'a RequestPlan,
    spec: &'a AttemptSpec,
}

impl<'a> HostCredentialResolver<'a> {
    pub(crate) fn new(
        state: &'a CoreState,
        account: &'a Account,
        selection: &'a LiveSendSelection,
        plan: &'a RequestPlan,
        spec: &'a AttemptSpec,
    ) -> Self {
        Self {
            state,
            account,
            selection,
            plan,
            spec,
        }
    }

    fn resolve_live(&self, handle: &CredentialHandle) -> Result<Option<String>, LiveSendAuthError> {
        match handle {
            CredentialHandle::None => Ok(None),
            CredentialHandle::Account { id } => {
                if id != &self.account.id {
                    return Err(LiveSendAuthError::Unauthorized(
                        "refusing to send credentials: selected credential is no longer authorized for this attempt"
                            .into(),
                    ));
                }
                authorize_live_send_secret(
                    self.state,
                    self.selection,
                    self.account,
                    self.plan,
                    self.spec,
                    LiveSendAccountGate::RequireEnabled,
                )
            }
        }
    }

    fn confirm_live(&self) -> Result<(), LiveSendAuthError> {
        confirm_live_send_secret(
            self.state,
            self.selection,
            self.account,
            self.plan,
            self.spec,
            LiveSendAccountGate::RequireEnabled,
        )
    }
}

impl CredentialResolver for HostCredentialResolver<'_> {
    fn resolve_credential(
        &self,
        handle: &CredentialHandle,
    ) -> Result<Option<String>, CredentialResolveError> {
        self.resolve_live(handle).map_err(|error| match error {
            LiveSendAuthError::Decrypt(inner) => CredentialResolveError::Decrypt(inner),
            LiveSendAuthError::Unauthorized(message) => {
                CredentialResolveError::Decrypt(anyhow::anyhow!(message))
            }
        })
    }
}

/// One insert per attempt and same-row finalize for streaming. The outer
/// fallback loop still decides retry; this sink only persists the row.
#[allow(clippy::too_many_arguments)]
trait AttemptSink {
    #[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        account: &Account,
        model: &str,
        status: &str,
        http_status: Option<i32>,
        metrics: ForwardMetrics,
        error_message: Option<&str>,
        context: &ForwardAttemptContext,
        failure: Option<FailureRecord>,
    ) -> Result<i64>;

    #[allow(clippy::too_many_arguments)]
    fn finalize(
        &self,
        id: i64,
        status: &str,
        http_status: Option<i32>,
        metrics: ForwardMetrics,
        error_message: Option<&str>,
        diagnostic: Option<&ForwardLogDiagnosticUpdate<'_>>,
        context: &ForwardAttemptContext,
    ) -> Result<()>;
}

struct DbAttemptSink<'a> {
    db: &'a Database,
}

impl<'a> DbAttemptSink<'a> {
    fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

#[allow(clippy::too_many_arguments)]
impl AttemptSink for DbAttemptSink<'_> {
    fn insert(
        &self,
        account: &Account,
        model: &str,
        status: &str,
        http_status: Option<i32>,
        metrics: ForwardMetrics,
        error_message: Option<&str>,
        context: &ForwardAttemptContext,
        failure: Option<FailureRecord>,
    ) -> Result<i64> {
        log_forward(
            self.db,
            account,
            model,
            status,
            http_status,
            metrics,
            error_message,
            context,
            failure,
        )
    }

    fn finalize(
        &self,
        id: i64,
        status: &str,
        http_status: Option<i32>,
        metrics: ForwardMetrics,
        error_message: Option<&str>,
        diagnostic: Option<&ForwardLogDiagnosticUpdate<'_>>,
        context: &ForwardAttemptContext,
    ) -> Result<()> {
        finalize_logged_forward(
            self.db,
            id,
            status,
            http_status,
            metrics,
            error_message,
            diagnostic,
            context,
        )
    }
}

struct ForwardOnceOutput {
    started: Instant,
    result: std::result::Result<reqwest::Response, AttemptTransportError>,
}

/// Exactly one upstream POST. Owns transport selection and timeouts only.
#[allow(clippy::too_many_arguments)]
async fn forward_once(
    spec: &AttemptSpec,
    snapshot_client: &Client,
    route: RouteLabel,
    config: &AppConfig,
    timeouts: AttemptTimeouts,
    url: &str,
    headers: reqwest::header::HeaderMap,
    body: bytes::Bytes,
    stream: bool,
) -> Result<ForwardOnceOutput> {
    let secret_bearing = headers_carry_upstream_secret(&headers);
    let mut request = match spec.proxy_routing {
        ProxyRoutingModel::IsolatedTrustedAdmin => {
            let client = build_custom_http_client(config)?;
            let url = reqwest::Url::parse(url)?;
            crate::custom_http::inspect_custom_url(&url)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            client.request(reqwest::Method::POST, url)
        }
        ProxyRoutingModel::ProcessWideNoRedirect => {
            let client = crate::http_client::build_no_redirect_for_route(config, route)?;
            client.post(url)
        }
        ProxyRoutingModel::LocalExternalIntegration => {
            let client = crate::http_client::build_no_redirect_for_route(
                config,
                crate::http_client::RouteLabel::Direct,
            )?;
            client.post(url)
        }
        ProxyRoutingModel::RequestEntrySnapshot
            if crate::custom_http::follows_redirects_with_secret(
                spec.follow_redirects,
                secret_bearing,
            ) =>
        {
            snapshot_client.post(url)
        }
        ProxyRoutingModel::RequestEntrySnapshot => {
            let client = crate::http_client::build_no_redirect_for_route(config, route)?;
            client.post(url)
        }
    };
    request = request.headers(headers).body(body);
    if !stream {
        request = request.timeout(timeouts.non_stream);
    }
    let started = Instant::now();
    let send_future = request.send();
    let result = if stream {
        match tokio::time::timeout(timeouts.stream_header, send_future).await {
            Ok(result) => result.map_err(map_attempt_send_error),
            Err(_) => Err(AttemptTransportError::HeaderTimeout {
                timeout: timeouts.stream_header,
            }),
        }
    } else {
        send_future.await.map_err(map_attempt_send_error)
    };
    Ok(ForwardOnceOutput { started, result })
}

fn map_attempt_send_error(error: reqwest::Error) -> AttemptTransportError {
    AttemptTransportError::Send(TransportSendFailure::from_send_error(
        error.is_connect(),
        error.is_timeout(),
        error.to_string(),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForwardAction {
    Return,
    RetrySameAccount,
    TryNextAccount,
    /// Free 429 is IP-shared; stop probing other keys and fall back to Go if allowed.
    ExhaustFreeChannel,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct UpstreamPayloadTooLargeResponse;

pub struct ForwardResult {
    pub response: Response,
    pub(crate) action: ForwardAction,
    pub error_message: Option<String>,
}

#[derive(Clone)]
enum RequestPricingSnapshot {
    OpenCode(Arc<PricingSnapshot>),
    Provider(Arc<ProviderScopedPricingSnapshot>),
    /// Linked Custom Key: exact frozen platform price, or fail-closed unknown.
    /// Never inherits Go / GOAT / Ollama / USD provider rows.
    Platform(PlatformAttemptPrice),
    Unpriced,
}

/// Per-attempt platform price captured from the link snapshot only.
#[derive(Clone)]
enum PlatformAttemptPrice {
    Frozen(FrozenPlatformPrice),
    Unknown { provenance: Option<String> },
}

#[derive(Clone)]
struct FrozenPlatformPrice {
    provenance: String,
    currency: String,
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

impl From<Arc<PricingSnapshot>> for RequestPricingSnapshot {
    fn from(snapshot: Arc<PricingSnapshot>) -> Self {
        Self::OpenCode(snapshot)
    }
}

impl RequestPricingSnapshot {
    fn for_account(state: &CoreState, account: &Account, go: Arc<PricingSnapshot>) -> Self {
        if account.provider_id == crate::provider::OPENCODE_PROVIDER_ID {
            return Self::OpenCode(go);
        }
        if !crate::provider::is_command_code_goat(&account.provider_id)
            && account.provider_id != crate::provider::OLLAMA_PROVIDER_ID
        {
            return Self::Unpriced;
        }
        let loaded = latest_provider_pricing_snapshot(&state.db.lock(), &account.provider_id);
        match loaded {
            Ok(Some(snapshot)) if snapshot.evidence() == ProviderPricingEvidence::Verified => {
                Self::Provider(Arc::new(snapshot))
            }
            Ok(_) => Self::Unpriced,
            Err(error) => {
                eprintln!(
                    "warning: failed to load provider pricing for {}/{}: {error}",
                    account.provider_id, account.provider_id
                );
                Self::Unpriced
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn estimate(
        &self,
        model: &str,
        prompt_tokens: i64,
        completion_tokens: i64,
        cached_tokens: i64,
        cache_creation_tokens: i64,
        service_tier: Option<&str>,
    ) -> crate::kernel::pricing::PricingEstimate {
        match self {
            Self::OpenCode(snapshot) => snapshot.estimate(
                model,
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                cache_creation_tokens,
                service_tier,
            ),
            Self::Provider(snapshot) => snapshot.estimate(
                model,
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                cache_creation_tokens,
                Utc::now(),
            ),
            Self::Platform(price) => price.estimate(),
            Self::Unpriced => crate::kernel::pricing::PricingEstimate {
                raw_cost_usd: None,
                quota_debit: None,
                effective_paid_cost_usd: None,
                cost: None,
                pricing_revision_id: None,
                quota_multiplier: None,
                local_adjustment_multiplier: None,
                cost_state: "unpriced",
            },
        }
    }

    fn revision(&self) -> Option<&str> {
        match self {
            Self::OpenCode(snapshot) => Some(&snapshot.revision),
            Self::Provider(snapshot) => Some(snapshot.revision()),
            Self::Platform(price) => price.provenance(),
            Self::Unpriced => None,
        }
    }

    fn provider_identity(&self) -> Option<&str> {
        match self {
            Self::OpenCode(_) => Some(crate::provider::OPENCODE_PROVIDER_ID),
            Self::Provider(snapshot) => Some(snapshot.provider_id()),
            Self::Platform(_) => Some(crate::provider::CUSTOM_PROVIDER_ID),
            Self::Unpriced => None,
        }
    }
}

impl PlatformAttemptPrice {
    fn provenance(&self) -> Option<&str> {
        match self {
            Self::Frozen(price) => Some(&price.provenance),
            Self::Unknown { provenance } => provenance.as_deref(),
        }
    }

    fn estimate(&self) -> crate::kernel::pricing::PricingEstimate {
        crate::kernel::pricing::PricingEstimate {
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            cost: None,
            pricing_revision_id: self.provenance().map(str::to_string),
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            cost_state: "unknown",
        }
    }
}

fn estimate_platform_native(
    price: &FrozenPlatformPrice,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_tokens: i64,
    cache_creation_tokens: i64,
) -> Option<f64> {
    let prompt = prompt_tokens.max(0) as f64;
    let completion = completion_tokens.max(0) as f64;
    let cached = (cached_tokens.max(0) as f64).min(prompt);
    let cache_creation = (cache_creation_tokens.max(0) as f64).min((prompt - cached).max(0.0));
    let uncached = prompt - cached - cache_creation;
    if cached > 0.0 && price.cache_read.is_none() {
        return None;
    }
    if cache_creation > 0.0 && price.cache_write.is_none() {
        return None;
    }
    let mut native = uncached * price.input + completion * price.output;
    if let Some(cache_read) = price.cache_read {
        native += cached * cache_read;
    }
    if let Some(cache_write) = price.cache_write {
        native += cache_creation * cache_write;
    }
    native.is_finite().then_some(native)
}

fn platform_price_for_attempt(
    state: &CoreState,
    account: &Account,
    upstream_model: &str,
    endpoint: Option<&str>,
) -> Option<PlatformAttemptPrice> {
    let db = state.db.lock();
    let links = db.list_platform_links().ok()?;
    let link = links
        .into_iter()
        .find(|link| link.account_id == account.id)?;
    if db
        .get_account(&account.id)
        .ok()
        .flatten()
        .is_none_or(|current| current.key_cipher != account.key_cipher)
        || endpoint.is_some_and(|url| {
            db.account_custom_config(&account.id)
                .ok()
                .flatten()
                .is_none_or(|config| config.endpoint_url != url)
        })
    {
        return Some(PlatformAttemptPrice::Unknown {
            provenance: Some("platform:attempt_identity_changed".into()),
        });
    }
    let parent = match db.platform_account(&link.platform_account_id) {
        Ok(Some(parent)) => parent,
        _ => {
            return Some(PlatformAttemptPrice::Unknown {
                provenance: Some(format!("{}:missing", link.platform_account_id)),
            });
        }
    };
    Some(select_link_platform_price(
        &link,
        &parent,
        upstream_model,
        Utc::now().timestamp(),
    ))
}

fn select_link_platform_price(
    link: &PlatformLink,
    parent: &PlatformAccount,
    upstream_model: &str,
    now: i64,
) -> PlatformAttemptPrice {
    let unknown = |reason: &str| PlatformAttemptPrice::Unknown {
        provenance: Some(format!(
            "{}:{}:{}:{}:{reason}",
            parent.id,
            parent.version,
            link.group.id.as_deref().unwrap_or(""),
            upstream_model
        )),
    };
    let Some(group_id) = link
        .group
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return unknown("auto");
    };
    if group_id == "auto" || !link.group.auto_groups.is_empty() {
        return unknown("auto");
    }
    let Some(snapshot) = link.snapshot.as_ref() else {
        return unknown("nosnap");
    };
    if snapshot.stale {
        return unknown("stale");
    }
    let matches = snapshot
        .prices
        .iter()
        .filter(|price| {
            price.model == upstream_model
                && price.group_id.as_deref() == Some(group_id)
                && !price.official_reference
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return unknown("nomatch");
    }
    let price = matches[0];
    if price.official_reference {
        return unknown("official");
    }
    if price.unavailable_reason.is_some() {
        return unknown("unavailable");
    }
    if now >= price.valid_until {
        return unknown("expired");
    }
    let Some(input) = finite_nonneg_rate(price.input) else {
        return unknown("incomplete");
    };
    let Some(output) = finite_nonneg_rate(price.output) else {
        return unknown("incomplete");
    };
    let currency = price.currency.trim();
    if currency.is_empty() {
        return unknown("incomplete");
    }
    let cache_read = match optional_finite_nonneg_rate(price.cache_read) {
        Ok(rate) => rate,
        Err(()) => return unknown("incomplete"),
    };
    let cache_write = match optional_finite_nonneg_rate(price.cache_write) {
        Ok(rate) => rate,
        Err(()) => return unknown("incomplete"),
    };
    PlatformAttemptPrice::Frozen(FrozenPlatformPrice {
        provenance: format!(
            "{}:{}:{group_id}:{upstream_model}:{}:{}:{}",
            parent.id, parent.version, price.valid_until, price.source, snapshot.observed_at
        ),
        currency: currency.to_string(),
        input,
        output,
        cache_read,
        cache_write,
    })
}

fn finite_nonneg_rate(value: Option<f64>) -> Option<f64> {
    value.filter(|rate| rate.is_finite() && *rate >= 0.0)
}

fn optional_finite_nonneg_rate(value: Option<f64>) -> Result<Option<f64>, ()> {
    match value {
        None => Ok(None),
        Some(rate) if rate.is_finite() && rate >= 0.0 => Ok(Some(rate)),
        Some(_) => Err(()),
    }
}

fn bind_platform_attempt_price(
    state: &CoreState,
    account: &Account,
    context: &mut ForwardAttemptContext,
    pricing: RequestPricingSnapshot,
    endpoint: Option<&str>,
) -> RequestPricingSnapshot {
    let Some(platform) =
        platform_price_for_attempt(state, account, &context.upstream_model, endpoint)
    else {
        return pricing;
    };
    context.platform_price = Some(platform.clone());
    RequestPricingSnapshot::Platform(platform)
}

fn apply_platform_native_attribution(
    attribution: &mut ForwardLogNativeAttribution,
    context: &ForwardAttemptContext,
    metrics: &ForwardMetrics,
) {
    let Some(PlatformAttemptPrice::Frozen(price)) = context.platform_price.as_ref() else {
        return;
    };
    if metrics.cost_state != "unknown" {
        return;
    }
    let Some(value) = estimate_platform_native(
        price,
        metrics.prompt_tokens,
        metrics.completion_tokens,
        metrics.cached_tokens,
        metrics.cache_creation_tokens,
    ) else {
        return;
    };
    attribution.native_cost_value = Some(value);
    attribution.native_cost_unit = Some(price.currency.clone());
    attribution.native_cost_currency = Some(price.currency.clone());
}

fn platform_request_has_variable_cost(body: &[u8], service_tier: Option<&str>) -> bool {
    fn media(value: &Value) -> bool {
        match value {
            Value::Object(object) => {
                object
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| {
                        matches!(
                            kind,
                            "image"
                                | "image_url"
                                | "input_image"
                                | "input_audio"
                                | "audio"
                                | "video"
                                | "input_video"
                                | "file"
                                | "input_file"
                        )
                    })
                    || object
                        .get("modalities")
                        .and_then(Value::as_array)
                        .is_some_and(|values| values.iter().any(|v| v.as_str() != Some("text")))
                    || object.contains_key("inline_data")
                    || object.contains_key("inlineData")
                    || object.values().any(media)
            }
            Value::Array(values) => values.iter().any(media),
            _ => false,
        }
    }
    service_tier.is_some_and(|tier| tier != "default")
        || serde_json::from_slice::<Value>(body).map_or(true, |value| media(&value))
}

#[derive(Clone)]
struct ForwardAttemptContext {
    trace: RequestTrace,
    client_body_bytes: usize,
    upstream_body_bytes: usize,
    attempt: u32,
    client_format: ApiFormat,
    upstream_format: ApiFormat,
    model: String,
    requested_model: String,
    resolved_alias: Option<String>,
    upstream_model: String,
    stream: bool,
    /// Route leg this attempt connected through, resolved by the handler from
    /// the request's route-set snapshot; recorded on the forward log row.
    route: RouteLabel,
    known_secret: Option<String>,
    route_account_id: Option<String>,
    provider_id: Option<String>,

    credential_account_id: Option<String>,
    client_key_id: Option<String>,
    client_key_name: Option<String>,
    platform_price: Option<PlatformAttemptPrice>,
}

impl ForwardAttemptContext {
    fn new(
        trace: &RequestTrace,
        client_body_bytes: usize,
        attempt: u32,
        plan: &RequestPlan,
        route: RouteLabel,
    ) -> Self {
        let identity = native_log_identity(plan);
        Self {
            trace: trace.clone(),
            client_body_bytes,
            upstream_body_bytes: plan.body.len(),
            attempt,
            client_format: plan.client,
            upstream_format: plan.upstream,
            model: plan.model.clone(),
            requested_model: identity.requested_model,
            resolved_alias: identity.resolved_alias,
            upstream_model: identity.upstream_model,
            stream: plan.stream,
            route,
            known_secret: None,
            route_account_id: None,
            provider_id: None,

            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            platform_price: None,
        }
    }

    /// Records which gateway key authenticated the request; the name is a
    /// write-time snapshot resolved from the credential snapshot (the primary
    /// key resolves to the fixed "Primary") so later renames keep historical
    /// attribution without a config or db lookup.
    fn set_client_key(&mut self, id: Option<&str>, state: &CoreState) {
        self.client_key_id = id.map(str::to_string);
        self.client_key_name = id.and_then(|id| state.client_key_name(id));
    }

    fn set_known_secret(&mut self, known_secret: &str) {
        self.known_secret = Some(known_secret.to_string());
    }

    fn set_provider_route(&mut self, account: &Account, spec: &AttemptSpec) {
        self.route_account_id = Some(account.id.clone());
        self.provider_id = Some(account.provider_id.clone());
        self.credential_account_id = spec.credential_account_id().map(str::to_string);
    }

    fn redact_known_secret(&self, text: &str) -> String {
        self.known_secret.as_deref().map_or_else(
            || text.to_string(),
            |secret| redact_known_secret(text, secret),
        )
    }

    fn sanitize_upstream_error(&self, text: &str) -> String {
        self.known_secret.as_deref().map_or_else(
            || sanitize_upstream_error(text, ""),
            |secret| sanitize_upstream_error(text, secret),
        )
    }

    fn failure(&self, spec: FailureSpec<'_>) -> FailureRecord {
        let mut diagnostic = ErrorDiagnostic::new(
            &self.trace,
            self.attempt,
            spec.error_source,
            spec.error_stage,
            self.client_format,
        );
        diagnostic.upstream_format = Some(api_format_name(self.upstream_format).to_string());
        diagnostic.model = Some(self.model.clone());
        diagnostic.stream = Some(self.stream);
        diagnostic.client_body_bytes = Some(self.client_body_bytes);
        diagnostic.upstream_body_bytes = Some(self.upstream_body_bytes);
        diagnostic.upstream_wait_ms = spec.upstream_wait_ms;
        diagnostic.downstream_status = spec.downstream_status;
        diagnostic.upstream_status = spec.upstream_status;
        diagnostic.retry_action = spec.retry_action.map(str::to_string);
        if let Some(headers) = spec.upstream_headers {
            diagnostic.upstream_headers =
                safe_upstream_headers(headers, self.known_secret.as_deref());
        }
        if let Some(body) = spec.request_body {
            diagnostic = diagnostic.with_request_summary(body);
        }
        if let Some(error) = spec.upstream_error {
            diagnostic.upstream_error = Some(self.known_secret.as_deref().map_or_else(
                || sanitize_upstream_error_value_with_known_secret(error, ""),
                |secret| sanitize_upstream_error_value_with_known_secret(error, secret),
            ));
        }
        let duration_ms = diagnostic.duration_ms.min(i64::MAX as u64) as i64;
        let diagnostic_json = serialize_diagnostic(diagnostic);
        emit_failure(&diagnostic_json);
        FailureRecord {
            error_source: spec.error_source.to_string(),
            error_stage: spec.error_stage.to_string(),
            duration_ms,
            diagnostic_json,
        }
    }
}

struct FailureSpec<'a> {
    error_source: &'static str,
    error_stage: &'static str,
    downstream_status: Option<u16>,
    upstream_status: Option<u16>,
    upstream_wait_ms: Option<u64>,
    retry_action: Option<&'static str>,
    upstream_headers: Option<&'a HeaderMap>,
    upstream_error: Option<&'a str>,
    request_body: Option<&'a [u8]>,
}

struct FailureRecord {
    error_source: String,
    error_stage: String,
    duration_ms: i64,
    diagnostic_json: String,
}

impl FailureRecord {
    fn update(&self) -> ForwardLogDiagnosticUpdate<'_> {
        ForwardLogDiagnosticUpdate {
            error_source: &self.error_source,
            error_stage: &self.error_stage,
            duration_ms: self.duration_ms,
            diagnostic_json: &self.diagnostic_json,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_request(
    client: &Client,
    route: RouteLabel,
    state: &CoreState,
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    trace: &RequestTrace,
    client_body: &[u8],
    attempt: u32,
    allow_same_account_retry: bool,
    headers: HeaderMap,
    pricing_snapshot: Arc<PricingSnapshot>,
    client_key_id: Option<&str>,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
    selection: &LiveSendSelection,
) -> Result<ForwardResult> {
    forward_request_impl(
        client,
        route,
        state,
        account,
        config,
        plan,
        trace,
        client_body,
        attempt,
        allow_same_account_retry,
        headers,
        pricing_snapshot,
        client_key_id,
        dynamics,
        selection,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn forward_request_impl(
    client: &Client,
    route: RouteLabel,
    state: &CoreState,
    account: &Account,
    config: &AppConfig,
    plan: &RequestPlan,
    trace: &RequestTrace,
    client_body: &[u8],
    attempt: u32,
    allow_same_account_retry: bool,
    headers: HeaderMap,
    pricing_snapshot: Arc<PricingSnapshot>,
    client_key_id: Option<&str>,
    dynamics: &[crate::dynamic::DynamicProviderRuntime],
    selection: &LiveSendSelection,
) -> Result<ForwardResult> {
    let mut attempt_context =
        ForwardAttemptContext::new(trace, client_body.len(), attempt, plan, route);
    let pricing_snapshot = bind_platform_attempt_price(
        state,
        account,
        &mut attempt_context,
        RequestPricingSnapshot::for_account(state, account, pricing_snapshot),
        plan.custom_route
            .as_ref()
            .map(|route| route.endpoint_url.as_str()),
    );
    let pricing_snapshot = if matches!(&pricing_snapshot, RequestPricingSnapshot::Platform(_))
        && platform_request_has_variable_cost(&plan.body, plan.service_tier.as_deref())
    {
        let unknown = PlatformAttemptPrice::Unknown {
            provenance: Some("platform:unsupported_request_pricing".into()),
        };
        attempt_context.platform_price = Some(unknown.clone());
        RequestPricingSnapshot::Platform(unknown)
    } else {
        pricing_snapshot
    };
    attempt_context.set_client_key(client_key_id, state);
    let attempt_spec =
        match provider_adapter::resolve_route_with_dynamics(account, config, plan, dynamics) {
            Ok(spec) => spec,
            Err(error) => {
                let class = classify_preflight(PreflightKind::Route);
                let message = format!("provider route is unavailable: {error}");
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "gateway",
                    error_stage: "provider_route",
                    downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                    upstream_status: None,
                    upstream_wait_ms: None,
                    retry_action: Some(retry_action_name(forward_action_for_class(
                        class,
                        allow_same_account_retry,
                        None,
                    ))),
                    upstream_headers: None,
                    upstream_error: None,
                    request_body: Some(client_body),
                });
                DbAttemptSink::new(&state.db.lock()).insert(
                    account,
                    &plan.model,
                    "error",
                    None,
                    metadata_metrics(
                        &pricing_snapshot,
                        plan.service_tier.as_deref(),
                        "not_applicable",
                    ),
                    Some(&message),
                    &attempt_context,
                    Some(failure),
                )?;
                return Ok(account_preflight_failure(plan, message));
            }
        };
    attempt_context.set_provider_route(account, &attempt_spec);
    // Attempt-level wire normalization: request-plan bytes are shared by every
    // candidate of a mixed chain, so the rewrite happens here after the
    // attempt is chosen and before the single send, and only for the family
    // whose adapter declared a marker. `upstream_body_bytes` records the
    // bytes actually sent.
    let attempt_body = attempt_spec
        .wire_normalization
        .normalize_request_body(plan.body.clone());
    attempt_context.upstream_body_bytes = attempt_body.len();
    if attempt_spec.is_local_external_integration() {
        crate::cpa::normalize_base_url(&attempt_spec.base_url, true)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    } else if attempt_spec.restricted_upstream_url() {
        ensure_safe_upstream_base_url(&attempt_spec.base_url)?;
    }
    let resolver = HostCredentialResolver::new(state, account, selection, plan, &attempt_spec);
    let key = match resolver.resolve_live(&attempt_spec.credential) {
        Ok(key) => key,
        Err(error) => {
            let class = if error.is_decrypt() {
                classify_preflight(PreflightKind::Decrypt)
            } else {
                classify_preflight(PreflightKind::Route)
            };
            let message = if error.is_decrypt() {
                format!("failed to decrypt account credentials: {error}")
            } else {
                error.to_string()
            };
            let failure = attempt_context.failure(FailureSpec {
                error_source: "gateway",
                error_stage: "credential",
                downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                upstream_status: None,
                upstream_wait_ms: None,
                retry_action: Some(retry_action_name(forward_action_for_class(
                    class,
                    allow_same_account_retry,
                    None,
                ))),
                upstream_headers: None,
                upstream_error: None,
                request_body: Some(client_body),
            });
            DbAttemptSink::new(&state.db.lock()).insert(
                account,
                &plan.model,
                "error",
                None,
                metadata_metrics(
                    &pricing_snapshot,
                    plan.service_tier.as_deref(),
                    "not_applicable",
                ),
                Some(&message),
                &attempt_context,
                Some(failure),
            )?;
            return Ok(account_preflight_failure(plan, message));
        }
    };
    if let Some(key) = key.as_deref() {
        attempt_context.set_known_secret(key);
    }
    let mut upstream_headers = reqwest::header::HeaderMap::new();

    // Forward harmless client headers only. Auth and hop-by-hop/private headers
    // belong to the gateway/client boundary, not the upstream request.
    for (name, value) in headers.iter() {
        let header = name.as_str().to_ascii_lowercase();
        if !(matches!(
            header.as_str(),
            "authorization"
                | "x-api-key"
                | "x-goog-api-key"
                | "cookie"
                | "proxy-authorization"
                | "host"
                | "content-length"
                | "connection"
                | "transfer-encoding"
                | "accept-encoding"
                | "x-ocg-conversation-id"
                | "x-cmdc-zdr"
                | "x-opencode-session"
                | "x-opencode-client"
                | "x-opencode-request"
                | "x-opencode-project"
                | "x-session-id"
                | "x-session-affinity"
        ) || (plan.upstream != ApiFormat::Messages
            && matches!(header.as_str(), "anthropic-version" | "anthropic-beta")))
        {
            upstream_headers.insert(name.clone(), value.clone());
        }
    }
    apply_provider_identity_headers(
        &mut upstream_headers,
        &headers,
        &account.provider_id,
        plan.client,
        plan.log_requested_model(),
        client_body,
        &trace.request_id,
    );
    // Match the attempt's authentication contract. The client wire protocol
    // alone is not an authentication decision. The executor constructs the
    // header from the Host-resolved secret; adapters never supplied plaintext.
    upstream_headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let resolved_auth = attempt_spec.wire_auth();
    let url = attempt_spec
        .request_url()
        .map_err(|error| anyhow::anyhow!(error))?;
    if matches!(resolved_auth, UpstreamAuth::Bearer | UpstreamAuth::XApiKey) {
        let key = key
            .as_deref()
            .expect("credential-bearing provider route must decrypt a key");
        let key_header = match reqwest::header::HeaderValue::from_str(key) {
            Ok(value) => value,
            Err(error) => {
                let class = classify_preflight(PreflightKind::Decrypt);
                let message = format!("account key is not a valid upstream header value: {error}");
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "gateway",
                    error_stage: "credential",
                    downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                    upstream_status: None,
                    upstream_wait_ms: None,
                    retry_action: Some(retry_action_name(forward_action_for_class(
                        class,
                        allow_same_account_retry,
                        None,
                    ))),
                    upstream_headers: None,
                    upstream_error: None,
                    request_body: Some(client_body),
                });
                DbAttemptSink::new(&state.db.lock()).insert(
                    account,
                    &plan.model,
                    "error",
                    None,
                    metadata_metrics(
                        &pricing_snapshot,
                        plan.service_tier.as_deref(),
                        "not_applicable",
                    ),
                    Some(&message),
                    &attempt_context,
                    Some(failure),
                )?;
                return Ok(account_preflight_failure(plan, message));
            }
        };
        match resolved_auth {
            UpstreamAuth::XApiKey => {
                upstream_headers.insert("x-api-key", key_header);
            }
            UpstreamAuth::Bearer => {
                let authorization =
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                        .expect("validated key must remain valid when prefixed as Bearer");
                upstream_headers.insert(reqwest::header::AUTHORIZATION, authorization);
            }
            _ => unreachable!(),
        }
    }
    if plan.upstream == ApiFormat::Messages && !upstream_headers.contains_key("anthropic-version") {
        upstream_headers.insert(
            "anthropic-version",
            reqwest::header::HeaderValue::from_static("2023-06-01"),
        );
    }
    upstream_headers.insert(
        reqwest::header::ACCEPT_ENCODING,
        reqwest::header::HeaderValue::from_static("identity"),
    );

    let model = plan.model.clone();
    let send_headers = if attempt_spec.isolates_client_headers() {
        let extra = json_content_headers(plan.upstream == ApiFormat::Messages)
            .map_err(|error| anyhow::anyhow!(error))?;
        if matches!(attempt_spec.wire_auth(), UpstreamAuth::None) {
            extra
        } else {
            let api_key = key
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("isolated route requires a decrypted key"))?;
            let scheme = match attempt_spec.auth {
                UpstreamAuth::XApiKey => crate::provider::UpstreamAuthScheme::XApiKey,
                _ => crate::provider::UpstreamAuthScheme::Bearer,
            };
            let mut headers = crate::custom_http::isolated_custom_headers(scheme, api_key)
                .map_err(|error| anyhow::anyhow!(error))?;
            for (name, value) in &extra {
                headers.insert(name.clone(), value.clone());
            }
            headers
        }
    } else {
        upstream_headers
    };

    if let Err(error) = resolver.confirm_live() {
        let class = if error.is_decrypt() {
            classify_preflight(PreflightKind::Decrypt)
        } else {
            classify_preflight(PreflightKind::Route)
        };
        let message = if error.is_decrypt() {
            format!("failed to decrypt account credentials: {error}")
        } else {
            error.to_string()
        };
        let failure = attempt_context.failure(FailureSpec {
            error_source: "gateway",
            error_stage: "credential",
            downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
            upstream_status: None,
            upstream_wait_ms: None,
            retry_action: Some(retry_action_name(forward_action_for_class(
                class,
                allow_same_account_retry,
                None,
            ))),
            upstream_headers: None,
            upstream_error: None,
            request_body: Some(client_body),
        });
        DbAttemptSink::new(&state.db.lock()).insert(
            account,
            &plan.model,
            "error",
            None,
            metadata_metrics(
                &pricing_snapshot,
                plan.service_tier.as_deref(),
                "not_applicable",
            ),
            Some(&message),
            &attempt_context,
            Some(failure),
        )?;
        return Ok(account_preflight_failure(plan, message));
    }

    if let Some(compat) = &plan.legacy_tool_compat {
        emit_legacy_tool_compat(
            &trace.request_id,
            compat.profile,
            compat.version,
            &compat.dropped_hosted_tools,
        );
    }

    let sent = forward_once(
        &attempt_spec,
        client,
        route,
        config,
        AttemptTimeouts::from_secs(
            config.non_stream_timeout_secs,
            config.stream_idle_timeout_secs,
        ),
        &url,
        send_headers,
        attempt_body,
        plan.stream,
    )
    .await?;
    let upstream_started = sent.started;
    let upstream_resp = match sent.result {
        Ok(resp) => resp,
        Err(AttemptTransportError::HeaderTimeout { timeout }) => {
            let class = classify_transport(TransportClassifyInput::HeaderTimeout);
            let detail = format!(
                "upstream did not return response headers within {}s",
                timeout.as_secs()
            );
            let error_message = outcome_unknown_message(&detail);
            let upstream_wait_ms = upstream_started.elapsed().as_millis() as u64;
            let action = forward_action_for_class(class, allow_same_account_retry, None);
            let failure = attempt_context.failure(FailureSpec {
                error_source: "transport",
                error_stage: "response_headers",
                downstream_status: Some(StatusCode::GATEWAY_TIMEOUT.as_u16()),
                upstream_status: None,
                upstream_wait_ms: Some(upstream_wait_ms),
                retry_action: Some(retry_action_name(action)),
                upstream_headers: None,
                upstream_error: Some(&detail),
                request_body: Some(client_body),
            });
            {
                let db = state.db.lock();
                DbAttemptSink::new(&db).insert(
                    account,
                    &model,
                    "outcome_unknown",
                    None,
                    metadata_metrics(
                        &pricing_snapshot,
                        plan.service_tier.as_deref(),
                        "outcome_unknown",
                    ),
                    Some(&error_message),
                    &attempt_context,
                    Some(failure),
                )?;
            }
            return Ok(ForwardResult {
                response: outcome_unknown_response(
                    plan.client,
                    StatusCode::GATEWAY_TIMEOUT,
                    &detail,
                ),
                action,
                error_message: Some(error_message),
            });
        }
        Err(AttemptTransportError::Send(failure)) => {
            let upstream_wait_ms = upstream_started.elapsed().as_millis() as u64;
            let kind: TransportFailureKind = failure.kind;
            let class = classify_transport(kind.into());
            let connect_failure = matches!(class, ProviderErrorClass::Connect);
            let outcome_unknown = matches!(class, ProviderErrorClass::OutcomeUnknown);
            let detail = failure.message;
            let error_message = if outcome_unknown {
                outcome_unknown_message(&detail)
            } else {
                detail.clone()
            };
            let status = if failure.timed_out {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
            let action = forward_action_for_class(class, allow_same_account_retry, None);
            let failure = attempt_context.failure(FailureSpec {
                error_source: "transport",
                error_stage: if connect_failure {
                    "connect"
                } else {
                    "response_headers"
                },
                downstream_status: Some(status.as_u16()),
                upstream_status: None,
                upstream_wait_ms: Some(upstream_wait_ms),
                retry_action: Some(retry_action_name(action)),
                upstream_headers: None,
                upstream_error: Some(&detail),
                request_body: Some(client_body),
            });
            {
                let db = state.db.lock();
                DbAttemptSink::new(&db).insert(
                    account,
                    &model,
                    if outcome_unknown {
                        "outcome_unknown"
                    } else {
                        "error"
                    },
                    None,
                    metadata_metrics(
                        &pricing_snapshot,
                        plan.service_tier.as_deref(),
                        if outcome_unknown {
                            "outcome_unknown"
                        } else {
                            "not_applicable"
                        },
                    ),
                    Some(&error_message),
                    &attempt_context,
                    Some(failure),
                )?;
            }
            return Ok(ForwardResult {
                response: if outcome_unknown {
                    outcome_unknown_response(plan.client, status, &detail)
                } else {
                    error_response(plan.client, &error_message, None)
                },
                action,
                error_message: Some(error_message),
            });
        }
    };

    let upstream_wait_ms = upstream_started.elapsed().as_millis() as u64;

    let status = upstream_resp.status();
    let is_stream = upstream_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    let body_timeout = plan
        .stream
        .then(|| StdDuration::from_secs(config.stream_idle_timeout_secs));

    if status.is_server_error() {
        // A response status is authoritative even if its error body stalls. Keep
        // that status and never replay the request; the bounded read only affects
        // how much safe diagnostic text we can return.
        let error_headers = upstream_resp.headers().clone();
        let text = response_text_with_timeout(
            upstream_resp,
            body_timeout,
            Some(MAX_UPSTREAM_ERROR_BODY_BYTES),
        )
        .await
        .unwrap_or_else(ResponseBodyFailure::into_detail);
        let class = classify_http(
            status.as_u16(),
            &account.provider_id,
            plan.channel,
            attempt_spec.auth == UpstreamAuth::None,
        );
        let action = forward_action_for_class(class, allow_same_account_retry, None);
        let error_message = format!(
            "upstream error {}: {}",
            status.as_u16(),
            attempt_context.sanitize_upstream_error(&text)
        );
        let failure = attempt_context.failure(FailureSpec {
            error_source: "upstream",
            error_stage: "upstream_http",
            downstream_status: Some(status.as_u16()),
            upstream_status: Some(status.as_u16()),
            upstream_wait_ms: Some(upstream_wait_ms),
            retry_action: Some(retry_action_name(action)),
            upstream_headers: Some(&error_headers),
            upstream_error: Some(&text),
            request_body: Some(client_body),
        });
        {
            let db = state.db.lock();
            DbAttemptSink::new(&db).insert(
                account,
                &model,
                "error",
                Some(status.as_u16() as i32),
                metadata_metrics(
                    &pricing_snapshot,
                    plan.service_tier.as_deref(),
                    "not_applicable",
                ),
                Some(&error_message),
                &attempt_context,
                Some(failure),
            )?;
        }
        return Ok(ForwardResult {
            response: protocol_status_error_response(plan.client, status, &error_message, None),
            action,
            error_message: Some(error_message),
        });
    }

    if status.is_client_error() {
        // As above, a known 4xx proves the upstream rejected the request. Body
        // read failures must not turn into a replay or account fallback except
        // for the explicit 401/403/429 status policy below.
        let error_headers = upstream_resp.headers().clone();
        let text = response_text_with_timeout(
            upstream_resp,
            body_timeout,
            Some(MAX_UPSTREAM_ERROR_BODY_BYTES),
        )
        .await
        .unwrap_or_else(ResponseBodyFailure::into_detail);
        let class = super::classify::classify_http_response(
            status.as_u16(),
            &account.provider_id,
            plan.channel,
            attempt_spec.auth == UpstreamAuth::None,
            &text,
        );

        match class {
            ProviderErrorClass::RateLimited { policy } => {
                let (window, cooldown) = rate_limit_window_and_cooldown(policy, &text);
                let until = Utc::now() + cooldown;
                let sanitized = attempt_context.sanitize_upstream_error(&text);
                let error_message = format!(
                    "rate limited: {} (resets in {}s)",
                    sanitized,
                    cooldown.num_seconds()
                );
                let action = forward_action_for_class(class, allow_same_account_retry, window);
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "upstream",
                    error_stage: "upstream_http",
                    downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                    upstream_status: Some(status.as_u16()),
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some(retry_action_name(action)),
                    upstream_headers: Some(&error_headers),
                    upstream_error: Some(&text),
                    request_body: Some(client_body),
                });
                {
                    let db = state.db.lock();
                    DbAttemptSink::new(&db).insert(
                        account,
                        &model,
                        "client_error",
                        Some(429),
                        metadata_metrics(
                            &pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "not_applicable",
                        ),
                        Some(&sanitized),
                        &attempt_context,
                        Some(failure),
                    )?;
                    db.set_account_rate_limit_if_key_matches(
                        &account.id,
                        &account.key_cipher,
                        until,
                        &sanitized,
                        window,
                    )?;
                }
                // Schedule (never inline) an official usage reconciliation shortly
                // after a real inference 429. Does not alter cooldown/failover.
                if schedule_go_usage_sync(class) {
                    crate::usage_sync::schedule_after_inference_429(state, &account.id);
                }
                return Ok(ForwardResult {
                    response: error_response(plan.client, &error_message, None),
                    action,
                    error_message: Some(error_message),
                });
            }
            ProviderErrorClass::HttpRequestTimeout => {
                let detail = format!(
                    "upstream returned 408: {}",
                    attempt_context.sanitize_upstream_error(&text)
                );
                let error_message = outcome_unknown_message(&detail);
                let action = forward_action_for_class(class, allow_same_account_retry, None);
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "upstream",
                    error_stage: "upstream_http",
                    downstream_status: Some(StatusCode::GATEWAY_TIMEOUT.as_u16()),
                    upstream_status: Some(status.as_u16()),
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some(retry_action_name(action)),
                    upstream_headers: Some(&error_headers),
                    upstream_error: Some(&text),
                    request_body: Some(client_body),
                });
                {
                    let db = state.db.lock();
                    DbAttemptSink::new(&db).insert(
                        account,
                        &model,
                        "outcome_unknown",
                        Some(408),
                        metadata_metrics(
                            &pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "outcome_unknown",
                        ),
                        Some(&error_message),
                        &attempt_context,
                        Some(failure),
                    )?;
                }
                return Ok(ForwardResult {
                    response: outcome_unknown_response(
                        plan.client,
                        StatusCode::GATEWAY_TIMEOUT,
                        &detail,
                    ),
                    action,
                    error_message: Some(error_message),
                });
            }
            ProviderErrorClass::UnauthorizedPassthrough => {
                let error_message = format!(
                    "upstream auth error 401: {}",
                    attempt_context.sanitize_upstream_error(&text)
                );
                let sanitized = attempt_context.sanitize_upstream_error(&text);
                let action = forward_action_for_class(class, allow_same_account_retry, None);
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "upstream",
                    error_stage: "upstream_http",
                    downstream_status: Some(status.as_u16()),
                    upstream_status: Some(status.as_u16()),
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some(retry_action_name(action)),
                    upstream_headers: Some(&error_headers),
                    upstream_error: Some(&text),
                    request_body: Some(client_body),
                });
                {
                    let db = state.db.lock();
                    DbAttemptSink::new(&db).insert(
                        account,
                        &model,
                        "client_error",
                        Some(401),
                        metadata_metrics(
                            &pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "not_applicable",
                        ),
                        Some(&sanitized),
                        &attempt_context,
                        Some(failure),
                    )?;
                }
                let upstream_error = Some(sanitize_upstream_error_value_with_known_secret(
                    &text,
                    key.as_deref().unwrap_or_default(),
                ));
                let body = format_error(plan.client, status, &sanitized, upstream_error.as_ref());
                return Ok(ForwardResult {
                    response: (status, axum::Json(body)).into_response(),
                    action,
                    error_message: Some(error_message),
                });
            }
            ProviderErrorClass::UnauthorizedRotate => {
                let error_message = format!(
                    "upstream account error 401: {}",
                    attempt_context.sanitize_upstream_error(&text)
                );
                let sanitized = attempt_context.sanitize_upstream_error(&text);
                let action = forward_action_for_class(class, allow_same_account_retry, None);
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "upstream",
                    error_stage: "upstream_http",
                    downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                    upstream_status: Some(status.as_u16()),
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some(retry_action_name(action)),
                    upstream_headers: Some(&error_headers),
                    upstream_error: Some(&text),
                    request_body: Some(client_body),
                });
                {
                    let db = state.db.lock();
                    DbAttemptSink::new(&db).insert(
                        account,
                        &model,
                        "client_error",
                        Some(401),
                        metadata_metrics(
                            &pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "not_applicable",
                        ),
                        Some(&sanitized),
                        &attempt_context,
                        Some(failure),
                    )?;
                    db.set_account_auth_error_if_key_matches(
                        &account.id,
                        &account.key_cipher,
                        Some(&error_message),
                    )?;
                }
                return Ok(ForwardResult {
                    response: error_response(plan.client, &error_message, None),
                    action,
                    error_message: Some(error_message),
                });
            }
            ProviderErrorClass::ForbiddenStop | ProviderErrorClass::ForbiddenRotate => {
                let anonymous_route = matches!(class, ProviderErrorClass::ForbiddenStop);
                let error_message = if anonymous_route {
                    format!(
                        "anonymous provider route was rejected with 403; no credential fallback was attempted: {}",
                        attempt_context.sanitize_upstream_error(&text)
                    )
                } else {
                    format!(
                        "upstream auth error 403: {}",
                        attempt_context.sanitize_upstream_error(&text)
                    )
                };
                let sanitized = attempt_context.sanitize_upstream_error(&text);
                let action = forward_action_for_class(class, allow_same_account_retry, None);
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "upstream",
                    error_stage: "upstream_http",
                    downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                    upstream_status: Some(status.as_u16()),
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some(retry_action_name(action)),
                    upstream_headers: Some(&error_headers),
                    upstream_error: Some(&text),
                    request_body: Some(client_body),
                });
                {
                    let db = state.db.lock();
                    DbAttemptSink::new(&db).insert(
                        account,
                        &model,
                        "client_error",
                        Some(403),
                        metadata_metrics(
                            &pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "not_applicable",
                        ),
                        Some(&sanitized),
                        &attempt_context,
                        Some(failure),
                    )?;
                }
                return Ok(ForwardResult {
                    response: error_response(plan.client, &error_message, None),
                    action,
                    error_message: Some(error_message),
                });
            }
            _ => {
                // Other 4xx: request-level error. Convert its envelope for the caller,
                // but don't retry another account for the same invalid request.
                let sanitized = attempt_context.sanitize_upstream_error(&text);
                let action = forward_action_for_class(class, allow_same_account_retry, None);
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "upstream",
                    error_stage: "upstream_http",
                    downstream_status: Some(status.as_u16()),
                    upstream_status: Some(status.as_u16()),
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some(retry_action_name(action)),
                    upstream_headers: Some(&error_headers),
                    upstream_error: Some(&text),
                    request_body: Some(client_body),
                });
                {
                    let db = state.db.lock();
                    DbAttemptSink::new(&db).insert(
                        account,
                        &model,
                        "client_error",
                        Some(status.as_u16() as i32),
                        metadata_metrics(
                            &pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "not_applicable",
                        ),
                        Some(&sanitized),
                        &attempt_context,
                        Some(failure),
                    )?;
                }
                let upstream_error = Some(sanitize_upstream_error_value_with_known_secret(
                    &text,
                    key.as_deref().unwrap_or_default(),
                ));
                let message = sanitized;
                let body = format_error(plan.client, status, &message, upstream_error.as_ref());
                let mut response = (status, axum::Json(body)).into_response();
                if status == StatusCode::PAYLOAD_TOO_LARGE {
                    response
                        .extensions_mut()
                        .insert(UpstreamPayloadTooLargeResponse);
                }
                return Ok(ForwardResult {
                    response,
                    action,
                    error_message: None,
                });
            }
        }
    }

    // Success path — for non-stream, record breaker success now.
    // For streams, don't pre-record success; the stream error handler
    // records errors, and we haven't proven success until the stream completes.

    if is_stream {
        let response_builder = Response::builder()
            .status(status)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .header("connection", "keep-alive");

        // Insert the "streaming" row up front so a process crash mid-stream still
        // leaves a record. The finalizer updates it once the stream ends. The error
        // path also updates this row (instead of inserting a duplicate) so every
        // request maps to exactly one row in forward_logs.
        let initial_id: i64 = {
            let db = state.db.lock();
            DbAttemptSink::new(&db).insert(
                account,
                &model,
                "streaming",
                Some(status.as_u16() as i32),
                metadata_metrics(
                    &pricing_snapshot,
                    plan.service_tier.as_deref(),
                    "not_applicable",
                ),
                None,
                &attempt_context,
                None,
            )?
        };

        let stream_idle_timeout = StdDuration::from_secs(config.stream_idle_timeout_secs);
        let mut upstream_stream = Box::pin(upstream_resp.bytes_stream());
        let st = Arc::new(Mutex::new(StreamState::default()));
        let converter = Arc::new(Mutex::new(
            StreamConverter::new_with_known_secret_and_normalization(
                plan,
                attempt_context.known_secret.as_deref(),
                attempt_spec.wire_normalization,
            ),
        ));
        let upstream_format = plan.upstream;
        let stream_idle_timeout_secs = config.stream_idle_timeout_secs;

        // Keep the retry decision in the request handler, before ForwardResult is
        // returned. We pre-read only until the converter has data for the client.
        // If the upstream dies before that point, replaying once cannot duplicate
        // downstream SSE events. The upstream outcome and quota charge can still
        // be ambiguous, so the retry remains bounded to the same account.
        let (initial_chunks, upstream_finished) = loop {
            let preflight = tokio::time::timeout(stream_idle_timeout, upstream_stream.next()).await;
            match preflight {
                Ok(Some(Ok(chunk))) => {
                    process_chunk_for_usage(&mut st.lock(), upstream_format, &chunk, Some(&model));
                    let (converted, terminal) = {
                        let mut converter = converter.lock();
                        let converted = converter.process_chunk(chunk);
                        let terminal = converter.is_terminal();
                        (converted, terminal)
                    };
                    match converted {
                        Ok(chunks) => {
                            if !chunks.is_empty() || terminal {
                                break (chunks, terminal);
                            }
                        }
                        Err(error) => {
                            let detail = format!("stream conversion failed: {}", error.message);
                            match handle_pre_output_stream_failure(
                                state,
                                &st,
                                &converter,
                                initial_id,
                                &pricing_snapshot,
                                &attempt_context,
                                plan,
                                status,
                                upstream_wait_ms,
                                StatusCode::BAD_GATEWAY,
                                "gateway",
                                "response_transform",
                                &detail,
                                StreamClassifyInput::ConversionFailedBeforeOutput,
                                allow_same_account_retry,
                            ) {
                                PreOutputFailure::Retry(result) => return Ok(result),
                                PreOutputFailure::Return(chunks) => break (chunks, true),
                            }
                        }
                    }
                }
                Ok(Some(Err(error))) => {
                    let detail = format!("upstream stream interrupted: {error}");
                    match handle_pre_output_stream_failure(
                        state,
                        &st,
                        &converter,
                        initial_id,
                        &pricing_snapshot,
                        &attempt_context,
                        plan,
                        status,
                        upstream_wait_ms,
                        StatusCode::BAD_GATEWAY,
                        "transport",
                        "stream",
                        &detail,
                        StreamClassifyInput::InterruptedBeforeOutput,
                        allow_same_account_retry,
                    ) {
                        PreOutputFailure::Retry(result) => return Ok(result),
                        PreOutputFailure::Return(chunks) => break (chunks, true),
                    }
                }
                Ok(None) => {
                    let finished = {
                        let mut converter = converter.lock();
                        converter.finish()
                    };
                    match finished {
                        Ok(chunks) => {
                            break (chunks, true);
                        }
                        Err(error) => {
                            let detail = format!(
                                "upstream stream ended before a complete response: {}",
                                error.message
                            );
                            match handle_pre_output_stream_failure(
                                state,
                                &st,
                                &converter,
                                initial_id,
                                &pricing_snapshot,
                                &attempt_context,
                                plan,
                                status,
                                upstream_wait_ms,
                                StatusCode::BAD_GATEWAY,
                                "gateway",
                                "response_transform",
                                &detail,
                                StreamClassifyInput::EndedIncompleteBeforeOutput,
                                allow_same_account_retry,
                            ) {
                                PreOutputFailure::Retry(result) => return Ok(result),
                                PreOutputFailure::Return(chunks) => break (chunks, true),
                            }
                        }
                    }
                }
                Err(_) => {
                    let detail =
                        format!("upstream stream idle timeout after {stream_idle_timeout_secs}s");
                    match handle_pre_output_stream_failure(
                        state,
                        &st,
                        &converter,
                        initial_id,
                        &pricing_snapshot,
                        &attempt_context,
                        plan,
                        status,
                        upstream_wait_ms,
                        StatusCode::GATEWAY_TIMEOUT,
                        "transport",
                        "stream",
                        &detail,
                        StreamClassifyInput::IdleTimeoutBeforeOutput,
                        allow_same_account_retry,
                    ) {
                        PreOutputFailure::Retry(result) => return Ok(result),
                        PreOutputFailure::Return(chunks) => break (chunks, true),
                    }
                }
            }
        };
        let stream = futures_util::stream::unfold(
            (upstream_stream, upstream_finished),
            move |(mut stream, finished)| async move {
                if finished {
                    return None;
                }
                match tokio::time::timeout(stream_idle_timeout, stream.next()).await {
                    Ok(Some(Ok(chunk))) => Some((StreamRead::Chunk(chunk), (stream, false))),
                    Ok(Some(Err(error))) => Some((StreamRead::Failed(error), (stream, true))),
                    Ok(None) => None,
                    Err(_) => Some((StreamRead::IdleTimeout, (stream, true))),
                }
            },
        );
        let state_h = state.clone();

        let st_map = st.clone();
        let converter_map = converter.clone();
        let model_for_stream = model.clone();
        let pricing_map = pricing_snapshot.clone();
        let service_tier_map = plan.service_tier.clone();
        let attempt_map = attempt_context.clone();

        let mapped = stream
            .flat_map(move |result| {
                let (chunks, stop) = match result {
                    StreamRead::Chunk(chunk) => {
                        let stopped = {
                            let state = st_map.lock();
                            state.error || state.terminal
                        } || converter_map.lock().is_terminal();
                        if stopped {
                            (Vec::new(), true)
                        } else {
                            process_chunk_for_usage(
                                &mut st_map.lock(),
                                upstream_format,
                                &chunk,
                                Some(&model_for_stream),
                            );
                            let converted = converter_map.lock().process_chunk(chunk);
                            match converted {
                                Ok(chunks) => (chunks, false),
                                Err(error) => {
                                    let detail =
                                        format!("stream conversion failed: {}", error.message);
                                    let msg = outcome_unknown_message(&detail);
                                    {
                                        let mut state = st_map.lock();
                                        state.error = true;
                                        state.outcome_unknown = true;
                                        state.error_message = Some(msg.clone());
                                        state.diagnostic_recorded = true;
                                    }
                                    let chunks = converter_map.lock().outcome_unknown_event(&msg);
                                    let failure = attempt_map.failure(FailureSpec {
                                        error_source: "gateway",
                                        error_stage: "response_transform",
                                        downstream_status: Some(status.as_u16()),
                                        upstream_status: Some(status.as_u16()),
                                        upstream_wait_ms: Some(upstream_wait_ms),
                                        retry_action: Some(no_replay_retry_action()),
                                        upstream_headers: None,
                                        upstream_error: Some(&detail),
                                        request_body: None,
                                    });
                                    let diagnostic = failure.update();
                                    let db = state_h.db.lock();
                                    let _ = DbAttemptSink::new(&db).finalize(
                                        initial_id,
                                        "outcome_unknown",
                                        None,
                                        metadata_metrics(
                                            &pricing_map,
                                            service_tier_map.as_deref(),
                                            "outcome_unknown",
                                        ),
                                        Some(&msg),
                                        Some(&diagnostic),
                                        &attempt_map,
                                    );
                                    (chunks, true)
                                }
                            }
                        }
                    }
                    StreamRead::Failed(error) => {
                        if converter_map.lock().is_terminal() {
                            (Vec::new(), true)
                        } else {
                            let detail = format!("upstream stream interrupted: {error}");
                            let msg = outcome_unknown_message(&detail);
                            {
                                let mut state = st_map.lock();
                                state.error = true;
                                state.outcome_unknown = true;
                                state.error_message = Some(msg.clone());
                                state.diagnostic_recorded = true;
                            }
                            let chunks = converter_map.lock().outcome_unknown_event(&msg);
                            let failure = attempt_map.failure(FailureSpec {
                                error_source: "transport",
                                error_stage: "stream",
                                downstream_status: Some(status.as_u16()),
                                upstream_status: Some(status.as_u16()),
                                upstream_wait_ms: Some(upstream_wait_ms),
                                retry_action: Some(no_replay_retry_action()),
                                upstream_headers: None,
                                upstream_error: Some(&detail),
                                request_body: None,
                            });
                            let diagnostic = failure.update();
                            let db = state_h.db.lock();
                            let _ = DbAttemptSink::new(&db).finalize(
                                initial_id,
                                "outcome_unknown",
                                None,
                                metadata_metrics(
                                    &pricing_map,
                                    service_tier_map.as_deref(),
                                    "outcome_unknown",
                                ),
                                Some(&msg),
                                Some(&diagnostic),
                                &attempt_map,
                            );
                            (chunks, true)
                        }
                    }
                    StreamRead::IdleTimeout => {
                        if converter_map.lock().is_terminal() {
                            (Vec::new(), true)
                        } else {
                            let detail = format!(
                                "upstream stream idle timeout after {stream_idle_timeout_secs}s"
                            );
                            let msg = outcome_unknown_message(&detail);
                            {
                                let mut state = st_map.lock();
                                state.error = true;
                                state.outcome_unknown = true;
                                state.error_message = Some(msg.clone());
                                state.diagnostic_recorded = true;
                            }
                            let chunks = converter_map.lock().outcome_unknown_event(&msg);
                            let failure = attempt_map.failure(FailureSpec {
                                error_source: "transport",
                                error_stage: "stream",
                                downstream_status: Some(status.as_u16()),
                                upstream_status: Some(status.as_u16()),
                                upstream_wait_ms: Some(upstream_wait_ms),
                                retry_action: Some(no_replay_retry_action()),
                                upstream_headers: None,
                                upstream_error: Some(&detail),
                                request_body: None,
                            });
                            let diagnostic = failure.update();
                            let db = state_h.db.lock();
                            let _ = DbAttemptSink::new(&db).finalize(
                                initial_id,
                                "outcome_unknown",
                                None,
                                metadata_metrics(
                                    &pricing_map,
                                    service_tier_map.as_deref(),
                                    "outcome_unknown",
                                ),
                                Some(&msg),
                                Some(&diagnostic),
                                &attempt_map,
                            );
                            (chunks, true)
                        }
                    }
                };
                let mut items = chunks
                    .into_iter()
                    .map(|chunk| Some(Ok::<bytes::Bytes, std::io::Error>(chunk)))
                    .collect::<Vec<_>>();
                if stop {
                    // The sentinel lets flat_map drain every generated error chunk, then
                    // stops without polling the stalled upstream body for another item.
                    items.push(None);
                }
                futures_util::stream::iter(items)
            })
            .take_while(|item| futures_util::future::ready(item.is_some()))
            .map(|item| item.expect("stream stop sentinel should be filtered"));

        // Finalizer runs once, after the real stream is fully drained. It updates
        // the streaming row with final token counts and cost (or marks
        // success_no_usage if the upstream never sent a usage chunk).
        let finalizer = {
            let db_h = state.clone();
            let st_f = st.clone();
            let converter_f = converter.clone();
            let mdl = model.clone();
            let service_tier_f = plan.service_tier.clone();
            let pricing_f = pricing_snapshot.clone();
            let attempt_f = attempt_context.clone();
            let stream_guard = StreamOutcomeGuard::new(
                state.clone(),
                initial_id,
                st.clone(),
                model.clone(),
                pricing_snapshot.clone(),
                plan.service_tier.clone(),
                attempt_context.clone(),
                status.as_u16(),
                upstream_wait_ms,
            );
            // `unfold` is a clean "run once, then end" stream. The DB write is the
            // unfold's state transition, the body emits a single empty chunk, and
            // the stream then terminates — no need for once() + flatten gymnastics.
            futures_util::stream::unfold(
                FinalizerState::Init {
                    db_h,
                    st_f,
                    converter_f,
                    mdl,
                    initial_id,
                    guard: Box::new(stream_guard),
                },
                move |state| {
                    let service_tier = service_tier_f.clone();
                    let pricing = pricing_f.clone();
                    let attempt = attempt_f.clone();
                    async move {
                        let (db_h, st_f, converter_f, mdl, initial_id, mut guard) = match state {
                            FinalizerState::Init {
                                db_h,
                                st_f,
                                converter_f,
                                mdl,
                                initial_id,
                                guard,
                            } => (db_h, st_f, converter_f, mdl, initial_id, guard),
                            FinalizerState::Done => return None,
                        };
                        let (output, finish_error, converter_usage) = if st_f.lock().error {
                            (bytes::Bytes::new(), None, None)
                        } else {
                            let mut converter = converter_f.lock();
                            match converter.finish() {
                                Ok(chunks) => {
                                    (join_chunks(chunks), None, converter.captured_usage())
                                }
                                Err(error) => {
                                    let detail = format!(
                                        "upstream stream ended before a complete response: {}",
                                        error.message
                                    );
                                    let message = outcome_unknown_message(&detail);
                                    {
                                        let mut state = st_f.lock();
                                        state.error = true;
                                        state.outcome_unknown = true;
                                        state.error_message = Some(message.clone());
                                    }
                                    let chunks = converter.outcome_unknown_event(&message);
                                    (
                                        join_chunks(chunks),
                                        Some(message),
                                        converter.captured_usage(),
                                    )
                                }
                            }
                        };
                        let stream_error = st_f.lock().error_message.clone();
                        let diagnostic_recorded = st_f.lock().diagnostic_recorded;
                        let (status_str, metrics) = {
                            let g = st_f.lock();
                            if g.error {
                                let status = if g.outcome_unknown {
                                    "outcome_unknown"
                                } else {
                                    "error"
                                };
                                (
                                    status.to_string(),
                                    metadata_metrics(
                                        &pricing,
                                        service_tier.as_deref(),
                                        if g.outcome_unknown {
                                            "outcome_unknown"
                                        } else {
                                            "not_applicable"
                                        },
                                    ),
                                )
                            } else if let Some(usage) =
                                g.has_usage.then_some(g.usage).or(converter_usage)
                            {
                                let (p, c, cached, cache_creation) = token_counts(usage);
                                let metrics = pricing_metrics(
                                    &pricing,
                                    &mdl,
                                    p,
                                    c,
                                    cached,
                                    cache_creation,
                                    service_tier.as_deref(),
                                );
                                let status = success_status_for_cost(metrics.cost_state);
                                (status.to_string(), metrics)
                            } else {
                                (
                                    "success_no_usage".to_string(),
                                    metadata_metrics(
                                        &pricing,
                                        service_tier.as_deref(),
                                        "usage_missing",
                                    ),
                                )
                            }
                        };
                        let failure = if diagnostic_recorded {
                            None
                        } else if let Some(error) = finish_error.as_deref() {
                            Some(attempt.failure(FailureSpec {
                                error_source: "gateway",
                                error_stage: "response_transform",
                                downstream_status: Some(status.as_u16()),
                                upstream_status: Some(status.as_u16()),
                                upstream_wait_ms: Some(upstream_wait_ms),
                                retry_action: Some(no_replay_retry_action()),
                                upstream_headers: None,
                                upstream_error: Some(error),
                                request_body: None,
                            }))
                        } else {
                            stream_error.as_deref().map(|error| {
                                attempt.failure(FailureSpec {
                                    error_source: "upstream",
                                    error_stage: "stream",
                                    downstream_status: Some(status.as_u16()),
                                    upstream_status: Some(status.as_u16()),
                                    upstream_wait_ms: Some(upstream_wait_ms),
                                    retry_action: Some(no_replay_retry_action()),
                                    upstream_headers: None,
                                    upstream_error: Some(error),
                                    request_body: None,
                                })
                            })
                        };
                        let diagnostic = failure.as_ref().map(FailureRecord::update);
                        let persisted_error = finish_error
                            .as_deref()
                            .or(stream_error.as_deref())
                            .map(|error| attempt.redact_known_secret(error));
                        let db = db_h.db.lock();
                        if let Err(e) = DbAttemptSink::new(&db).finalize(
                            initial_id,
                            &status_str,
                            None,
                            metrics,
                            persisted_error.as_deref(),
                            diagnostic.as_ref(),
                            &attempt,
                        ) {
                            let _ = db.log_gateway(
                                "warn",
                                "forwarder",
                                &format!("failed to finalize streaming row {initial_id}: {e}"),
                            );
                        }
                        guard.disarm();
                        Some((
                            Ok::<bytes::Bytes, std::io::Error>(output),
                            FinalizerState::Done,
                        ))
                    }
                },
            )
        };

        let initial = futures_util::stream::iter(
            initial_chunks
                .into_iter()
                .map(Ok::<bytes::Bytes, std::io::Error>),
        );

        Ok(ForwardResult {
            response: response_builder
                .body(Body::from_stream(initial.chain(mapped).chain(finalizer)))?,
            action: ForwardAction::Return,
            error_message: None,
        })
    } else {
        let text = match response_text_with_timeout(upstream_resp, body_timeout, None).await {
            Ok(text) => text,
            Err(error) => {
                let class = classify_transport(if error.is_timeout() {
                    TransportClassifyInput::BodyTimeout
                } else {
                    TransportClassifyInput::OtherSendFailure
                });
                let action = forward_action_for_class(class, allow_same_account_retry, None);
                let downstream_status = if error.is_timeout() {
                    StatusCode::GATEWAY_TIMEOUT
                } else {
                    StatusCode::BAD_GATEWAY
                };
                let detail = error.into_detail();
                let error_message = outcome_unknown_message(&detail);
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "transport",
                    error_stage: "response_body",
                    downstream_status: Some(downstream_status.as_u16()),
                    upstream_status: Some(status.as_u16()),
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some(retry_action_name(action)),
                    upstream_headers: None,
                    upstream_error: Some(&detail),
                    request_body: Some(client_body),
                });
                {
                    let db = state.db.lock();
                    DbAttemptSink::new(&db).insert(
                        account,
                        &model,
                        "outcome_unknown",
                        Some(status.as_u16() as i32),
                        metadata_metrics(
                            &pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "outcome_unknown",
                        ),
                        Some(&error_message),
                        &attempt_context,
                        Some(failure),
                    )?;
                }
                return Ok(ForwardResult {
                    response: outcome_unknown_response(plan.client, downstream_status, &detail),
                    action,
                    error_message: Some(error_message),
                });
            }
        };
        let mut upstream_json = match serde_json::from_str::<Value>(&text) {
            Ok(value) => value,
            Err(_) => {
                let message = "upstream returned invalid JSON";
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "upstream",
                    error_stage: "response_body",
                    downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                    upstream_status: Some(status.as_u16()),
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some("return"),
                    upstream_headers: None,
                    upstream_error: Some(&text),
                    request_body: Some(client_body),
                });
                let db = state.db.lock();
                DbAttemptSink::new(&db).insert(
                    account,
                    &model,
                    "error",
                    Some(status.as_u16() as i32),
                    metadata_metrics(
                        &pricing_snapshot,
                        plan.service_tier.as_deref(),
                        "not_applicable",
                    ),
                    Some(message),
                    &attempt_context,
                    Some(failure),
                )?;
                return Ok(ForwardResult {
                    response: error_response(plan.client, message, None),
                    action: ForwardAction::Return,
                    error_message: Some(message.to_string()),
                });
            }
        };

        let metrics = if has_complete_usage(plan.upstream, &upstream_json) {
            let usage = extract_usage(plan.upstream, &upstream_json, Some(&model));
            let (prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens) =
                token_counts(usage);
            pricing_metrics(
                &pricing_snapshot,
                &model,
                prompt_tokens,
                completion_tokens,
                cached_tokens,
                cache_creation_tokens,
                plan.service_tier.as_deref(),
            )
        } else {
            metadata_metrics(
                &pricing_snapshot,
                plan.service_tier.as_deref(),
                "usage_missing",
            )
        };
        // Normalize the upstream response before protocol conversion so the
        // marker family's reasoning backfill is visible to every client
        // format, not only Chat-to-Chat passthrough.
        attempt_spec
            .wire_normalization
            .normalize_response_value(&mut upstream_json);
        // Redact before protocol conversion as well as after it. Some response
        // adapters serialize source values into opaque replay fields (for
        // example, Anthropic thinking blocks in Responses encrypted_content),
        // where a post-conversion exact-string pass could no longer see the Key.
        // Metrics extraction above is read-only, so redact in place instead of
        // cloning the whole response tree.
        if let Some(secret) = attempt_context.known_secret.as_deref() {
            redact_known_secret_values(&mut upstream_json, secret);
        }
        let mut response_json = match transform_response(plan, &upstream_json) {
            Ok(value) => value,
            Err(error) => {
                let message = format!("response conversion failed: {}", error.message);
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "gateway",
                    error_stage: "response_transform",
                    downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                    upstream_status: Some(status.as_u16()),
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some("return"),
                    upstream_headers: None,
                    upstream_error: Some(&message),
                    request_body: Some(client_body),
                });
                let db = state.db.lock();
                DbAttemptSink::new(&db).insert(
                    account,
                    &model,
                    "error",
                    Some(status.as_u16() as i32),
                    metrics.clone(),
                    Some(&message),
                    &attempt_context,
                    Some(failure),
                )?;
                return Ok(ForwardResult {
                    response: error_response(plan.client, &message, Some(&upstream_json)),
                    action: ForwardAction::Return,
                    error_message: Some(message),
                });
            }
        };
        if let Some(secret) = attempt_context.known_secret.as_deref() {
            redact_known_secret_values(&mut response_json, secret);
        }

        {
            let db = state.db.lock();
            DbAttemptSink::new(&db).insert(
                account,
                &model,
                success_status_for_cost(metrics.cost_state),
                Some(status.as_u16() as i32),
                metrics,
                None,
                &attempt_context,
                None,
            )?;
        }

        Ok(ForwardResult {
            response: (status, axum::Json(response_json)).into_response(),
            action: ForwardAction::Return,
            error_message: None,
        })
    }
}

struct StreamOutcomeGuard {
    state: CoreState,
    log_id: i64,
    stream_state: Arc<Mutex<StreamState>>,
    model: String,
    pricing: RequestPricingSnapshot,
    service_tier: Option<String>,
    attempt_context: ForwardAttemptContext,
    upstream_status: u16,
    upstream_wait_ms: u64,
    armed: bool,
}

impl StreamOutcomeGuard {
    #[allow(clippy::too_many_arguments)]
    fn new(
        state: CoreState,
        log_id: i64,
        stream_state: Arc<Mutex<StreamState>>,
        model: String,
        pricing: impl Into<RequestPricingSnapshot>,
        service_tier: Option<String>,
        attempt_context: ForwardAttemptContext,
        upstream_status: u16,
        upstream_wait_ms: u64,
    ) -> Self {
        Self {
            state,
            log_id,
            stream_state,
            model,
            pricing: pricing.into(),
            service_tier,
            attempt_context,
            upstream_status,
            upstream_wait_ms,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StreamOutcomeGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let (status, metrics, error_message, failure) = {
            let stream = self.stream_state.lock();
            if !stream.terminal && !stream.error {
                let message = outcome_unknown_message(
                    "downstream disconnected before the upstream stream outcome was confirmed",
                );
                let failure = self.attempt_context.failure(FailureSpec {
                    error_source: "downstream",
                    error_stage: "downstream_disconnect",
                    downstream_status: Some(self.upstream_status),
                    upstream_status: Some(self.upstream_status),
                    upstream_wait_ms: Some(self.upstream_wait_ms),
                    retry_action: Some("return"),
                    upstream_headers: None,
                    upstream_error: Some(&message),
                    request_body: None,
                });
                (
                    "outcome_unknown",
                    metadata_metrics(
                        &self.pricing,
                        self.service_tier.as_deref(),
                        "outcome_unknown",
                    ),
                    Some(message),
                    Some(failure),
                )
            } else if stream.error {
                let status = if stream.outcome_unknown {
                    "outcome_unknown"
                } else {
                    "error"
                };
                let failure = (!stream.diagnostic_recorded).then(|| {
                    let error = stream
                        .error_message
                        .as_deref()
                        .unwrap_or("upstream stream error");
                    self.attempt_context.failure(FailureSpec {
                        error_source: "upstream",
                        error_stage: "stream",
                        downstream_status: Some(self.upstream_status),
                        upstream_status: Some(self.upstream_status),
                        upstream_wait_ms: Some(self.upstream_wait_ms),
                        retry_action: Some("return"),
                        upstream_headers: None,
                        upstream_error: Some(error),
                        request_body: None,
                    })
                });
                (
                    status,
                    metadata_metrics(
                        &self.pricing,
                        self.service_tier.as_deref(),
                        if stream.outcome_unknown {
                            "outcome_unknown"
                        } else {
                            "not_applicable"
                        },
                    ),
                    stream.error_message.clone(),
                    failure,
                )
            } else if stream.has_usage {
                let (prompt, completion, cached, cache_creation) = token_counts(stream.usage);
                let metrics = pricing_metrics(
                    &self.pricing,
                    &self.model,
                    prompt,
                    completion,
                    cached,
                    cache_creation,
                    self.service_tier.as_deref(),
                );
                (
                    success_status_for_cost(metrics.cost_state),
                    metrics,
                    None,
                    None,
                )
            } else {
                (
                    "success_no_usage",
                    metadata_metrics(&self.pricing, self.service_tier.as_deref(), "usage_missing"),
                    None,
                    None,
                )
            }
        };

        let diagnostic = failure.as_ref().map(FailureRecord::update);
        let persisted_error = error_message
            .as_deref()
            .map(|message| self.attempt_context.redact_known_secret(message));
        let db = self.state.db.lock();
        if let Err(error) = DbAttemptSink::new(&db).finalize(
            self.log_id,
            status,
            None,
            metrics,
            persisted_error.as_deref(),
            diagnostic.as_ref(),
            &self.attempt_context,
        ) {
            let _ = db.log_gateway(
                "warn",
                "forwarder",
                &format!(
                    "failed to finalize dropped streaming row {}: {}",
                    self.log_id, error
                ),
            );
        }
    }
}

// `unfold` with an Init/Done state runs the normal finalizer once. The guard
// handles the complementary path where Hyper drops the body because the
// downstream client disconnected before polling that finalizer.
enum FinalizerState {
    Init {
        db_h: CoreState,
        st_f: Arc<Mutex<StreamState>>,
        converter_f: Arc<Mutex<StreamConverter>>,
        mdl: String,
        initial_id: i64,
        guard: Box<StreamOutcomeGuard>,
    },
    Done,
}

enum StreamRead {
    Chunk(bytes::Bytes),
    Failed(reqwest::Error),
    IdleTimeout,
}

enum PreOutputFailure {
    Retry(ForwardResult),
    Return(Vec<bytes::Bytes>),
}

#[allow(clippy::too_many_arguments)]
fn handle_pre_output_stream_failure(
    state: &CoreState,
    stream_state: &Arc<Mutex<StreamState>>,
    converter: &Arc<Mutex<StreamConverter>>,
    log_id: i64,
    pricing: &RequestPricingSnapshot,
    attempt: &ForwardAttemptContext,
    plan: &RequestPlan,
    upstream_status: StatusCode,
    upstream_wait_ms: u64,
    failure_status: StatusCode,
    error_source: &'static str,
    error_stage: &'static str,
    detail: &str,
    stream_input: StreamClassifyInput,
    allow_retry: bool,
) -> PreOutputFailure {
    let class = classify_stream(stream_input);
    let action = forward_action_for_class(class, allow_retry, None);
    let retry = matches!(action, ForwardAction::RetrySameAccount);
    let message = if retry {
        outcome_unknown_retry_message(detail)
    } else {
        outcome_unknown_message(detail)
    };
    {
        let mut stream = stream_state.lock();
        stream.error = true;
        stream.outcome_unknown = true;
        stream.error_message = Some(message.clone());
        stream.diagnostic_recorded = true;
    }
    let chunks = converter.lock().outcome_unknown_event(&message);
    let failure = attempt.failure(FailureSpec {
        error_source,
        error_stage,
        downstream_status: Some(upstream_status.as_u16()),
        upstream_status: Some(upstream_status.as_u16()),
        upstream_wait_ms: Some(upstream_wait_ms),
        retry_action: Some(retry_action_name(action)),
        upstream_headers: None,
        upstream_error: Some(detail),
        request_body: None,
    });
    let diagnostic = failure.update();
    let db = state.db.lock();
    if let Err(error) = DbAttemptSink::new(&db).finalize(
        log_id,
        "outcome_unknown",
        None,
        metadata_metrics(pricing, plan.service_tier.as_deref(), "outcome_unknown"),
        Some(&message),
        Some(&diagnostic),
        attempt,
    ) {
        let _ = db.log_gateway(
            "warn",
            "forwarder",
            &format!("failed to update streaming row {log_id}: {error}"),
        );
    }
    if retry {
        PreOutputFailure::Retry(ForwardResult {
            response: outcome_unknown_response_with_message(plan.client, failure_status, &message),
            action,
            error_message: Some(message),
        })
    } else {
        PreOutputFailure::Return(chunks)
    }
}

fn join_chunks(chunks: Vec<bytes::Bytes>) -> bytes::Bytes {
    let capacity = chunks.iter().map(bytes::Bytes::len).sum();
    let mut joined = BytesMut::with_capacity(capacity);
    for chunk in chunks {
        joined.extend_from_slice(&chunk);
    }
    joined.freeze()
}

pub(crate) fn headers_carry_upstream_secret(headers: &reqwest::header::HeaderMap) -> bool {
    headers.keys().any(|name| {
        matches!(
            name.as_str(),
            "authorization" | "x-api-key" | "x-goog-api-key"
        )
    })
}

fn ensure_safe_upstream_base_url(base: &str) -> Result<()> {
    let url = reqwest::Url::parse(base)?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(&url) => Ok(()),
        scheme => anyhow::bail!("unsafe upstream scheme or host: {scheme}"),
    }
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    )
}

fn sanitize_upstream_error(text: &str, known_secret: &str) -> String {
    let safe = sanitize_upstream_error_value_with_known_secret(text, known_secret);
    safe.get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| safe.to_string())
        .chars()
        .take(500)
        .collect()
}

fn response_body_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "upstream response body timed out".to_string()
    } else {
        format!("upstream response body failed: {error}")
    }
}

#[derive(Debug)]
enum ResponseBodyFailure {
    IdleTimeout(StdDuration),
    Transport(reqwest::Error),
}

impl ResponseBodyFailure {
    fn is_timeout(&self) -> bool {
        match self {
            Self::IdleTimeout(_) => true,
            Self::Transport(error) => error.is_timeout(),
        }
    }

    fn into_detail(self) -> String {
        match self {
            Self::IdleTimeout(timeout) => format!(
                "upstream response body timed out after {}s",
                timeout.as_secs()
            ),
            Self::Transport(error) => response_body_error(&error),
        }
    }
}

async fn response_text_with_timeout(
    response: reqwest::Response,
    timeout: Option<StdDuration>,
    max_bytes: Option<usize>,
) -> std::result::Result<String, ResponseBodyFailure> {
    let read = response_text(response, max_bytes);
    match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, read).await {
            Ok(result) => result.map_err(ResponseBodyFailure::Transport),
            Err(_) => Err(ResponseBodyFailure::IdleTimeout(timeout)),
        },
        None => read.await.map_err(ResponseBodyFailure::Transport),
    }
}

async fn response_text(
    response: reqwest::Response,
    max_bytes: Option<usize>,
) -> std::result::Result<String, reqwest::Error> {
    let Some(max_bytes) = max_bytes else {
        return response.text().await;
    };

    let read_limit = max_bytes.saturating_add(1);
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

    let truncated = body.len() > max_bytes;
    body.truncate(max_bytes);
    let mut text = String::from_utf8_lossy(&body).into_owned();
    if truncated {
        text.push_str("\n<upstream error body truncated>");
    }
    Ok(text)
}

fn error_response(format: ApiFormat, message: &str, upstream: Option<&Value>) -> Response {
    let body = format_error(format, StatusCode::BAD_GATEWAY, message, upstream);
    (StatusCode::BAD_GATEWAY, axum::Json(body)).into_response()
}

pub(crate) fn forward_action_for_class(
    class: ProviderErrorClass,
    allow_same_account_retry: bool,
    rate_limit_window: Option<UsageWindowKind>,
) -> ForwardAction {
    if class.same_account_retry_eligible() && allow_same_account_retry {
        return ForwardAction::RetrySameAccount;
    }
    match class {
        ProviderErrorClass::RouteUnavailable
        | ProviderErrorClass::DecryptFailed
        | ProviderErrorClass::UnauthorizedRotate
        | ProviderErrorClass::ForbiddenRotate => ForwardAction::TryNextAccount,
        ProviderErrorClass::RateLimited { .. } => match rate_limit_fallback(rate_limit_window) {
            RateLimitFallback::ExhaustFreeChannel => ForwardAction::ExhaustFreeChannel,
            RateLimitFallback::TryNextAccount => ForwardAction::TryNextAccount,
        },
        _ => ForwardAction::Return,
    }
}

fn no_replay_retry_action() -> &'static str {
    retry_action_name(forward_action_for_class(
        classify_stream(StreamClassifyInput::AfterDownstreamBytes),
        false,
        None,
    ))
}

fn retry_action_name(action: ForwardAction) -> &'static str {
    match action {
        ForwardAction::Return => "return",
        ForwardAction::RetrySameAccount => "retry_same_account",
        ForwardAction::TryNextAccount => "try_next_account",
        ForwardAction::ExhaustFreeChannel => "exhaust_free_channel",
    }
}

fn account_preflight_failure(plan: &RequestPlan, message: String) -> ForwardResult {
    ForwardResult {
        response: error_response(plan.client, &message, None),
        action: ForwardAction::TryNextAccount,
        error_message: Some(message),
    }
}

fn protocol_status_error_response(
    format: ApiFormat,
    status: StatusCode,
    message: &str,
    upstream: Option<&Value>,
) -> Response {
    let body = format_error(format, status, message, upstream);
    (status, axum::Json(body)).into_response()
}

fn outcome_unknown_message(detail: &str) -> String {
    format!(
        "upstream outcome is unknown: {detail}; the request may have completed and consumed quota; the gateway did not retry it"
    )
}

fn outcome_unknown_retry_message(detail: &str) -> String {
    format!(
        "upstream outcome is unknown: {detail}; the request may have completed and consumed quota; the gateway is retrying it once because no downstream SSE data was emitted"
    )
}

fn outcome_unknown_response(format: ApiFormat, status: StatusCode, detail: &str) -> Response {
    let message = outcome_unknown_message(detail);
    outcome_unknown_response_with_message(format, status, &message)
}

fn outcome_unknown_response_with_message(
    format: ApiFormat,
    status: StatusCode,
    message: &str,
) -> Response {
    let mut body = error_body(format, "upstream_outcome_unknown", message);
    if format == ApiFormat::Gemini {
        body["error"]["code"] = serde_json::json!(status.as_u16());
        body["error"]["status"] = serde_json::json!("UPSTREAM_OUTCOME_UNKNOWN");
    }
    (status, axum::Json(body)).into_response()
}

pub(crate) fn rate_limited_response(
    format: ApiFormat,
    resets_at: chrono::DateTime<Utc>,
) -> Response {
    let message = format!(
        "all accounts rate-limited, soonest resets at {}",
        resets_at.to_rfc3339()
    );
    let mut body = format_error(format, StatusCode::TOO_MANY_REQUESTS, &message, None);
    body["error"]["resets_at"] = serde_json::json!(resets_at.to_rfc3339());
    (StatusCode::TOO_MANY_REQUESTS, axum::Json(body)).into_response()
}

#[allow(clippy::too_many_arguments)]
fn log_forward(
    db: &Database,
    account: &Account,
    model: &str,
    status: &str,
    http_status: Option<i32>,
    mut metrics: ForwardMetrics,
    error_message: Option<&str>,
    context: &ForwardAttemptContext,
    failure: Option<FailureRecord>,
) -> Result<i64> {
    metrics.scope_to_provider(
        Some(account.provider_id.as_str()),
        status.starts_with("success"),
    );
    let cost_state = match (metrics.cost_state, status) {
        ("not_applicable", "outcome_unknown") => "outcome_unknown",
        ("not_applicable", "success_no_usage") => "usage_missing",
        ("not_applicable", "success_unpriced") => "unpriced",
        (state, _) => state,
    };
    let failure_value = failure
        .as_ref()
        .and_then(|failure| serde_json::from_str(&failure.diagnostic_json).ok());
    let persist_metrics = metrics.clone();
    let id = db.log_forward(&ForwardLog {
        id: 0,
        timestamp: Utc::now(),
        model: model.to_string(),
        account_id: account.id.clone(),
        account_name: account.name.clone(),
        route_account_id: context.route_account_id.clone(),
        provider_id: context.provider_id.clone(),

        credential_account_id: context.credential_account_id.clone(),
        client_key_id: context.client_key_id.clone(),
        client_key_name: context.client_key_name.clone(),
        status: if status.starts_with("success") {
            success_status_for_cost(cost_state).to_string()
        } else {
            status.to_string()
        },
        http_status,
        route: context.route.as_str().to_string(),
        prompt_tokens: metrics.prompt_tokens,
        completion_tokens: metrics.completion_tokens,
        cached_tokens: metrics.cached_tokens,
        cache_creation_tokens: metrics.cache_creation_tokens,
        cost: (cost_state == "priced").then_some(metrics.cost),
        raw_cost_usd: metrics.raw_cost_usd,
        quota_debit: metrics.quota_debit,
        effective_paid_cost_usd: metrics.effective_paid_cost_usd,
        pricing_revision_id: metrics.pricing_revision_id,
        quota_multiplier: metrics.quota_multiplier,
        local_adjustment_multiplier: metrics.local_adjustment_multiplier,
        service_tier: metrics.service_tier.clone(),
        cost_state: cost_state.to_string(),
        error_message: error_message.map(|message| context.redact_known_secret(message)),
        request_id: Some(context.trace.request_id.clone()),
        attempt: Some(context.attempt as i64),
        error_source: failure.as_ref().map(|failure| failure.error_source.clone()),
        error_stage: failure.as_ref().map(|failure| failure.error_stage.clone()),
        duration_ms: failure.as_ref().map(|failure| failure.duration_ms),
        diagnostic: failure_value,
    })?;
    persist_log_identity(db, id, context, &persist_metrics)?;
    Ok(id)
}

fn persist_log_identity(
    db: &Database,
    id: i64,
    context: &ForwardAttemptContext,
    metrics: &ForwardMetrics,
) -> Result<()> {
    let Some(mut attribution) = db.forward_log_native_attribution(id)? else {
        return Ok(());
    };
    attribution.requested_model = Some(context.requested_model.clone());
    attribution.resolved_alias = context.resolved_alias.clone();
    attribution.upstream_model = Some(context.upstream_model.clone());
    apply_platform_native_attribution(&mut attribution, context, metrics);
    db.set_forward_log_native_attribution(id, &attribution)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finalize_logged_forward(
    db: &Database,
    id: i64,
    status: &str,
    http_status: Option<i32>,
    metrics: ForwardMetrics,
    error_message: Option<&str>,
    diagnostic: Option<&ForwardLogDiagnosticUpdate<'_>>,
    context: &ForwardAttemptContext,
) -> Result<()> {
    db.update_forward_log(
        id,
        status,
        http_status,
        metrics.clone(),
        error_message,
        diagnostic,
    )?;
    persist_log_identity(db, id, context, &metrics)
}

fn success_status_for_cost(cost_state: &str) -> &'static str {
    match cost_state {
        "priced" | "free" => "success",
        "usage_missing" => "success_no_usage",
        _ => "success_unpriced",
    }
}

fn pricing_metrics(
    snapshot: &RequestPricingSnapshot,
    model: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    cached_tokens: i64,
    cache_creation_tokens: i64,
    service_tier: Option<&str>,
) -> ForwardMetrics {
    let estimate = snapshot.estimate(
        model,
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cache_creation_tokens,
        service_tier,
    );
    let provider_identity = snapshot.provider_identity();
    ForwardMetrics {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        cache_creation_tokens,
        cost: estimate.cost.unwrap_or(0.0),
        raw_cost_usd: estimate.raw_cost_usd,
        quota_debit: estimate.quota_debit,
        effective_paid_cost_usd: estimate.effective_paid_cost_usd,
        pricing_revision_id: estimate.pricing_revision_id,
        quota_multiplier: estimate.quota_multiplier,
        local_adjustment_multiplier: estimate.local_adjustment_multiplier,
        pricing_provider_id: provider_identity.map(str::to_string),

        service_tier: service_tier.map(str::to_string),
        cost_state: estimate.cost_state,
    }
}

fn metadata_metrics(
    snapshot: &RequestPricingSnapshot,
    service_tier: Option<&str>,
    cost_state: &'static str,
) -> ForwardMetrics {
    let provider_identity = snapshot.provider_identity();
    ForwardMetrics {
        pricing_revision_id: snapshot.revision().map(str::to_string),
        pricing_provider_id: provider_identity.map(str::to_string),

        service_tier: service_tier.map(str::to_string),
        cost_state,
        ..ForwardMetrics::default()
    }
}

// ----- SSE usage accumulation -----

// ponytail: single Mutex<StreamState> instead of 3 separate Arc<Mutex<>>/
// AtomicBool. Lock is held for a single chunk's processing (microseconds);
// upgrade to per-chunk allocator if cross-stream contention ever shows up.
#[derive(Default)]
struct StreamState {
    buf: BytesMut,
    usage: UsageCounts,
    has_usage: bool,
    terminal: bool,
    /// Set by the mapped Err arm so the finalizer can skip its status overwrite.
    error: bool,
    outcome_unknown: bool,
    error_message: Option<String>,
    diagnostic_recorded: bool,
}

// Match the stream converter's pending-frame cap. The old 64 KiB limit dropped
// DeepSeek flash reasoning bursts before the trailing usage chunk could be parsed.
const MAX_SSE_BUF: usize = 8 * 1024 * 1024;

// Accept LF and CRLF event boundaries in wire order, including mixed endings.
fn find_event_boundary(buf: &[u8]) -> Option<usize> {
    (0..buf.len().saturating_sub(1))
        .find(|&i| buf[i..].starts_with(b"\n\n") || buf[i..].starts_with(b"\r\n\r\n"))
}

fn event_boundary_len(buf: &[u8], start: usize) -> usize {
    if start + 3 < buf.len() && &buf[start..start + 4] == b"\r\n\r\n" {
        4
    } else {
        2
    }
}

fn extract_data_payload(event: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(event).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("data:") {
            parts.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

// ponytail: ignore_err on JSON parse — SSE frames may be comments or keep-alive
// heartbeats. Silent skip; the last non-null usage frame still wins.
// ponytail: bounded buffer — if the upstream never sends a complete event
// (malformed stream, CRLF-only chunks, dropped keep-alive framing), drop the
// garbage so memory can't grow unbounded.
fn process_chunk_for_usage(
    st: &mut StreamState,
    format: ApiFormat,
    chunk: &bytes::Bytes,
    model_hint: Option<&str>,
) {
    if st.terminal {
        return;
    }
    st.buf.extend_from_slice(chunk);
    loop {
        let bytes = st.buf.as_ref();
        let Some(idx) = find_event_boundary(bytes) else {
            break;
        };
        let take = event_boundary_len(bytes, idx);
        let event = st.buf.split_to(idx + take);
        if let Some(payload) = extract_data_payload(&event) {
            let payload = payload.trim();
            if payload == "[DONE]" {
                st.terminal = true;
                st.buf.clear();
                break;
            }
            if payload.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(payload) {
                let is_error = matches!(
                    v.get("type").and_then(Value::as_str),
                    Some("error" | "response.failed")
                ) || v.get("error").is_some_and(|error| !error.is_null());
                if is_error {
                    st.error = true;
                    st.error_message = Some(
                        v.pointer("/response/error/message")
                            .or_else(|| v.pointer("/error/message"))
                            .or_else(|| v.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("upstream stream error")
                            .to_string(),
                    );
                }
                if has_usage(format, &v) {
                    // Always retain the request model as the hint. Some compatible
                    // upstreams rewrite the response model to a generic alias, and
                    // extract_usage already combines that response model with this
                    // original hint when applying model-specific normalization.
                    merge_stream_usage(format, &v, &mut st.usage, model_hint);
                    st.has_usage = true;
                }
                let event_type = v.get("type").and_then(Value::as_str);
                let is_terminal = is_error
                    || match format {
                        ApiFormat::ChatCompletions => false,
                        ApiFormat::Messages => event_type == Some("message_stop"),
                        ApiFormat::Responses => matches!(
                            event_type,
                            Some("response.completed" | "response.incomplete")
                        ),
                        ApiFormat::Gemini => false,
                    };
                if is_terminal {
                    st.terminal = true;
                    st.buf.clear();
                    break;
                }
            }
        }
    }
    if st.buf.len() > MAX_SSE_BUF {
        st.buf.clear();
    }
}

fn token_counts(usage: UsageCounts) -> (i64, i64, i64, i64) {
    let to_i64 = |value: u64| value.min(i64::MAX as u64) as i64;
    (
        to_i64(usage.input_tokens),
        to_i64(usage.output_tokens),
        to_i64(usage.cached_tokens),
        to_i64(usage.cache_creation_tokens),
    )
}

#[cfg(test)]
mod stream_usage_tests {
    use super::*;
    use bytes::Bytes;

    fn usage_event() -> Vec<u8> {
        b"data: {\"id\":\"x\",\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":20,\"total_tokens\":30,\"prompt_tokens_details\":{\"cached_tokens\":5}}}\n\ndata: [DONE]\n\n".to_vec()
    }

    #[test]
    fn captured_allowlisted_upstream_header_redacts_known_secret() {
        let secret = format!("opaque/{}", "account-key".repeat(32));
        let context = ForwardAttemptContext {
            trace: RequestTrace::new(),
            client_body_bytes: 0,
            upstream_body_bytes: 0,
            attempt: 1,
            client_format: ApiFormat::ChatCompletions,
            upstream_format: ApiFormat::ChatCompletions,
            model: "test-model".into(),
            requested_model: "test-model".into(),
            resolved_alias: None,
            upstream_model: "test-model".into(),
            stream: false,
            route: RouteLabel::Proxy,
            known_secret: Some(secret.clone()),
            route_account_id: None,
            provider_id: None,

            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            platform_price: None,
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", format!("request-{secret}").parse().unwrap());
        let failure = context.failure(FailureSpec {
            error_source: "upstream",
            error_stage: "upstream_http",
            downstream_status: Some(500),
            upstream_status: Some(500),
            upstream_wait_ms: None,
            retry_action: Some("return"),
            upstream_headers: Some(&headers),
            upstream_error: None,
            request_body: None,
        });
        assert!(secret.len() > 256);
        assert!(
            !failure.diagnostic_json.contains(&secret[..256]),
            "truncated header prefix leaked Key: {}",
            failure.diagnostic_json
        );
        let diagnostic: Value = serde_json::from_str(&failure.diagnostic_json).unwrap();
        assert_eq!(
            diagnostic["upstream_headers"]["x-request-id"],
            "request-<redacted>"
        );
    }

    #[test]
    fn single_chunk_extracts_usage() {
        let mut st = StreamState::default();
        let chunk = Bytes::from(usage_event());
        process_chunk_for_usage(&mut st, ApiFormat::ChatCompletions, &chunk, None);
        assert!(st.has_usage, "usage should be set");
        let (p, c, cached, cache_creation) = token_counts(st.usage);
        assert_eq!(p, 10);
        assert_eq!(c, 20);
        assert_eq!(cached, 5);
        assert_eq!(cache_creation, 0);
        assert!(st.buf.is_empty(), "buffer should drain on full events");
    }

    #[test]
    fn chunk_boundary_handling() {
        let full = usage_event();
        let a = &full[..20];
        let b = &full[20..full.len() - 5];
        let c = &full[full.len() - 5..];

        let mut st = StreamState::default();
        process_chunk_for_usage(
            &mut st,
            ApiFormat::ChatCompletions,
            &Bytes::copy_from_slice(a),
            None,
        );
        process_chunk_for_usage(
            &mut st,
            ApiFormat::ChatCompletions,
            &Bytes::copy_from_slice(b),
            None,
        );
        process_chunk_for_usage(
            &mut st,
            ApiFormat::ChatCompletions,
            &Bytes::copy_from_slice(c),
            None,
        );

        assert!(st.has_usage, "usage should be set after boundary");
        let (p, c, cached, cache_creation) = token_counts(st.usage);
        assert_eq!((p, c, cached, cache_creation), (10, 20, 5, 0));
        assert!(st.buf.is_empty(), "buffer should be empty after all chunks");
    }

    #[test]
    fn no_usage_event_yields_none() {
        let mut st = StreamState::default();
        let payload =
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n".to_vec();
        process_chunk_for_usage(
            &mut st,
            ApiFormat::ChatCompletions,
            &Bytes::from(payload),
            None,
        );
        assert!(!st.has_usage, "no usage field means no usage");
        assert!(st.buf.is_empty());
    }

    #[test]
    fn last_non_null_usage_wins() {
        let mut st = StreamState::default();
        let first = b"data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\n\n".to_vec();
        let second = b"data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":200,\"prompt_tokens_details\":{\"cached_tokens\":50}}}\n\n".to_vec();
        process_chunk_for_usage(
            &mut st,
            ApiFormat::ChatCompletions,
            &Bytes::from(first),
            None,
        );
        process_chunk_for_usage(
            &mut st,
            ApiFormat::ChatCompletions,
            &Bytes::from(second),
            None,
        );
        assert!(st.has_usage, "usage set");
        let (p, c, cached, cache_creation) = token_counts(st.usage);
        assert_eq!((p, c, cached, cache_creation), (100, 200, 50, 0));
    }

    #[test]
    fn messages_stream_merges_start_and_delta_usage() {
        let mut st = StreamState::default();
        let start = Bytes::from_static(
            b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":6,\"cache_read_input_tokens\":4,\"cache_creation_input_tokens\":2}}}\n\n",
        );
        let delta = Bytes::from_static(
            b"event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":7}}\n\n",
        );
        process_chunk_for_usage(&mut st, ApiFormat::Messages, &start, None);
        process_chunk_for_usage(&mut st, ApiFormat::Messages, &delta, None);
        assert!(st.has_usage);
        assert_eq!(token_counts(st.usage), (12, 7, 4, 2));
    }

    #[test]
    fn messages_stream_sanitizes_minimax_bogus_cache_from_request_hint() {
        // Upstream may omit the model field in message_start or rewrite it to an
        // internal id, and the plan's hint may arrive in mixed case ("MiniMax-M3");
        // the request hint must still sanitize the bogus all-cache usage in every shape.
        for (hint, start_model) in [
            ("minimax-m3", None),
            ("minimax-m3", Some("ocg-generic")),
            ("MiniMax-M3", None),
        ] {
            let message = match start_model {
                Some(model) => format!(
                    "{{\"model\":\"{model}\",\"usage\":{{\"input_tokens\":0,\"output_tokens\":5,\"cache_read_input_tokens\":40500}}}}"
                ),
                None => "{\"usage\":{\"input_tokens\":0,\"output_tokens\":5,\"cache_read_input_tokens\":40500}}".to_string() };
            let start = Bytes::from(format!(
                "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{message}}}\n\n"
            ));
            let mut st = StreamState::default();
            process_chunk_for_usage(&mut st, ApiFormat::Messages, &start, Some(hint));
            assert!(st.has_usage, "hint={hint} start_model={start_model:?}");
            let (input, output, cached, _) = token_counts(st.usage);
            assert_eq!(
                (input, output, cached),
                (40500, 5, 0),
                "hint={hint} start_model={start_model:?}"
            );
        }
    }

    #[test]
    fn upstream_stream_error_marks_log_state() {
        let mut st = StreamState::default();
        let event = Bytes::from_static(
            b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"api_error\",\"message\":\"boom\"}}\n\n",
        );
        process_chunk_for_usage(&mut st, ApiFormat::Messages, &event, None);
        assert!(st.error);
        assert_eq!(st.error_message.as_deref(), Some("boom"));

        let mut responses = StreamState::default();
        let event = Bytes::from_static(
            b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_error\",\"message\":\"codex boom\"}}}\n\n",
        );
        process_chunk_for_usage(&mut responses, ApiFormat::Responses, &event, None);
        assert!(responses.error);
        assert_eq!(responses.error_message.as_deref(), Some("codex boom"));
    }

    #[test]
    fn terminal_usage_ignores_late_stream_errors() {
        let mut st = StreamState::default();
        let chunk = Bytes::from_static(
            b"event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":2}}}\n\nevent: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"late\"}}}\n\n",
        );
        process_chunk_for_usage(&mut st, ApiFormat::Responses, &chunk, None);
        assert!(st.terminal);
        assert!(!st.error);
        assert_eq!(token_counts(st.usage), (7, 2, 0, 0));

        let later = Bytes::from_static(
            b"event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"later\"}}}\n\n",
        );
        process_chunk_for_usage(&mut st, ApiFormat::Responses, &later, None);
        assert!(!st.error);
        assert_eq!(token_counts(st.usage), (7, 2, 0, 0));
    }

    #[test]
    fn crlf_event_boundary_is_detected() {
        // \r\n\r\n-terminated event must be split out, not accumulated.
        let mut st = StreamState::default();
        let payload =
            b"data: {\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":11}}\r\n\r\n".to_vec();
        process_chunk_for_usage(
            &mut st,
            ApiFormat::ChatCompletions,
            &Bytes::from(payload),
            None,
        );
        assert!(st.has_usage, "CRLF usage should be parsed");
        let (p, c, _, _) = token_counts(st.usage);
        assert_eq!((p, c), (7, 11));
        assert!(st.buf.is_empty());
    }

    #[test]
    fn buffer_bound_clears_on_oversize() {
        let mut st = StreamState::default();
        // Incomplete leftover larger than MAX_SSE_BUF is dropped after the drain.
        let big = vec![b'x'; MAX_SSE_BUF + 1];
        process_chunk_for_usage(&mut st, ApiFormat::ChatCompletions, &Bytes::from(big), None);
        assert!(
            st.buf.is_empty(),
            "oversize incomplete leftovers are dropped"
        );
        assert!(!st.has_usage);
    }

    #[test]
    fn large_reasoning_chunk_still_captures_trailing_usage() {
        // Flash-style burst: one HTTP chunk larger than the old 64 KiB scanner
        // cap, with usage in a later SSE event of the same chunk.
        let reasoning = "r".repeat(80_000);
        let payload = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"reasoning_content\":\"{reasoning}\"}}}}]}}\n\n\
             data: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":9,\"completion_tokens\":3}}}}\n\n\
             data: [DONE]\n\n"
        );
        let mut st = StreamState::default();
        process_chunk_for_usage(
            &mut st,
            ApiFormat::ChatCompletions,
            &Bytes::from(payload),
            Some("deepseek-v4-flash"),
        );
        assert!(
            st.has_usage,
            "usage after a large reasoning event must be kept"
        );
        assert_eq!(token_counts(st.usage), (9, 3, 0, 0));
        assert!(st.terminal);
        assert!(st.buf.is_empty());
    }
}

#[cfg(test)]
mod stream_outcome_guard_tests {
    use super::*;
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::gateway::diagnostics::RequestTrace;
    use crate::http_client::RouteLabel;
    use crate::kernel::protocol::ApiFormat;
    use crate::models::{Account, AccountSetupStep, AccountType};
    use crate::state::CoreStateInner;
    use chrono::Utc;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ocg-stream-guard-{}-{}",
            label,
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_state(label: &str) -> (PathBuf, CoreState) {
        let dir = temp_dir(label);
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("stream-guard"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        (dir, state)
    }

    fn account(state: &CoreState) -> Account {
        let now = Utc::now();
        Account {
            id: "acct-1".into(),
            provider_id: crate::provider::default_provider_id(),

            credential_kind: crate::provider::default_credential_kind(),
            quota_scope: crate::provider::default_quota_scope(),
            name: "acct-1".into(),
            username: None,
            password_cipher: None,
            key_cipher: state.encrypt_key("sk-guard").unwrap(),
            enabled: true,
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

    fn attempt_context() -> ForwardAttemptContext {
        ForwardAttemptContext {
            trace: RequestTrace::new(),
            client_body_bytes: 0,
            upstream_body_bytes: 0,
            attempt: 1,
            client_format: ApiFormat::ChatCompletions,
            upstream_format: ApiFormat::ChatCompletions,
            model: "deepseek-v4-flash".into(),
            requested_model: "deepseek-v4-flash".into(),
            resolved_alias: Some("deepseek-v4-flash".into()),
            upstream_model: "deepseek-v4-flash".into(),
            stream: true,
            route: RouteLabel::Direct,
            known_secret: None,
            route_account_id: Some("acct-1".into()),
            provider_id: Some(crate::provider::default_provider_id()),

            credential_account_id: Some("acct-1".into()),
            client_key_id: None,
            client_key_name: None,
            platform_price: None,
        }
    }

    fn insert_streaming_row(
        state: &CoreState,
        account: &Account,
        context: &ForwardAttemptContext,
    ) -> i64 {
        let pricing = RequestPricingSnapshot::from(state.pricing_snapshot());
        DbAttemptSink::new(&state.db.lock())
            .insert(
                account,
                "deepseek-v4-flash",
                "streaming",
                Some(200),
                metadata_metrics(&pricing, None, "not_applicable"),
                None,
                context,
                None,
            )
            .unwrap()
    }

    #[test]
    fn command_code_requests_use_the_verified_provider_price_and_multiplier() {
        let (dir, state) = test_state("goat-pricing");
        let mut goat = account(&state);
        goat.provider_id = crate::provider::COMMAND_CODE_PROVIDER_ID.into();
        let missing = RequestPricingSnapshot::for_account(&state, &goat, state.pricing_snapshot());
        let mut missing_metrics = pricing_metrics(
            &missing,
            "deepseek-v4-flash",
            1_000_000,
            100_000,
            0,
            0,
            None,
        );
        missing_metrics.scope_to_provider(Some(&goat.provider_id), true);
        assert_eq!(missing_metrics.cost_state, "unpriced");
        assert_eq!(missing_metrics.raw_cost_usd, None);
        assert_eq!(missing_metrics.pricing_revision_id, None);

        let snapshot = crate::pricing::ProviderScopedPricingSnapshot::new(
            crate::provider::COMMAND_CODE_PROVIDER_ID,
            "goat-runtime-test",
            "2030-01-01T00:00:00Z",
            None,
            crate::pricing::GOAT_SOURCE_URL,
            "goat-runtime-hash",
            crate::pricing::ProviderPricingEvidence::Verified,
            vec![
                crate::pricing::ProviderPricingValue::new(
                    "deepseek-v4-flash",
                    "DeepSeek V4 Flash (latest)",
                    Some(0.22),
                    Some(0.66),
                    Some(0.007),
                    None,
                    Some(70.0),
                    Some(60.0),
                    Some(10.0),
                    Some("USD".into()),
                    None,
                    None,
                    crate::pricing::PricingTimeWindow::Always,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        crate::pricing::store_provider_pricing_snapshot(&state.db.lock(), &snapshot).unwrap();

        let pricing = RequestPricingSnapshot::for_account(&state, &goat, state.pricing_snapshot());
        let mut metrics = pricing_metrics(
            &pricing,
            "deepseek/deepseek-v4-flash",
            1_000_000,
            100_000,
            0,
            0,
            None,
        );
        metrics.scope_to_provider(Some(&goat.provider_id), true);

        assert_eq!(metrics.cost_state, "priced");
        assert!((metrics.raw_cost_usd.unwrap() - 0.286).abs() < 1e-12);
        assert!((metrics.quota_multiplier.unwrap() - (70.0 / 60.0)).abs() < 1e-12);
        assert!((metrics.cost - (0.286 * 70.0 / 60.0)).abs() < 1e-12);
        assert_eq!(
            metrics.pricing_revision_id.as_deref(),
            Some("goat-runtime-test")
        );

        drop(state);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn drop_without_terminal_finalizes_outcome_unknown() {
        let (dir, state) = test_state("drop-unknown");
        let account = account(&state);
        state.db.lock().create_account(&account).unwrap();
        let context = attempt_context();
        let log_id = insert_streaming_row(&state, &account, &context);
        {
            let _guard = StreamOutcomeGuard::new(
                state.clone(),
                log_id,
                Arc::new(Mutex::new(StreamState::default())),
                "deepseek-v4-flash".into(),
                state.pricing_snapshot(),
                None,
                context,
                200,
                0,
            );
        }
        let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
        assert_eq!(log.status, "outcome_unknown");
        assert_eq!(log.cost_state, "outcome_unknown");
        assert_eq!(
            log.diagnostic
                .as_ref()
                .and_then(|value| value.get("error_stage"))
                .and_then(serde_json::Value::as_str),
            Some("downstream_disconnect")
        );
        drop(state);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn drop_after_terminal_usage_prices_from_stream_state_not_the_converter() {
        let (dir, state) = test_state("drop-usage");
        let account = account(&state);
        state.db.lock().create_account(&account).unwrap();
        let context = attempt_context();
        let log_id = insert_streaming_row(&state, &account, &context);
        let stream = StreamState {
            terminal: true,
            has_usage: true,
            usage: crate::gateway::protocol::UsageCounts {
                input_tokens: 11,
                output_tokens: 4,
                cached_tokens: 1,
                cache_creation_tokens: 0,
            },
            ..StreamState::default()
        };
        {
            let _guard = StreamOutcomeGuard::new(
                state.clone(),
                log_id,
                Arc::new(Mutex::new(stream)),
                "deepseek-v4-flash".into(),
                state.pricing_snapshot(),
                None,
                context,
                200,
                0,
            );
        }
        let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
        assert_ne!(log.status, "streaming");
        assert_eq!(log.prompt_tokens, 11);
        assert_eq!(log.completion_tokens, 4);
        assert_eq!(log.cached_tokens, 1);
        assert!(
            log.status.starts_with("success"),
            "terminal usage captured on Drop must finalize independently of StreamConverter: {}",
            log.status
        );
        drop(state);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn disarm_leaves_the_streaming_row_untouched() {
        let (dir, state) = test_state("disarm");
        let account = account(&state);
        state.db.lock().create_account(&account).unwrap();
        let context = attempt_context();
        let log_id = insert_streaming_row(&state, &account, &context);
        {
            let mut guard = StreamOutcomeGuard::new(
                state.clone(),
                log_id,
                Arc::new(Mutex::new(StreamState::default())),
                "deepseek-v4-flash".into(),
                state.pricing_snapshot(),
                None,
                context,
                200,
                0,
            );
            guard.disarm();
        }
        let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
        assert_eq!(log.status, "streaming");
        drop(state);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn db_attempt_sink_inserts_once_and_finalizes_the_same_row() {
        let (dir, state) = test_state("sink-once");
        let account = account(&state);
        state.db.lock().create_account(&account).unwrap();
        let context = attempt_context();
        let pricing = RequestPricingSnapshot::from(state.pricing_snapshot());
        let id = {
            let db = state.db.lock();
            let sink = DbAttemptSink::new(&db);
            let id = sink
                .insert(
                    &account,
                    "deepseek-v4-flash",
                    "streaming",
                    Some(200),
                    metadata_metrics(&pricing, None, "not_applicable"),
                    None,
                    &context,
                    None,
                )
                .unwrap();
            sink.finalize(
                id,
                "success",
                Some(200),
                metadata_metrics(&pricing, None, "not_applicable"),
                None,
                None,
                &context,
            )
            .unwrap();
            id
        };
        let logs = state.db.lock().list_forward_logs(10).unwrap();
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert_eq!(logs[0].id, id);
        assert_ne!(logs[0].status, "streaming");
        assert!(
            logs[0].status.starts_with("success"),
            "same-row finalize must leave one completed attempt: {}",
            logs[0].status
        );
        drop(state);
        let _ = fs::remove_dir_all(dir);
    }
}

const OPENCODE_ZEN_FREE_CLIENT: &str = "cli";
const OPENCODE_ZEN_FREE_USER_AGENT: &str = "opencode";

fn carries_opencode_session_header(provider_id: &str) -> bool {
    provider_id == crate::provider::OPENCODE_PROVIDER_ID
        || provider_id == crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID
}

/// Shared by inference and operational probes so a working account is not
/// rejected merely because its test omitted provider-required identity.
pub(crate) fn apply_provider_identity_headers(
    upstream: &mut reqwest::header::HeaderMap,
    client_headers: &HeaderMap,
    provider_id: &str,
    client: ApiFormat,
    model: &str,
    body: &[u8],
    request_id: &str,
) {
    if carries_opencode_session_header(provider_id) {
        let session =
            resolve_opencode_session_header(client_headers, client, model, body, request_id);
        upstream.insert("x-opencode-session", session.clone());
        copy_explicit_opencode_identity_headers(upstream, client_headers);
        if provider_id == crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID {
            apply_zen_free_identity_headers(upstream, &session, request_id);
        }
    }
}

fn resolve_opencode_session_header(
    headers: &HeaderMap,
    client: ApiFormat,
    model: &str,
    client_body: &[u8],
    request_id: &str,
) -> reqwest::header::HeaderValue {
    headers
        .get("x-opencode-session")
        .or_else(|| headers.get("x-session-id"))
        .or_else(|| headers.get("x-session-affinity"))
        .cloned()
        .or_else(|| {
            resolve_conversation_key(client, model, headers, client_body)
                .and_then(|key| format!("ocg-{key}").parse().ok())
        })
        .unwrap_or_else(|| {
            request_id
                .parse()
                .expect("generated request id must be a valid header value")
        })
}

fn copy_explicit_opencode_identity_headers(
    upstream: &mut reqwest::header::HeaderMap,
    client: &HeaderMap,
) {
    for name in [
        "x-opencode-client",
        "x-opencode-request",
        "x-opencode-project",
    ] {
        if let Some(value) = client.get(name) {
            upstream.insert(name, value.clone());
        }
    }
}

fn apply_zen_free_identity_headers(
    headers: &mut reqwest::header::HeaderMap,
    session: &reqwest::header::HeaderValue,
    request_id: &str,
) {
    if headers.get("x-opencode-client").is_none() {
        headers.insert(
            "x-opencode-client",
            reqwest::header::HeaderValue::from_static(OPENCODE_ZEN_FREE_CLIENT),
        );
    }
    if headers.get("x-opencode-request").is_none()
        && let Ok(value) = request_id.parse()
    {
        headers.insert("x-opencode-request", value);
    }
    if headers.get("x-opencode-project").is_none() {
        headers.insert("x-opencode-project", session.clone());
    }
    let ua_is_opencode = headers
        .get(reqwest::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("opencode"));
    if !ua_is_opencode {
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(OPENCODE_ZEN_FREE_USER_AGENT),
        );
    }
}

#[cfg(test)]
mod forward_once_tests {
    use super::*;

    #[test]
    fn send_failure_kinds_keep_stage0_classification() {
        let timeout = TransportSendFailure::from_send_error(false, true, "operation timed out");
        assert_eq!(
            classify_transport(timeout.kind.into()),
            ProviderErrorClass::OutcomeUnknown
        );
        assert!(timeout.timed_out);
        assert!(
            timeout.message.starts_with("upstream request timed out:"),
            "{}",
            timeout.message
        );

        let connect = TransportSendFailure::from_send_error(true, false, "connection refused");
        assert_eq!(
            classify_transport(connect.kind.into()),
            ProviderErrorClass::Connect
        );
        assert!(!connect.timed_out);
        assert!(
            connect.message.starts_with("upstream request failed:"),
            "{}",
            connect.message
        );

        let other = TransportSendFailure::from_send_error(false, false, "connection reset");
        assert_eq!(
            classify_transport(other.kind.into()),
            ProviderErrorClass::OutcomeUnknown
        );
        assert!(!other.timed_out);

        let connect_timeout =
            TransportSendFailure::from_send_error(true, true, "error trying to connect");
        assert_eq!(
            classify_transport(connect_timeout.kind.into()),
            ProviderErrorClass::Connect
        );
        assert!(connect_timeout.timed_out);
        assert!(
            connect_timeout
                .message
                .starts_with("upstream request timed out:"),
            "{}",
            connect_timeout.message
        );
    }

    #[test]
    fn zen_free_identity_fills_missing_headers_and_keeps_official_user_agent() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            "opencode/1.17.7".parse().unwrap(),
        );
        let session = reqwest::header::HeaderValue::from_static("ses_1");
        apply_zen_free_identity_headers(&mut headers, &session, "req_1");
        assert_eq!(headers.get("x-opencode-client").unwrap(), "cli");
        assert_eq!(headers.get("x-opencode-request").unwrap(), "req_1");
        assert_eq!(headers.get("x-opencode-project").unwrap(), "ses_1");
        assert_eq!(
            headers.get(reqwest::header::USER_AGENT).unwrap(),
            "opencode/1.17.7"
        );
    }

    #[test]
    fn zen_free_identity_replaces_foreign_user_agent_and_preserves_client_fields() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::USER_AGENT, "reqwest/0.12".parse().unwrap());
        headers.insert("x-opencode-client", "cli".parse().unwrap());
        headers.insert("x-opencode-request", "keep-me".parse().unwrap());
        headers.insert("x-opencode-project", "proj_keep".parse().unwrap());
        let session = reqwest::header::HeaderValue::from_static("ses_1");
        apply_zen_free_identity_headers(&mut headers, &session, "req_1");
        assert_eq!(
            headers.get(reqwest::header::USER_AGENT).unwrap(),
            "opencode"
        );
        assert_eq!(headers.get("x-opencode-client").unwrap(), "cli");
        assert_eq!(headers.get("x-opencode-request").unwrap(), "keep-me");
        assert_eq!(headers.get("x-opencode-project").unwrap(), "proj_keep");
    }

    #[test]
    fn opencode_session_header_is_go_and_zen_free_only() {
        assert!(carries_opencode_session_header(
            crate::provider::OPENCODE_PROVIDER_ID
        ));
        assert!(carries_opencode_session_header(
            crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID
        ));
        assert!(!carries_opencode_session_header(
            crate::provider::COMMAND_CODE_PROVIDER_ID
        ));
    }
}

#[cfg(test)]
mod tests;
