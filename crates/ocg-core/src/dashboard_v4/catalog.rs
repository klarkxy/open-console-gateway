//! Local built-in Provider catalog edits.
//!
//! Writes the persisted snapshot only. Official `/models` refresh stays on
//! the V3 adapter; this slice never issues outbound requests.

use std::collections::HashSet;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::Utc;

use crate::dashboard_v3::{ControlRevision, V3ApiError, check_expectation, parse_mutation_json};
use crate::provider_contracts::{
    ContractScope, SCOPE_KIND_CUSTOM_ENDPOINT, SCOPE_KIND_PROVIDER, builtin_provider_scope_ids,
};
use crate::state::CoreState;

use super::types::{CatalogModelsRemoveRequest, CatalogModelsRemoveResult};

pub(super) async fn remove_models(
    State(state): State<CoreState>,
    Path((scope_kind, scope_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<CatalogModelsRemoveResult>, V3ApiError> {
    let input = parse_mutation_json::<CatalogModelsRemoveRequest>(&body)?;
    let scope = ContractScope::parse(&scope_kind, &scope_id)
        .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
    if scope_kind == SCOPE_KIND_CUSTOM_ENDPOINT {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "Custom API model catalogs are account declarations and cannot be edited here",
        ));
    }
    if scope_kind != SCOPE_KIND_PROVIDER || !builtin_provider_scope_ids().contains(&scope.id()) {
        return Err(V3ApiError::not_found_at(&state, "provider scope not found"));
    }

    let mut seen = HashSet::new();
    let mut model_ids = Vec::with_capacity(input.model_ids.len());
    for model_id in &input.model_ids {
        let model_id = model_id.trim();
        if model_id.is_empty() || !seen.insert(model_id) {
            return Err(V3ApiError::invalid_request_at(
                &state,
                "modelIds must be distinct nonempty catalog models",
            ));
        }
        model_ids.push(model_id.to_string());
    }
    if model_ids.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "modelIds must be nonempty",
        ));
    }

    let now = Utc::now();
    let snapshot = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        let (row, reload) = {
            let db = state.db.lock();
            let Some(current) = db
                .load_persisted_scope(&scope)
                .map_err(V3ApiError::internal)?
            else {
                return Err(V3ApiError::precondition_failed_at(
                    &state,
                    "provider model catalog has not been refreshed",
                ));
            };
            let known: HashSet<&str> = current.catalog_models.iter().map(String::as_str).collect();
            if model_ids
                .iter()
                .any(|model_id| !known.contains(model_id.as_str()))
            {
                return Err(V3ApiError::invalid_request_at(
                    &state,
                    "modelIds must be distinct models from the saved catalog",
                ));
            }
            let row = db
                .remove_contract_catalog_models(&scope, &model_ids, now)
                .map_err(V3ApiError::internal)?;
            // The catalog write is already durable. Advance CAS before the
            // fallible reload so persisted state cannot hide behind the
            // caller's token, including when reload fails.
            let _revision = state.bump_settings_revision();
            let reload = state.reload_provider_contracts_locked(&db);
            (row, reload)
        };
        if reload.is_err() {
            state.restrict_provider_catalog_after_reload_failure(&row);
        }
        state.routing.reset();
        let snapshot = CatalogModelsRemoveResult {
            revision: ControlRevision::from_state(&state),
            removed_ids: model_ids,
            catalog_models: row.catalog_models,
        };
        reload.map_err(V3ApiError::internal)?;
        snapshot
    };
    Ok(Json(snapshot))
}
