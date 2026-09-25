//! Authenticated, CAS-protected local rules and read-only recovery diagnostics.
//! Neither configuration nor reset issues an upstream request.
use super::types::{TemporaryPolicySnapshot, TemporaryPolicyUpdate};
use crate::dashboard_v3::{
    ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::db::temporary_policy;
use crate::routing_snapshot::RoutingSnapshot;
use crate::state::CoreState;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
};

pub(super) async fn get(
    State(state): State<CoreState>,
) -> Result<Json<TemporaryPolicySnapshot>, V3ApiError> {
    let _settings = state.settings_update.lock();
    snapshot(&state).map(Json)
}

pub(super) async fn put(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<TemporaryPolicySnapshot>, V3ApiError> {
    let input = parse_mutation_json::<TemporaryPolicyUpdate>(&body)?;
    let _settings = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    {
        let db = state.db.lock();
        // Retain the current projection gate. Invalid routing data is never
        // silently repaired by a rules write.
        RoutingSnapshot::load(&db).map_err(V3ApiError::internal)?;
        let tx = db
            .conn
            .unchecked_transaction()
            .map_err(V3ApiError::internal)?;
        let rules = temporary_policy::save_on(&tx, &input.rules)
            .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
        tx.commit().map_err(V3ApiError::internal)?;
        state.recovery.reconcile_policies(&rules);
        state.bump_settings_revision();
    }
    snapshot(&state).map(Json)
}

pub(super) async fn reset(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<TemporaryPolicySnapshot>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _settings = state.settings_update.lock();
    check_expectation(&state, &expectation)?;
    if !snapshot(&state)?.waits.iter().any(|wait| wait.id == id)
        || !state.recovery.reset_policy_wait(&id)
    {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "temporary wait changed; refresh before resetting",
        ));
    }
    // Process-local mutation, still serialized with CAS. Do not reset an
    // account, drop sticky state, clear a header wait, or change enablement.
    state.bump_settings_revision();
    snapshot(&state).map(Json)
}

fn snapshot(state: &CoreState) -> Result<TemporaryPolicySnapshot, V3ApiError> {
    let db = state.db.lock();
    let routing = RoutingSnapshot::load(&db).map_err(V3ApiError::internal)?;
    let rules = temporary_policy::load_on(&db.conn).map_err(V3ApiError::internal)?;
    let (wall, mono) = state.sample_gateway_clock();
    Ok(TemporaryPolicySnapshot {
        revision: ControlRevision::from_state(state),
        waits: state.recovery.policy_waits(&routing, &rules, wall, mono),
        rules: rules.rules.into_iter().map(|saved| saved.rule).collect(),
    })
}

#[cfg(test)]
mod tests;
