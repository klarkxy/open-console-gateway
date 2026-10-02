//! A single CAS write owns the visible layout and its flattened routing order.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

use super::destinations::{DestinationsError, projection_refused};
use crate::dashboard_v3::{ControlRevision, V3ApiError, parse_mutation_json};
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
    // One settings lock covers the projection gate, the rank write, and the
    // receipt. Membership changes do not replace credentials or transport.
    let _settings_update = state.settings_update.lock();
    if input.expectation.expected_revision != state.settings_revision()
        || input.expectation.process_generation != state.process_generation()
    {
        return Err(V3ApiError::revision_conflict(&state).into());
    }
    snapshot(&state)?;
    crate::account_control::replace_routing_cards_locked(&state, &input.cards).map_err(
        |error| match error {
            crate::account_control::AccountControlError::Invalid(message) => {
                V3ApiError::invalid_request_at(&state, message)
            }
            crate::account_control::AccountControlError::Internal(error) => {
                V3ApiError::internal(error)
            }
            crate::account_control::AccountControlError::RevisionConflict => {
                V3ApiError::revision_conflict(&state)
            }
            other => V3ApiError::invalid_request_at(&state, other.to_string()),
        },
    )?;
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
