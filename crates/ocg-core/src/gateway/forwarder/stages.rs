//! Explicit stages of one forward attempt.
//!
//! `prepare_attempt` finishes before the upstream send. `send_attempt` decides
//! the transport retry, and `classify_attempt_response` decides the HTTP retry.
//! Both run before any downstream byte exists. `stream_attempt_response`
//! commits output at the first converted chunk; `buffered_attempt_response`
//! commits when the transformed body is logged. The caller keeps ownership of
//! `credit_guard` and `recovery_permit`, so both still drop where they dropped before.

use super::*;

pub(super) struct PreparedAttempt {
    pub(super) policy_provider_id: &'static str,
    pub(super) openrouter_free: bool,
    pub(super) pricing_snapshot: RequestPricingSnapshot,
    pub(super) attempt_context: ForwardAttemptContext,
    pub(super) key: Option<String>,
    pub(super) model: String,
    pub(super) free_contract: bool,
    pub(super) restriction_endpoint: String,
    pub(super) recovery_permit: RecoveryPermit,
    pub(super) quota_observation: QuotaObservation,
    pub(super) quota_trial: std::sync::Arc<Mutex<Option<QuotaTrialGuard>>>,
    pub(super) credit_guard: Option<CreditRequestGuard>,
    pub(super) request: reqwest::RequestBuilder,
    pub(super) timeouts: AttemptTimeouts,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_attempt(
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
    headers: HeaderMap,
    pricing_snapshot: RequestPricingSnapshot,
    client_key_id: Option<&str>,
    attempt_spec: &AttemptSpec,
    selection: &LiveSendSelection,
    allow_same_account_retry: bool,

    request_deadline: Option<tokio::time::Instant>,
) -> Result<std::result::Result<PreparedAttempt, ForwardResult>> {
    let policy_provider_id = crate::provider::ProviderRegistry::get_by_kind(adapter)
        .map(|descriptor| descriptor.provider_id)
        .unwrap_or(crate::provider::CUSTOM_PROVIDER_ID);
    let openrouter_free = attempt_spec
        .request_url()
        .ok()
        .and_then(|raw| reqwest::Url::parse(&raw).ok())
        .is_some_and(|url| is_openrouter_free_request(&url, &plan.model));
    let pricing_snapshot = if openrouter_free {
        // Do not debit a configured paid Credit estimate for an official Free
        // model. Keep total cost unknown because optional upstream features may
        // have independent charges.
        RequestPricingSnapshot::Unpriced
    } else {
        pricing_snapshot
    };
    let mut attempt_context =
        ForwardAttemptContext::new(trace, client_body.len(), attempt, plan, route);
    attempt_context.attach_pricing(&pricing_snapshot);
    attempt_context.set_client_key(client_key_id, state);
    attempt_context.set_provider_route(account, attempt_spec);
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
    let resolver = HostCredentialResolver::new(state, account, selection, plan, attempt_spec);
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
            return Ok(Err(account_preflight_failure(plan, message)));
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
                | "api-key"
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
        adapter,
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
    if matches!(
        resolved_auth,
        UpstreamAuth::Bearer | UpstreamAuth::XApiKey | UpstreamAuth::ApiKey
    ) {
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
                return Ok(Err(account_preflight_failure(plan, message)));
            }
        };
        match resolved_auth {
            UpstreamAuth::XApiKey => {
                upstream_headers.insert("x-api-key", key_header);
            }
            UpstreamAuth::ApiKey => {
                upstream_headers.insert("api-key", key_header);
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
                UpstreamAuth::ApiKey => crate::provider::UpstreamAuthScheme::ApiKey,
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

    // Admission is operational state, not account quota or selector state.
    // Its key uses the authorized exact endpoint/model, route and current
    // credential/pool generation. No raw identity digest is logged.
    let free_contract = matches!(
        classify_http(
            429,
            &account.provider_id,
            plan.channel,
            attempt_spec.auth == UpstreamAuth::None
        ),
        ProviderErrorClass::RateLimited {
            profile: ocg_gateway::classify::ErrorProfile::ZenFree
        }
    );
    let proxy_identity = (route == RouteLabel::Proxy).then_some(config.proxy_url.as_str());
    let restriction_endpoint =
        restriction_endpoint_identity(&url, route, plan.upstream, proxy_identity);
    let resources = ResourceSet::capture(
        &state.db.lock(),
        account,
        &restriction_endpoint,
        &plan.model,
        free_contract,
    )?;
    let (wall, mono) = state.sample_gateway_clock();
    let recovery_permit = match state.recovery.acquire(resources, wall, mono) {
        Ok(permit) => permit,
        Err(wait) => {
            let message = if wait.is_local_policy() {
                "compatible upstream resource is waiting on local_policy; no upstream request sent"
            } else if wait.is_capacity() {
                "compatible upstream resource cannot be tracked (recovery_capacity); no upstream request sent"
            } else {
                "compatible upstream resource is waiting for recovery; no upstream request sent"
            };
            attempt_context.restriction_details = Some(serde_json::json!({"wait": wait}));
            let failure = attempt_context.failure(FailureSpec {
                error_source: "gateway",
                error_stage: wait.skip_stage(),
                downstream_status: Some(StatusCode::SERVICE_UNAVAILABLE.as_u16()),
                upstream_status: None,
                upstream_wait_ms: None,
                retry_action: Some("try_next_account"),
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
                Some(message),
                &attempt_context,
                Some(failure),
            )?;
            return Ok(Err(ForwardResult {
                response: protocol_status_error_response(
                    plan.client,
                    StatusCode::SERVICE_UNAVAILABLE,
                    message,
                    None,
                ),
                action: ForwardAction::TryNextAccount,
                error_message: Some(message.into()),
                sent: false,
            }));
        }
    };

    if state.debug_capture.enabled() {
        let mut capture_secrets = crate::gateway::debug_capture::authentication_secrets(&headers);
        capture_secrets.extend(key.iter().cloned());
        if let Err(reason) = state
            .debug_capture
            .save(
                trace,
                "upstream",
                attempt,
                &url,
                &send_headers,
                bytes::Bytes::copy_from_slice(&attempt_body),
                &capture_secrets,
            )
            .await
        {
            crate::gateway::diagnostics::log_event(
                &state.db.lock(),
                trace,
                "warn",
                "debug_capture",
                "capture_failed",
                Some(attempt),
                serde_json::json!({"stage": "upstream", "reason": reason}),
            );
        }
    }
    crate::gateway::diagnostics::log_event(
        &state.db.lock(),
        trace,
        "debug",
        "routing",
        "attempt_prepared",
        Some(attempt),
        serde_json::json!({"account_id": account.id, "provider_id": account.provider_id,
            "client_format": crate::gateway::diagnostics::api_format_name(plan.client),
            "upstream_format": crate::gateway::diagnostics::api_format_name(plan.upstream),
            "body_bytes": attempt_context.upstream_body_bytes, "stream": plan.stream, "route": route.as_str(),
            "requested_model": attempt_context.redact_known_secret(&attempt_context.requested_model),
            "upstream_model": attempt_context.redact_known_secret(&attempt_context.upstream_model)}),
    );
    let mut timeouts = AttemptTimeouts::from_secs(
        config.non_stream_timeout_secs,
        config.stream_idle_timeout_secs,
    );
    if let Some(deadline) = request_deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            let message = "Gateway request deadline exceeded before send; no upstream request sent";
            let failure = attempt_context.failure(FailureSpec {
                error_source: "gateway",
                error_stage: "request_budget",
                downstream_status: Some(StatusCode::SERVICE_UNAVAILABLE.as_u16()),
                upstream_status: None,
                upstream_wait_ms: None,
                retry_action: Some("return"),
                upstream_headers: None,
                upstream_error: None,
                request_body: Some(client_body),
            });
            DbAttemptSink::new(&state.db.lock()).insert(
                account,
                &model,
                "error",
                None,
                metadata_metrics(
                    &pricing_snapshot,
                    plan.service_tier.as_deref(),
                    "not_applicable",
                ),
                Some(message),
                &attempt_context,
                Some(failure),
            )?;
            return Ok(Err(ForwardResult {
                response: protocol_status_error_response(
                    plan.client,
                    StatusCode::SERVICE_UNAVAILABLE,
                    message,
                    None,
                ),
                action: ForwardAction::Return,
                error_message: Some(message.into()),
                sent: false,
            }));
        }
        timeouts.non_stream = timeouts.non_stream.min(remaining);
        timeouts.stream_header = timeouts.stream_header.min(remaining);
    }
    let request = build_attempt_request(
        attempt_spec,
        client,
        route,
        config,
        timeouts,
        &url,
        send_headers,
        attempt_body,
        plan.stream,
    )?;
    let quota_observation = QuotaObservation {
        selection: selection.clone(),
        fence: recovery_permit.quota_observation(),
    };
    let quota_trial = match resolver.confirm_live() {
        Ok(episode) => episode.map(|episode| {
            QuotaTrialGuard::new(state.clone(), episode).with_observation(quota_observation.clone())
        }),
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
            return Ok(Err(account_preflight_failure(plan, message)));
        }
    };
    let quota_trial = Arc::new(Mutex::new(quota_trial));

    if let Some(compat) = &plan.legacy_tool_compat {
        emit_legacy_tool_compat(
            &trace.request_id,
            compat.profile,
            compat.version,
            &compat.dropped_hosted_tools,
        );
    }

    // Persist the attempt before the upstream can consume it. A process exit or
    // cancelled header/body read must remain visible to credit calibration.
    let credit_guard = if attempt_context.credit_attempt.is_some() {
        let id = DbAttemptSink::new(&state.db.lock()).insert(
            account,
            &model,
            "streaming",
            None,
            metadata_metrics(
                &pricing_snapshot,
                plan.service_tier.as_deref(),
                "not_applicable",
            ),
            None,
            &attempt_context,
            None,
        )?;
        attempt_context.credit_log_id = Some(id);
        Some(CreditRequestGuard {
            state: state.clone(),
            context: attempt_context.clone(),
            pricing: pricing_snapshot.clone(),
            service_tier: plan.service_tier.clone(),
            armed: true,
        })
    } else {
        None
    };
    Ok(Ok(PreparedAttempt {
        policy_provider_id,
        openrouter_free,
        pricing_snapshot,
        attempt_context,
        key,
        model,
        free_contract,
        restriction_endpoint,
        recovery_permit,
        quota_observation,
        quota_trial,
        credit_guard,
        request,
        timeouts,
    }))
}

pub(super) struct SentAttempt {
    pub(super) upstream_started: Instant,
    pub(super) upstream_resp: reqwest::Response,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn send_attempt(
    request: reqwest::RequestBuilder,
    timeouts: AttemptTimeouts,
    plan: &RequestPlan,
    state: &CoreState,
    account: &ExecutionCredential,
    model: &str,
    pricing_snapshot: &RequestPricingSnapshot,
    attempt_context: &mut ForwardAttemptContext,
    client_body: &[u8],
    allow_same_account_retry: bool,
    free_contract: bool,
    openrouter_free: bool,
    selection: &LiveSendSelection,
    recovery_permit: &mut RecoveryPermit,
    restriction_endpoint: &str,
) -> Result<std::result::Result<SentAttempt, ForwardResult>> {
    let sent = forward_once(request, timeouts, plan.stream).await?;
    let upstream_started = sent.started;
    let upstream_resp = match sent.result {
        Ok(resp) => resp,
        Err(AttemptTransportError::HeaderTimeout { timeout }) => {
            let class = classify_transport(TransportClassifyInput::HeaderTimeout);
            let detail = format!(
                "upstream response header timeout after {}s",
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
                    model,
                    "outcome_unknown",
                    None,
                    metadata_metrics(
                        pricing_snapshot,
                        plan.service_tier.as_deref(),
                        "outcome_unknown",
                    ),
                    Some(&error_message),
                    attempt_context,
                    Some(failure),
                )?;
            }
            return Ok(Err(ForwardResult {
                response: outcome_unknown_response(
                    plan.client,
                    StatusCode::GATEWAY_TIMEOUT,
                    &detail,
                ),
                action,
                error_message: Some(error_message),
                sent: true,
            }));
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
            let action = if free_contract && connect_failure {
                observe_free_rejection(
                    state,
                    account,
                    selection,
                    &mut *recovery_permit,
                    restriction_endpoint,
                    &plan.model,
                    None,
                    &mut *attempt_context,
                )?;
                ForwardAction::ExhaustFreeChannel
            } else if openrouter_free && connect_failure {
                observe_openrouter_free_rejection(
                    state,
                    account,
                    selection,
                    &mut *recovery_permit,
                    restriction_endpoint,
                    &plan.model,
                    None,
                    &mut *attempt_context,
                )?;
                ForwardAction::TryNextAccount
            } else {
                forward_action_for_class(class, allow_same_account_retry, None)
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
                retry_action: Some(retry_action_name(action)),
                upstream_headers: None,
                upstream_error: Some(&detail),
                request_body: Some(client_body),
            });
            {
                let db = state.db.lock();
                DbAttemptSink::new(&db).insert(
                    account,
                    model,
                    if outcome_unknown {
                        "outcome_unknown"
                    } else {
                        "error"
                    },
                    None,
                    metadata_metrics(
                        pricing_snapshot,
                        plan.service_tier.as_deref(),
                        if outcome_unknown {
                            "outcome_unknown"
                        } else {
                            "not_applicable"
                        },
                    ),
                    Some(&error_message),
                    attempt_context,
                    Some(failure),
                )?;
            }
            return Ok(Err(ForwardResult {
                response: if outcome_unknown {
                    outcome_unknown_response(plan.client, status, &detail)
                } else {
                    error_response(plan.client, &error_message, None)
                },
                action,
                error_message: Some(error_message),
                sent: true,
            }));
        }
    };
    Ok(Ok(SentAttempt {
        upstream_started,
        upstream_resp,
    }))
}

pub(super) struct ClassifiedAttempt {
    pub(super) upstream_resp: reqwest::Response,
    pub(super) status: StatusCode,
    pub(super) is_stream: bool,
    pub(super) body_timeout: Option<StdDuration>,
    pub(super) upstream_wait_ms: u64,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn classify_attempt_response(
    upstream_resp: reqwest::Response,
    upstream_started: Instant,
    state: &CoreState,
    account: &ExecutionCredential,
    plan: &RequestPlan,
    trace: &RequestTrace,
    model: &str,
    pricing_snapshot: &RequestPricingSnapshot,
    attempt_context: &mut ForwardAttemptContext,
    client_body: &[u8],
    attempt: u32,
    allow_same_account_retry: bool,
    key: &Option<String>,
    free_contract: bool,
    openrouter_free: bool,
    selection: &LiveSendSelection,
    recovery_permit: &mut RecoveryPermit,
    restriction_endpoint: &str,
    quota_observation: &QuotaObservation,
    config: &AppConfig,
    request_deadline: Option<tokio::time::Instant>,
    policy_provider_id: &str,
    attempt_spec: &AttemptSpec,
    adapter: ProviderAdapterKind,
) -> Result<std::result::Result<ClassifiedAttempt, ForwardResult>> {
    let upstream_wait_ms = upstream_started.elapsed().as_millis() as u64;

    let status = upstream_resp.status();
    crate::gateway::diagnostics::log_event(
        &state.db.lock(),
        trace,
        if status.is_success() { "debug" } else { "warn" },
        "upstream",
        "upstream_headers",
        Some(attempt),
        serde_json::json!({"status": status.as_u16(),
            "wait_ms": upstream_started.elapsed().as_millis(),
            "headers": crate::gateway::diagnostics::safe_upstream_headers(upstream_resp.headers(), key.as_deref())}),
    );
    let is_stream = upstream_resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    let body_timeout = plan
        .stream
        .then(|| StdDuration::from_secs(config.stream_idle_timeout_secs));
    let body_timeout = match request_deadline {
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            Some(body_timeout.map_or(remaining, |timeout| timeout.min(remaining)))
        }
        None => body_timeout,
    };

    if openrouter_free
        && (status.is_server_error()
            || (status.is_client_error() && status != StatusCode::TOO_MANY_REQUESTS))
    {
        let error_headers = upstream_resp.headers().clone();
        let (text, policy_body) = match response_text_with_timeout(
            upstream_resp,
            body_timeout,
            Some(MAX_UPSTREAM_ERROR_BODY_BYTES),
        )
        .await
        {
            Ok(text) => {
                let policy = text.clone();
                (text, Some(policy))
            }
            Err(error) => (error.into_detail(), None),
        };
        let class = classify_http(
            status.as_u16(),
            policy_provider_id,
            plan.channel,
            attempt_spec.auth == UpstreamAuth::None,
        );
        let retry_after = error_headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok());
        let (observed_at, observed_mono) = state.sample_gateway_clock();
        observe_local_policy(
            state,
            account,
            adapter,
            class,
            selection,
            &mut *recovery_permit,
            restriction_endpoint,
            &plan.model,
            free_contract,
            status.as_u16(),
            policy_body.as_deref(),
            retry_after,
            observed_at,
            observed_mono,
        )?;
        observe_openrouter_free_rejection(
            state,
            account,
            selection,
            &mut *recovery_permit,
            restriction_endpoint,
            &plan.model,
            retry_after,
            &mut *attempt_context,
        )?;
        let action = ForwardAction::TryNextAccount;
        let message = format!("OpenRouter free model was rejected ({status})");
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
        DbAttemptSink::new(&state.db.lock()).insert(
            account,
            model,
            if status.is_client_error() {
                "client_error"
            } else {
                "error"
            },
            Some(status.as_u16() as i32),
            metadata_metrics(
                pricing_snapshot,
                plan.service_tier.as_deref(),
                "not_applicable",
            ),
            Some(&attempt_context.sanitize_upstream_error(&text)),
            attempt_context,
            Some(failure),
        )?;
        return Ok(Err(ForwardResult {
            response: error_response(plan.client, &message, None),
            action,
            error_message: Some(message),
            sent: true,
        }));
    }

    if status.is_server_error() {
        // A response status is authoritative even if its error body stalls.
        // Ordinary routes retain it; Zen Free can try another compatible route.
        let error_headers = upstream_resp.headers().clone();
        let (text, policy_body) = match response_text_with_timeout(
            upstream_resp,
            body_timeout,
            Some(MAX_UPSTREAM_ERROR_BODY_BYTES),
        )
        .await
        {
            Ok(text) => {
                let policy = text.clone();
                (text, Some(policy))
            }
            Err(error) => (error.into_detail(), None),
        };
        let class = classify_http(
            status.as_u16(),
            policy_provider_id,
            plan.channel,
            attempt_spec.auth == UpstreamAuth::None,
        );
        let retry_after = error_headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok());
        let (observed_at, observed_mono) = state.sample_gateway_clock();
        observe_local_policy(
            state,
            account,
            adapter,
            class,
            selection,
            &mut *recovery_permit,
            restriction_endpoint,
            &plan.model,
            free_contract,
            status.as_u16(),
            policy_body.as_deref(),
            retry_after,
            observed_at,
            observed_mono,
        )?;
        let action = forward_action_for_class(class, allow_same_account_retry, None);
        if class == ProviderErrorClass::FreeRejected {
            observe_free_rejection(
                state,
                account,
                selection,
                &mut *recovery_permit,
                restriction_endpoint,
                &plan.model,
                retry_after,
                &mut *attempt_context,
            )?;
        }
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
                model,
                "error",
                Some(status.as_u16() as i32),
                metadata_metrics(
                    pricing_snapshot,
                    plan.service_tier.as_deref(),
                    "not_applicable",
                ),
                Some(&error_message),
                attempt_context,
                Some(failure),
            )?;
        }
        return Ok(Err(ForwardResult {
            response: protocol_status_error_response(plan.client, status, &error_message, None),
            action,
            error_message: Some(error_message),
            sent: true,
        }));
    }

    if status.is_client_error() {
        // A known 4xx proves the upstream rejected the request. Its status
        // policy still applies if the bounded error-body read fails.
        let error_headers = upstream_resp.headers().clone();
        let (text, policy_body) = match response_text_with_timeout(
            upstream_resp,
            body_timeout,
            Some(MAX_UPSTREAM_ERROR_BODY_BYTES),
        )
        .await
        {
            Ok(text) => {
                let policy = text.clone();
                (text, Some(policy))
            }
            Err(error) => (error.into_detail(), None),
        };
        let class = crate::gateway::classify::classify_http_response(
            status.as_u16(),
            policy_provider_id,
            plan.channel,
            attempt_spec.auth == UpstreamAuth::None,
            &text,
        );
        let retry_after = error_headers
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok());
        let (observed_at, observed_mono) = state.sample_gateway_clock();
        observe_local_policy(
            state,
            account,
            adapter,
            class,
            selection,
            &mut *recovery_permit,
            restriction_endpoint,
            &plan.model,
            free_contract,
            status.as_u16(),
            policy_body.as_deref(),
            retry_after,
            observed_at,
            observed_mono,
        )?;
        if let Some(facts) = decode_failure(class, &text, retry_after, observed_at) {
            let decision = facts.decide();
            let sanitized = attempt_context.sanitize_upstream_error(&text);
            let action = if decision.exhaust_free {
                ForwardAction::ExhaustFreeChannel
            } else {
                ForwardAction::TryNextAccount
            };
            let error_message = format!(
                "upstream rejected this resource ({:?}): {sanitized}",
                facts.cause
            );
            let downstream = if status == StatusCode::TOO_MANY_REQUESTS {
                StatusCode::BAD_GATEWAY
            } else {
                status
            };
            let rate_limited = matches!(class, ProviderErrorClass::RateLimited { .. });
            // Re-read live Key/binding/endpoint identity after I/O. Stale
            // replies never write backoff, and output has not started so
            // fallback remains allowed.
            let recorded = {
                let db = state.db.lock();
                let same_generation = live_send::selection_identity_is_current(&db, selection)?
                    && recovery_permit.permits_observation(&facts)
                    && recovery_permit.same_generation(&ResourceSet::capture(
                        &db,
                        account,
                        restriction_endpoint,
                        &plan.model,
                        free_contract,
                    )?);
                if same_generation {
                    if rate_limited && !free_contract {
                        recovery_permit.observe_credential_retry(
                            Some(temporary_429_deadline(retry_after, observed_at)),
                            observed_mono,
                        );
                    } else {
                        recovery_permit.observe_failure(&facts, decision, observed_mono);
                    }
                }
                same_generation
            };
            attempt_context.restriction_details = Some(serde_json::json!({
                "facts": facts, "recorded_for_current_generation": recorded,
                "local_reprobe": decision.wait_for_recovery,
            }));
            let failure = attempt_context.failure(FailureSpec {
                error_source: "upstream",
                error_stage: "upstream_http",
                downstream_status: Some(downstream.as_u16()),
                upstream_status: Some(status.as_u16()),
                upstream_wait_ms: Some(upstream_wait_ms),
                retry_action: Some(retry_action_name(action)),
                upstream_headers: Some(&error_headers),
                upstream_error: Some(&text),
                request_body: Some(client_body),
            });
            DbAttemptSink::new(&state.db.lock()).insert(
                account,
                model,
                "client_error",
                Some(status.as_u16() as i32),
                metadata_metrics(
                    pricing_snapshot,
                    plan.service_tier.as_deref(),
                    "not_applicable",
                ),
                Some(&sanitized),
                attempt_context,
                Some(failure),
            )?;
            if recorded && rate_limited && !free_contract {
                spawn_reactive_usage_refresh(state, &account.id);
            }
            return Ok(Err(ForwardResult {
                response: protocol_status_error_response(
                    plan.client,
                    downstream,
                    &error_message,
                    None,
                ),
                action,
                error_message: Some(error_message),
                sent: true,
            }));
        }

        match class {
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
                        model,
                        "outcome_unknown",
                        Some(408),
                        metadata_metrics(
                            pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "outcome_unknown",
                        ),
                        Some(&error_message),
                        attempt_context,
                        Some(failure),
                    )?;
                }
                return Ok(Err(ForwardResult {
                    response: outcome_unknown_response(
                        plan.client,
                        StatusCode::GATEWAY_TIMEOUT,
                        &detail,
                    ),
                    action,
                    error_message: Some(error_message),
                    sent: true,
                }));
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
                        model,
                        "client_error",
                        Some(401),
                        metadata_metrics(
                            pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "not_applicable",
                        ),
                        Some(&sanitized),
                        attempt_context,
                        Some(failure),
                    )?;
                }
                let upstream_error = Some(sanitize_upstream_error_value_with_known_secret(
                    &text,
                    key.as_deref().unwrap_or_default(),
                ));
                let body = format_error(plan.client, status, &sanitized, upstream_error.as_ref());
                return Ok(Err(ForwardResult {
                    response: (status, axum::Json(body)).into_response(),
                    action,
                    error_message: Some(error_message),
                    sent: true,
                }));
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
                        model,
                        "client_error",
                        Some(401),
                        metadata_metrics(
                            pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "not_applicable",
                        ),
                        Some(&sanitized),
                        attempt_context,
                        Some(failure),
                    )?;
                    if quota_observation.is_current(&db)? {
                        db.set_account_auth_error_if_key_matches(
                            &account.id,
                            &account.key_cipher,
                            Some(&error_message),
                        )?;
                    }
                }
                return Ok(Err(ForwardResult {
                    response: error_response(plan.client, &error_message, None),
                    action,
                    error_message: Some(error_message),
                    sent: true,
                }));
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
                        "upstream returned 403: {}",
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
                        model,
                        "client_error",
                        Some(403),
                        metadata_metrics(
                            pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "not_applicable",
                        ),
                        Some(&sanitized),
                        attempt_context,
                        Some(failure),
                    )?;
                }
                return Ok(Err(ForwardResult {
                    response: error_response(plan.client, &error_message, None),
                    action,
                    error_message: Some(error_message),
                    sent: true,
                }));
            }
            _ => {
                // Unrecognized 4xx remain request errors. Provider decoders may
                // refine only verified rejection envelopes above.
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
                        model,
                        "client_error",
                        Some(status.as_u16() as i32),
                        metadata_metrics(
                            pricing_snapshot,
                            plan.service_tier.as_deref(),
                            "not_applicable",
                        ),
                        Some(&sanitized),
                        attempt_context,
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
                return Ok(Err(ForwardResult {
                    response,
                    action,
                    error_message: (class == ProviderErrorClass::InsufficientCredits)
                        .then_some(message),
                    sent: true,
                }));
            }
        }
    }

    // Success path — for non-stream, record breaker success now.
    // For streams, don't pre-record success; the stream error handler
    // records errors, and we haven't proven success until the stream completes.

    Ok(Ok(ClassifiedAttempt {
        upstream_resp,
        status,
        is_stream,
        body_timeout,
        upstream_wait_ms,
    }))
}

/// Streaming setup. Retries only happen before the first converted chunk;
/// once `initial_chunks` is non-empty the output is committed and the body
/// guard owns settlement. `credit_guard` is disarmed here and dropped by the caller.
#[allow(clippy::too_many_arguments)]
pub(super) async fn stream_attempt_response(
    upstream_resp: reqwest::Response,
    status: StatusCode,
    upstream_wait_ms: u64,
    state: &CoreState,
    account: &ExecutionCredential,
    plan: &RequestPlan,
    model: String,
    pricing_snapshot: &RequestPricingSnapshot,
    attempt_context: &ForwardAttemptContext,
    config: &AppConfig,
    request_deadline: Option<tokio::time::Instant>,
    allow_same_account_retry: bool,
    attempt_spec: &AttemptSpec,
    recovery_permit: RecoveryPermit,
    quota_trial: std::sync::Arc<Mutex<Option<QuotaTrialGuard>>>,
    credit_guard: &mut Option<CreditRequestGuard>,
) -> Result<ForwardResult> {
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
                pricing_snapshot,
                plan.service_tier.as_deref(),
                "not_applicable",
            ),
            None,
            attempt_context,
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
        // Heartbeats or partial frames must not restart the logical request
        // budget while no usable downstream output has been produced.
        let read_timeout = request_deadline.map_or(stream_idle_timeout, |deadline| {
            stream_idle_timeout.min(deadline.saturating_duration_since(tokio::time::Instant::now()))
        });
        let preflight = tokio::time::timeout(read_timeout, upstream_stream.next()).await;
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
                            pricing_snapshot,
                            attempt_context,
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
                    pricing_snapshot,
                    attempt_context,
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
                            pricing_snapshot,
                            attempt_context,
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
                let budget_expired = request_deadline
                    .is_some_and(|deadline| tokio::time::Instant::now() >= deadline);
                let detail = if budget_expired {
                    "Gateway request deadline exceeded before stream output (timeout)".to_string()
                } else {
                    format!("upstream stream idle timeout after {stream_idle_timeout_secs}s")
                };
                match handle_pre_output_stream_failure(
                    state,
                    &st,
                    &converter,
                    initial_id,
                    pricing_snapshot,
                    attempt_context,
                    plan,
                    status,
                    upstream_wait_ms,
                    StatusCode::GATEWAY_TIMEOUT,
                    "transport",
                    if budget_expired {
                        "request_budget"
                    } else {
                        "stream"
                    },
                    &detail,
                    StreamClassifyInput::IdleTimeoutBeforeOutput,
                    allow_same_account_retry && !budget_expired,
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
                                let detail = format!("stream conversion failed: {}", error.message);
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
        let mut stream_guard = StreamOutcomeGuard::new(
            state.clone(),
            initial_id,
            st.clone(),
            model.clone(),
            pricing_snapshot.clone(),
            plan.service_tier.clone(),
            attempt_context.clone(),
            status.as_u16(),
            upstream_wait_ms,
            quota_trial.clone(),
        );
        stream_guard.recovery = Some(recovery_permit);
        // `unfold` is a clean "run once, then end" stream. The DB write is the
        // unfold's state transition, the body emits a single empty chunk, and
        // the stream then terminates 鈥?no need for once() + flatten gymnastics.
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
                            Ok(chunks) => (join_chunks(chunks), None, converter.captured_usage()),
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
                    if !st_f.lock().error
                        && let Some(permit) = guard.recovery.as_mut()
                    {
                        permit.confirm_success();
                    }
                    drop(db);
                    guard.settle_quota(status_str.starts_with("success"));
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

    let response =
        response_builder.body(Body::from_stream(initial.chain(mapped).chain(finalizer)))?;
    // StreamOutcomeGuard now owns cancellation and settlement.
    if let Some(guard) = credit_guard.as_mut() {
        guard.armed = false;
    }
    Ok(ForwardResult {
        response,
        action: ForwardAction::Return,
        error_message: None,
        sent: true,
    })
}

/// Buffered success and body-read failure handling. Every return here is a
/// terminal response; quota settlement happens before the `Ok` return.
#[allow(clippy::too_many_arguments)]
pub(super) async fn buffered_attempt_response(
    upstream_resp: reqwest::Response,
    status: StatusCode,
    body_timeout: Option<StdDuration>,
    upstream_wait_ms: u64,
    state: &CoreState,
    account: &ExecutionCredential,
    plan: &RequestPlan,
    model: &str,
    pricing_snapshot: &RequestPricingSnapshot,
    attempt_context: &mut ForwardAttemptContext,
    client_body: &[u8],
    allow_same_account_retry: bool,
    free_contract: bool,
    openrouter_free: bool,
    selection: &LiveSendSelection,
    recovery_permit: &mut RecoveryPermit,
    restriction_endpoint: &str,
    quota_trial: std::sync::Arc<Mutex<Option<QuotaTrialGuard>>>,
    policy_provider_id: &str,
    attempt_spec: &AttemptSpec,
) -> Result<ForwardResult> {
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
                    model,
                    "outcome_unknown",
                    Some(status.as_u16() as i32),
                    metadata_metrics(
                        pricing_snapshot,
                        plan.service_tier.as_deref(),
                        "outcome_unknown",
                    ),
                    Some(&error_message),
                    attempt_context,
                    Some(failure),
                )?;
            }
            return Ok(ForwardResult {
                response: outcome_unknown_response(plan.client, downstream_status, &detail),
                action,
                error_message: Some(error_message),
                sent: true,
            });
        }
    };
    let mut upstream_json = match serde_json::from_str::<Value>(&text) {
        Ok(value) => value,
        Err(_) => {
            let message = "upstream returned invalid JSON";
            let action = if free_contract {
                observe_free_rejection(
                    state,
                    account,
                    selection,
                    &mut *recovery_permit,
                    restriction_endpoint,
                    &plan.model,
                    None,
                    &mut *attempt_context,
                )?;
                ForwardAction::ExhaustFreeChannel
            } else if openrouter_free {
                observe_openrouter_free_rejection(
                    state,
                    account,
                    selection,
                    &mut *recovery_permit,
                    restriction_endpoint,
                    &plan.model,
                    None,
                    &mut *attempt_context,
                )?;
                ForwardAction::TryNextAccount
            } else {
                ForwardAction::Return
            };
            let failure = attempt_context.failure(FailureSpec {
                error_source: "upstream",
                error_stage: "response_body",
                downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                upstream_status: Some(status.as_u16()),
                upstream_wait_ms: Some(upstream_wait_ms),
                retry_action: Some(retry_action_name(action)),
                upstream_headers: None,
                upstream_error: Some(&text),
                request_body: Some(client_body),
            });
            let db = state.db.lock();
            DbAttemptSink::new(&db).insert(
                account,
                model,
                "error",
                Some(status.as_u16() as i32),
                metadata_metrics(
                    pricing_snapshot,
                    plan.service_tier.as_deref(),
                    "not_applicable",
                ),
                Some(message),
                attempt_context,
                Some(failure),
            )?;
            return Ok(ForwardResult {
                response: error_response(plan.client, message, None),
                action,
                error_message: Some(message.to_string()),
                sent: true,
            });
        }
    };

    let body_for_quota = serde_json::to_string(&upstream_json).unwrap_or_else(|_| text.clone());
    if crate::provider::ProviderAdapterKind::from_provider_id(policy_provider_id)
        == Some(crate::provider::ProviderAdapterKind::MiniMaxCn)
        && let Some(envelope) = crate::quota_recovery::minimax_envelope(&body_for_quota)
        && !matches!(envelope, crate::quota_recovery::MiniMaxEnvelope::Success)
    {
        let sanitized = attempt_context.sanitize_upstream_error(&body_for_quota);
        let action = ForwardAction::TryNextAccount;
        {
            let db = state.db.lock();
            DbAttemptSink::new(&db).insert(
                account,
                model,
                "client_error",
                Some(status.as_u16() as i32),
                metadata_metrics(
                    pricing_snapshot,
                    plan.service_tier.as_deref(),
                    "not_applicable",
                ),
                Some(&sanitized),
                attempt_context,
                None,
            )?;
        }
        return Ok(ForwardResult {
            response: error_response(plan.client, &sanitized, Some(&upstream_json)),
            action,
            error_message: Some(sanitized),
            sent: true,
        });
    }

    let application_error = explicit_nonquota_application_error(&upstream_json);
    if application_error && (free_contract || openrouter_free) {
        let action = if free_contract {
            observe_free_rejection(
                state,
                account,
                selection,
                &mut *recovery_permit,
                restriction_endpoint,
                &plan.model,
                None,
                &mut *attempt_context,
            )?;
            ForwardAction::ExhaustFreeChannel
        } else {
            observe_openrouter_free_rejection(
                state,
                account,
                selection,
                &mut *recovery_permit,
                restriction_endpoint,
                &plan.model,
                None,
                &mut *attempt_context,
            )?;
            ForwardAction::TryNextAccount
        };
        let message = attempt_context.sanitize_upstream_error(&text);
        let failure = attempt_context.failure(FailureSpec {
            error_source: "upstream",
            error_stage: "response_body",
            downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
            upstream_status: Some(status.as_u16()),
            upstream_wait_ms: Some(upstream_wait_ms),
            retry_action: Some(retry_action_name(action)),
            upstream_headers: None,
            upstream_error: Some(&text),
            request_body: Some(client_body),
        });
        DbAttemptSink::new(&state.db.lock()).insert(
            account,
            model,
            "client_error",
            Some(status.as_u16() as i32),
            metadata_metrics(
                pricing_snapshot,
                plan.service_tier.as_deref(),
                "not_applicable",
            ),
            Some(&message),
            attempt_context,
            Some(failure),
        )?;
        return Ok(ForwardResult {
            response: error_response(plan.client, &message, None),
            action,
            error_message: Some(message),
            sent: true,
        });
    }
    let metrics = if has_complete_usage(plan.upstream, &upstream_json) {
        let usage = extract_usage(plan.upstream, &upstream_json, Some(model));
        let (prompt_tokens, completion_tokens, cached_tokens, cache_creation_tokens) =
            token_counts(usage);
        pricing_metrics(
            pricing_snapshot,
            model,
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            cache_creation_tokens,
            plan.service_tier.as_deref(),
        )
    } else {
        metadata_metrics(
            pricing_snapshot,
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
            let action = if free_contract {
                observe_free_rejection(
                    state,
                    account,
                    selection,
                    &mut *recovery_permit,
                    restriction_endpoint,
                    &plan.model,
                    None,
                    &mut *attempt_context,
                )?;
                ForwardAction::ExhaustFreeChannel
            } else if openrouter_free {
                observe_openrouter_free_rejection(
                    state,
                    account,
                    selection,
                    &mut *recovery_permit,
                    restriction_endpoint,
                    &plan.model,
                    None,
                    &mut *attempt_context,
                )?;
                ForwardAction::TryNextAccount
            } else {
                ForwardAction::Return
            };
            let failure = attempt_context.failure(FailureSpec {
                error_source: "gateway",
                error_stage: "response_transform",
                downstream_status: Some(StatusCode::BAD_GATEWAY.as_u16()),
                upstream_status: Some(status.as_u16()),
                upstream_wait_ms: Some(upstream_wait_ms),
                retry_action: Some(retry_action_name(action)),
                upstream_headers: None,
                upstream_error: Some(&message),
                request_body: Some(client_body),
            });
            let db = state.db.lock();
            DbAttemptSink::new(&db).insert(
                account,
                model,
                "error",
                Some(status.as_u16() as i32),
                metrics.clone(),
                Some(&message),
                attempt_context,
                Some(failure),
            )?;
            return Ok(ForwardResult {
                response: error_response(plan.client, &message, Some(&upstream_json)),
                action,
                error_message: Some(message),
                sent: true,
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
            model,
            success_status_for_cost(metrics.cost_state),
            Some(status.as_u16() as i32),
            metrics,
            None,
            attempt_context,
            None,
        )?;
    }
    let protocol = match plan.upstream {
        ApiFormat::ChatCompletions => {
            Some(ocg_domain::catalog::UpstreamProtocolKind::ChatCompletions)
        }
        ApiFormat::Responses => Some(ocg_domain::catalog::UpstreamProtocolKind::Responses),
        ApiFormat::Messages => Some(ocg_domain::catalog::UpstreamProtocolKind::Messages),
        ApiFormat::Gemini => None,
    };
    let complete_success = !application_error
        && protocol.is_some_and(|protocol| {
            crate::custom::prove_verified_protocol_response(status, text.as_bytes(), protocol)
                .is_ok()
        });
    if let Some(trial) = quota_trial.lock().as_mut() {
        if complete_success {
            trial.succeed();
        } else {
            trial.fail_nonquota(state.sample_gateway_clock().0);
        }
    }
    if complete_success {
        recovery_permit.confirm_success();
    }
    Ok(ForwardResult {
        response: (status, axum::Json(response_json)).into_response(),
        action: ForwardAction::Return,
        error_message: None,
        sent: true,
    })
}
