//! Local CPA catalog projection and routing selection.
//!
//! Reads and writes the persisted snapshot only. Refreshing models from CPA
//! stays on the V3 adapter; this slice never issues outbound requests.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::state::CoreState;

use super::types::{CpaCatalog, CpaCatalogEntry, CpaCatalogUpdate};

pub(super) async fn get_models(
    State(state): State<CoreState>,
) -> Result<Json<CpaCatalog>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    Ok(Json(catalog_payload(&state)?))
}

pub(super) async fn put_models(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<CpaCatalog>, V3ApiError> {
    let input = parse_mutation_json::<CpaCatalogUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    state
        .replace_cpa_model_selection_locked(&input.enabled_ids)
        .map_err(|error| match error {
            crate::state::CpaSelectionError::RevisionConflict => {
                V3ApiError::revision_conflict(&state)
            }
            crate::state::CpaSelectionError::Invalid(message) => {
                V3ApiError::invalid_request_at(&state, message)
            }
            crate::state::CpaSelectionError::Unavailable(message) => {
                V3ApiError::precondition_failed_at(&state, message)
            }
            crate::state::CpaSelectionError::Internal(error) => V3ApiError::internal(error),
        })?;
    Ok(Json(catalog_payload(&state)?))
}

fn catalog_payload(state: &CoreState) -> Result<CpaCatalog, V3ApiError> {
    let catalog = {
        let db = state.db.lock();
        db.cpa_model_catalog().map_err(V3ApiError::internal)?
    };
    Ok(CpaCatalog {
        revision: ControlRevision::from_state(state),
        models: catalog
            .as_ref()
            .map(|item| {
                item.models
                    .iter()
                    .map(|model| CpaCatalogEntry {
                        id: model.id.clone(),
                        owned_by: model.owned_by.clone(),
                        enabled: model.enabled,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        source_url: catalog.as_ref().map(|item| item.source_url.clone()),
        refreshed_at: catalog
            .as_ref()
            .and_then(|item| item.refreshed_at)
            .map(|value| value.to_rfc3339()),
    })
}

#[cfg(test)]
mod tests;
