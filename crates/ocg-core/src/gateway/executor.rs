//! Captures a logical request's identities, catalog, transport and prices,
//! then executes bounded selection, retry and fallback. The handler owns
//! client authentication and parsing; single-attempt I/O lives in forwarder.

use crate::alias;
use crate::gateway::diagnostics::{
    ErrorDiagnostic, RequestTrace, emit_failure, log_request_failure, serialize_diagnostic,
};
use crate::gateway::forwarder::{
    ForwardAction, LiveSendSelection, forward_request_with_deadline, rate_limited_response,
};
use crate::gateway::materialize::materialize_execution_routes;
use crate::gateway::protocol::{
    ProtocolError, RequestFacts, RequestPlan, validate_client_request_features,
};
use crate::gateway::response::{local_protocol_failure, protocol_error_response};
use crate::gateway::routing::resolve_conversation_key;

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
    fn capture(
        state: &CoreState,
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
}

impl LoopState {
    fn new() -> Self {
        Self {
            last_error: None,
            failed_ids: Vec::new(),
            attempt: 0,
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
            // Publish settings, catalog, credentials, route and pricing identities
            // as one preparation phase. No guard crosses upstream I/O.
            let _settings_update = state.settings_update.lock();
            let routing = match crate::routing_snapshot::RoutingSnapshot::load(&state.db.lock()) {
                Ok(routing) => routing,
                Err(error) => {
                    return protocol_error_response(
                        client_format,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("failed to capture routing state: {error}"),
                        None,
                    );
                }
            };
            let catalog = crate::gateway::handler::RuntimeCatalogSnapshot::from_routing(
                routing,
                state.sample_gateway_clock().0,
            );
            let resolved = match catalog.resolve(&routing_model) {
                Ok(resolved) => resolved,
                Err(error) => {
                    return local_protocol_failure(
                        &state,
                        &trace,
                        client_format,
                        crate::gateway::materialize::protocol_error_from_resolve(error),
                        Some(client_body.len()),
                        Some(&client_body),
                    );
                }
            };
            let snapshots = match RequestSnapshots::capture(
                &state,
                state.config(),
                resolved,
                catalog.routing,
            ) {
                Ok(snapshots) => snapshots,
                Err(error) => {
                    return protocol_error_response(
                        client_format,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("failed to capture route configuration: {error}"),
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
            let (decision_wall, decision_mono) = state.sample_gateway_clock();
            let live = {
                let db = state.db.lock();
                crate::routing_snapshot::RoutingSnapshot::load(&db).and_then(|routing| {
                    db.free_channel_cooldown_until_at(decision_wall)
                        .map(|cooldown| (routing, cooldown))
                })
            };
            let (mut live, free_cooldown) = match live {
                Ok(live) => live,
                Err(error) => {
                    return protocol_error_response(
                        client_format,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &format!("failed to load routing state: {error}"),
                        None,
                    );
                }
            };
            let free_egress_wait = state
                .recovery
                .free_egress_retry_until(decision_wall, decision_mono);
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
            {
                let probes = state.quota_probes.lock();
                live.apply_quota_probes(&probes);
            }
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
            let selected_index = match state.routing.try_select_candidate_index_at(
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
                            let temporary = crate::gateway::recovery::ResourceSet::from_snapshot(
                                &live,
                                credential,
                                "",
                                &route.plan.model,
                                free_contract,
                            )
                            .ok()
                            .and_then(|resources| {
                                state
                                    .recovery
                                    .credential_retry_until(&resources, decision_wall)
                            });
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
            let adapter = route.routing.adapter;
            let account = route.routing.account;
            let active_plan = route.plan;
            let frozen_spec = route.spec;

            let mut retried_same_account = false;
            loop {
                if loop_state.attempt >= MAX_REQUEST_ATTEMPTS
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
                let forwarded = forward_request_with_deadline(
                    client,
                    route,
                    &state,
                    &account,
                    adapter,
                    &snapshots.config,
                    &active_plan,
                    &trace,
                    &client_body,
                    loop_state.attempt,
                    !retried_same_account,
                    headers.clone(),
                    prices[selected_index].clone(),
                    client_key_id.as_deref(),
                    &frozen_spec,
                    &selection,
                    Some(request_deadline),
                )
                .await;
                match forwarded {
                    Ok(result) => match result.action {
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
                    },
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
    log_request_failure(&state.db.lock(), trace, &diagnostic, &encoded, message);
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
    log_request_failure(&state.db.lock(), trace, &diagnostic, &encoded, message);
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

#[cfg(test)]
mod tests;
