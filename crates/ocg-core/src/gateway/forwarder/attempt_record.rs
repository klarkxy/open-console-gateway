//! Attempt context, failure records, and the persistence sink.

use super::*;

/// Host secret resolution for the account the outer loop selected. Live
/// account/binding/grant checks and decrypt share one DB read lock. Same-account
/// retry reuses the original captured selection.
pub(crate) struct HostCredentialResolver<'a> {
    state: &'a CoreState,
    selection: &'a LiveSendSelection,
    spec: &'a AttemptSpec,
}

impl<'a> HostCredentialResolver<'a> {
    pub(crate) fn new(
        state: &'a CoreState,
        _account: &'a ExecutionCredential,
        selection: &'a LiveSendSelection,
        _plan: &'a RequestPlan,
        spec: &'a AttemptSpec,
    ) -> Self {
        Self {
            state,
            selection,
            spec,
        }
    }

    pub(super) fn resolve_live(
        &self,
        _handle: &CredentialHandle,
    ) -> Result<Option<String>, LiveSendAuthError> {
        live_send::authorize_execution_send(self.state, self.selection, self.spec, true)
    }
    pub(super) fn confirm_live(
        &self,
    ) -> Result<Option<crate::quota_recovery::QuotaEpisode>, LiveSendAuthError> {
        live_send::confirm_execution_send(self.state, self.selection, self.spec)
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
pub trait AttemptSink {
    #[allow(clippy::too_many_arguments)]
    fn insert(
        &self,
        account: &ExecutionCredential,
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

pub(super) struct DbAttemptSink<'a> {
    db: &'a Database,
}

impl<'a> DbAttemptSink<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }
}

#[allow(clippy::too_many_arguments)]
impl AttemptSink for DbAttemptSink<'_> {
    fn insert(
        &self,
        account: &ExecutionCredential,
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

#[derive(Clone)]
pub(super) struct ForwardAttemptContext {
    pub(super) trace: RequestTrace,
    pub(super) client_body_bytes: usize,
    pub(super) upstream_body_bytes: usize,
    pub(super) attempt: u32,
    pub(super) client_format: ApiFormat,
    pub(super) upstream_format: ApiFormat,
    pub(super) model: String,
    pub(super) requested_model: String,
    pub(super) resolved_alias: Option<String>,
    pub(super) upstream_model: String,
    pub(super) stream: bool,
    /// Route leg this attempt connected through, resolved by the handler from
    /// the request's route-set snapshot; recorded on the forward log row.
    pub(super) route: RouteLabel,
    pub(super) known_secret: Option<String>,
    pub(super) restriction_details: Option<Value>,
    pub(super) route_account_id: Option<String>,
    pub(super) provider_id: Option<String>,

    pub(super) credential_account_id: Option<String>,
    pub(super) client_key_id: Option<String>,
    pub(super) client_key_name: Option<String>,
    pub(super) platform_price: Option<PlatformAttemptPrice>,
    pub(super) official_price: Option<crate::official_api::OfficialAttemptPrice>,
    pub(super) credit_attempt: Option<crate::billing::CreditAttempt>,
    pub(super) credit_log_id: Option<i64>,
    pub(super) credit_token_pricing_supported: bool,
}

impl ForwardAttemptContext {
    pub(super) fn new(
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
            official_price: None,
            restriction_details: None,
            credit_attempt: None,
            credit_log_id: None,
            credit_token_pricing_supported: true,
        }
    }

    /// Records which gateway key authenticated the request; the name is a
    /// write-time snapshot resolved from the credential snapshot (the primary
    /// key resolves to the fixed "Primary") so later renames keep historical
    /// attribution without a config or db lookup.
    pub(super) fn set_client_key(&mut self, id: Option<&str>, state: &CoreState) {
        self.client_key_id = id.map(str::to_string);
        self.client_key_name = id.and_then(|id| state.client_key_name(id));
    }

    pub(super) fn set_known_secret(&mut self, known_secret: &str) {
        self.known_secret = Some(known_secret.to_string());
    }

    pub(super) fn set_provider_route(&mut self, account: &ExecutionCredential, spec: &AttemptSpec) {
        self.route_account_id = Some(account.id.clone());
        self.provider_id = Some(account.provider_id.clone());
        self.credential_account_id = spec.credential_account_id().map(str::to_string);
    }

    pub(super) fn attach_pricing(&mut self, pricing: &RequestPricingSnapshot) {
        match pricing {
            RequestPricingSnapshot::Platform(price) => {
                self.platform_price = Some(price.clone());
            }
            RequestPricingSnapshot::OfficialApi(price) => {
                self.official_price = Some(price.clone());
            }
            RequestPricingSnapshot::Credits {
                attempt,
                token_pricing_supported,
                ..
            } => {
                self.credit_attempt = Some(attempt.clone());
                self.credit_token_pricing_supported = *token_pricing_supported;
            }
            _ => {}
        }
    }

    pub(super) fn redact_known_secret(&self, text: &str) -> String {
        self.known_secret.as_deref().map_or_else(
            || text.to_string(),
            |secret| redact_known_secret(text, secret),
        )
    }

    pub(super) fn sanitize_upstream_error(&self, text: &str) -> String {
        self.known_secret.as_deref().map_or_else(
            || sanitize_upstream_error(text, ""),
            |secret| sanitize_upstream_error(text, secret),
        )
    }

    pub(super) fn failure(&self, spec: FailureSpec<'_>) -> FailureRecord {
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
        let mut diagnostic_json = serialize_diagnostic(diagnostic);
        if let Some(details) = &self.restriction_details
            && let Ok(Value::Object(mut value)) = serde_json::from_str::<Value>(&diagnostic_json)
        {
            value.insert("restriction".into(), details.clone());
            diagnostic_json = Value::Object(value).to_string();
        }
        emit_failure(&diagnostic_json);
        FailureRecord {
            error_source: spec.error_source.to_string(),
            error_stage: spec.error_stage.to_string(),
            duration_ms,
            diagnostic_json,
        }
    }
}

pub(super) struct FailureSpec<'a> {
    pub(super) error_source: &'static str,
    pub(super) error_stage: &'static str,
    pub(super) downstream_status: Option<u16>,
    pub(super) upstream_status: Option<u16>,
    pub(super) upstream_wait_ms: Option<u64>,
    pub(super) retry_action: Option<&'static str>,
    pub(super) upstream_headers: Option<&'a HeaderMap>,
    pub(super) upstream_error: Option<&'a str>,
    pub(super) request_body: Option<&'a [u8]>,
}

pub(super) struct FailureRecord {
    pub(super) error_source: String,
    pub(super) error_stage: String,
    pub(super) duration_ms: i64,
    pub(super) diagnostic_json: String,
}

impl FailureRecord {
    pub(super) fn update(&self) -> ForwardLogDiagnosticUpdate<'_> {
        ForwardLogDiagnosticUpdate {
            error_source: &self.error_source,
            error_stage: &self.error_stage,
            duration_ms: self.duration_ms,
            diagnostic_json: &self.diagnostic_json,
        }
    }
}

pub(super) fn no_replay_retry_action() -> &'static str {
    retry_action_name(forward_action_for_class(
        classify_stream(StreamClassifyInput::AfterDownstreamBytes),
        false,
        None,
    ))
}

pub(super) fn retry_action_name(action: ForwardAction) -> &'static str {
    match action {
        ForwardAction::Return => "return",
        ForwardAction::RetrySameAccount => "retry_same_account",
        ForwardAction::TryNextAccount => "try_next_account",
        ForwardAction::ExhaustFreeChannel => "exhaust_free_channel",
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn log_unsent_admission_skip(
    state: &CoreState,
    account: &ExecutionCredential,
    plan: &RequestPlan,
    route: RouteLabel,
    spec: &AttemptSpec,
    trace: &RequestTrace,
    client_body: &[u8],
    attempt: u32,
    client_key_id: Option<&str>,
    pricing_snapshot: &RequestPricingSnapshot,
    wait: &crate::gateway::recovery::WaitState,
) -> Result<()> {
    let mut attempt_context =
        ForwardAttemptContext::new(trace, client_body.len(), attempt, plan, route);
    attempt_context.attach_pricing(pricing_snapshot);
    attempt_context.set_client_key(client_key_id, state);
    attempt_context.set_provider_route(account, spec);
    attempt_context.restriction_details = Some(serde_json::json!({"wait": wait}));
    let message = if wait.is_local_policy() {
        "compatible upstream resource is waiting on local_policy; no upstream request sent"
    } else if wait.is_capacity() {
        "compatible upstream resource cannot be tracked (recovery_capacity); no upstream request sent"
    } else {
        "compatible upstream resource is waiting for recovery; no upstream request sent"
    };
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
            pricing_snapshot,
            plan.service_tier.as_deref(),
            "not_applicable",
        ),
        Some(message),
        &attempt_context,
        Some(failure),
    )?;
    Ok(())
}

pub(super) fn account_preflight_failure(plan: &RequestPlan, message: String) -> ForwardResult {
    ForwardResult {
        response: error_response(plan.client, &message, None),
        action: ForwardAction::TryNextAccount,
        error_message: Some(message),
        sent: false,
    }
}

pub(super) fn protocol_status_error_response(
    format: ApiFormat,
    status: StatusCode,
    message: &str,
    upstream: Option<&Value>,
) -> Response {
    let body = format_error(format, status, message, upstream);
    (status, axum::Json(body)).into_response()
}

pub(super) fn outcome_unknown_message(detail: &str) -> String {
    format!(
        "upstream outcome is unknown: {detail}; the request may have completed and consumed quota; the gateway did not retry it"
    )
}

pub(super) fn outcome_unknown_retry_message(detail: &str) -> String {
    format!(
        "upstream outcome is unknown: {detail}; the request may have completed and consumed quota; the gateway is retrying it once because no downstream SSE data was emitted"
    )
}

pub(crate) fn outcome_unknown_response(
    format: ApiFormat,
    status: StatusCode,
    detail: &str,
) -> Response {
    let message = outcome_unknown_message(detail);
    outcome_unknown_response_with_message(format, status, &message)
}

pub(super) fn outcome_unknown_response_with_message(
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
    let retry_after = resets_at
        .signed_duration_since(Utc::now())
        .num_seconds()
        .max(1)
        .to_string();
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(axum::http::header::RETRY_AFTER, retry_after)],
        axum::Json(body),
    )
        .into_response()
}

fn log_attempt_outcome(
    db: &Database,
    context: &ForwardAttemptContext,
    status: &str,
    http_status: Option<i32>,
    diagnostic: Option<&ForwardLogDiagnosticUpdate<'_>>,
) {
    let level = if status.starts_with("success") {
        "info"
    } else if status == "streaming" {
        "debug"
    } else if matches!(status, "client_error" | "cancelled") {
        "warn"
    } else {
        "error"
    };
    let fields = serde_json::json!({"status": status, "http_status": http_status});
    crate::gateway::diagnostics::log_event(
        db,
        &context.trace,
        level,
        "upstream",
        "attempt_outcome",
        Some(context.attempt),
        fields,
    );
    if let Some(diagnostic) = diagnostic {
        let _ = db.log_gateway_diagnostic(
            level,
            "upstream",
            "attempt_failure",
            Some(&context.trace.request_id),
            Some(i64::from(context.attempt)),
            Some(diagnostic.error_source),
            Some(diagnostic.error_stage),
            Some(diagnostic.duration_ms),
            Some(diagnostic.diagnostic_json),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn log_forward(
    db: &Database,
    account: &ExecutionCredential,
    model: &str,
    status: &str,
    http_status: Option<i32>,
    mut metrics: ForwardMetrics,
    error_message: Option<&str>,
    context: &ForwardAttemptContext,
    failure: Option<FailureRecord>,
) -> Result<i64> {
    if let Some(id) = context.credit_log_id {
        // The pre-send credit row also becomes the existing SSE row.
        if status != "streaming" {
            let diagnostic = failure.as_ref().map(FailureRecord::update);
            let redacted = error_message.map(|message| context.redact_known_secret(message));
            finalize_logged_forward(
                db,
                id,
                status,
                http_status,
                metrics,
                redacted.as_deref(),
                diagnostic.as_ref(),
                context,
            )?;
        } else if let Some(http_status) = http_status {
            db.conn.execute(
                "UPDATE forward_logs SET http_status=?1 WHERE id=?2",
                rusqlite::params![http_status, id],
            )?;
        }
        return Ok(id);
    }
    log_attempt_outcome(
        db,
        context,
        status,
        http_status,
        failure.as_ref().map(FailureRecord::update).as_ref(),
    );
    let transaction = context
        .credit_attempt
        .as_ref()
        .map(|_| db.conn.unchecked_transaction())
        .transpose()?;
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
    if let Some(credit) = context.credit_attempt.as_ref() {
        crate::db::billing::attach_attempt_on(&db.conn, id, credit)?;
        if status != "streaming" {
            settle_credit_log(db, id, context, &persist_metrics, status)?;
        }
    }
    if let Some(transaction) = transaction {
        transaction.commit()?;
    }
    Ok(id)
}

fn settle_credit_log(
    db: &Database,
    id: i64,
    context: &ForwardAttemptContext,
    metrics: &ForwardMetrics,
    status: &str,
) -> Result<()> {
    let Some(credit) = context.credit_attempt.as_ref() else {
        return Ok(());
    };
    let tokens = ocg_domain::billing::BillingTokens::new(
        metrics.prompt_tokens,
        metrics.completion_tokens,
        metrics.cached_tokens,
        metrics.cache_creation_tokens,
    );
    let usable_usage = matches!(metrics.cost_state, "unknown" | "priced" | "free");
    let settlement_status = if !context.credit_token_pricing_supported
        && (status.starts_with("success") || usable_usage)
    {
        "success_no_usage"
    } else if usable_usage {
        "success_unpriced"
    } else if status == "error" {
        // A failed decode/transform after upstream acceptance is not proof of
        // zero usage. Preserve the HTTP/log/retry behavior while settling the
        // receipt as uncertain. Known token usage above can still be priced.
        let upstream_status: Option<i32> = db.conn.query_row(
            "SELECT http_status FROM forward_logs WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        if upstream_status.is_some_and(|code| (200..300).contains(&code)) {
            "outcome_unknown"
        } else {
            status
        }
    } else {
        status
    };
    crate::db::billing::settle_on(&db.conn, id, credit, tokens, settlement_status, Utc::now())
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
    apply_native_cost_attribution(
        &mut attribution,
        context.platform_price.as_ref(),
        context.official_price.as_ref(),
        metrics,
    );
    db.set_forward_log_native_attribution(id, &attribution)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn finalize_logged_forward(
    db: &Database,
    id: i64,
    status: &str,
    http_status: Option<i32>,
    metrics: ForwardMetrics,
    error_message: Option<&str>,
    diagnostic: Option<&ForwardLogDiagnosticUpdate<'_>>,
    context: &ForwardAttemptContext,
) -> Result<()> {
    let transaction = context
        .credit_attempt
        .as_ref()
        .map(|_| db.conn.unchecked_transaction())
        .transpose()?;
    if context.credit_attempt.is_some() && crate::db::billing::settlement_finished_on(&db.conn, id)?
    {
        return Ok(());
    }
    db.update_forward_log(
        id,
        status,
        http_status,
        metrics.clone(),
        error_message,
        diagnostic,
    )?;
    persist_log_identity(db, id, context, &metrics)?;
    settle_credit_log(db, id, context, &metrics, status)?;
    if let Some(transaction) = transaction {
        transaction.commit()?;
    }
    log_attempt_outcome(db, context, status, http_status, diagnostic);
    Ok(())
}

pub(super) fn success_status_for_cost(cost_state: &str) -> &'static str {
    match cost_state {
        "priced" | "free" => "success",
        "usage_missing" => "success_no_usage",
        _ => "success_unpriced",
    }
}
