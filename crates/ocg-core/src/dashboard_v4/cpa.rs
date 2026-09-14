//! Local CPA catalog projection and routing selection.
//!
//! Reads and writes the persisted snapshot only. Refreshing models from CPA
//! stays on the V3 adapter; this slice never issues outbound requests.

use std::collections::HashSet;

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
    let catalog = {
        let db = state.db.lock();
        db.cpa_model_catalog().map_err(V3ApiError::internal)?
    };
    let Some(catalog) = catalog else {
        return Err(V3ApiError::precondition_failed_at(
            &state,
            "CPA model catalog has not been refreshed",
        ));
    };
    let known: HashSet<&str> = catalog
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    let mut seen = HashSet::new();
    let mut enabled_ids = Vec::new();
    for id in &input.enabled_ids {
        let id = id.trim();
        if id.is_empty() || !known.contains(id) || !seen.insert(id) {
            return Err(V3ApiError::invalid_request_at(
                &state,
                "enabledIds must be distinct models from the saved CPA catalog",
            ));
        }
        enabled_ids.push(id.to_string());
    }
    state
        .set_cpa_model_routing(&enabled_ids)
        .map_err(V3ApiError::internal)?;
    state.bump_settings_revision();
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
