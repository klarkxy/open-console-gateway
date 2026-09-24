//! A single CAS write owns the visible layout and its flattened routing order.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

use super::destinations::{DestinationsError, projection_refused};
use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::db::routing_cards;
use crate::destination_projection::read_v4_projection;
use crate::state::CoreState;

use super::types::{DestinationDto, RoutingCardList, RoutingCardUpdate};

pub(super) async fn list(
    State(state): State<CoreState>,
) -> Result<Json<RoutingCardList>, DestinationsError> {
    let _settings_update = state.settings_update.lock();
    snapshot(&state).map(Json)
}

pub(super) async fn replace(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<RoutingCardList>, DestinationsError> {
    let input = parse_mutation_json::<RoutingCardUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    // Preserve the destination read gate before changing any saved ranks.
    snapshot(&state)?;
    {
        let db = state.db.lock();
        let tx = db
            .conn
            .unchecked_transaction()
            .map_err(V3ApiError::internal)?;
        routing_cards::save_on(&tx, &input.cards)
            .and_then(|()| routing_cards::reconcile_on(&tx))
            .map_err(|error| V3ApiError::invalid_request_at(&state, error.to_string()))?;
        tx.commit().map_err(V3ApiError::internal)?;
        // Membership/rank changes do not replace credentials or transport.
        // Preserve conversation bindings and selector progress, as the legacy
        // account-order writer does; each new request loads the saved ranks.
        state.bump_settings_revision();
    }
    snapshot(&state).map(Json)
}

/// Caller holds settings_update so revision, layout and rows describe one state.
fn snapshot(state: &CoreState) -> Result<RoutingCardList, DestinationsError> {
    let db = state.db.lock();
    let projection = read_v4_projection(&db)
        .map_err(V3ApiError::internal)?
        .map_err(|refusals| DestinationsError::Refused(projection_refused(state, &refusals)))?;
    let cards = routing_cards::load_on(&db.conn).map_err(V3ApiError::internal)?;
    let recoveries = crate::db::quota_recovery::load_all_identified_on(&db.conn)
        .map_err(V3ApiError::internal)?;
    let probes = state.quota_probes.lock().clone();
    let revision = ControlRevision::from_state(state);
    Ok(RoutingCardList {
        revision,
        cards,
        destinations: projection
            .destinations
            .iter()
            .map(DestinationDto::from)
            .collect(),
        credentials: super::destinations::overlay_credential_dtos(
            state,
            &projection.credentials,
            &recoveries,
            &probes,
        ),
    })
}

#[cfg(test)]
mod tests;
