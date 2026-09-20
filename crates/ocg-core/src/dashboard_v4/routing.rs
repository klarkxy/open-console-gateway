//! Read-only `GET /routing/explain` prediction.
//!
//! Reuses live alias resolution, materialization, availability, and selector
//! policy. Never sends, decrypts, probes, writes cooldown, or advances sticky
//! / round-robin / conversation state.

use axum::Json;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

use crate::dashboard_v3::{ControlRevision, RoutingMode as RoutingModeDto, V3ApiError};
use crate::gateway::handler::runtime_catalog_snapshot;
use crate::gateway::materialize::{
    InferenceBindingGate, InferenceBindingIndex, RouteRejection, RouteRejectionCode,
    materialize_account_routes_with_bindings, protocol_error_from_resolve,
    resolved_alias_from_model,
};
use crate::gateway::protocol::{ParsedClientRequest, parse_client_request, parse_gemini_request};
use crate::kernel::protocol::ApiFormat;
use crate::models::{RoutingMode, UpstreamChannel};
use crate::routing_runtime::{
    CandidateAvailability, RoutingCandidate, assess_candidate_availability,
};
use crate::state::CoreState;

use super::types::{
    RoutingChannel, RoutingClientProtocol, RoutingConversationBinding, RoutingEligibleCandidate,
    RoutingExclusion, RoutingExclusionCode, RoutingExplanation, RoutingResolvedKind,
    RoutingResolvedMapping, RoutingResolvedModel, RuntimeOnlyUncertainty,
};

#[derive(Debug, Deserialize)]
pub(super) struct ExplainQuery {
    model: Option<String>,
    #[serde(rename = "clientProtocol")]
    client_protocol: Option<String>,
}

pub(super) struct RoutingExplainQuery(ExplainQuery);

impl FromRequestParts<CoreState> for RoutingExplainQuery {
    type Rejection = V3ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &CoreState,
    ) -> Result<Self, Self::Rejection> {
        Query::<ExplainQuery>::try_from_uri(&parts.uri)
            .map(|Query(value)| Self(value))
            .map_err(|_| V3ApiError::invalid_request_at(state, "invalid query"))
    }
}

pub(super) async fn explain(
    State(state): State<CoreState>,
    RoutingExplainQuery(query): RoutingExplainQuery,
) -> Result<Json<RoutingExplanation>, V3ApiError> {
    let (model, protocol) = parse_explain_query(&query)
        .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    let explanation = explain_model(&state, &model, protocol)?;
    Ok(Json(explanation))
}

fn parse_explain_query(query: &ExplainQuery) -> Result<(String, RoutingClientProtocol), String> {
    let model = query
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| "model is required".to_string())?
        .to_string();
    let protocol = match query.client_protocol.as_deref() {
        None => RoutingClientProtocol::ChatCompletions,
        Some(value) => parse_client_protocol(value.trim()).ok_or_else(|| {
            "clientProtocol must be chat_completions, responses, messages, or gemini".to_string()
        })?,
    };
    Ok((model, protocol))
}

fn parse_client_protocol(value: &str) -> Option<RoutingClientProtocol> {
    match value {
        "chat_completions" => Some(RoutingClientProtocol::ChatCompletions),
        "responses" => Some(RoutingClientProtocol::Responses),
        "messages" => Some(RoutingClientProtocol::Messages),
        "gemini" => Some(RoutingClientProtocol::Gemini),
        _ => None,
    }
}

fn routing_mode_dto(mode: RoutingMode) -> RoutingModeDto {
    match mode {
        RoutingMode::StrictPriority => RoutingModeDto::StrictPriority,
        RoutingMode::StickyGlobal => RoutingModeDto::StickyGlobal,
        RoutingMode::RoundRobin => RoutingModeDto::RoundRobin,
    }
}

fn channel_dto(channel: UpstreamChannel) -> RoutingChannel {
    match channel {
        UpstreamChannel::Go => RoutingChannel::Go,
        UpstreamChannel::Free => RoutingChannel::Free,
    }
}

fn exclusion_code(code: RouteRejectionCode) -> RoutingExclusionCode {
    match code.as_str() {
        "mapping_protocol_incompatible" => RoutingExclusionCode::MappingProtocolIncompatible,
        "credential_disabled" => RoutingExclusionCode::CredentialDisabled,
        "binding_disabled" => RoutingExclusionCode::BindingDisabled,
        "model_scope_denied" => RoutingExclusionCode::ModelScopeDenied,
        "goat_not_eligible" => RoutingExclusionCode::GoatNotEligible,
        "goat_unverified" => RoutingExclusionCode::GoatUnverified,
        "candidate_materialization_failed" => RoutingExclusionCode::CandidateMaterializationFailed,
        "production_route_unsupported" => RoutingExclusionCode::ProductionRouteUnsupported,
        other => unreachable!("unmapped materialize rejection code {other}"),
    }
}

fn availability_code(reason: CandidateAvailability) -> Option<RoutingExclusionCode> {
    match reason {
        CandidateAvailability::Available => None,
        CandidateAvailability::AccountDisabled => Some(RoutingExclusionCode::AccountDisabled),
        CandidateAvailability::SetupNotReady => Some(RoutingExclusionCode::SetupNotReady),
        CandidateAvailability::ChannelMismatch => Some(RoutingExclusionCode::ChannelMismatch),
        CandidateAvailability::CredentialMissing => Some(RoutingExclusionCode::CredentialMissing),
        CandidateAvailability::AuthError => Some(RoutingExclusionCode::AuthError),
        CandidateAvailability::CoolingDown => Some(RoutingExclusionCode::CoolingDown),
        CandidateAvailability::FreeChannelUnavailable => {
            Some(RoutingExclusionCode::FreeChannelUnavailable)
        }
    }
}

fn from_rejection(rejection: &RouteRejection) -> RoutingExclusion {
    RoutingExclusion {
        code: exclusion_code(rejection.code),
        detail: rejection.detail.clone(),
        account_id: rejection.account_id.clone(),
        provider_id: rejection.provider_id.clone(),
        upstream_model: rejection.upstream_model.clone(),
    }
}

fn availability_exclusion(
    candidate: &RoutingCandidate,
    reason: CandidateAvailability,
) -> RoutingExclusion {
    RoutingExclusion {
        code: availability_code(reason).expect("unavailable reason"),
        detail: format!(
            "account `{}`: {}",
            candidate.account.id,
            reason.as_str().replace('_', " ")
        ),
        account_id: Some(candidate.account.id.clone()),
        provider_id: Some(candidate.account.provider_id.clone()),
        upstream_model: Some(candidate.resolved_model.clone()),
    }
}

fn protocol_dto(protocol: ApiFormat) -> RoutingClientProtocol {
    match protocol {
        ApiFormat::ChatCompletions => RoutingClientProtocol::ChatCompletions,
        ApiFormat::Responses => RoutingClientProtocol::Responses,
        ApiFormat::Messages => RoutingClientProtocol::Messages,
        ApiFormat::Gemini => RoutingClientProtocol::Gemini,
    }
}

fn eligible_candidate(
    candidate: &RoutingCandidate,
    upstream_protocol: ApiFormat,
    routing_rank: u32,
    destination: Option<&(String, String)>,
) -> RoutingEligibleCandidate {
    RoutingEligibleCandidate {
        account_id: candidate.account.id.clone(),
        account_name: candidate.account.name.clone(),
        provider_id: candidate.account.provider_id.clone(),
        destination_id: destination.map(|(id, _)| id.clone()),
        destination_name: destination.map(|(_, name)| name.clone()),
        adapter_kind: candidate.adapter.as_str().to_string(),
        channel: channel_dto(candidate.channel),
        resolved_model: candidate.resolved_model.clone(),
        upstream_protocol: protocol_dto(upstream_protocol),
        routing_rank,
    }
}

const RUNTIME_UNCERTAINTY: [RuntimeOnlyUncertainty; 5] = [
    RuntimeOnlyUncertainty::StateChangedAfterSnapshot,
    RuntimeOnlyUncertainty::ConversationBindingNotEvaluated,
    RuntimeOnlyUncertainty::RetryExclusionsNotApplied,
    RuntimeOnlyUncertainty::CredentialRecheckPending,
    RuntimeOnlyUncertainty::UpstreamResultUnknown,
];

fn minimal_parsed_request(
    protocol: RoutingClientProtocol,
    model: &str,
) -> Result<ParsedClientRequest, crate::gateway::protocol::ProtocolError> {
    match protocol {
        RoutingClientProtocol::Gemini => parse_gemini_request(
            model.to_string(),
            false,
            Bytes::from(
                serde_json::to_vec(&json!({
                    "contents": [{"role": "user", "parts": [{"text": "hi"}]}]
                }))
                .expect("minimal Gemini body"),
            ),
        ),
        RoutingClientProtocol::Responses => parse_client_request(
            ApiFormat::Responses,
            Bytes::from(
                serde_json::to_vec(&json!({
                    "model": model,
                    "input": "hi",
                    "store": false
                }))
                .expect("minimal Responses body"),
            ),
        ),
        RoutingClientProtocol::Messages => parse_client_request(
            ApiFormat::Messages,
            Bytes::from(
                serde_json::to_vec(&json!({
                    "model": model,
                    "max_tokens": 16,
                    "messages": [{"role": "user", "content": "hi"}]
                }))
                .expect("minimal Messages body"),
            ),
        ),
        RoutingClientProtocol::ChatCompletions => parse_client_request(
            ApiFormat::ChatCompletions,
            Bytes::from(
                serde_json::to_vec(&json!({
                    "model": model,
                    "messages": [{"role": "user", "content": "hi"}]
                }))
                .expect("minimal ChatCompletions body"),
            ),
        ),
    }
}

fn cpa_base_url(state: &CoreState) -> Option<String> {
    match crate::cpa::env_base_url() {
        Ok(Some(base_url)) => Some(base_url),
        Ok(None) => state
            .db
            .lock()
            .cpa_integration()
            .ok()
            .flatten()
            .map(|record| record.base_url),
        Err(_) => None,
    }
}

fn explain_model(
    state: &CoreState,
    model: &str,
    protocol: RoutingClientProtocol,
) -> Result<RoutingExplanation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let (wall, mono) = state.sample_gateway_clock();
    let config = state.config();
    let contracts = state.provider_contracts();
    let dynamics = state.dynamic_providers();
    let snapshot =
        runtime_catalog_snapshot(state, &contracts, &dynamics).map_err(V3ApiError::internal)?;
    let catalogs = snapshot.catalogs();
    let resolved =
        crate::alias::resolve_with_runtime_catalogs(model, catalogs).map_err(|error| {
            V3ApiError::invalid_request_at(state, protocol_error_from_resolve(error).message)
        })?;
    let parsed = minimal_parsed_request(protocol, model)
        .map_err(|error| V3ApiError::invalid_request_at(state, error.message))?;

    let (
        accounts,
        free_cooldown,
        stored_bindings,
        routing,
        custom_runtimes,
        goat_runtimes,
        projection,
    ) = {
        let db = state.db.lock();
        let accounts = crate::destination_projection::list_accounts_for_v3(&db)
            .map_err(V3ApiError::internal)?;
        let free_cooldown = db
            .free_channel_cooldown_until_at(wall)
            .map_err(V3ApiError::internal)?;
        let stored_bindings = db.list_inference_bindings().map_err(V3ApiError::internal)?;
        let routing = crate::destination_projection::routing_projection(&db)
            .ok()
            .flatten();
        let custom_runtimes = db
            .list_custom_account_runtimes()
            .map_err(V3ApiError::internal)?;
        let goat_runtimes = db
            .list_goat_account_runtimes()
            .map_err(V3ApiError::internal)?;
        let projection = crate::destination_projection::read_v4_projection(&db)
            .map_err(V3ApiError::internal)?
            .map_err(|_| V3ApiError::conflict_at(state, "destination projection refused"))?;
        (
            accounts,
            free_cooldown,
            stored_bindings,
            routing,
            custom_runtimes,
            goat_runtimes,
            projection,
        )
    };
    let destination_names = projection
        .destinations
        .iter()
        .map(|destination| (destination.id.as_str(), destination.name.as_str()))
        .collect::<HashMap<_, _>>();
    let destination_by_account = projection
        .credentials
        .iter()
        .filter_map(|credential| {
            destination_names
                .get(credential.destination_id.as_str())
                .map(|name| {
                    (
                        credential.legacy_account_id.as_str(),
                        (credential.destination_id.clone(), (*name).to_string()),
                    )
                })
        })
        .collect::<HashMap<_, _>>();
    let routing_rank_by_account = accounts
        .iter()
        .enumerate()
        .map(|(index, account)| (account.id.as_str(), index as u32))
        .collect::<HashMap<_, _>>();
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
            Some(projection) => {
                crate::destination_projection::free_channel_exhausted(projection, wall)
            }
            None => crate::routing_runtime::free_channel_is_exhausted_at(&accounts, wall),
        };
    let custom_by_account = crate::custom::custom_runtimes_by_account(&custom_runtimes);
    let goat_by_account = crate::goat::goat_runtimes_by_account(&goat_runtimes);
    let cpa_base = cpa_base_url(state);
    let route_set = materialize_account_routes_with_bindings(
        &accounts,
        &config,
        &parsed,
        &resolved,
        model,
        model,
        free_available,
        &custom_by_account,
        &goat_by_account,
        cpa_base.as_deref(),
        &contracts,
        &dynamics,
        &bindings,
        routing.as_ref(),
    )
    .map_err(|error| V3ApiError::invalid_request_at(state, error.message))?;

    let mut exclusions: Vec<RoutingExclusion> =
        route_set.rejections.iter().map(from_rejection).collect();
    let mut eligible = Vec::new();
    for route in &route_set.routes {
        let reason = assess_candidate_availability(&route.routing, free_available, wall);
        if reason.is_available() {
            eligible.push(eligible_candidate(
                &route.routing,
                route.plan.upstream,
                routing_rank_by_account
                    .get(route.routing.account.id.as_str())
                    .copied()
                    .unwrap_or(u32::MAX),
                destination_by_account.get(route.routing.account.id.as_str()),
            ));
        } else {
            exclusions.push(availability_exclusion(&route.routing, reason));
        }
    }

    let routing_candidates = route_set
        .routes
        .iter()
        .map(|route| route.routing.clone())
        .collect::<Vec<_>>();
    let first_pick = state
        .routing
        .preview_candidate_index_at(
            &routing_candidates,
            config.routing_mode,
            false,
            None,
            &[],
            free_available,
            wall,
            mono,
        )
        .ok()
        .flatten()
        .and_then(|index| route_set.routes.get(index))
        .map(|route| {
            eligible_candidate(
                &route.routing,
                route.plan.upstream,
                routing_rank_by_account
                    .get(route.routing.account.id.as_str())
                    .copied()
                    .unwrap_or(u32::MAX),
                destination_by_account.get(route.routing.account.id.as_str()),
            )
        });

    let resolved_dto = match &resolved {
        crate::alias::ResolvedModel::Alias {
            alias, mappings, ..
        } => RoutingResolvedModel {
            kind: RoutingResolvedKind::Alias,
            alias: Some((*alias).to_string()),
            mappings: mappings.iter().map(mapping_dto).collect(),
        },
        crate::alias::ResolvedModel::PinnedRaw { mapping, .. } => RoutingResolvedModel {
            kind: RoutingResolvedKind::PinnedRaw,
            alias: resolved_alias_from_model(&resolved),
            mappings: vec![mapping_dto(mapping)],
        },
    };

    Ok(RoutingExplanation {
        requested_model: model.to_string(),
        client_protocol: protocol,
        resolved: resolved_dto,
        revision: ControlRevision::from_state(state),
        observed_at: wall.to_rfc3339(),
        routing_mode: routing_mode_dto(config.routing_mode),
        conversation_sticky: config.conversation_sticky,
        conversation_binding: RoutingConversationBinding::NotEvaluated,
        eligible,
        exclusions,
        expected_base_policy_first_pick: first_pick,
        runtime_only_uncertainty: RUNTIME_UNCERTAINTY.to_vec(),
    })
}

fn mapping_dto(mapping: &crate::alias::ProviderMapping) -> RoutingResolvedMapping {
    RoutingResolvedMapping {
        provider_id: mapping.provider_id.clone(),
        upstream_model: mapping.upstream_model.clone(),
        routeable: mapping.routeable,
    }
}

#[cfg(test)]
mod tests;
