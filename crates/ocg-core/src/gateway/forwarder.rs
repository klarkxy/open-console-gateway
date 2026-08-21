use crate::db::{Database, ForwardLogDiagnosticUpdate};
use crate::gateway::diagnostics::{
    ErrorDiagnostic, RequestTrace, api_format_name, emit_failure, redact_known_secret,
    redact_known_secret_values, safe_upstream_headers,
    sanitize_upstream_error_value_with_known_secret, serialize_diagnostic,
};
use crate::gateway::limit::{parse_free_reset_or_default, parse_reset, parse_usage_limit_window};
use crate::gateway::materialize::native_log_identity;
use crate::gateway::protocol::{
    ApiFormat, RequestPlan, UsageCounts, error_body, extract_usage, format_error,
    has_complete_usage, has_usage, merge_stream_usage, transform_response,
};
use crate::gateway::protocol_stream::StreamConverter;
use crate::gateway::provider_adapter::{self, UpstreamAuth};
use crate::gateway::selector::AccountSelector;
use crate::models::{
    Account, AppConfig, ForwardLog, ForwardMetrics, UpstreamChannel, UsageWindowKind,
};
use crate::pricing::PricingSnapshot;
use crate::provider::is_command_code_goat;
use crate::state::CoreState;
use anyhow::Result;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::BytesMut;
use chrono::{Duration, Utc};
use futures_util::StreamExt;
use parking_lot::Mutex;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

const MAX_UPSTREAM_ERROR_BODY_BYTES: usize = 64 * 1024;

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
    known_secret: Option<String>,
    route_account_id: Option<String>,
    provider_id: Option<String>,
    offering_id: Option<String>,
    credential_account_id: Option<String>,
    client_key_id: Option<String>,
    client_key_name: Option<String>,
}

impl ForwardAttemptContext {
    fn new(
        trace: &RequestTrace,
        client_body_bytes: usize,
        attempt: u32,
        plan: &RequestPlan,
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
            known_secret: None,
            route_account_id: None,
            provider_id: None,
            offering_id: None,
            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
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

    fn set_provider_route(
        &mut self,
        account: &Account,
        route: &provider_adapter::ResolvedProviderRoute,
    ) {
        self.route_account_id = Some(account.id.clone());
        self.provider_id = Some(account.provider_id.clone());
        self.offering_id = Some(account.offering_id.clone());
        self.credential_account_id = route.credential_account_id.clone();
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
pub async fn forward_request(
    client: &Client,
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
) -> Result<ForwardResult> {
    forward_request_impl(
        client,
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
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn forward_request_impl(
    client: &Client,
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
) -> Result<ForwardResult> {
    let mut attempt_context = ForwardAttemptContext::new(trace, client_body.len(), attempt, plan);
    attempt_context.set_client_key(client_key_id, state);
    let provider_route = match provider_adapter::resolve_route(account, config, plan) {
        Ok(route) => route,
        Err(error) => {
            let message = format!("provider route is unavailable: {error}");
            let failure = attempt_context.failure(FailureSpec {
                error_source: "gateway",
                error_stage: "provider_route",
                downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                upstream_status: None,
                upstream_wait_ms: None,
                retry_action: Some("try_next_account"),
                upstream_headers: None,
                upstream_error: None,
                request_body: Some(client_body),
            });
            log_forward(
                &state.db.lock(),
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
    attempt_context.set_provider_route(account, &provider_route);
    ensure_safe_upstream_base_url(&provider_route.base_url)?;
    let key = if provider_route.auth == UpstreamAuth::None {
        None
    } else {
        match state.decrypt_key(&account.key_cipher) {
            Ok(key) => Some(key),
            Err(error) => {
                let message = format!("failed to decrypt account credentials: {error}");
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "gateway",
                    error_stage: "credential",
                    downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                    upstream_status: None,
                    upstream_wait_ms: None,
                    retry_action: Some("try_next_account"),
                    upstream_headers: None,
                    upstream_error: None,
                    request_body: Some(client_body),
                });
                log_forward(
                    &state.db.lock(),
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
        ) || (plan.upstream != ApiFormat::Messages
            && matches!(header.as_str(), "anthropic-version" | "anthropic-beta")))
        {
            upstream_headers.insert(name.clone(), value.clone());
        }
    }
    // Match the provider offering's authentication contract. The client wire
    // protocol alone is not an authentication decision.
    upstream_headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    let resolved_auth = match provider_route.auth {
        UpstreamAuth::OpenCodeProtocolDefault if plan.upstream == ApiFormat::Messages => {
            UpstreamAuth::XApiKey
        }
        UpstreamAuth::OpenCodeProtocolDefault => UpstreamAuth::Bearer,
        auth => auth,
    };
    if matches!(resolved_auth, UpstreamAuth::Bearer | UpstreamAuth::XApiKey) {
        let key = key
            .as_deref()
            .expect("credential-bearing provider route must decrypt a key");
        let key_header = match reqwest::header::HeaderValue::from_str(key) {
            Ok(value) => value,
            Err(error) => {
                let message = format!("account key is not a valid upstream header value: {error}");
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "gateway",
                    error_stage: "credential",
                    downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                    upstream_status: None,
                    upstream_wait_ms: None,
                    retry_action: Some("try_next_account"),
                    upstream_headers: None,
                    upstream_error: None,
                    request_body: Some(client_body),
                });
                log_forward(
                    &state.db.lock(),
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

    let upstream_path = if provider_route.path.is_empty() {
        provider_route
            .upstream
            .upstream_path()
            .ok_or_else(|| anyhow::anyhow!("Gemini is a client-only protocol"))?
            .to_string()
    } else {
        provider_route.path.clone()
    };
    let url = format!(
        "{}{}",
        provider_route.base_url.trim_end_matches('/'),
        upstream_path
    );

    let goat_client;
    let send_client = if !provider_route.follow_redirects {
        goat_client = crate::http_client::build_no_redirect(config)?;
        &goat_client
    } else {
        client
    };

    let model = plan.model.clone();
    let upstream_req = send_client
        .post(&url)
        .headers(upstream_headers)
        .body(plan.body.clone());
    let upstream_started = Instant::now();
    let upstream_resp = if plan.stream {
        match tokio::time::timeout(
            StdDuration::from_secs(config.stream_idle_timeout_secs),
            upstream_req.send(),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => {
                let detail = format!(
                    "upstream did not return response headers within {}s",
                    config.stream_idle_timeout_secs
                );
                let error_message = outcome_unknown_message(&detail);
                let upstream_wait_ms = upstream_started.elapsed().as_millis() as u64;
                let failure = attempt_context.failure(FailureSpec {
                    error_source: "transport",
                    error_stage: "response_headers",
                    downstream_status: Some(StatusCode::GATEWAY_TIMEOUT.as_u16()),
                    upstream_status: None,
                    upstream_wait_ms: Some(upstream_wait_ms),
                    retry_action: Some("return"),
                    upstream_headers: None,
                    upstream_error: Some(&detail),
                    request_body: Some(client_body),
                });
                {
                    let db = state.db.lock();
                    log_forward(
                        &db,
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
                    action: ForwardAction::Return,
                    error_message: Some(error_message),
                });
            }
        }
    } else {
        upstream_req
            .timeout(StdDuration::from_secs(config.non_stream_timeout_secs))
            .send()
            .await
    };

    let upstream_resp = match upstream_resp {
        Ok(resp) => resp,
        Err(e) => {
            let upstream_wait_ms = upstream_started.elapsed().as_millis() as u64;
            let connect_failure = e.is_connect();
            let outcome_unknown = !connect_failure;
            let detail = if e.is_timeout() {
                format!("upstream request timed out: {e}")
            } else {
                format!("upstream request failed: {e}")
            };
            let error_message = if outcome_unknown {
                outcome_unknown_message(&detail)
            } else {
                detail.clone()
            };
            let status = if e.is_timeout() {
                StatusCode::GATEWAY_TIMEOUT
            } else {
                StatusCode::BAD_GATEWAY
            };
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
                retry_action: Some(if connect_failure && allow_same_account_retry {
                    "retry_same_account"
                } else {
                    "return"
                }),
                upstream_headers: None,
                upstream_error: Some(&detail),
                request_body: Some(client_body),
            });
            {
                let db = state.db.lock();
                log_forward(
                    &db,
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
                action: if connect_failure && allow_same_account_retry {
                    ForwardAction::RetrySameAccount
                } else {
                    ForwardAction::Return
                },
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
            retry_action: Some("return"),
            upstream_headers: Some(&error_headers),
            upstream_error: Some(&text),
            request_body: Some(client_body),
        });
        {
            let db = state.db.lock();
            log_forward(
                &db,
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
            action: ForwardAction::Return,
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

        if status.as_u16() == 429 {
            // Go usage windows and Zen free promo limits both arrive as 429; cool the matching channel.
            // Unlike a rejected 429, ambiguous transport failures and 5xx responses are not replayed.
            // The endpoint/channel is authoritative. Free providers sometimes
            // reuse Go-window wording (5-hour/weekly/monthly); that text must not
            // downgrade an IP-shared Free 429 into per-account key rotation.
            // Command Code GOAT 429 is a generic provider-key cooldown: never parse
            // OpenCode Go limit windows and never schedule Go usage sync.
            let goat = is_command_code_goat(&account.provider_id, &account.offering_id);
            let (window, cooldown) = rate_limit_window_and_cooldown(goat, plan.channel, &text);
            let until = Utc::now() + cooldown;
            let sanitized = attempt_context.sanitize_upstream_error(&text);
            let error_message = format!(
                "rate limited: {} (resets in {}s)",
                sanitized,
                cooldown.num_seconds()
            );
            let action = action_for_usage_limit(window, plan.allow_go_fallback);
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
                log_forward(
                    &db,
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
            if !goat && plan.channel != UpstreamChannel::Free {
                crate::usage_sync::schedule_after_inference_429(state, &account.id);
            }
            return Ok(ForwardResult {
                response: error_response(plan.client, &error_message, None),
                action,
                error_message: Some(error_message),
            });
        }

        if status.as_u16() == 408 {
            let detail = format!(
                "upstream returned 408: {}",
                attempt_context.sanitize_upstream_error(&text)
            );
            let error_message = outcome_unknown_message(&detail);
            let failure = attempt_context.failure(FailureSpec {
                error_source: "upstream",
                error_stage: "upstream_http",
                downstream_status: Some(StatusCode::GATEWAY_TIMEOUT.as_u16()),
                upstream_status: Some(status.as_u16()),
                upstream_wait_ms: Some(upstream_wait_ms),
                retry_action: Some("return"),
                upstream_headers: Some(&error_headers),
                upstream_error: Some(&text),
                request_body: Some(client_body),
            });
            {
                let db = state.db.lock();
                log_forward(
                    &db,
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
                action: ForwardAction::Return,
                error_message: Some(error_message),
            });
        }

        // Key-level auth failures may be isolated to this account; fail over.
        if matches!(status.as_u16(), 401 | 403) {
            let anonymous_route = provider_route.auth == UpstreamAuth::None;
            let error_message = if anonymous_route {
                format!(
                    "anonymous provider route was rejected with {}; no credential fallback was attempted: {}",
                    status.as_u16(),
                    attempt_context.sanitize_upstream_error(&text)
                )
            } else {
                format!(
                    "upstream auth error {}: {}",
                    status.as_u16(),
                    attempt_context.sanitize_upstream_error(&text)
                )
            };
            let sanitized = attempt_context.sanitize_upstream_error(&text);
            let action = if anonymous_route {
                ForwardAction::Return
            } else {
                ForwardAction::TryNextAccount
            };
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
                log_forward(
                    &db,
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
                if !anonymous_route {
                    db.set_account_auth_error_if_key_matches(
                        &account.id,
                        &account.key_cipher,
                        Some(&error_message),
                    )?;
                }
            }
            return Ok(ForwardResult {
                response: error_response(plan.client, &error_message, None),
                action,
                error_message: Some(error_message),
            });
        }

        // Other 4xx: request-level error. Convert its envelope for the caller,
        // but don't retry another account for the same invalid request.
        let sanitized = attempt_context.sanitize_upstream_error(&text);
        let failure = attempt_context.failure(FailureSpec {
            error_source: "upstream",
            error_stage: "upstream_http",
            downstream_status: Some(status.as_u16()),
            upstream_status: Some(status.as_u16()),
            upstream_wait_ms: Some(upstream_wait_ms),
            retry_action: Some("return"),
            upstream_headers: Some(&error_headers),
            upstream_error: Some(&text),
            request_body: Some(client_body),
        });
        {
            let db = state.db.lock();
            log_forward(
                &db,
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
            action: ForwardAction::Return,
            error_message: None,
        });
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
            log_forward(
                &db,
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
        let converter = Arc::new(Mutex::new(StreamConverter::new_with_known_secret(
            plan,
            attempt_context.known_secret.as_deref(),
        )));
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
                                false,
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
                        true,
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
                                true,
                                allow_same_account_retry,
                            ) {
                                PreOutputFailure::Retry(result) => return Ok(result),
                                PreOutputFailure::Return(chunks) => break (chunks, true),
                            }
                        }
                    }
                }
                Err(_) => {
                    let detail = format!(
                        "upstream stream idle timeout after {}s",
                        stream_idle_timeout_secs
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
                        StatusCode::GATEWAY_TIMEOUT,
                        "transport",
                        "stream",
                        &detail,
                        false,
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
                                        retry_action: Some("return"),
                                        upstream_headers: None,
                                        upstream_error: Some(&detail),
                                        request_body: None,
                                    });
                                    let diagnostic = failure.update();
                                    let db = state_h.db.lock();
                                    let _ = finalize_logged_forward(
                                        &db,
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
                                retry_action: Some("return"),
                                upstream_headers: None,
                                upstream_error: Some(&detail),
                                request_body: None,
                            });
                            let diagnostic = failure.update();
                            let db = state_h.db.lock();
                            let _ = finalize_logged_forward(
                                &db,
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
                                "upstream stream idle timeout after {}s",
                                stream_idle_timeout_secs
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
                                retry_action: Some("return"),
                                upstream_headers: None,
                                upstream_error: Some(&detail),
                                request_body: None,
                            });
                            let diagnostic = failure.update();
                            let db = state_h.db.lock();
                            let _ = finalize_logged_forward(
                                &db,
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
                                retry_action: Some("return"),
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
                                    retry_action: Some("return"),
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
                        if let Err(e) = finalize_logged_forward(
                            &db,
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
                                &format!("failed to finalize streaming row {}: {}", initial_id, e),
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
                    retry_action: Some("return"),
                    upstream_headers: None,
                    upstream_error: Some(&detail),
                    request_body: Some(client_body),
                });
                {
                    let db = state.db.lock();
                    log_forward(
                        &db,
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
                    action: ForwardAction::Return,
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
                log_forward(
                    &db,
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
                log_forward(
                    &db,
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
            log_forward(
                &db,
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
    pricing: Arc<PricingSnapshot>,
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
        pricing: Arc<PricingSnapshot>,
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
            pricing,
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
        if let Err(error) = finalize_logged_forward(
            &db,
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
    pricing: &PricingSnapshot,
    attempt: &ForwardAttemptContext,
    plan: &RequestPlan,
    upstream_status: StatusCode,
    upstream_wait_ms: u64,
    failure_status: StatusCode,
    error_source: &'static str,
    error_stage: &'static str,
    detail: &str,
    retryable: bool,
    allow_retry: bool,
) -> PreOutputFailure {
    let retry = retryable && allow_retry;
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
        retry_action: Some(if retry {
            "retry_same_account"
        } else {
            "return"
        }),
        upstream_headers: None,
        upstream_error: Some(detail),
        request_body: None,
    });
    let diagnostic = failure.update();
    let db = state.db.lock();
    if let Err(error) = finalize_logged_forward(
        &db,
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
            action: ForwardAction::RetrySameAccount,
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

/// Simple GET forward for endpoints like /v1/models — uses configured selection strategy.
pub async fn forward_get(
    client: &Client,
    state: &CoreState,
    config: &AppConfig,
    upstream_path: &str,
) -> Result<Response> {
    ensure_safe_upstream_base_url(&config.upstream_base_url)?;
    let mut failed_ids = Vec::new();
    let mut last_http_error = None;
    let mut last_transport_error = None;

    loop {
        let account = {
            let db = state.db.lock();
            let excluded = failed_ids.iter().map(String::as_str).collect::<Vec<_>>();
            db.list_accounts()?.into_iter().find(|account| {
                provider_adapter::supports_model_discovery(account)
                    && AccountSelector::is_available_for(account, UpstreamChannel::Go, &excluded)
            })
        };
        let Some(account) = account else {
            if let Some(until) = state.db.lock().soonest_cooldown_reset()? {
                return Ok(rate_limited_response(ApiFormat::ChatCompletions, until));
            }
            if let Some((status, body)) = last_http_error {
                let mut headers = HeaderMap::new();
                headers.insert("content-type", HeaderValue::from_static("application/json"));
                return Ok((status, headers, body).into_response());
            }
            if let Some(error) = last_transport_error {
                return Err(error);
            }
            anyhow::bail!("no enabled accounts available");
        };

        let key = match state.decrypt_key(&account.key_cipher) {
            Ok(key) => key,
            Err(error) => {
                last_transport_error = Some(error);
                failed_ids.push(account.id);
                continue;
            }
        };
        let authorization = match HeaderValue::from_str(&format!("Bearer {key}")) {
            Ok(value) => value,
            Err(error) => {
                last_transport_error = Some(anyhow::anyhow!(
                    "account key is not a valid upstream header value: {error}"
                ));
                failed_ids.push(account.id);
                continue;
            }
        };
        let url = format!(
            "{}{}",
            config.upstream_base_url.trim_end_matches('/'),
            upstream_path
        );
        let mut retried_same_account = false;

        loop {
            let resp = match client
                .get(&url)
                .header(reqwest::header::AUTHORIZATION, authorization.clone())
                .timeout(StdDuration::from_secs(config.non_stream_timeout_secs))
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    // A connect error occurs before an HTTP response exists and
                    // is the only transport failure safe to repeat. Retry this
                    // account once, then return the failure without trying a
                    // different account. Any post-connect failure is ambiguous.
                    if error.is_connect() && !retried_same_account {
                        retried_same_account = true;
                        continue;
                    }
                    return Err(error.into());
                }
            };

            let status = resp.status();
            let body = match resp.text().await {
                Ok(body) => body,
                Err(error) => {
                    // Headers may already represent a completed upstream call;
                    // never replay when reading the response body fails.
                    return Err(anyhow::anyhow!(response_body_error(&error)));
                }
            };
            let downstream_body = if status.is_success() {
                redact_success_body(&body, &key)
            } else {
                redact_known_secret(&body, &key)
            };

            if status.as_u16() == 429 {
                let db = state.db.lock();
                let cooldown = parse_reset(&body).unwrap_or_else(|| Duration::minutes(5));
                let sanitized = sanitize_upstream_error(&body, &key);
                db.set_account_rate_limit_if_key_matches(
                    &account.id,
                    &account.key_cipher,
                    Utc::now() + cooldown,
                    &sanitized,
                    parse_usage_limit_window(&body),
                )?;
                drop(db);
                crate::usage_sync::schedule_after_inference_429(state, &account.id);
            }
            if status == StatusCode::UNAUTHORIZED {
                let auth_error = format!(
                    "upstream auth error 401: {}",
                    sanitize_upstream_error(&body, &key)
                );
                state.db.lock().set_account_auth_error_if_key_matches(
                    &account.id,
                    &account.key_cipher,
                    Some(&auth_error),
                )?;
            }
            if matches!(status.as_u16(), 401 | 403 | 429) {
                last_http_error = Some((status, downstream_body));
                failed_ids.push(account.id.clone());
                break;
            }

            let mut headers = HeaderMap::new();
            headers.insert("content-type", HeaderValue::from_static("application/json"));
            let mut response = (status, headers, downstream_body).into_response();
            if status == StatusCode::PAYLOAD_TOO_LARGE {
                response
                    .extensions_mut()
                    .insert(UpstreamPayloadTooLargeResponse);
            }
            return Ok(response);
        }
    }
}

fn ensure_safe_upstream_base_url(base: &str) -> Result<()> {
    let url = reqwest::Url::parse(base)?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(&url) => Ok(()),
        scheme => anyhow::bail!("unsafe upstream scheme or host: {}", scheme),
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

fn redact_success_body(text: &str, known_secret: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return redact_known_secret(text, known_secret);
    };
    redact_known_secret_values(&mut value, known_secret);
    serde_json::to_string(&value).unwrap_or_else(|_| redact_known_secret(text, known_secret))
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

fn usage_limit_window_for_channel(channel: UpstreamChannel, text: &str) -> Option<UsageWindowKind> {
    if channel == UpstreamChannel::Free {
        Some(UsageWindowKind::Free)
    } else {
        parse_usage_limit_window(text)
    }
}

fn rate_limit_window_and_cooldown(
    goat: bool,
    channel: UpstreamChannel,
    text: &str,
) -> (Option<UsageWindowKind>, Duration) {
    if goat {
        return (None, Duration::minutes(5));
    }
    let window = usage_limit_window_for_channel(channel, text);
    let cooldown = if window == Some(UsageWindowKind::Free) {
        parse_free_reset_or_default(text)
    } else {
        parse_reset(text).unwrap_or_else(|| Duration::minutes(5))
    };
    (window, cooldown)
}

fn action_for_usage_limit(
    window: Option<UsageWindowKind>,
    allow_go_fallback: bool,
) -> ForwardAction {
    if window == Some(UsageWindowKind::Free) {
        if allow_go_fallback {
            ForwardAction::ExhaustFreeChannel
        } else {
            ForwardAction::Return
        }
    } else {
        ForwardAction::TryNextAccount
    }
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
        Some(account.offering_id.as_str()),
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
    let id = db.log_forward(&ForwardLog {
        id: 0,
        timestamp: Utc::now(),
        model: model.to_string(),
        account_id: account.id.clone(),
        account_name: account.name.clone(),
        route_account_id: context.route_account_id.clone(),
        provider_id: context.provider_id.clone(),
        offering_id: context.offering_id.clone(),
        credential_account_id: context.credential_account_id.clone(),
        client_key_id: context.client_key_id.clone(),
        client_key_name: context.client_key_name.clone(),
        status: if status.starts_with("success") {
            success_status_for_cost(cost_state).to_string()
        } else {
            status.to_string()
        },
        http_status,
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
        service_tier: metrics.service_tier,
        cost_state: cost_state.to_string(),
        error_message: error_message.map(|message| context.redact_known_secret(message)),
        request_id: Some(context.trace.request_id.clone()),
        attempt: Some(context.attempt as i64),
        error_source: failure.as_ref().map(|failure| failure.error_source.clone()),
        error_stage: failure.as_ref().map(|failure| failure.error_stage.clone()),
        duration_ms: failure.as_ref().map(|failure| failure.duration_ms),
        diagnostic: failure_value,
    })?;
    persist_log_identity(db, id, context)?;
    Ok(id)
}

fn persist_log_identity(db: &Database, id: i64, context: &ForwardAttemptContext) -> Result<()> {
    let Some(mut attribution) = db.forward_log_native_attribution(id)? else {
        return Ok(());
    };
    attribution.requested_model = Some(context.requested_model.clone());
    attribution.resolved_alias = context.resolved_alias.clone();
    attribution.upstream_model = Some(context.upstream_model.clone());
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
    db.update_forward_log(id, status, http_status, metrics, error_message, diagnostic)?;
    persist_log_identity(db, id, context)
}

fn success_status_for_cost(cost_state: &str) -> &'static str {
    match cost_state {
        "priced" | "free" => "success",
        "usage_missing" => "success_no_usage",
        _ => "success_unpriced",
    }
}

fn pricing_metrics(
    snapshot: &PricingSnapshot,
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
        service_tier: service_tier.map(str::to_string),
        cost_state: estimate.cost_state,
    }
}

fn metadata_metrics(
    snapshot: &PricingSnapshot,
    service_tier: Option<&str>,
    cost_state: &'static str,
) -> ForwardMetrics {
    ForwardMetrics {
        pricing_revision_id: Some(snapshot.revision.clone()),
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

// ponytail: SSE spec allows \n\n OR \r\n\r\n as event boundaries. Match both
// so Windows-origin / proxy-CRLF upstreams don't accumulate buffer forever.
fn find_event_boundary(buf: &[u8]) -> Option<usize> {
    // \n\n
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some(i);
        }
    }
    // \r\n\r\n
    for i in 0..buf.len().saturating_sub(3) {
        if &buf[i..i + 4] == b"\r\n\r\n" {
            return Some(i);
        }
    }
    None
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
    fn free_429_does_not_rotate_keys() {
        for misleading_body in [
            "5-hour usage limit reached. Resets in 13min.",
            "Weekly usage limit reached. Resets in 4 days.",
            "Monthly usage limit reached. Resets in 13 days.",
        ] {
            assert_eq!(
                usage_limit_window_for_channel(UpstreamChannel::Free, misleading_body),
                Some(UsageWindowKind::Free),
            );
        }
        assert_eq!(
            action_for_usage_limit(Some(UsageWindowKind::Free), true),
            ForwardAction::ExhaustFreeChannel
        );
        assert_eq!(
            action_for_usage_limit(Some(UsageWindowKind::Free), false),
            ForwardAction::Return
        );
        assert_eq!(
            action_for_usage_limit(Some(UsageWindowKind::FiveHours), true),
            ForwardAction::TryNextAccount
        );
        assert_eq!(
            action_for_usage_limit(None, false),
            ForwardAction::TryNextAccount
        );
    }

    #[test]
    fn goat_429_is_generic_and_ignores_go_limit_windows() {
        for misleading_body in [
            "5-hour usage limit reached. Resets in 13min.",
            "Weekly usage limit reached. Resets in 4 days.",
            "Monthly usage limit reached. Resets in 13 days.",
            r#"{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 3 days."}"#,
        ] {
            let (window, cooldown) =
                rate_limit_window_and_cooldown(true, UpstreamChannel::Go, misleading_body);
            assert_eq!(window, None, "{misleading_body}");
            assert_eq!(cooldown, Duration::minutes(5), "{misleading_body}");
        }
        let (go_window, go_cooldown) = rate_limit_window_and_cooldown(
            false,
            UpstreamChannel::Go,
            "Weekly usage limit reached. Resets in 4 days.",
        );
        assert_eq!(go_window, Some(UsageWindowKind::Week));
        assert_eq!(go_cooldown, Duration::days(4));
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
            known_secret: Some(secret.clone()),
            route_account_id: None,
            provider_id: None,
            offering_id: None,
            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
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
                None => "{\"usage\":{\"input_tokens\":0,\"output_tokens\":5,\"cache_read_input_tokens\":40500}}".to_string(),
            };
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
