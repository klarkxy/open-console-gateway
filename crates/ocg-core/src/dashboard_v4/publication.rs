//! Per-public-name downstream listing publication.
//!
//! Writes the unpublished-name set only. Routing is unchanged. This slice
//! never issues outbound requests.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

use crate::alias_publication::normalize_public_model_key;
use crate::dashboard_v3::{ControlRevision, V3ApiError, parse_mutation_json};
use crate::state::CoreState;

use super::types::{AliasPublication, AliasPublicationUpdate};

pub(super) async fn get_publication(
    State(state): State<CoreState>,
) -> Result<Json<AliasPublication>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    Ok(Json(publication_payload(&state)))
}

pub(super) async fn patch_publication(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<AliasPublication>, V3ApiError> {
    let input = parse_mutation_json::<AliasPublicationUpdate>(&body)?;
    let key = normalize_public_model_key(&input.public_model)
        .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    let _settings = state.settings_update.lock();
    if input.expectation.expected_revision != state.settings_revision()
        || input.expectation.process_generation != state.process_generation()
    {
        return Err(V3ApiError::revision_conflict(&state));
    }
    let unpublished =
        crate::account_control::set_public_model_publication_locked(&state, &key, input.published)
            .map_err(|error| match error {
                crate::account_control::AccountControlError::Internal(error) => {
                    V3ApiError::internal(error)
                }
                crate::account_control::AccountControlError::RevisionConflict => {
                    V3ApiError::revision_conflict(&state)
                }
                other => V3ApiError::invalid_request_at(&state, other.to_string()),
            })?;
    Ok(Json(AliasPublication {
        revision: ControlRevision::from_state(&state),
        unpublished,
    }))
}

fn publication_payload(state: &CoreState) -> AliasPublication {
    AliasPublication {
        revision: ControlRevision::from_state(state),
        unpublished: state.unpublished_public_model_list(),
    }
}

#[cfg(test)]
mod tests;
