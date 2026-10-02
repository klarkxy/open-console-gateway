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
    let receipt = super::applications::DashboardReceipt::open(
        &state,
        "catalog.remove",
        "provider.catalog",
        super::applications::opaque_subject(&scope_id),
    );
    let mut durable = None;
    let mut removed = 0_u32;
    let result = remove_models_work(
        &state,
        scope_kind,
        scope_id,
        body,
        &mut durable,
        &mut removed,
    );
    if result.is_ok() {
        durable = None;
    }
    let removed = removed;
    receipt
        .observe(result, durable, |value| {
            crate::log_types::OperationMetadata {
                revision: Some(value.revision.revision),
                requested_count: Some(removed),
                completed_count: Some(removed),
                failed_count: Some(0),
                ..crate::log_types::OperationMetadata::default()
            }
        })
        .map(Json)
}

fn remove_models_work(
    state: &CoreState,
    scope_kind: String,
    scope_id: String,
    body: Bytes,
    durable: &mut Option<super::applications::DurableEffect>,
    removed: &mut u32,
) -> Result<CatalogModelsRemoveResult, V3ApiError> {
    let input = parse_mutation_json::<CatalogModelsRemoveRequest>(&body)?;
    let scope = ContractScope::parse(&scope_kind, &scope_id)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    if scope_kind == SCOPE_KIND_CUSTOM_ENDPOINT {
        return Err(V3ApiError::invalid_request_at(
            state,
            "Custom API model catalogs are account declarations and cannot be edited here",
        ));
    }
    if scope_kind != SCOPE_KIND_PROVIDER || !builtin_provider_scope_ids().contains(&scope.id()) {
        return Err(V3ApiError::not_found_at(state, "provider scope not found"));
    }

    let mut seen = HashSet::new();
    let mut model_ids = Vec::with_capacity(input.model_ids.len());
    for model_id in &input.model_ids {
        let model_id = model_id.trim();
        if model_id.is_empty() || !seen.insert(model_id) {
            return Err(V3ApiError::invalid_request_at(
                state,
                "modelIds must be distinct nonempty catalog models",
            ));
        }
        model_ids.push(model_id.to_string());
    }
    if model_ids.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "modelIds must be nonempty",
        ));
    }

    let now = Utc::now();
    {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, &input.expectation)?;
        let (row, reload) = {
            let db = state.db.lock();
            let Some(current) = db
                .load_persisted_scope(&scope)
                .map_err(V3ApiError::internal)?
            else {
                return Err(V3ApiError::precondition_failed_at(
                    state,
                    "provider model catalog has not been refreshed",
                ));
            };
            let known: HashSet<&str> = current.catalog_models.iter().map(String::as_str).collect();
            if model_ids
                .iter()
                .any(|model_id| !known.contains(model_id.as_str()))
            {
                return Err(V3ApiError::invalid_request_at(
                    state,
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
        *removed = super::applications::count_u32(model_ids.len());
        let snapshot = CatalogModelsRemoveResult {
            revision: ControlRevision::from_state(state),
            removed_ids: model_ids,
            catalog_models: row.catalog_models,
        };
        if reload.is_err() {
            *durable = Some(super::applications::DurableEffect {
                revision: state.settings_revision(),
                completed: *removed,
                failed: 1,
                related_ids: Vec::new(),
            });
        }
        reload.map_err(V3ApiError::internal)?;
        Ok(snapshot)
    }
}

pub(super) async fn add_models(
    State(state): State<CoreState>,
    Path((scope_kind, scope_id)): Path<(String, String)>,
    body: Bytes,
) -> Result<Json<crate::dashboard_v3::ProviderContracts>, V3ApiError> {
    let mut receipt = super::applications::DashboardReceipt::open(
        &state,
        "catalog.add",
        "provider.catalog",
        super::applications::opaque_subject(&scope_id),
    );
    let mut added = 0_u32;
    let result = add_models_work(&state, scope_kind, scope_id, body, &mut added, &mut receipt);
    let added = added;
    receipt
        .observe(result, None, |value| crate::log_types::OperationMetadata {
            revision: Some(value.revision),
            requested_count: Some(added),
            completed_count: Some(added),
            failed_count: Some(0),
            ..crate::log_types::OperationMetadata::default()
        })
        .map(Json)
}

fn add_models_work(
    state: &CoreState,
    scope_kind: String,
    scope_id: String,
    body: Bytes,
    added: &mut u32,
    receipt: &mut super::applications::DashboardReceipt,
) -> Result<crate::dashboard_v3::ProviderContracts, V3ApiError> {
    let input = parse_mutation_json::<super::types::CatalogModelsAddRequest>(&body)?;
    if scope_kind != SCOPE_KIND_PROVIDER
        || !builtin_provider_scope_ids().contains(&scope_id.as_str())
    {
        return Err(V3ApiError::invalid_request_at(
            state,
            "only built-in provider catalogs can be added here",
        ));
    }
    let scope = ContractScope::provider(&scope_id);
    let _settings = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let contracts = state.provider_contracts();
    let current = contracts
        .scope(&scope)
        .ok_or_else(|| V3ApiError::not_found_at(state, "provider scope not found"))?;
    let model_ids = validate_additions(&current.catalog.models, &input.model_ids)
        .map_err(|message| V3ApiError::invalid_request_at(state, message))?;
    *added = super::applications::count_u32(model_ids.len());
    let completed = *added;
    receipt
        .commit_recorded(
            state,
            crate::log_types::OperationMetadata {
                completed_count: Some(completed),
                ..crate::log_types::OperationMetadata::default()
            },
            |db| db.add_contract_catalog_models(&scope, &model_ids, Utc::now()),
        )
        .map_err(V3ApiError::internal)?;
    crate::dashboard_v3::provider_contracts_response(state).map(|value| value.0)
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
    let mut receipt = super::applications::DashboardReceipt::open(
        &state,
        "catalog.edit",
        "provider.catalog",
        super::applications::opaque_subject(&scope_id),
    );
    let result = edit_model_work(&state, scope_id, body, &mut receipt);
    receipt
        .observe(result, None, |value| crate::log_types::OperationMetadata {
            changed_fields: vec![
                "public_model".to_string(),
                "upstream_model".to_string(),
                "protocols".to_string(),
                "enabled".to_string(),
            ],
            revision: Some(value.revision),
            completed_count: Some(1),
            failed_count: Some(0),
            ..crate::log_types::OperationMetadata::default()
        })
        .map(Json)
}

fn edit_model_work(
    state: &CoreState,
    scope_id: String,
    body: Bytes,
    receipt: &mut super::applications::DashboardReceipt,
) -> Result<crate::dashboard_v3::ProviderContracts, V3ApiError> {
    let input = parse_mutation_json::<super::types::CatalogModelEditRequest>(&body)?;
    if !builtin_provider_scope_ids().contains(&scope_id.as_str()) {
        return Err(V3ApiError::invalid_request_at(
            state,
            "unknown built-in provider",
        ));
    }
    let scope = ContractScope::provider(&scope_id);
    let _settings = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let model = ocg_domain::destination::CatalogModel {
        public_model: input.public_model.trim().to_string(),
        upstream_model: input.upstream_model.trim().to_string(),
        protocols: input.protocols.into_iter().map(Into::into).collect(),
        preferred: input.preferred.map(Into::into),
        enabled: input.enabled,
        upstream_override: None,
    };
    receipt
        .commit_recorded(
            state,
            crate::log_types::OperationMetadata {
                changed_fields: vec![
                    "public_model".to_string(),
                    "upstream_model".to_string(),
                    "protocols".to_string(),
                    "enabled".to_string(),
                ],
                completed_count: Some(1),
                ..crate::log_types::OperationMetadata::default()
            },
            |db| {
                db.edit_contract_catalog_model(
                    &scope,
                    input.original_model_id.as_deref(),
                    model,
                    Utc::now(),
                )
            },
        )
        .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
    crate::dashboard_v3::provider_contracts_response(state).map(|value| value.0)
}

#[cfg(test)]
mod tests;
