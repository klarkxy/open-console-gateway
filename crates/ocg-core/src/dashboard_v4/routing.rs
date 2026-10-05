//! Read-only `GET /routing/explain`.
//!
//! One `explain_owned_routes` call. The response copies the revision and routing
//! mode captured inside that read. It does not select, decrypt, admit, or send.

use axum::Json;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use ocg_domain::ids::model_ids_match;
use serde::Deserialize;
use serde_json::Value;

use crate::cpa_execution::ExecutionError;
use crate::cpa_execution::explain::{
    GrantFact, HistoricalPlacement, MaterialFact, OwnedRoutingFacts, QueryMapping, QueryResolution,
    QueryResolutionKind, RouteExclusion, RouteFact, RoutePlane, RoutePosture,
    RoutingProductChannel, RuntimeFacts, Spelling, explain_owned_routes,
};
use crate::cpa_policy::{ResetEvidence, ScopedQuotaView, Subject};
use crate::dashboard_v3::{ControlRevision, RoutingMode as RoutingModeDto, V3ApiError};
use crate::models::RoutingMode;
use crate::state::CoreState;

use super::types::{
    RoutingChannel, RoutingClientProtocol, RoutingConfiguredRoute, RoutingConversationBinding,
    RoutingEligibleCandidate, RoutingExclusion, RoutingExclusionCode, RoutingExplanation,
    RoutingGrantDisposition, RoutingHistoricalPlacement, RoutingMaterialFact, RoutingNativePin,
    RoutingOperationFact, RoutingOwnedProjection, RoutingPlaneTuple, RoutingQuotaEvidence,
    RoutingQuotaFact, RoutingQuotaState, RoutingResolvedKind, RoutingResolvedMapping,
    RoutingResolvedModel, RoutingRouteAuthority, RoutingRoutePlane, RoutingRoutePosture,
    RoutingSpelling, RuntimeOnlyUncertainty,
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

/// Public Gemini uses the existing chat completions callable association.
fn callable_protocol(protocol: RoutingClientProtocol) -> &'static str {
    match protocol {
        RoutingClientProtocol::ChatCompletions | RoutingClientProtocol::Gemini => {
            "chat_completions"
        }
        RoutingClientProtocol::Responses => "responses",
        RoutingClientProtocol::Messages => "messages",
    }
}

fn routing_mode_dto(mode: RoutingMode) -> RoutingModeDto {
    match mode {
        RoutingMode::StrictPriority => RoutingModeDto::StrictPriority,
        RoutingMode::StickyGlobal => RoutingModeDto::StickyGlobal,
        RoutingMode::RoundRobin => RoutingModeDto::RoundRobin,
    }
}

fn explain_model(
    state: &CoreState,
    model: &str,
    protocol: RoutingClientProtocol,
) -> Result<RoutingExplanation, V3ApiError> {
    explain_model_at(state, model, protocol, Utc::now())
}

fn explain_model_at(
    state: &CoreState,
    model: &str,
    protocol: RoutingClientProtocol,
    now: DateTime<Utc>,
) -> Result<RoutingExplanation, V3ApiError> {
    let facts = explain_owned_routes(state, model, callable_protocol(protocol), now)
        .map_err(|error| execution_error(state, error))?;
    let Some(kind) = facts.resolution.kind else {
        return Err(V3ApiError::invalid_request_at(
            state,
            "model is unknown or ambiguous",
        ));
    };
    if !facts.resolution.known || facts.resolution.ambiguous {
        return Err(V3ApiError::invalid_request_at(
            state,
            "model is unknown or ambiguous",
        ));
    }
    Ok(explanation_from_facts(&facts, protocol, kind))
}

fn explanation_from_facts(
    facts: &OwnedRoutingFacts,
    protocol: RoutingClientProtocol,
    kind: QueryResolutionKind,
) -> RoutingExplanation {
    let captured = &facts.captured;
    RoutingExplanation {
        requested_model: facts.requested_model.clone(),
        client_protocol: protocol,
        resolved: resolved_model(facts, kind),
        revision: ControlRevision {
            revision: captured.settings_revision,
            process_generation: captured.process_generation,
            pricing_revision: captured.pricing_revision.clone(),
        },
        observed_at: facts.evaluated_at.to_rfc3339(),
        routing_mode: routing_mode_dto(captured.routing_mode),
        conversation_sticky: captured.conversation_sticky,
        conversation_binding: RoutingConversationBinding::NotEvaluated,
        eligible: facts
            .applied
            .iter()
            .filter(|route| public_eligible(route, &facts.runtime, &facts.resolution))
            .map(eligible_candidate)
            .collect(),
        exclusions: facts
            .applied
            .iter()
            .flat_map(|route| applied_exclusions(route, &facts.runtime, &facts.resolution))
            .collect(),
        expected_base_policy_first_pick: None,
        runtime_only_uncertainty: uncertainties(facts),
        desired_routes: facts
            .desired
            .iter()
            .map(|route| RoutingConfiguredRoute {
                authority: authority_of(route),
            })
            .collect(),
        owned_projection: projection_of(facts),
    }
}

fn execution_error(state: &CoreState, error: ExecutionError) -> V3ApiError {
    match error {
        ExecutionError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        other => V3ApiError::internal_at(state, other),
    }
}

fn public_eligible(
    route: &RouteFact,
    runtime: &RuntimeFacts,
    resolution: &QueryResolution,
) -> bool {
    route.plane == RoutePlane::Applied
        && route.client_configuration_eligible
        && matches!(route.posture, RoutePosture::Client)
        && !route.validation_only
        && runtime.owned_running
        && !runtime.state_changed
        && runtime.origin_verified
        && runtime.verified_ready
        && runtime.policy_ready
        && runtime.pin_capabilities_ready
        && runtime.tuple_aligned
        && !runtime.policy_malformed
        && !runtime.unavailable
        && !runtime.poisoned
        && !runtime.stopped
        && !route.known_restriction_blocks
        && !route.trial_pending
        && !matches!(route.quota, ScopedQuotaView::Malformed)
        && !route.migration_required
        && !matches!(route.historical_placement, HistoricalPlacement::Remote)
        && selected_routeable(route, resolution)
}

fn selected_routeable(route: &RouteFact, resolution: &QueryResolution) -> bool {
    resolution
        .mappings
        .iter()
        .any(|mapping| mapping.routeable && mapping_identity_matches(route, mapping))
}

fn mapping_identity_matches(route: &RouteFact, mapping: &QueryMapping) -> bool {
    route.destination_id == mapping.destination_id
        && route.provider_id == mapping.provider_id
        && model_ids_match(&route.upstream_model, &mapping.upstream_model)
}

fn resolved_model(facts: &OwnedRoutingFacts, kind: QueryResolutionKind) -> RoutingResolvedModel {
    let kind = match kind {
        QueryResolutionKind::Alias => RoutingResolvedKind::Alias,
        QueryResolutionKind::PinnedRaw => RoutingResolvedKind::PinnedRaw,
    };
    RoutingResolvedModel {
        kind,
        alias: facts.resolution.alias.clone(),
        mappings: facts
            .resolution
            .mappings
            .iter()
            .map(|mapping| RoutingResolvedMapping {
                provider_id: mapping.provider_id.clone(),
                upstream_model: mapping.upstream_model.clone(),
                routeable: mapping.routeable,
                destination_id: mapping.destination_id.clone(),
                adapter_kind: mapping.adapter_kind.clone(),
                migration_required: mapping.migration_required,
            })
            .collect(),
    }
}

fn uncertainties(facts: &OwnedRoutingFacts) -> Vec<RuntimeOnlyUncertainty> {
    let mut out = vec![RuntimeOnlyUncertainty::ConversationBindingNotEvaluated];
    if facts
        .desired
        .iter()
        .chain(facts.applied.iter())
        .any(|route| route.secret_recheck_pending)
    {
        out.push(RuntimeOnlyUncertainty::CredentialRecheckPending);
    }
    for item in &facts.uncertainties {
        match item.as_str() {
            "cpa_selection_not_evaluated" => {
                push_uncertainty(&mut out, RuntimeOnlyUncertainty::CpaSelectionNotEvaluated);
            }
            "quota_trial_not_evaluated" => {
                push_uncertainty(&mut out, RuntimeOnlyUncertainty::QuotaTrialNotEvaluated);
            }
            _ => {}
        }
    }
    if facts.runtime.state_changed {
        push_uncertainty(&mut out, RuntimeOnlyUncertainty::StateChangedAfterSnapshot);
    }
    out
}

fn push_uncertainty(out: &mut Vec<RuntimeOnlyUncertainty>, item: RuntimeOnlyUncertainty) {
    if !out.contains(&item) {
        out.push(item);
    }
}

fn applied_exclusions(
    route: &RouteFact,
    runtime: &RuntimeFacts,
    resolution: &QueryResolution,
) -> Vec<RoutingExclusion> {
    if public_eligible(route, runtime, resolution) {
        return Vec::new();
    }
    let mut codes = Vec::new();
    for item in &route.exclusions {
        push_code(&mut codes, authority_code(*item));
    }
    if route.validation_only {
        push_code(&mut codes, RoutingExclusionCode::ValidationOnly);
    }
    if route.known_restriction_blocks {
        push_code(&mut codes, RoutingExclusionCode::QuotaKnownReset);
    }
    if route.trial_pending {
        push_code(&mut codes, RoutingExclusionCode::QuotaUnknownReset);
    }
    if matches!(route.quota, ScopedQuotaView::Malformed) {
        push_code(&mut codes, RoutingExclusionCode::QuotaMalformed);
    }
    if route.migration_required || matches!(route.historical_placement, HistoricalPlacement::Remote)
    {
        push_code(&mut codes, RoutingExclusionCode::MigrationRequired);
    }
    if runtime.poisoned {
        push_code(&mut codes, RoutingExclusionCode::Untrusted);
    }
    if runtime.policy_malformed {
        push_code(&mut codes, RoutingExclusionCode::PolicyMalformed);
    }
    if runtime.unavailable {
        push_code(&mut codes, RoutingExclusionCode::ConfigurationUnavailable);
    }
    if runtime.stopped {
        push_code(&mut codes, RoutingExclusionCode::Stopped);
    }
    if runtime.state_changed {
        push_code(&mut codes, RoutingExclusionCode::StateChanged);
    }
    if !runtime.owned_running {
        push_code(&mut codes, RoutingExclusionCode::OwnedNotRunning);
    }
    if !runtime.origin_verified {
        push_code(&mut codes, RoutingExclusionCode::OriginUnverified);
    }
    if !runtime.verified_ready {
        push_code(&mut codes, RoutingExclusionCode::NotReady);
    }
    if !runtime.policy_ready {
        push_code(&mut codes, RoutingExclusionCode::PolicyNotReady);
    }
    if !runtime.pin_capabilities_ready {
        push_code(&mut codes, RoutingExclusionCode::PinCapabilities);
    }
    if !runtime.tuple_aligned {
        push_code(&mut codes, RoutingExclusionCode::TupleUnaligned);
    }
    if !resolution
        .mappings
        .iter()
        .any(|mapping| mapping_identity_matches(route, mapping))
    {
        push_code(&mut codes, RoutingExclusionCode::Model);
    }
    if codes.is_empty() {
        push_code(&mut codes, RoutingExclusionCode::ConfigurationUnavailable);
    }
    let authority = Some(authority_of(route));
    codes
        .into_iter()
        .map(|code| RoutingExclusion {
            detail: format!("{} for `{}`", code_label(code), subject_label(route)),
            account_id: nonempty(&route.legacy_account_id)
                .or_else(|| nonempty(&route.credential_id)),
            provider_id: nonempty(&route.provider_id),
            upstream_model: nonempty(&route.upstream_model),
            authority: authority.clone(),
            code,
        })
        .collect()
}

fn eligible_candidate(route: &RouteFact) -> RoutingEligibleCandidate {
    RoutingEligibleCandidate {
        account_id: subject_label(route),
        account_name: route.account_label.clone(),
        provider_id: route.provider_id.clone(),
        destination_id: nonempty(&route.destination_id),
        destination_name: nonempty(&route.destination_label),
        adapter_kind: route.adapter_kind.clone(),
        channel: channel_of(route.channel),
        resolved_model: if route.upstream_model.is_empty() {
            route.public_model.clone()
        } else {
            route.upstream_model.clone()
        },
        upstream_protocol: protocol_from_route(&route.protocol),
        routing_rank: route.routing_rank,
        authority: authority_of(route),
    }
}

fn protocol_from_route(protocol: &str) -> RoutingClientProtocol {
    match protocol {
        "responses" => RoutingClientProtocol::Responses,
        "messages" => RoutingClientProtocol::Messages,
        _ => RoutingClientProtocol::ChatCompletions,
    }
}

fn projection_of(facts: &OwnedRoutingFacts) -> RoutingOwnedProjection {
    let runtime = &facts.runtime;
    RoutingOwnedProjection {
        desired: RoutingPlaneTuple {
            generation: facts.projection.desired.generation,
            revision: facts.projection.desired.revision,
            digest: facts.projection.desired.digest.clone(),
        },
        applied: RoutingPlaneTuple {
            generation: facts.projection.applied.generation,
            revision: facts.projection.applied.revision,
            digest: facts.projection.applied.digest.clone(),
        },
        apply_status: facts.projection.apply_status.clone(),
        desired_running: facts.projection.desired_running,
        runtime_child_generation: facts.projection.runtime_child_generation,
        unavailable: runtime.unavailable,
        state_changed: runtime.state_changed,
        stopped: runtime.stopped,
        poisoned: runtime.poisoned,
        origin_verified: runtime.origin_verified,
        verified_ready: runtime.verified_ready,
        policy_ready: runtime.policy_ready,
        policy_malformed: runtime.policy_malformed,
        tuple_aligned: runtime.tuple_aligned,
        pin_capabilities_ready: runtime.pin_capabilities_ready,
        owned_running_before: runtime.owned_running_before,
        owned_running_after: runtime.owned_running_after,
        owned_running: runtime.owned_running,
    }
}

fn authority_of(route: &RouteFact) -> RoutingRouteAuthority {
    RoutingRouteAuthority {
        plane: match route.plane {
            RoutePlane::Desired => RoutingRoutePlane::Desired,
            RoutePlane::Applied => RoutingRoutePlane::Applied,
        },
        credential_id: route.credential_id.clone(),
        credential_version: route.credential_version,
        current_version: route.current_version,
        provider_id: route.provider_id.clone(),
        binding_id: route.binding_id.clone(),
        auth_id: route.auth_id.clone(),
        registration_epoch: route.registration_epoch,
        routing_rank: route.routing_rank,
        destination_id: route.destination_id.clone(),
        legacy_account_id: route.legacy_account_id.clone(),
        account_label: route.account_label.clone(),
        destination_label: route.destination_label.clone(),
        public_model: route.public_model.clone(),
        upstream_model: route.upstream_model.clone(),
        spelling: match route.spelling {
            Spelling::Empty => RoutingSpelling::Empty,
            Spelling::Same => RoutingSpelling::Same,
            Spelling::DistinctUpstream => RoutingSpelling::DistinctUpstream,
        },
        protocol: route.protocol.clone(),
        endpoint_id: route.endpoint_id.clone(),
        origin: route.origin.clone(),
        endpoint_fingerprint: route.endpoint_fingerprint.clone(),
        validation_only: route.validation_only,
        channel: channel_of(route.channel),
        adapter_kind: route.adapter_kind.clone(),
        material: match route.material {
            MaterialFact::HttpNone => RoutingMaterialFact::HttpNone,
            MaterialFact::KeyedUnchecked => RoutingMaterialFact::KeyedUnchecked,
            MaterialFact::NativePresent => RoutingMaterialFact::NativePresent,
            MaterialFact::Unproven => RoutingMaterialFact::Unproven,
        },
        posture: match route.posture {
            RoutePosture::Client => RoutingRoutePosture::Client,
            RoutePosture::ValidationOnly => RoutingRoutePosture::ValidationOnly,
            RoutePosture::Excluded => RoutingRoutePosture::Excluded,
        },
        exclusions: route
            .exclusions
            .iter()
            .copied()
            .map(authority_code)
            .collect(),
        credential_enabled: route.credential_enabled,
        binding_enabled: route.binding_enabled,
        destination_enabled: route.destination_enabled,
        destination_draft: route.destination_draft,
        setup_step: route.setup_step.clone(),
        native_provider: route.native_provider.clone(),
        native_mode: route.native_mode.clone(),
        capability_listed: route.capability_listed,
        grants_cover: route.grants_cover,
        native_operations: route
            .native_operations
            .iter()
            .map(|fact| RoutingOperationFact {
                generation_kind: fact.generation_kind.clone(),
                disposition: grant_of(&fact.disposition),
            })
            .collect(),
        caller_pending: route.caller_pending,
        secret_recheck_pending: route.secret_recheck_pending,
        send_pending: route.send_pending,
        quota: quota_of(&route.quota),
        known_restriction_blocks: route.known_restriction_blocks,
        trial_pending: route.trial_pending,
        client_configuration_eligible: route.client_configuration_eligible,
        migration_required: route.migration_required,
        historical_placement: match route.historical_placement {
            HistoricalPlacement::NotApplicable => RoutingHistoricalPlacement::NotApplicable,
            HistoricalPlacement::OwnedPool => RoutingHistoricalPlacement::OwnedPool,
            HistoricalPlacement::Remote => RoutingHistoricalPlacement::Remote,
        },
    }
}

fn grant_of(grant: &GrantFact) -> RoutingGrantDisposition {
    match grant {
        GrantFact::Granted { pins } => RoutingGrantDisposition::Granted {
            pins: pins
                .iter()
                .map(|pin| RoutingNativePin {
                    protocol: pin.protocol.clone(),
                    endpoint_id: pin.endpoint_id.clone(),
                    origin: pin.origin.clone(),
                    endpoint_fingerprint: pin.endpoint_fingerprint.clone(),
                    http_method: pin.http_method.clone(),
                })
                .collect(),
        },
        GrantFact::NotGranted => RoutingGrantDisposition::NotGranted,
        GrantFact::LocalOnly => RoutingGrantDisposition::LocalOnly,
        GrantFact::Unavailable => RoutingGrantDisposition::Unavailable,
    }
}

fn quota_of(quota: &ScopedQuotaView) -> RoutingQuotaFact {
    match quota {
        ScopedQuotaView::Unknown => RoutingQuotaFact {
            state: RoutingQuotaState::Unknown,
            evidence: Vec::new(),
        },
        ScopedQuotaView::Malformed => RoutingQuotaFact {
            state: RoutingQuotaState::Malformed,
            evidence: Vec::new(),
        },
        ScopedQuotaView::Evidence(rows) => RoutingQuotaFact {
            state: RoutingQuotaState::Evidence,
            evidence: rows
                .iter()
                .map(|row| {
                    let (
                        subject_kind,
                        credential_id,
                        credential_version,
                        provider_id,
                        binding_id,
                        pool_id,
                        pool_version,
                    ) = match &row.scope.subject {
                        Subject::Credential {
                            credential_id,
                            credential_version,
                            provider_id,
                            binding_id,
                        } => (
                            "credential".to_string(),
                            Some(credential_id.clone()),
                            Some(*credential_version),
                            Some(provider_id.clone()),
                            Some(binding_id.clone()),
                            None,
                            None,
                        ),
                        Subject::Pool {
                            pool_id,
                            pool_version,
                        } => (
                            "pool".to_string(),
                            None,
                            None,
                            None,
                            None,
                            Some(pool_id.clone()),
                            Some(*pool_version),
                        ),
                    };
                    let (reset, reset_at) = match row.reset {
                        ResetEvidence::Known { at } => ("known".to_string(), Some(at.to_rfc3339())),
                        ResetEvidence::UnknownReset => ("unknown_reset".to_string(), None),
                        ResetEvidence::Expired { at } => {
                            ("expired".to_string(), Some(at.to_rfc3339()))
                        }
                    };
                    RoutingQuotaEvidence {
                        subject_kind,
                        credential_id,
                        credential_version,
                        provider_id,
                        binding_id,
                        pool_id,
                        pool_version,
                        public_model: row.scope.public_model.clone(),
                        window: enum_name(&row.window),
                        source: enum_name(&row.source),
                        observed_at: row.observed_at.to_rfc3339(),
                        observation_id: row.observation_id.clone(),
                        reset,
                        reset_at,
                        applicable: row.applicable,
                    }
                })
                .collect(),
        },
    }
}

fn authority_code(exclusion: RouteExclusion) -> RoutingExclusionCode {
    match exclusion {
        RouteExclusion::Identity => RoutingExclusionCode::Identity,
        RouteExclusion::Rebound => RoutingExclusionCode::Rebound,
        RouteExclusion::Version => RoutingExclusionCode::Version,
        RouteExclusion::Setup => RoutingExclusionCode::SetupBlocked,
        RouteExclusion::Disabled => RoutingExclusionCode::Disabled,
        RouteExclusion::Draft => RoutingExclusionCode::Draft,
        RouteExclusion::Scope => RoutingExclusionCode::Scope,
        RouteExclusion::Model => RoutingExclusionCode::Model,
        RouteExclusion::Protocol => RoutingExclusionCode::Protocol,
        RouteExclusion::Capability => RoutingExclusionCode::Capability,
        RouteExclusion::NativePresence => RoutingExclusionCode::NativePresence,
        RouteExclusion::NativeMode => RoutingExclusionCode::NativeMode,
        RouteExclusion::NativeTargets => RoutingExclusionCode::NativeTargets,
        RouteExclusion::NotGranted => RoutingExclusionCode::NotGranted,
        RouteExclusion::Material => RoutingExclusionCode::Material,
        RouteExclusion::Unavailable => RoutingExclusionCode::ConfigurationUnavailable,
    }
}

fn channel_of(channel: RoutingProductChannel) -> RoutingChannel {
    match channel {
        RoutingProductChannel::Go => RoutingChannel::Go,
        RoutingProductChannel::Free => RoutingChannel::Free,
    }
}

fn push_code(codes: &mut Vec<RoutingExclusionCode>, code: RoutingExclusionCode) {
    if !codes.contains(&code) {
        codes.push(code);
    }
}

fn code_label(code: RoutingExclusionCode) -> String {
    enum_name(&code)
}

fn enum_name<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        _ => String::new(),
    }
}

fn subject_label(route: &RouteFact) -> String {
    if !route.legacy_account_id.is_empty() {
        route.legacy_account_id.clone()
    } else {
        route.credential_id.clone()
    }
}

fn nonempty(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests;
