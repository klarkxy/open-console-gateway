//! Explicit model discovery for saved configurable HTTP destinations.
//! Reuses the same bounded, no-redirect discovery transport as draft discovery.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
};
use ocg_domain::destination::{AdapterKind, AuthScheme, Protocol};

use super::types::{
    DestinationCatalogModelUpdate, DestinationCatalogRefreshResult, DestinationCatalogUpdate,
    DestinationDto, DestinationModelTestRequest, DestinationModelTestResult,
    DestinationPatchResult,
};
use crate::account_control::{CatalogRefreshError, MutationCas};
use crate::dashboard_v3::{
    ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::routing_snapshot::RoutingSnapshot;
use crate::state::CoreState;

#[cfg(test)]
thread_local! {
    static INTERPOSE_AFTER_CATALOG_REFRESH_SNAPSHOT: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn interpose_catalog_refresh_snapshot(state: &CoreState) {
    if INTERPOSE_AFTER_CATALOG_REFRESH_SNAPSHOT.with(|flag| flag.replace(false)) {
        state.bump_settings_revision();
    }
}

#[cfg(test)]
fn arm_catalog_refresh_snapshot_interpose() {
    INTERPOSE_AFTER_CATALOG_REFRESH_SNAPSHOT.with(|flag| flag.set(true));
}

pub(super) async fn refresh(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationCatalogRefreshResult>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _refresh = state.provider_models_refresh.try_lock().map_err(|_| {
        V3ApiError::conflict_at(&state, "provider model refresh is already running")
    })?;
    let cas = MutationCas {
        expected_revision: expectation.expected_revision,
        process_generation: expectation.process_generation,
    };
    let prepared = crate::account_control::prepare_catalog_refresh(&state, &id, cas)
        .map_err(|error| map_catalog_refresh_error(&state, error))?;
    let (config, input, auth, key) = prepared.discovery_input();
    let (discovered, metadata) =
        crate::custom::discover_models_with_metadata(config, input, auth, key)
            .await
            .map_err(|error| {
                V3ApiError::outbound_failed(&state, prepared.redact(&error.message))
            })?;
    let models: Vec<_> = discovered
        .models
        .into_iter()
        .filter(|model| key.is_empty() || !model.contains(key))
        .collect();
    let truncated = discovered.truncated;
    finish_catalog_refresh(&state, &id, cas, prepared, &models, &metadata, truncated).map(Json)
}

/// Commit after discovery and copy the destination with this write's revision
/// before releasing `settings_update`.
fn finish_catalog_refresh(
    state: &CoreState,
    id: &str,
    cas: MutationCas,
    prepared: crate::account_control::PreparedCatalogRefresh,
    models: &[String],
    metadata: &std::collections::BTreeMap<String, crate::model_metadata::ModelMetadata>,
    truncated: bool,
) -> Result<DestinationCatalogRefreshResult, V3ApiError> {
    let _settings = state.settings_update.lock();
    let (updated, added_count) = crate::account_control::commit_catalog_refresh_locked(
        state, id, cas, prepared, models, metadata,
    )
    .map_err(|error| map_catalog_refresh_error(state, error))?;
    let result = DestinationCatalogRefreshResult {
        revision: ControlRevision::from_state(state),
        destination: DestinationDto::from(&updated),
        added_count,
        truncated,
    };
    #[cfg(test)]
    interpose_catalog_refresh_snapshot(state);
    Ok(result)
}

fn map_catalog_refresh_error(state: &CoreState, error: CatalogRefreshError) -> V3ApiError {
    match error {
        CatalogRefreshError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        CatalogRefreshError::NotFound(message) => V3ApiError::not_found_at(state, message),
        CatalogRefreshError::Conflict(message) if message == "revision conflict" => {
            V3ApiError::revision_conflict(state)
        }
        CatalogRefreshError::Conflict(message) => V3ApiError::conflict_at(state, message),
        CatalogRefreshError::Outbound(message) => V3ApiError::outbound_failed(state, message),
        CatalogRefreshError::Internal(error) => V3ApiError::internal(error),
    }
}

pub(super) async fn update(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationPatchResult>, super::destinations::DestinationsError> {
    let input = parse_mutation_json::<DestinationCatalogUpdate>(&body)?;
    let edits = catalog_edits(&input.updates);
    let _settings = state.settings_update.lock();
    if input.expectation.expected_revision != state.settings_revision()
        || input.expectation.process_generation != state.process_generation()
    {
        return Err(map_catalog_update_error(
            &state,
            crate::account_control::AccountControlError::RevisionConflict,
        )
        .into());
    }
    crate::account_control::update_http_catalog_locked(&state, &id, &edits, &input.remove_models)
        .map_err(|error| map_catalog_update_error(&state, error))?;
    super::destinations::mutation_result_locked(&state, &id).map(Json)
}

fn catalog_edits(
    updates: &[DestinationCatalogModelUpdate],
) -> Vec<crate::account_control::CatalogModelEdit> {
    updates
        .iter()
        .map(|update| crate::account_control::CatalogModelEdit {
            public_model: update.public_model.clone(),
            enabled: update.enabled,
            protocols: update
                .protocols
                .as_ref()
                .map(|protocols| protocols.iter().copied().map(Into::into).collect()),
            preferred: update.preferred.map(Into::into),
        })
        .collect()
}

fn map_catalog_update_error(
    state: &CoreState,
    error: crate::account_control::AccountControlError,
) -> V3ApiError {
    use crate::account_control::AccountControlError;
    match error {
        AccountControlError::NotFound => V3ApiError::not_found_at(state, "destination not found"),
        AccountControlError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        AccountControlError::Conflict(message) => V3ApiError::conflict_at(state, message),
        AccountControlError::RevisionConflict => V3ApiError::revision_conflict(state),
        AccountControlError::Unavailable(message) => {
            V3ApiError::precondition_failed_at(state, message)
        }
        AccountControlError::Internal(error) => V3ApiError::internal(error),
    }
}

pub(super) async fn test_model(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationModelTestResult>, V3ApiError> {
    let input = parse_mutation_json::<DestinationModelTestRequest>(&body)?;
    let protocol: Protocol = input.protocol.into();
    let (destination, model, credential, route, config, key) = {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        let snapshot = RoutingSnapshot::load(&state.db.lock()).map_err(V3ApiError::internal)?;
        let destination = snapshot
            .projection
            .destinations
            .into_iter()
            .find(|row| row.id == id)
            .ok_or_else(|| V3ApiError::not_found_at(&state, "destination not found"))?;
        if destination.adapter != AdapterKind::Http || destination.capabilities.observer {
            return Err(V3ApiError::invalid_request_at(
                &state,
                "destination is not a configurable HTTP connection",
            ));
        }
        let model = destination
            .catalog
            .iter()
            .find(|row| {
                row.public_model
                    .eq_ignore_ascii_case(input.public_model.trim())
            })
            .cloned()
            .ok_or_else(|| V3ApiError::not_found_at(&state, "model not found"))?;
        let route = ocg_domain::destination::http_model_route(&destination, &model, protocol)
            .ok_or_else(|| {
                V3ApiError::invalid_request_at(&state, "model protocol is not configured")
            })?;
        let format = match protocol {
            Protocol::ChatCompletions => crate::gateway::protocol::ApiFormat::ChatCompletions,
            Protocol::Responses => crate::gateway::protocol::ApiFormat::Responses,
            Protocol::Messages => crate::gateway::protocol::ApiFormat::Messages,
        };
        let mut candidates: Vec<_> = snapshot
            .credentials
            .into_iter()
            .filter(|c| {
                c.destination_id == id
                    && c.ready
                    && c.binding_enabled
                    && !c.key_cipher.is_empty()
                    && crate::gateway::materialize::endpoint_id_for_target(
                        c,
                        &destination,
                        &model,
                        format,
                    )
                    .is_ok_and(|endpoint| c.grants.allowed_endpoint_ids.contains(&endpoint))
                    && crate::custom_http::ensure_secret_origin_granted(
                        &route.endpoint_url,
                        &c.grants.allowed_origins,
                    )
                    .is_ok()
                    && crate::gateway::materialize::binding_allows_requested_model(
                        &c.scope,
                        &model.public_model,
                        &model.public_model,
                        [&model.public_model, &model.upstream_model],
                    )
            })
            .collect();
        candidates.sort_by_key(|c| (!c.enabled, c.auth_error.is_some()));
        let credential = if route.auth_scheme == AuthScheme::None {
            None
        } else {
            Some(candidates.into_iter().next().ok_or_else(|| {
                V3ApiError::invalid_request_at(
                    &state,
                    "model test requires a ready Key authorized for this model and protocol",
                )
            })?)
        };
        let key = credential
            .as_ref()
            .map(|c| state.decrypt_key(&c.key_cipher))
            .transpose()
            .map_err(V3ApiError::internal)?
            .unwrap_or_default();
        (destination, model, credential, route, state.config(), key)
    };
    let now = chrono::Utc::now();
    let config_input = crate::models::AccountCustomConfig {
        account_id: String::new(),
        endpoint_url: route.endpoint_url.clone(),
        upstream_protocol: protocol,
        created_at: now,
        updated_at: now,
    };
    let capability = crate::models::AccountModelCapability {
        account_id: String::new(),
        public_model: model.public_model.clone(),
        upstream_model: model.upstream_model.clone(),
        protocol,
        verified_at: None,
        source: "manual".into(),
    };
    let auth = match route.auth_scheme {
        AuthScheme::Bearer => Some(crate::provider::UpstreamAuthScheme::Bearer),
        AuthScheme::XApiKey => Some(crate::provider::UpstreamAuthScheme::XApiKey),
        AuthScheme::ApiKey => Some(crate::provider::UpstreamAuthScheme::ApiKey),
        AuthScheme::None => None,
    };
    let result =
        crate::custom::probe_connection_with_auth(&config, &config_input, &capability, auth, &key)
            .await;
    let error = result
        .err()
        .map(|e| crate::redaction::redact_known_secret(&e.message, &key));
    drop(key);
    let _settings = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    let current = RoutingSnapshot::load(&state.db.lock()).map_err(V3ApiError::internal)?;
    if current
        .projection
        .destinations
        .iter()
        .find(|row| row.id == id)
        != Some(&destination)
        || credential.as_ref().is_some_and(|before| {
            !current.credentials.iter().any(|after| {
                after.credential_id == before.credential_id
                    && after.credential_version == before.credential_version
                    && after.key_cipher == before.key_cipher
                    && after.ready == before.ready
                    && after.binding_enabled == before.binding_enabled
                    && after.destination_id == before.destination_id
                    && after.authorization_connection_id == before.authorization_connection_id
                    && after.grants == before.grants
                    && after.scope == before.scope
            })
        })
    {
        return Err(V3ApiError::conflict_at(
            &state,
            "connection or Key changed during model test",
        ));
    }
    Ok(Json(DestinationModelTestResult {
        revision: ControlRevision::from_state(&state),
        public_model: model.public_model,
        protocol: input.protocol,
        ok: error.is_none(),
        error,
    }))
}

#[cfg(test)]
mod tests;
