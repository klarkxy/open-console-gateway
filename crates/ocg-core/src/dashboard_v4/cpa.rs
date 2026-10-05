//! Historical shared CPA catalog.
//!
//! `GET` returns the stored snapshot as nonroutable. `PUT` does not change
//! the snapshot, revision, grants, or routes. Owned-native models stay on
//! the destination-scoped APIs. This slice never issues outbound requests.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;

use crate::dashboard_v3::{ControlRevision, V3ApiError, parse_mutation_json};
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
    // The body shape stays the same. A valid selection still cannot write.
    let _input = parse_mutation_json::<CpaCatalogUpdate>(&body)?;
    let _settings_update = state.settings_update.lock();
    Err(V3ApiError::precondition_failed_at(
        &state,
        crate::state::RETIRED_CPA_CATALOG,
    ))
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
                        // Stored `enabled` is historical. The read is not a route grant.
                        enabled: false,
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
