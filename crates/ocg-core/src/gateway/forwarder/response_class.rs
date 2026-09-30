//! Buffered response classification and free-channel rejection observation.

use super::*;

pub(super) fn error_response(
    format: ApiFormat,
    message: &str,
    upstream: Option<&Value>,
) -> Response {
    let body = format_error(format, StatusCode::BAD_GATEWAY, message, upstream);
    (StatusCode::BAD_GATEWAY, axum::Json(body)).into_response()
}

pub(super) fn is_openrouter_free_request(url: &reqwest::Url, model: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("openrouter.ai")
        && url.port_or_known_default() == Some(443)
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(
            url.path().trim_end_matches('/'),
            "/api/v1/chat/completions" | "/api/v1/responses" | "/api/v1/messages"
        )
        && (model == "openrouter/free" || model.ends_with(":free"))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observe_local_policy(
    state: &CoreState,
    account: &ExecutionCredential,
    adapter: ProviderAdapterKind,
    class: ProviderErrorClass,
    selection: &LiveSendSelection,
    recovery_permit: &mut RecoveryPermit,
    endpoint: &str,
    model: &str,
    free_contract: bool,
    http_status: u16,
    policy_body: Option<&str>,
    retry_after: Option<&str>,
    observed_at: chrono::DateTime<chrono::Utc>,
    observed_mono: Instant,
) -> Result<()> {
    if !(400..=599).contains(&http_status) {
        return Ok(());
    }
    let input = crate::gateway::policy::PolicyInput {
        adapter,
        class,
        http_status: Some(http_status),
        error: policy_body.and_then(crate::gateway::policy::extract_top_level_error),
    };
    let decisions = recovery_permit.evaluate_captured(&input);
    if decisions.is_empty() {
        return Ok(());
    }
    let retry_hint = retry_after.and_then(|value| parse_retry_after(value, observed_at));
    let db = state.db.lock();
    let identity_ok = live_send::selection_identity_is_current(&db, selection)?
        && recovery_permit.same_policy_identity(&ResourceSet::capture(
            &db,
            account,
            endpoint,
            model,
            free_contract,
        )?);
    if identity_ok {
        for decision in &decisions {
            if !recovery_permit.permits_policy(decision) {
                continue;
            }
            recovery_permit.observe_policy(decision, observed_mono);
            if let Some(hint) = retry_hint {
                recovery_permit.observe_policy_retry(decision.scope, hint, observed_mono);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observe_openrouter_free_rejection(
    state: &CoreState,
    account: &ExecutionCredential,
    selection: &LiveSendSelection,
    recovery_permit: &mut RecoveryPermit,
    endpoint: &str,
    model: &str,
    retry_after: Option<&str>,
    attempt: &mut ForwardAttemptContext,
) -> Result<()> {
    let (observed_at, observed_mono) = state.sample_gateway_clock();
    let facts = openrouter_free_rejection(retry_after, observed_at);
    let decision = facts.decide();
    let db = state.db.lock();
    let current = live_send::selection_identity_is_current(&db, selection)?
        && recovery_permit.permits_observation(&facts)
        && recovery_permit
            .same_generation(&ResourceSet::capture(&db, account, endpoint, model, false)?);
    if current {
        recovery_permit.observe_failure(&facts, decision, observed_mono);
    }
    attempt.restriction_details = Some(serde_json::json!({
        "facts": facts, "recorded_for_current_generation": current,
        "local_reprobe": decision.wait_for_recovery,
    }));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observe_free_rejection(
    state: &CoreState,
    account: &ExecutionCredential,
    selection: &LiveSendSelection,
    recovery_permit: &mut RecoveryPermit,
    endpoint: &str,
    model: &str,
    retry_after: Option<&str>,
    attempt: &mut ForwardAttemptContext,
) -> Result<()> {
    let (observed_at, observed_mono) = state.sample_gateway_clock();
    let facts = decode_failure(
        ProviderErrorClass::FreeRejected,
        "",
        retry_after,
        observed_at,
    )
    .ok_or_else(|| anyhow::anyhow!("Free rejection lacks a recovery policy"))?;
    let decision = facts.decide();
    let db = state.db.lock();
    let current = live_send::selection_identity_is_current(&db, selection)?
        && recovery_permit.permits_observation(&facts)
        && recovery_permit
            .same_generation(&ResourceSet::capture(&db, account, endpoint, model, true)?);
    if current {
        recovery_permit.observe_failure(&facts, decision, observed_mono);
    }
    attempt.restriction_details = Some(serde_json::json!({
        "facts": facts, "recorded_for_current_generation": current,
        "local_reprobe": decision.wait_for_recovery,
    }));
    Ok(())
}

pub fn forward_action_for_class(
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
        | ProviderErrorClass::ForbiddenRotate
        | ProviderErrorClass::InsufficientCredits => ForwardAction::TryNextAccount,
        ProviderErrorClass::FreeRejected => ForwardAction::ExhaustFreeChannel,
        ProviderErrorClass::RateLimited { .. } => match rate_limit_fallback(rate_limit_window) {
            RateLimitFallback::ExhaustFreeChannel => ForwardAction::ExhaustFreeChannel,
            RateLimitFallback::TryNextAccount => ForwardAction::TryNextAccount,
        },
        _ => ForwardAction::Return,
    }
}
