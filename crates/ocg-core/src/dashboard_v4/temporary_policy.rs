//! V4 temporary-unavailability configuration and restriction control plane.
//!
//! GET/PUT persist `settings.temporary_unavailability_v1`. Restriction lookup
//! and clear are process-local and never send.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};

use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::gateway::policy::{
    ConfiguredRule, CustomMatch, DEFAULT_INITIAL_SECS, DEFAULT_MAX_SECS,
    GOAT_CREDITS_REJECTION_RULE, PolicyBackoff, PolicyDocumentError, RestrictionScope,
    compile_from_previous, destination_ids, load_configured_rules, persist_configured_rules,
    validate_configured,
};
use crate::gateway::recovery::RestrictionRecord;
use crate::state::CoreState;

use super::types::{
    TemporaryPolicyBackoff, TemporaryPolicyBuiltin, TemporaryPolicyClearRequest,
    TemporaryPolicyConfiguration, TemporaryPolicyMatch, TemporaryPolicyRestriction,
    TemporaryPolicyRestrictionState, TemporaryPolicyRestrictions, TemporaryPolicyRule,
    TemporaryPolicyScope, TemporaryPolicySource, TemporaryPolicyUpdate,
};

pub(super) async fn get_configuration(
    State(state): State<CoreState>,
) -> Result<Json<TemporaryPolicyConfiguration>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    configuration_locked(&state)
}

pub(super) async fn put_configuration(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<TemporaryPolicyConfiguration>, V3ApiError> {
    let input = parse_mutation_json::<TemporaryPolicyUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    let rules = input
        .rules
        .iter()
        .map(rule_from_dto)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
    {
        let db = state.db.lock();
        let destinations = destination_ids(&db).map_err(V3ApiError::internal)?;
        validate_configured(&rules, &destinations)
            .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
        persist_configured_rules(&db, &rules)
            .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
        let previous = state.recovery.policy_snapshot();
        let compiled = compile_from_previous(&rules, &previous);
        state.recovery.install_snapshot(compiled);
    }
    state.bump_settings_revision();
    configuration_locked(&state)
}

pub(super) async fn get_restrictions(
    State(state): State<CoreState>,
) -> Result<Json<TemporaryPolicyRestrictions>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    Ok(Json(restrictions_locked(&state)))
}

pub(super) async fn clear_restriction(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<TemporaryPolicyRestrictions>, V3ApiError> {
    let input = parse_mutation_json::<TemporaryPolicyClearRequest>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    state.recovery.clear_restriction(&id);
    state.bump_settings_revision();
    Ok(Json(restrictions_locked(&state)))
}

fn configuration_locked(
    state: &CoreState,
) -> Result<Json<TemporaryPolicyConfiguration>, V3ApiError> {
    let db = state.db.lock();
    let rules = load_configured_rules(&db)
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    Ok(Json(TemporaryPolicyConfiguration {
        revision: ControlRevision::from_state(state),
        rules: rules.iter().map(rule_to_dto).collect(),
        builtins: vec![TemporaryPolicyBuiltin {
            id: GOAT_CREDITS_REJECTION_RULE.into(),
            scope: TemporaryPolicyScope::CredentialModel,
            backoff: TemporaryPolicyBackoff {
                initial_seconds: DEFAULT_INITIAL_SECS,
                max_seconds: DEFAULT_MAX_SECS,
            },
        }],
    }))
}

fn restrictions_locked(state: &CoreState) -> TemporaryPolicyRestrictions {
    let (_, mono) = state.sample_gateway_clock();
    TemporaryPolicyRestrictions {
        revision: ControlRevision::from_state(state),
        restrictions: state
            .recovery
            .list_restrictions(mono)
            .into_iter()
            .map(restriction_to_dto)
            .collect(),
    }
}

fn rule_from_dto(rule: &TemporaryPolicyRule) -> Result<ConfiguredRule, PolicyDocumentError> {
    match rule {
        TemporaryPolicyRule::Custom {
            id,
            destination_id,
            enabled,
            scope,
            matcher,
            backoff,
        } => {
            reject_supplied_empty_match(matcher)?;
            Ok(ConfiguredRule::Custom {
                id: id.clone(),
                destination_id: destination_id.clone(),
                enabled: *enabled,
                scope: scope_from_dto(*scope),
                matcher: CustomMatch {
                    status_codes: matcher.status_codes.clone().unwrap_or_default(),
                    error_codes: matcher.error_codes.clone().unwrap_or_default(),
                    error_types: matcher.error_types.clone().unwrap_or_default(),
                    message_contains: matcher.message_contains.clone().unwrap_or_default(),
                },
                backoff: PolicyBackoff {
                    initial_secs: backoff.initial_seconds,
                    max_secs: backoff.max_seconds,
                },
            })
        }
        TemporaryPolicyRule::BuiltinOverride {
            id,
            destination_id,
            enabled,
            backoff,
        } => Ok(ConfiguredRule::BuiltinOverride {
            id: id.clone(),
            destination_id: destination_id.clone(),
            enabled: *enabled,
            backoff: backoff.map(|backoff| PolicyBackoff {
                initial_secs: backoff.initial_seconds,
                max_secs: backoff.max_seconds,
            }),
        }),
    }
}

fn reject_supplied_empty_match(matcher: &TemporaryPolicyMatch) -> Result<(), PolicyDocumentError> {
    if matcher.status_codes.as_ref().is_some_and(Vec::is_empty)
        || matcher.error_codes.as_ref().is_some_and(Vec::is_empty)
        || matcher.error_types.as_ref().is_some_and(Vec::is_empty)
        || matcher.message_contains.as_ref().is_some_and(Vec::is_empty)
    {
        return Err(PolicyDocumentError::InvalidMatch);
    }
    Ok(())
}

fn rule_to_dto(rule: &ConfiguredRule) -> TemporaryPolicyRule {
    match rule {
        ConfiguredRule::Custom {
            id,
            destination_id,
            enabled,
            scope,
            matcher,
            backoff,
        } => TemporaryPolicyRule::Custom {
            id: id.clone(),
            destination_id: destination_id.clone(),
            enabled: *enabled,
            scope: scope_to_dto(*scope),
            matcher: TemporaryPolicyMatch {
                status_codes: nonempty_opt(&matcher.status_codes),
                error_codes: nonempty_opt(&matcher.error_codes),
                error_types: nonempty_opt(&matcher.error_types),
                message_contains: nonempty_opt(&matcher.message_contains),
            },
            backoff: TemporaryPolicyBackoff {
                initial_seconds: backoff.initial_secs,
                max_seconds: backoff.max_secs,
            },
        },
        ConfiguredRule::BuiltinOverride {
            id,
            destination_id,
            enabled,
            backoff,
        } => TemporaryPolicyRule::BuiltinOverride {
            id: id.clone(),
            destination_id: destination_id.clone(),
            enabled: *enabled,
            backoff: backoff.map(|backoff| TemporaryPolicyBackoff {
                initial_seconds: backoff.initial_secs,
                max_seconds: backoff.max_secs,
            }),
        },
    }
}

fn nonempty_opt<T: Clone>(values: &[T]) -> Option<Vec<T>> {
    if values.is_empty() {
        None
    } else {
        Some(values.to_vec())
    }
}

fn scope_from_dto(scope: TemporaryPolicyScope) -> RestrictionScope {
    match scope {
        TemporaryPolicyScope::Credential => RestrictionScope::Credential,
        TemporaryPolicyScope::CredentialModel => RestrictionScope::CredentialModel,
    }
}

fn scope_to_dto(scope: RestrictionScope) -> TemporaryPolicyScope {
    match scope {
        RestrictionScope::Credential => TemporaryPolicyScope::Credential,
        RestrictionScope::CredentialModel => TemporaryPolicyScope::CredentialModel,
    }
}

fn restriction_to_dto(row: RestrictionRecord) -> TemporaryPolicyRestriction {
    TemporaryPolicyRestriction {
        id: row.id,
        rule_id: row.rule_id,
        rule_generation: row.rule_generation,
        source: match row.source {
            crate::gateway::policy::PolicySourceKind::Global => TemporaryPolicySource::Global,
            crate::gateway::policy::PolicySourceKind::Connection => {
                TemporaryPolicySource::Connection
            }
            crate::gateway::policy::PolicySourceKind::Builtin => TemporaryPolicySource::Builtin,
        },
        credential_id: row.credential_id,
        destination_id: row.destination_id,
        scope: scope_to_dto(row.scope),
        upstream_model: row.upstream_model,
        state: match row.state {
            "probing" => TemporaryPolicyRestrictionState::Probing,
            "waiting" => TemporaryPolicyRestrictionState::Waiting,
            _ => TemporaryPolicyRestrictionState::Ready,
        },
        next_probe_in_seconds: row.next_probe_in_seconds,
        probe_in_flight: row.probe_in_flight,
    }
}

#[cfg(test)]
mod tests;
