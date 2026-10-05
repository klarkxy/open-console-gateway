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

    let removed = state
        .remove_builtin_catalog_models(
            input.expectation.expected_revision,
            input.expectation.process_generation,
            &scope,
            &model_ids,
            Utc::now(),
        )
        .map_err(|error| map_catalog_removal_error(&state, error))?;
    crate::cpa_execution::note_product_apply(&state).await;
    Ok(Json(CatalogModelsRemoveResult {
        revision: ControlRevision {
            revision: removed.revision,
            process_generation: removed.process_generation,
            pricing_revision: removed.pricing_revision,
        },
        removed_ids: model_ids,
        catalog_models: removed.row.catalog_models,
    }))
}

fn map_catalog_removal_error(
    state: &CoreState,
    error: crate::state::CatalogRemovalError,
) -> V3ApiError {
    use crate::state::CatalogRemovalError;
    match error {
        CatalogRemovalError::RevisionConflict => V3ApiError::revision_conflict(state),
        CatalogRemovalError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        CatalogRemovalError::Unavailable(message) => {
            V3ApiError::precondition_failed_at(state, message)
        }
        CatalogRemovalError::Internal(error) => V3ApiError::internal(error),
    }
}

pub(super) async fn add_models(
    State(state): State<CoreState>,
    Path((scope_kind, scope_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<crate::dashboard_v3::ProviderContracts>, V3ApiError> {
    let input = parse_mutation_json::<super::types::CatalogModelsAddRequest>(&body)?;
    if scope_kind != SCOPE_KIND_PROVIDER
        || !builtin_provider_scope_ids().contains(&scope_id.as_str())
    {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "only built-in provider catalogs can be added here",
        ));
    }
    let scope = ContractScope::provider(&scope_id);
    let receipt = {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        let contracts = state.provider_contracts();
        let current = contracts
            .scope(&scope)
            .ok_or_else(|| V3ApiError::not_found_at(&state, "provider scope not found"))?;
        let model_ids = validate_additions(&current.catalog.models, &input.model_ids)
            .map_err(|message| V3ApiError::invalid_request_at(&state, message))?;
        crate::account_control::add_builtin_catalog_models_locked(&state, &scope_id, &model_ids)
            .map_err(|error| match error {
                crate::account_control::AccountControlError::RevisionConflict => {
                    V3ApiError::revision_conflict(&state)
                }
                crate::account_control::AccountControlError::Internal(error) => {
                    V3ApiError::internal(error)
                }
                other => V3ApiError::invalid_request_at(&state, other.to_string()),
            })?;
        contracts_receipt(&state)
    };
    crate::cpa_execution::note_product_apply(&state).await;
    receipt
}

fn validate_additions(existing: &[String], input: &[String]) -> Result<Vec<String>, &'static str> {
    if input.is_empty() || input.len() > 200 || existing.len() + input.len() > 2000 {
        return Err("modelIds must contain 1 to 200 IDs and catalog must not exceed 2000 models");
    }
    let mut known: HashSet<String> = existing.iter().map(|id| id.to_ascii_lowercase()).collect();
    input
        .iter()
        .map(|id| {
            let id = id.trim();
            if id.is_empty()
                || id.len() > 200
                || id.chars().any(|c| c.is_whitespace() || c.is_control())
            {
                return Err(
                    "model IDs must be 1 to 200 bytes without whitespace or control characters",
                );
            }
            if !known.insert(id.to_ascii_lowercase()) {
                return Err("model ID already exists");
            }
            Ok(id.to_string())
        })
        .collect()
}

pub(super) async fn edit_model(
    State(state): State<CoreState>,
    Path(scope_id): Path<String>,
    body: Bytes,
) -> Result<Json<crate::dashboard_v3::ProviderContracts>, V3ApiError> {
    let input = parse_mutation_json::<super::types::CatalogModelEditRequest>(&body)?;
    if !builtin_provider_scope_ids().contains(&scope_id.as_str()) {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "unknown built-in provider",
        ));
    }
    let model = ocg_domain::destination::CatalogModel {
        public_model: input.public_model.trim().to_string(),
        upstream_model: input.upstream_model.trim().to_string(),
        protocols: input.protocols.into_iter().map(Into::into).collect(),
        preferred: input.preferred.map(Into::into),
        enabled: input.enabled,
        upstream_override: None,
    };
    let receipt = {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        crate::account_control::edit_builtin_catalog_model_locked(
            &state,
            &scope_id,
            input.original_model_id.as_deref(),
            model,
        )
        .map_err(|error| match error {
            crate::account_control::AccountControlError::RevisionConflict => {
                V3ApiError::revision_conflict(&state)
            }
            other => V3ApiError::invalid_request_at(&state, other.to_string()),
        })?;
        contracts_receipt(&state)
    };
    crate::cpa_execution::note_product_apply(&state).await;
    receipt
}

#[cfg(test)]
thread_local! {
    static FAIL_NEXT_CONTRACTS_RECEIPT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Next post-commit contracts read returns an error after the catalog write.
#[cfg(test)]
pub(super) fn fail_next_contracts_receipt() {
    FAIL_NEXT_CONTRACTS_RECEIPT.with(|flag| flag.set(true));
}

#[cfg(test)]
pub(super) fn clear_contracts_receipt_failure() {
    FAIL_NEXT_CONTRACTS_RECEIPT.with(|flag| flag.set(false));
}

fn contracts_receipt(
    state: &CoreState,
) -> Result<Json<crate::dashboard_v3::ProviderContracts>, V3ApiError> {
    #[cfg(test)]
    if FAIL_NEXT_CONTRACTS_RECEIPT.with(|flag| flag.replace(false)) {
        return Err(V3ApiError::internal("provider contracts could not be read"));
    }
    crate::dashboard_v3::provider_contracts_response(state)
}

#[cfg(test)]
mod tests;
