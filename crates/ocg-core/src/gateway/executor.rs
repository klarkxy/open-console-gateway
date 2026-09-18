//! Outer Gateway request orchestration.
//!
//! [`GatewayExecutor`] owns the request-entry snapshot, candidate selection,
//! same-account retry, and account fallback loop. The handler retains
//! trace/auth, client parse/format validation, and Alias resolution.
//! Single-attempt forwarding stays in [`super::forwarder`].
//! This slice does not consolidate decision policy.

use crate::alias;
use crate::gateway::diagnostics::{
    ErrorDiagnostic, RequestTrace, emit_failure, log_request_failure, serialize_diagnostic,
};
use crate::gateway::forwarder::{
    ForwardAction, LiveSendSelection, forward_request, rate_limited_response,
};
use crate::gateway::materialize::{
    InferenceBindingGate, InferenceBindingIndex, diagnostic_forced_upstream, mapping_is_zen_free,
    materialize_account_routes_with_bindings, resolved_alias_from_model,
};
use crate::gateway::protocol::{MaterializeSpec, RequestPlan, materialize_parsed_request};
use crate::gateway::response::{local_protocol_failure, protocol_error_response};
use crate::gateway::routing::resolve_conversation_key;
use crate::gateway::shadow::{ShadowPlanInput, maybe_compare_live_routes};

use crate::http_client::{ForwardRouteSet, RouteLabel};
use crate::kernel::pricing::PricingSnapshot;
use crate::kernel::protocol::ApiFormat;
use crate::models::{AppConfig, UpstreamChannel};
use crate::provider_contracts::EffectiveContractSet;
use crate::state::CoreState;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use ocg_gateway::selector::SelectionError;
use std::sync::Arc;

/// Process-state values frozen at request entry. Each fallback iteration still
/// re-reads accounts, eligible Custom runtimes, and Zen Free cooldown.
pub(crate) struct RequestSnapshots {
    config: AppConfig,
    pricing: Arc<PricingSnapshot>,
    routes: Arc<ForwardRouteSet>,
    contracts: Arc<EffectiveContractSet>,
    resolved: alias::ResolvedModel,
    cpa_base_url: Option<String>,
    dynamics: Arc<Vec<crate::dynamic::DynamicProviderRuntime>>,
}

impl RequestSnapshots {
    fn capture(
        state: &CoreState,
        config: AppConfig,
        contracts: Arc<EffectiveContractSet>,
        resolved: alias::ResolvedModel,
        dynamics: Arc<Vec<crate::dynamic::DynamicProviderRuntime>>,
    ) -> Self {
        let cpa_base_url = match crate::cpa::env_base_url() {
            Ok(Some(base_url)) => Some(base_url),
            Ok(None) => state
                .db
                .lock()
                .cpa_integration()
                .ok()
                .flatten()
                .map(|record| record.base_url),
            Err(_) => None,
        };
        Self {
            config,
            pricing: state.pricing_snapshot(),
            routes: state.forward_route_set(),
            contracts,
            resolved,
            cpa_base_url,
            dynamics,
        }
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

/// Concrete orchestration facade for one already-parsed, already-resolved
/// Gateway request.
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
        resolved: alias::ResolvedModel,
        client_model: String,
        routing_model: String,
        config: AppConfig,
        client_key_id: Option<String>,
        contracts: Arc<EffectiveContractSet>,
        dynamics: Arc<Vec<crate::dynamic::DynamicProviderRuntime>>,
    ) -> Response {
        // One logical client request, including safe retries and account fallback,
        // must use one immutable pricing revision from start to finish.
        // Routing snapshot captured once at entry: every attempt (including after
        // free fallback rewrites the model) resolves its leg from this snapshot,
        // and a concurrent settings switch only affects requests starting later.
        let snapshots = RequestSnapshots::capture(&state, config, contracts, resolved, dynamics);
        let mut loop_state = LoopState::new();
        let conversation_key = if snapshots.config.conversation_sticky {
            resolve_conversation_key(client_format, &routing_model, &headers, &client_body)
        } else {
            None
        };
        let (diagnostic_model, diagnostic_channel) = match &snapshots.resolved {
            alias::ResolvedModel::Alias {
                alias, mappings, ..
            } => {
                let zen_only = mappings
                    .iter()
                    .filter(|mapping| mapping.routeable)
                    .all(mapping_is_zen_free);
                (
                    (*alias).to_string(),
                    if zen_only {
                        UpstreamChannel::Free
                    } else {
                        UpstreamChannel::Go
                    },
                )
            }
            alias::ResolvedModel::PinnedRaw { mapping, .. } => (
                if mapping.upstream_model.is_empty() {
                    routing_model.clone()
                } else {
                    mapping.upstream_model.to_string()
                },
                if mapping_is_zen_free(mapping) {
                    UpstreamChannel::Free
                } else {
                    UpstreamChannel::Go
                },
            ),
        };
        let diagnostic_forced_upstream =
            diagnostic_forced_upstream(&snapshots.resolved, parsed.client);
        let requested_plan = match materialize_parsed_request(
            &parsed,
            &MaterializeSpec {
                client_model: client_model.clone(),
                upstream_model: diagnostic_model,
                resolved_alias: resolved_alias_from_model(&snapshots.resolved),
                channel: diagnostic_channel,
                upstream_base_override: None,
                original_model: None,
                forced_upstream: diagnostic_forced_upstream,
                custom_route: None,
            },
        ) {
            Ok(plan) => plan,
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

        loop {
            let (decision_wall, decision_mono) = state.sample_gateway_clock();
            let (accounts, free_cooldown, stored_bindings, routing) = {
                let db = state.db.lock();
                let accounts = match crate::destination_projection::list_accounts_for_v3(&db) {
                    Ok(accounts) => accounts,
                    Err(error) => {
                        let message = format!("failed to select account: {error}");
                        record_plan_failure(
                            &state,
                            &trace,
                            &client_body,
                            loop_state.attempt.max(1),
                            client_format,
                            &requested_plan,
                            "gateway",
                            "account_selection",
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &message,
                        );
                        return protocol_error_response(
                            client_format,
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &message,
                            None,
                        );
                    }
                };
                let free_cooldown = match db.free_channel_cooldown_until_at(decision_wall) {
                    Ok(cooldown) => cooldown,
                    Err(error) => {
                        let message = format!("failed to read free-channel cooldown: {error}");
                        return protocol_error_response(
                            client_format,
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &message,
                            None,
                        );
                    }
                };
                let stored_bindings = match db.list_inference_bindings() {
                    Ok(rows) => rows,
                    Err(error) => {
                        let message = format!("failed to load inference bindings: {error}");
                        return protocol_error_response(
                            client_format,
                            StatusCode::INTERNAL_SERVER_ERROR,
                            &message,
                            None,
                        );
                    }
                };
                let routing = crate::destination_projection::routing_projection(&db)
                    .ok()
                    .flatten();
                (accounts, free_cooldown, stored_bindings, routing)
            };
            let bindings = stored_bindings
                .iter()
                .map(|row| {
                    (
                        row.account_id.clone(),
                        InferenceBindingGate {
                            enabled: row.enabled,
                            model_scope: row.model_scope.clone(),
                        },
                    )
                })
                .collect::<InferenceBindingIndex>();
            let free_available = free_cooldown.is_none()
                && !match routing.as_ref() {
                    Some(projection) => crate::destination_projection::free_channel_exhausted(
                        projection,
                        decision_wall,
                    ),
                    None => crate::routing_runtime::free_channel_is_exhausted_at(
                        &accounts,
                        decision_wall,
                    ),
                };
            let custom_runtimes = match state.db.lock().list_custom_account_runtimes() {
                Ok(runtimes) => crate::custom::custom_runtimes_by_account(&runtimes),
                Err(error) => {
                    let message = format!("failed to load Custom accounts: {error}");
                    return protocol_error_response(
                        client_format,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &message,
                        None,
                    );
                }
            };
            let goat_runtimes = match state.db.lock().list_goat_account_runtimes() {
                Ok(runtimes) => crate::goat::goat_runtimes_by_account(&runtimes),
                Err(error) => {
                    let message = format!("failed to load Command Code GOAT accounts: {error}");
                    return protocol_error_response(
                        client_format,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        &message,
                        None,
                    );
                }
            };
            let route_set = match materialize_account_routes_with_bindings(
                &accounts,
                &snapshots.config,
                &parsed,
                &snapshots.resolved,
                &client_model,
                &routing_model,
                free_available,
                &custom_runtimes,
                &goat_runtimes,
                snapshots.cpa_base_url.as_deref(),
                &snapshots.contracts,
                &snapshots.dynamics,
                &bindings,
                routing.as_ref(),
            ) {
                Ok(route_set) => route_set,
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
            maybe_compare_live_routes(
                &ShadowPlanInput {
                    accounts: &accounts,
                    config: &snapshots.config,
                    parsed: &parsed,
                    resolved: &snapshots.resolved,
                    client_model: &client_model,
                    routing_model: &routing_model,
                    free_available,
                    custom_runtimes: &custom_runtimes,
                    goat_runtimes: &goat_runtimes,
                    cpa_base_url: snapshots.cpa_base_url.as_deref(),
                    contracts: &snapshots.contracts,
                    dynamics: &snapshots.dynamics,
                    bindings: &bindings,
                    projection: routing.as_ref(),
                },
                &route_set,
            );
            let excluded = loop_state
                .failed_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let routing_candidates = route_set
                .routes
                .iter()
                .map(|route| route.routing.clone())
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
                    if route_set.free_only
                        && let Some(until) = free_cooldown
                    {
                        record_plan_failure(
                            &state,
                            &trace,
                            &client_body,
                            loop_state.attempt.max(1),
                            client_format,
                            &requested_plan,
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
                            route
                                .routing
                                .account
                                .cooldown_ends_at_for(route.routing.channel, decision_wall)
                        })
                        .min();
                    return match soonest {
                        Some(until) => {
                            record_plan_failure(
                                &state,
                                &trace,
                                &client_body,
                                loop_state.attempt.max(1),
                                client_format,
                                &requested_plan,
                                "gateway",
                                "account_selection",
                                StatusCode::TOO_MANY_REQUESTS,
                                "all compatible accounts are rate-limited",
                            );
                            rate_limited_response(client_format, until)
                        }
                        None => {
                            let msg = loop_state.last_error.clone().unwrap_or_else(|| {
                                route_set.incompatibility.unwrap_or_else(|| {
                                    "no compatible provider accounts are available".to_string()
                                })
                            });
                            record_plan_failure(
                                &state,
                                &trace,
                                &client_body,
                                loop_state.attempt.max(1),
                                client_format,
                                &requested_plan,
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
                    record_plan_failure(
                        &state,
                        &trace,
                        &client_body,
                        loop_state.attempt.max(1),
                        client_format,
                        &requested_plan,
                        "gateway",
                        "account_selection",
                        status,
                        &message,
                    );
                    return protocol_error_response(client_format, status, &message, None);
                }
            };
            let route = match route_set.routes.into_iter().nth(selected_index) {
                Some(route) => route,
                None => {
                    let (status, message) =
                        routing_selector_invariant(SelectorInvariant::CandidateIndexOutOfRange {
                            selected_index,
                        });
                    record_plan_failure(
                        &state,
                        &trace,
                        &client_body,
                        loop_state.attempt.max(1),
                        client_format,
                        &requested_plan,
                        "gateway",
                        "account_selection",
                        status,
                        &message,
                    );
                    return protocol_error_response(client_format, status, &message, None);
                }
            };
            let adapter = route.routing.adapter;
            let account = route.routing.account;
            let active_plan = route.plan;
            let selection = LiveSendSelection::from_binding(
                &account,
                stored_bindings
                    .iter()
                    .find(|row| row.account_id == account.id),
                &client_model,
                &routing_model,
                &active_plan.model,
            );

            let mut retried_same_account = false;
            loop {
                loop_state.attempt = loop_state.attempt.saturating_add(1);
                // Re-resolve the leg on every attempt: free fallback or sticky
                // rewrites can swap `active_plan.model` mid-request.
                let (client, selected_route) = snapshots.routes.client_for(&active_plan.model);
                let route = if adapter == crate::provider::ProviderAdapterKind::Cpa {
                    RouteLabel::Direct
                } else {
                    selected_route
                };
                match forward_request(
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
                    snapshots.pricing.clone(),
                    client_key_id.as_deref(),
                    &snapshots.dynamics,
                    &selection,
                )
                .await
                {
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
    let encoded = serialize_diagnostic(diagnostic.clone());
    log_request_failure(&state.db.lock(), trace, &diagnostic, &encoded, message);
    emit_failure(&encoded);
}

enum SelectorInvariant {
    Duplicate(SelectionError),
    CandidateIndexOutOfRange { selected_index: usize },
}

/// Status/message pair for selector invariant failures. Callers pass the same
/// values to both `record_plan_failure` and `protocol_error_response`.
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
mod tests {
    #[test]
    fn selector_invariant_maps_to_internal_error() {
        for (label, failure, expected) in [
            (
                "duplicate",
                super::SelectorInvariant::Duplicate(
                    ocg_gateway::selector::SelectionError::DuplicateAccountId {
                        first: 0,
                        duplicate: 2,
                    },
                ),
                "routing selector invariant: duplicate account id at candidate index 2 (first seen at 0)",
            ),
            (
                "index-out-of-range",
                super::SelectorInvariant::CandidateIndexOutOfRange { selected_index: 9 },
                "routing selector invariant: candidate index 9 is out of range",
            ),
        ] {
            let (status, message) = super::routing_selector_invariant(failure);
            assert_eq!(
                status,
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "{label}"
            );
            assert_eq!(message, expected, "{label}");
        }
    }
}
