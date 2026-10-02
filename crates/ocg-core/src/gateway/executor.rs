//! Captures a logical request's identities, catalog, transport and prices,
//! then executes bounded selection, retry and fallback. The handler owns
//! client authentication and parsing; single-attempt I/O lives in forwarder.

use crate::alias;
use crate::gateway::classify::{ProviderErrorClass, classify_http};
use crate::gateway::diagnostics::{
    ErrorDiagnostic, RequestTrace, emit_failure, serialize_diagnostic,
};
use crate::gateway::forwarder::{
    AttemptRequest, AttemptSender, ForwardAction, LiveSendSelection, log_unsent_admission_skip,
    rate_limited_response,
};
use crate::gateway::materialize::materialize_execution_routes;
use crate::gateway::protocol::{
    ProtocolError, RequestFacts, RequestPlan, validate_client_request_features,
};
use crate::gateway::response::{local_protocol_failure, protocol_error_response};
use crate::gateway::routing::resolve_conversation_key;

use crate::gateway::attempt::UpstreamAuth;
use crate::gateway::recovery::{ResourceSet, restriction_endpoint_identity};
use crate::http_client::{ForwardRouteSet, RouteLabel};
use crate::kernel::pricing::PricingSnapshot;
use crate::kernel::protocol::ApiFormat;
use crate::models::AppConfig;
use crate::state::CoreState;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use ocg_gateway::selector::SelectionError;
use std::sync::Arc;
use std::time::Duration;
const MAX_REQUEST_ATTEMPTS: u32 = 32;

fn request_budget_duration(config: &AppConfig, stream: bool) -> Duration {
    Duration::from_secs(
        if stream {
            config.stream_idle_timeout_secs
        } else {
            config.non_stream_timeout_secs
        }
        .max(1),
    )
}

/// Catalog, route, and pricing identities frozen once at request entry.
pub(crate) enum RequestEntryError {
    Resolve(crate::alias::ResolveError),
    Capture(anyhow::Error),
}

pub(crate) struct RequestEntry {
    pub(crate) snapshots: RequestSnapshots,
}

/// One selection decision's live view. The host reads these together so the
/// executor does not touch the database, probe map, or recovery gate itself.
pub(crate) struct SelectionView {
    pub(crate) live: crate::routing_snapshot::RoutingSnapshot,
    pub(crate) decision_wall: chrono::DateTime<chrono::Utc>,
    pub(crate) decision_mono: std::time::Instant,
    pub(crate) free_cooldown: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) free_egress_wait: Option<chrono::DateTime<chrono::Utc>>,
}

/// Process-state values frozen at request entry. Live credential availability
/// and authorization are reread before every dispatch.
pub(crate) struct RequestSnapshots {
    config: AppConfig,
    pricing: Arc<PricingSnapshot>,
    routes: Arc<ForwardRouteSet>,
    resolved: alias::ResolvedModel,
    cpa_base_url: Option<String>,
    routing: crate::routing_snapshot::RoutingSnapshot,
}

impl RequestSnapshots {
    pub(crate) fn capture(
        state: &crate::state::CoreStateInner,
        config: AppConfig,
        resolved: alias::ResolvedModel,
        routing: crate::routing_snapshot::RoutingSnapshot,
    ) -> anyhow::Result<Self> {
        let cpa_base_url = crate::cpa::env_base_url()?.or_else(|| {
            routing
                .projection
                .destinations
                .iter()
                .find(|d| d.adapter == ocg_domain::destination::AdapterKind::Cpa)
                .and_then(|d| d.base_url.clone())
        });
        Ok(Self {
            config,
            pricing: state.pricing_snapshot(),
            routes: state.forward_route_set(),
            resolved,
            cpa_base_url,
            routing,
        })
    }
}

/// Mutable selection and retry counters for one client request.
struct LoopState {
    last_error: Option<String>,
    failed_ids: Vec<String>,
    attempt: u32,
    send_attempts: u32,
}

impl LoopState {
    fn new() -> Self {
        Self {
            last_error: None,
            failed_ids: Vec::new(),
            attempt: 0,
            send_attempts: 0,
        }
    }
}

/// Orchestrates one parsed client request.
pub(crate) struct GatewayExecutor;

impl GatewayExecutor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn run(
        state: CoreState,
        trace: RequestTrace,
        client_body: Bytes,
        headers: HeaderMap,
        client_format: ApiFormat,
        parsed: crate::gateway::protocol::ParsedClientRequest,
        client_model: String,
        routing_model: String,
        client_key_id: Option<String>,
    ) -> Response {
        let (snapshots, facts, route_set, prices) = {
            // The host freezes catalog, route and pricing identities. No guard
            // crosses upstream I/O.
            let snapshots = match state.capture_request_entry(&routing_model) {
                Ok(entry) => entry.snapshots,
                Err(RequestEntryError::Resolve(error)) => {
                    return local_protocol_failure(
                        &state,
                        &trace,
                        client_format,
                        crate::gateway::materialize::protocol_error_from_resolve(error),
                        Some(client_body.len()),
                        Some(&client_body),
                    );
                }
                Err(RequestEntryError::Capture(error)) => {
                    return protocol_error_response(
                        client_format,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("failed to capture routing state: {error}"),
                        None,
                    );
                }
            };
            let facts = RequestFacts::from_parsed(&parsed);
            if let Err(error) = validate_client_request_features(&parsed) {
                return local_protocol_failure(
                    &state,
                    &trace,
                    client_format,
                    error,
                    Some(client_body.len()),
                    Some(&client_body),
                );
            }

            let route_set = match materialize_execution_routes(
                &snapshots.routing,
                &snapshots.config,
                &parsed,
                &snapshots.resolved,
                &client_model,
                &routing_model,
                snapshots.cpa_base_url.as_deref(),
            ) {
                Ok(routes) => routes,
                Err(error) => {
                    return local_protocol_failure(
                        &state,
                        &trace,
                        client_format,
                        error,
                        Some(client_body.len()),
                        Some(&client_body),
                    );
                }
            };
            if route_set.routes.is_empty()
                && !route_set.rejections.is_empty()
                && route_set.rejections.iter().all(|rejection| {
                    rejection.code
                        == crate::gateway::materialize::RouteRejectionCode::MappingProtocolIncompatible
                })
            {
                return local_protocol_failure(
                    &state,
                    &trace,
                    client_format,
                    ProtocolError::new(crate::provider_contracts::NO_ENABLED_UPSTREAM_PROTOCOL),
                    Some(client_body.len()),
                    Some(&client_body),
                );
            }
            let prices = route_set
                .routes
                .iter()
                .map(|route| {
                    crate::gateway::attempt_pricing::capture_execution_pricing(
                        &state,
                        &route.routing.account,
                        route.routing.adapter,
                        &route.plan,
                        snapshots.pricing.clone(),
                    )
                })
                .collect::<Vec<_>>();
            (snapshots, facts, route_set, prices)
        };
        let mut loop_state = LoopState::new();
        let conversation_key = if snapshots.config.conversation_sticky {
            resolve_conversation_key(client_format, &routing_model, &headers, &client_body)
        } else {
            None
        };
        let request_deadline =
            tokio::time::Instant::now() + request_budget_duration(&snapshots.config, facts.stream);
        loop {
            if tokio::time::Instant::now() >= request_deadline {
                let message =
                    "Gateway request retry budget exhausted; no further upstream attempt sent";
                record_request_failure(
                    &state,
                    &trace,
                    &client_body,
                    loop_state.attempt.max(1),
                    &facts,
                    "gateway",
                    "request_budget",
                    StatusCode::SERVICE_UNAVAILABLE,
                    message,
                );
                return protocol_error_response(
                    client_format,
                    StatusCode::SERVICE_UNAVAILABLE,
                    message,
                    None,
                );
            }
            let max_scans = (route_set.routes.len() as u32)
                .saturating_add(MAX_REQUEST_ATTEMPTS)
                .max(1);
            if loop_state.attempt >= max_scans {
                let message =
                    "Gateway request candidate scan exhausted; no further upstream attempt sent";
                record_request_failure(
                    &state,
                    &trace,
                    &client_body,
                    loop_state.attempt.max(1),
                    &facts,
                    "gateway",
                    "request_budget",
                    StatusCode::SERVICE_UNAVAILABLE,
                    message,
                );
                return protocol_error_response(
                    client_format,
                    StatusCode::SERVICE_UNAVAILABLE,
                    message,
                    None,
                );
            }
            let SelectionView {
                live,
                decision_wall,
                decision_mono,
                free_cooldown,
                free_egress_wait,
            } = match state.capture_selection_view() {
                Ok(view) => view,
                Err(error) => {
                    return protocol_error_response(
                        client_format,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("failed to load routing state: {error}"),
                        None,
                    );
                }
            };
            let free_available = free_cooldown.is_none()
                && free_egress_wait.is_none()
                && !crate::destination_projection::free_channel_exhausted(
                    &live.projection,
                    decision_wall,
                );
            let excluded = loop_state
                .failed_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let mut live_authorization_error = None;
            let routing_candidates = route_set
                .routes
                .iter()
                .map(|route| {
                    let mut candidate = route.routing.clone();
                    if let Some(current) = live
                        .credentials
                        .iter()
                        .find(|c| c.id == candidate.account.id)
                    {
                        candidate.account = current.clone();
                    } else {
                        candidate.account.enabled = false;
                    }
                    let selection =
                        LiveSendSelection::from_execution(route, &client_model, &routing_model);
                    if let Err(error) = crate::gateway::forwarder::verify_execution_authorization(
                        &live,
                        &selection,
                        &route.spec,
                        decision_wall,
                        free_available,
                    ) {
                        candidate.account.enabled = false;
                        if !excluded.contains(&candidate.account.id.as_str()) {
                            live_authorization_error.get_or_insert_with(|| error.to_string());
                        }
                    }
                    candidate
                })
                .collect::<Vec<_>>();
            let selected_index = match state.select_candidate_index(
                &routing_candidates,
                snapshots.config.routing_mode,
                snapshots.config.conversation_sticky,
                conversation_key.as_deref(),
                &excluded,
                free_available,
                decision_wall,
                decision_mono,
            ) {
                Ok(Some(index)) => index,
                Ok(None) => {
                    let free_wait = free_cooldown.or(free_egress_wait);
                    if route_set.free_only
                        && let Some(until) = free_wait
                    {
                        record_request_failure(
                            &state,
                            &trace,
                            &client_body,
                            loop_state.attempt.max(1),
                            &facts,
                            "gateway",
                            "account_selection",
                            StatusCode::TOO_MANY_REQUESTS,
                            "free channel is rate-limited",
                        );
                        return rate_limited_response(client_format, until);
                    }
                    let mut any_local_policy = false;
                    let soonest = route_set
                        .routes
                        .iter()
                        .filter_map(|route| {
                            let credential = live
                                .credentials
                                .iter()
                                .find(|row| row.id == route.routing.account.id)?;
                            let cooldown = credential
                                .cooldown_ends_at_for(route.routing.channel, decision_wall);
                            let quota = (!credential.quota_probe)
                                .then_some(credential.quota_recovery.as_ref())
                                .flatten()
                                .and_then(|recovery| {
                                    (recovery.next_retry_at > decision_wall)
                                        .then_some(recovery.next_retry_at)
                                });
                            let free_contract =
                                route.routing.channel == crate::models::UpstreamChannel::Free;
                            let temporary = ResourceSet::from_snapshot(
                                &live,
                                credential,
                                "",
                                &route.plan.model,
                                free_contract,
                            )
                            .ok()
                            .and_then(|resources| {
                                state.credential_retry_until(&resources, decision_wall)
                            });
                            if let Ok(resources) = capture_route_resources(&live, route, &snapshots)
                                && let Err(wait) = state.inspect_route_admission(
                                    &resources,
                                    decision_wall,
                                    decision_mono,
                                )
                                && (wait.is_local_policy() || wait.is_capacity())
                            {
                                any_local_policy = true;
                            }
                            let free = free_contract.then_some(free_egress_wait).flatten();
                            // A Key must outwait every known blocker; another Key
                            // may become available sooner.
                            [cooldown, quota, temporary, free]
                                .into_iter()
                                .flatten()
                                .max()
                        })
                        .min();
                    let probe_only = soonest.is_none()
                        && route_set.routes.iter().any(|route| {
                            live.credentials.iter().any(|credential| {
                                credential.id == route.routing.account.id && credential.quota_probe
                            })
                        });
                    return match soonest {
                        Some(until) => {
                            record_request_failure(
                                &state,
                                &trace,
                                &client_body,
                                loop_state.attempt.max(1),
                                &facts,
                                "gateway",
                                "account_selection",
                                StatusCode::TOO_MANY_REQUESTS,
                                "all compatible accounts are rate-limited",
                            );
                            rate_limited_response(client_format, until)
                        }
                        None if probe_only => {
                            let msg = "quota recovery trial is already in flight";
                            record_request_failure(
                                &state,
                                &trace,
                                &client_body,
                                loop_state.attempt.max(1),
                                &facts,
                                "gateway",
                                "account_selection",
                                StatusCode::SERVICE_UNAVAILABLE,
                                msg,
                            );
                            protocol_error_response(
                                client_format,
                                StatusCode::SERVICE_UNAVAILABLE,
                                msg,
                                None,
                            )
                        }
                        None if any_local_policy => {
                            let msg = "all compatible accounts are waiting on local_policy";
                            record_request_failure(
                                &state,
                                &trace,
                                &client_body,
                                loop_state.attempt.max(1),
                                &facts,
                                "gateway",
                                "local_policy",
                                StatusCode::SERVICE_UNAVAILABLE,
                                msg,
                            );
                            protocol_error_response(
                                client_format,
                                StatusCode::SERVICE_UNAVAILABLE,
                                msg,
                                None,
                            )
                        }
                        None => {
                            let msg = live_authorization_error
                                .or_else(|| loop_state.last_error.clone())
                                .unwrap_or_else(|| {
                                    route_set.incompatibility.unwrap_or_else(|| {
                                        "no compatible provider accounts are available".to_string()
                                    })
                                });
                            record_request_failure(
                                &state,
                                &trace,
                                &client_body,
                                loop_state.attempt.max(1),
                                &facts,
                                "gateway",
                                "account_selection",
                                StatusCode::SERVICE_UNAVAILABLE,
                                &msg,
                            );
                            protocol_error_response(
                                client_format,
                                StatusCode::SERVICE_UNAVAILABLE,
                                &msg,
                                None,
                            )
                        }
                    };
                }
                Err(error) => {
                    let (status, message) =
                        routing_selector_invariant(SelectorInvariant::Duplicate(error));
                    record_request_failure(
                        &state,
                        &trace,
                        &client_body,
                        loop_state.attempt.max(1),
                        &facts,
                        "gateway",
                        "account_selection",
                        status,
                        &message,
                    );
                    return protocol_error_response(client_format, status, &message, None);
                }
            };
            let route = match route_set.routes.get(selected_index).cloned() {
                Some(route) => route,
                None => {
                    let (status, message) =
                        routing_selector_invariant(SelectorInvariant::CandidateIndexOutOfRange {
                            selected_index,
                        });
                    record_request_failure(
                        &state,
                        &trace,
                        &client_body,
                        loop_state.attempt.max(1),
                        &facts,
                        "gateway",
                        "account_selection",
                        status,
                        &message,
                    );
                    return protocol_error_response(client_format, status, &message, None);
                }
            };
            let selection =
                LiveSendSelection::from_execution(&route, &client_model, &routing_model);
            if let Ok(resources) = capture_route_resources(&live, &route, &snapshots)
                && let Err(wait) =
                    state
                        .recovery
                        .inspect_admission(&resources, decision_wall, decision_mono)
            {
                loop_state.attempt = loop_state.attempt.saturating_add(1);
                let skip_route = send_route_label(&snapshots, route.routing.adapter, &route.plan);
                let _ = log_unsent_admission_skip(
                    &state,
                    &route.routing.account,
                    &route.plan,
                    skip_route,
                    &route.spec,
                    &trace,
                    &client_body,
                    loop_state.attempt,
                    client_key_id.as_deref(),
                    &prices[selected_index],
                    &wait,
                );
                loop_state.last_error = Some(
                    if wait.is_local_policy() {
                        "compatible upstream resource is waiting on local_policy; no upstream request sent"
                    } else if wait.is_capacity() {
                        "compatible upstream resource cannot be tracked (recovery_capacity); no upstream request sent"
                    } else {
                        "compatible upstream resource is waiting for recovery; no upstream request sent"
                    }
                    .into(),
                );
                loop_state.failed_ids.push(route.routing.account.id.clone());
                continue;
            }
            let adapter = route.routing.adapter;
            let account = route.routing.account;
            let active_plan = route.plan;
            let frozen_spec = route.spec;

            let mut retried_same_account = false;
            loop {
                if loop_state.send_attempts >= MAX_REQUEST_ATTEMPTS
                    || tokio::time::Instant::now() >= request_deadline
                {
                    let message =
                        "Gateway request retry budget exhausted; no further upstream attempt sent";
                    record_plan_failure(
                        &state,
                        &trace,
                        &client_body,
                        loop_state.attempt.max(1),
                        client_format,
                        &active_plan,
                        "gateway",
                        "request_budget",
                        StatusCode::SERVICE_UNAVAILABLE,
                        message,
                    );
                    return protocol_error_response(
                        client_format,
                        StatusCode::SERVICE_UNAVAILABLE,
                        message,
                        None,
                    );
                }
                loop_state.attempt = loop_state.attempt.saturating_add(1);
                // Re-resolve the leg on every attempt: free fallback or sticky
                // rewrites can swap `active_plan.model` mid-request.
                let (client, selected_route) = snapshots.routes.client_for(&active_plan.model);
                let route = if adapter == crate::provider::ProviderAdapterKind::Cpa {
                    RouteLabel::Direct
                } else {
                    selected_route
                };
                // The attempt owns timeout finalization so a known HTTP status
                // and the selected account cannot be lost to outer cancellation.
                let forwarded = state
                    .send_attempt(AttemptRequest {
                        client,
                        route,
                        account: &account,
                        adapter,
                        config: &snapshots.config,
                        plan: &active_plan,
                        trace: &trace,
                        client_body: &client_body,
                        attempt: loop_state.attempt,
                        allow_same_account_retry: !retried_same_account,
                        headers: headers.clone(),
                        pricing_snapshot: prices[selected_index].clone(),
                        client_key_id: client_key_id.as_deref(),
                        attempt_spec: &frozen_spec,
                        selection: &selection,
                        deadline: Some(request_deadline),
                    })
                    .await;
                match forwarded {
                    Ok(result) => {
                        if result.sent {
                            loop_state.send_attempts = loop_state.send_attempts.saturating_add(1);
                        }
                        match result.action {
                            ForwardAction::Return => return result.response,
                            ForwardAction::RetrySameAccount if !retried_same_account => {
                                retried_same_account = true;
                                continue;
                            }
                            ForwardAction::RetrySameAccount => return result.response,
                            ForwardAction::ExhaustFreeChannel => {
                                loop_state.last_error = result.error_message.clone();
                                loop_state.failed_ids.push(account.id.clone());
                                break;
                            }
                            ForwardAction::TryNextAccount => {
                                loop_state.last_error = result.error_message.clone();
                                loop_state.failed_ids.push(account.id.clone());
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let message = format!("forward error: {e}");
                        record_plan_failure(
                            &state,
                            &trace,
                            &client_body,
                            loop_state.attempt,
                            client_format,
                            &active_plan,
                            "gateway",
                            "internal",
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &format!("account {} forward failed locally: {e}", account.name),
                        );
                        return protocol_error_response(
                            client_format,
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &message,
                            None,
                        );
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn record_request_failure(
    state: &CoreState,
    trace: &RequestTrace,
    client_body: &[u8],
    attempt: u32,
    facts: &RequestFacts,
    error_source: &str,
    error_stage: &str,
    status: StatusCode,
    message: &str,
) {
    let mut diagnostic =
        ErrorDiagnostic::new(trace, attempt, error_source, error_stage, facts.client)
            .with_request_summary(client_body);
    diagnostic.client_body_bytes = Some(client_body.len());
    diagnostic.model = Some(facts.client_model.clone());
    diagnostic.stream = Some(facts.stream);
    diagnostic.downstream_status = Some(status.as_u16());
    let encoded = serialize_diagnostic(diagnostic.clone());
    state.record_request_failure(trace, &diagnostic, &encoded, message);
    emit_failure(&encoded);
}

#[allow(clippy::too_many_arguments)]
fn record_plan_failure(
    state: &CoreState,
    trace: &RequestTrace,
    client_body: &[u8],
    attempt: u32,
    client_format: ApiFormat,
    plan: &RequestPlan,
    error_source: &str,
    error_stage: &str,
    status: StatusCode,
    message: &str,
) {
    let mut diagnostic =
        ErrorDiagnostic::new(trace, attempt, error_source, error_stage, client_format)
            .with_request_summary(client_body);
    diagnostic.client_body_bytes = Some(client_body.len());
    diagnostic.upstream_body_bytes = Some(plan.body.len());
    diagnostic.upstream_format =
        Some(crate::gateway::diagnostics::api_format_name(plan.upstream).to_string());
    diagnostic.model = Some(plan.model.clone());
    diagnostic.stream = Some(plan.stream);
    diagnostic.downstream_status = Some(status.as_u16());
    if error_stage == "request_budget" {
        diagnostic.retry_action = Some(
            if error_source == "transport" {
                "no_replay_outcome_unknown"
            } else {
                "return"
            }
            .to_string(),
        );
    }
    let encoded = serialize_diagnostic(diagnostic.clone());
    state.record_request_failure(trace, &diagnostic, &encoded, message);
    emit_failure(&encoded);
}

enum SelectorInvariant {
    Duplicate(SelectionError),
    CandidateIndexOutOfRange { selected_index: usize },
}

/// Status/message pair for selector invariant failures. Callers pass the same
/// values to both `record_request_failure` and `protocol_error_response`.
fn routing_selector_invariant(failure: SelectorInvariant) -> (StatusCode, String) {
    let detail = match failure {
        SelectorInvariant::Duplicate(error) => error.to_string(),
        SelectorInvariant::CandidateIndexOutOfRange { selected_index } => {
            format!("candidate index {selected_index} is out of range")
        }
    };
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("routing selector invariant: {detail}"),
    )
}

fn send_route_label(
    snapshots: &RequestSnapshots,
    adapter: crate::provider::ProviderAdapterKind,
    plan: &RequestPlan,
) -> RouteLabel {
    if adapter == crate::provider::ProviderAdapterKind::Cpa {
        RouteLabel::Direct
    } else {
        snapshots.routes.client_for(&plan.model).1
    }
}

fn capture_route_resources(
    live: &crate::routing_snapshot::RoutingSnapshot,
    route: &crate::gateway::materialize::ExecutionRoute,
    snapshots: &RequestSnapshots,
) -> anyhow::Result<ResourceSet> {
    let account = live
        .credentials
        .iter()
        .find(|row| row.id == route.routing.account.id)
        .unwrap_or(&route.routing.account);
    let url = route.spec.request_url().unwrap_or_default();
    let route_label = send_route_label(snapshots, route.routing.adapter, &route.plan);
    let proxy_identity =
        (route_label == RouteLabel::Proxy).then_some(snapshots.config.proxy_url.as_str());
    let endpoint =
        restriction_endpoint_identity(&url, route_label, route.plan.upstream, proxy_identity);
    let free_contract = matches!(
        classify_http(
            429,
            &account.provider_id,
            route.plan.channel,
            route.spec.auth == UpstreamAuth::None
        ),
        ProviderErrorClass::RateLimited {
            profile: ocg_gateway::classify::ErrorProfile::ZenFree
        }
    );
    ResourceSet::from_snapshot(live, account, &endpoint, &route.plan.model, free_contract)
}

#[cfg(test)]
mod tests;
