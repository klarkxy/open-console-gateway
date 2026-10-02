use crate::account_control::AccountControlHost;
use crate::custom_http::{build_custom_http_client_for_route, json_content_headers};
use crate::db::{Database, ForwardLogDiagnosticUpdate};
use crate::gateway::attempt::{
    AttemptSpec, AttemptTimeouts, AttemptTransportError, CredentialHandle, CredentialResolveError,
    CredentialResolver, ProxyRoutingModel, TransportFailureKind, TransportSendFailure,
    UpstreamAuth,
};
use crate::gateway::attempt_pricing::apply_native_cost_attribution;
use crate::gateway::classify::{
    PreflightKind, ProviderErrorClass, RateLimitFallback, StreamClassifyInput,
    TransportClassifyInput, classify_http, classify_preflight, classify_stream, classify_transport,
    rate_limit_fallback,
};
use crate::gateway::diagnostics::{
    ErrorDiagnostic, RequestTrace, api_format_name, emit_failure, emit_legacy_tool_compat,
    redact_known_secret, redact_known_secret_values, safe_upstream_headers,
    sanitize_upstream_error_value_with_known_secret, serialize_diagnostic,
};
use crate::gateway::failure::decode::{
    decode as decode_failure, openrouter_free_rejection, parse_retry_after, temporary_429_deadline,
};
use crate::gateway::materialize::native_log_identity;
use crate::gateway::protocol::{
    RequestPlan, UsageCounts, error_body, extract_usage, format_error, has_complete_usage,
    has_usage, merge_stream_usage, transform_response,
};
use crate::gateway::protocol_stream::StreamConverter;
use crate::gateway::recovery::{RecoveryPermit, ResourceSet, restriction_endpoint_identity};
use crate::gateway::routing::resolve_conversation_key;
use crate::http_client::RouteLabel;
use crate::kernel::protocol::ApiFormat;
use crate::models::{AppConfig, ForwardLog, ForwardMetrics, UsageWindowKind};
use crate::provider::ProviderAdapterKind;
use crate::routing_snapshot::ExecutionCredential;
use crate::state::CoreState;
use crate::usage_sync::spawn_reactive_usage_refresh;
use anyhow::Result;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::BytesMut;
use chrono::Utc;
use futures_util::StreamExt;
use ocg_domain::destination::{AdapterKind, sealed_capabilities};
use parking_lot::Mutex;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

mod attempt_http;
mod attempt_record;
mod identity;
mod live_send;
mod response_class;
mod settlement;
mod sse_usage;
mod stages;

use attempt_http::{
    MAX_UPSTREAM_ERROR_BODY_BYTES, build_attempt_request, ensure_safe_upstream_base_url,
    forward_once, join_chunks, response_text_with_timeout, sanitize_upstream_error,
};
use attempt_record::{
    AttemptSink, DbAttemptSink, FailureRecord, FailureSpec, ForwardAttemptContext,
    HostCredentialResolver, account_preflight_failure, no_replay_retry_action,
    outcome_unknown_message, outcome_unknown_response_with_message, outcome_unknown_retry_message,
    protocol_status_error_response, retry_action_name, success_status_for_cost,
};
use response_class::{
    error_response, is_openrouter_free_request, observe_free_rejection, observe_local_policy,
    observe_openrouter_free_rejection,
};
use settlement::{
    CreditRequestGuard, FinalizerState, PreOutputFailure, QuotaObservation, QuotaTrialGuard,
    StreamOutcomeGuard, StreamRead, explicit_nonquota_application_error,
    handle_pre_output_stream_failure,
};
use sse_usage::{StreamState, process_chunk_for_usage, token_counts};

use crate::gateway::attempt_pricing::{
    PlatformAttemptPrice, RequestPricingSnapshot, metadata_metrics, pricing_metrics,
};
#[cfg(test)]
pub(crate) use attempt_http::headers_carry_upstream_secret;
pub(crate) use attempt_record::{
    log_unsent_admission_skip, outcome_unknown_response, rate_limited_response,
};
pub(crate) use identity::apply_provider_identity_headers;
#[cfg(test)]
pub(crate) use identity::copy_explicit_opencode_identity_headers;
pub(crate) use live_send::{
    LiveSendAccountGate, LiveSendAuthError, LiveSendSelection, authorize_live_send_secret,
    confirm_live_send_secret, verify_execution_authorization,
};
pub(crate) use response_class::forward_action_for_class;

/// Host secret resolution for the account the outer loop selected. Live
/// account/binding/grant checks and decrypt share one DB read lock. Same-account
/// retry reuses the original captured selection.
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
    /// False when admission failed before the HTTP send. Inspect→acquire races
    /// must not consume the request send budget.
    pub(crate) sent: bool,
}

// Isolated attempt tests do not own a logical-request budget.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_request(
    client: &Client,
    route: RouteLabel,
    state: &CoreState,
    account: &ExecutionCredential,
    adapter: ProviderAdapterKind,
    config: &AppConfig,
    plan: &RequestPlan,
    trace: &RequestTrace,
    client_body: &[u8],
    attempt: u32,
    allow_same_account_retry: bool,
    headers: HeaderMap,
    pricing_snapshot: RequestPricingSnapshot,
    client_key_id: Option<&str>,
    attempt_spec: &AttemptSpec,
    selection: &LiveSendSelection,
) -> Result<ForwardResult> {
    forward_request_with_deadline(
        client,
        route,
        state,
        account,
        adapter,
        config,
        plan,
        trace,
        client_body,
        attempt,
        allow_same_account_retry,
        headers,
        pricing_snapshot,
        client_key_id,
        attempt_spec,
        selection,
        None,
    )
    .await
}

/// Production send boundary used by the executor. The concrete state remains
/// inside the forwarder; the executor names only this operation.
pub(crate) trait AttemptSender {
    fn send_attempt<'a>(
        &'a self,
        request: AttemptRequest<'a>,
    ) -> impl std::future::Future<Output = Result<ForwardResult>> + Send + 'a;
}

pub(crate) struct AttemptRequest<'a> {
    pub client: &'a Client,
    pub route: RouteLabel,
    pub account: &'a ExecutionCredential,
    pub adapter: ProviderAdapterKind,
    pub config: &'a AppConfig,
    pub plan: &'a RequestPlan,
    pub trace: &'a RequestTrace,
    pub client_body: &'a [u8],
    pub attempt: u32,
    pub allow_same_account_retry: bool,
    pub headers: HeaderMap,
    pub pricing_snapshot: RequestPricingSnapshot,
    pub client_key_id: Option<&'a str>,
    pub attempt_spec: &'a AttemptSpec,
    pub selection: &'a LiveSendSelection,
    pub deadline: Option<tokio::time::Instant>,
}

impl AttemptSender for CoreState {
    fn send_attempt<'a>(
        &'a self,
        request: AttemptRequest<'a>,
    ) -> impl std::future::Future<Output = Result<ForwardResult>> + Send + 'a {
        forward_request_with_deadline(
            request.client,
            request.route,
            self,
            request.account,
            request.adapter,
            request.config,
            request.plan,
            request.trace,
            request.client_body,
            request.attempt,
            request.allow_same_account_retry,
            request.headers,
            request.pricing_snapshot,
            request.client_key_id,
            request.attempt_spec,
            request.selection,
            request.deadline,
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_request_with_deadline(
    client: &Client,
    route: RouteLabel,
    state: &CoreState,
    account: &ExecutionCredential,
    adapter: ProviderAdapterKind,
    config: &AppConfig,
    plan: &RequestPlan,
    trace: &RequestTrace,
    client_body: &[u8],
    attempt: u32,
    allow_same_account_retry: bool,
    headers: HeaderMap,
    pricing_snapshot: RequestPricingSnapshot,
    client_key_id: Option<&str>,
    attempt_spec: &AttemptSpec,
    selection: &LiveSendSelection,
    request_deadline: Option<tokio::time::Instant>,
) -> Result<ForwardResult> {
    let stages::PreparedAttempt {
        policy_provider_id,
        openrouter_free,
        pricing_snapshot,
        mut attempt_context,
        key,
        model,
        free_contract,
        restriction_endpoint,
        mut recovery_permit,
        quota_observation,
        quota_trial,
        mut credit_guard,
        request,
        timeouts,
    } = match stages::prepare_attempt(
        client,
        route,
        state,
        account,
        adapter,
        config,
        plan,
        trace,
        client_body,
        attempt,
        headers,
        pricing_snapshot,
        client_key_id,
        attempt_spec,
        selection,
        allow_same_account_retry,
        request_deadline,
    )
    .await?
    {
        Ok(prepared) => prepared,
        Err(result) => return Ok(result),
    };
    let stages::SentAttempt {
        upstream_started,
        upstream_resp,
    } = match stages::send_attempt(
        request,
        timeouts,
        plan,
        state,
        account,
        &model,
        &pricing_snapshot,
        &mut attempt_context,
        client_body,
        allow_same_account_retry,
        free_contract,
        openrouter_free,
        selection,
        &mut recovery_permit,
        &restriction_endpoint,
    )
    .await?
    {
        Ok(sent) => sent,
        Err(result) => return Ok(result),
    };

    let stages::ClassifiedAttempt {
        upstream_resp,
        status,
        is_stream,
        body_timeout,
        upstream_wait_ms,
    } = match stages::classify_attempt_response(
        upstream_resp,
        upstream_started,
        state,
        account,
        plan,
        trace,
        &model,
        &pricing_snapshot,
        &mut attempt_context,
        client_body,
        attempt,
        allow_same_account_retry,
        &key,
        free_contract,
        openrouter_free,
        selection,
        &mut recovery_permit,
        &restriction_endpoint,
        &quota_observation,
        config,
        request_deadline,
        policy_provider_id,
        attempt_spec,
        adapter,
    )
    .await?
    {
        Ok(classified) => classified,
        Err(result) => return Ok(result),
    };

    if is_stream {
        return stages::stream_attempt_response(
            upstream_resp,
            status,
            upstream_wait_ms,
            state,
            account,
            plan,
            model.clone(),
            &pricing_snapshot,
            &attempt_context,
            config,
            request_deadline,
            allow_same_account_retry,
            attempt_spec,
            recovery_permit,
            quota_trial,
            &mut credit_guard,
        )
        .await;
    }
    stages::buffered_attempt_response(
        upstream_resp,
        status,
        body_timeout,
        upstream_wait_ms,
        state,
        account,
        plan,
        &model,
        &pricing_snapshot,
        &mut attempt_context,
        client_body,
        allow_same_account_retry,
        free_contract,
        openrouter_free,
        selection,
        &mut recovery_permit,
        &restriction_endpoint,
        quota_trial,
        policy_provider_id,
        attempt_spec,
    )
    .await
}

#[cfg(test)]
mod tests;
