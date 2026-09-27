//! Explicit model discovery for saved configurable HTTP destinations.
//! Reuses the same bounded, no-redirect discovery transport as draft discovery.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
};
use ocg_domain::connection::{ConnectionId, EndpointOperation, endpoint_id_for};
use ocg_domain::destination::{AdapterKind, AuthScheme, CatalogModel, Destination, Protocol};

use super::types::{
    DestinationCatalogModelUpdate, DestinationCatalogRefreshResult, DestinationCatalogUpdate,
    DestinationDto, DestinationModelTestRequest, DestinationModelTestResult,
    DestinationPatchResult,
};
use crate::dashboard_v3::{
    ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::routing_snapshot::{ExecutionCredential, RoutingSnapshot};
use crate::state::CoreState;

pub(super) async fn refresh(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationCatalogRefreshResult>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _refresh = state.provider_models_refresh.try_lock().map_err(|_| {
        V3ApiError::conflict_at(&state, "provider model refresh is already running")
    })?;
    let (destination, credential, input, config, key) = {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        let snapshot = RoutingSnapshot::load(&state.db.lock()).map_err(V3ApiError::internal)?;
        let destination = snapshot
            .projection
            .destinations
            .into_iter()
            .find(|d| d.id == id)
            .ok_or_else(|| V3ApiError::not_found_at(&state, "destination not found"))?;
        if destination.adapter != AdapterKind::Http
            || !destination.capabilities.discoverable_models
            || destination.capabilities.observer
        {
            return Err(V3ApiError::invalid_request_at(
                &state,
                "destination does not support HTTP model discovery",
            ));
        }
        let protocol = destination.protocols.first().copied().ok_or_else(|| {
            V3ApiError::invalid_request_at(&state, "destination has no upstream protocol")
        })?;
        let input = crate::models::AccountCustomConfigInput {
            endpoint_url: destination.base_url.clone().unwrap_or_default(),
            upstream_protocol: protocol,
        };
        let models_url =
            crate::custom::derive_custom_models_endpoint(&input.endpoint_url, protocol)
                .map_err(|error| V3ApiError::invalid_request_at(&state, error.message))?;
        let credential = if destination.auth_scheme == AuthScheme::None {
            None
        } else {
            // Routing switches and cooldowns do not prevent explicit directory reads.
            // Prefer enabled Keys, but never borrow a Key from another destination.
            let mut candidates: Vec<_> = snapshot
                .credentials
                .into_iter()
                .filter(|c| {
                    discovery_credential_allowed(c, &destination, protocol, models_url.as_str())
                })
                .collect();
            candidates.sort_by_key(|c| !c.enabled);
            Some(candidates.into_iter().next().ok_or_else(|| {
                V3ApiError::invalid_request_at(
                    &state,
                    "model discovery requires a ready Key authorized for this destination",
                )
            })?)
        };
        let key = credential
            .as_ref()
            .map(|c| state.decrypt_key(&c.key_cipher))
            .transpose()
            .map_err(V3ApiError::internal)?
            .unwrap_or_default();
        (destination, credential, input, state.config(), key)
    };
    let auth = match destination.auth_scheme {
        AuthScheme::Bearer => Some(crate::provider::UpstreamAuthScheme::Bearer),
        AuthScheme::XApiKey => Some(crate::provider::UpstreamAuthScheme::XApiKey),
        AuthScheme::ApiKey => Some(crate::provider::UpstreamAuthScheme::ApiKey),
        AuthScheme::None => None,
    };
    let (discovered, metadata) =
        crate::custom::discover_models_with_metadata(&config, &input, auth, &key)
            .await
            .map_err(|error| {
                V3ApiError::outbound_failed(
                    &state,
                    crate::redaction::redact_known_secret(&error.message, &key),
                )
            })?;
    // Upstreams are untrusted: never persist or expose an echoed credential as a model id.
    let models: Vec<_> = discovered
        .models
        .into_iter()
        .filter(|model| key.is_empty() || !model.contains(key.as_str()))
        .collect();
    if models.is_empty() {
        return Err(V3ApiError::outbound_failed(
            &state,
            "model discovery returned no usable models; saved catalog retained",
        ));
    }
    drop(key);
    let _settings = state.settings_update.lock();
    check_expectation(&state, &expectation)?;
    let current = RoutingSnapshot::load(&state.db.lock()).map_err(V3ApiError::internal)?;
    if current.projection.destinations.iter().find(|d| d.id == id) != Some(&destination)
        || credential.as_ref().is_some_and(|before| {
            !current.credentials.iter().any(|after| {
                after.credential_id == before.credential_id
                    && after.destination_id == before.destination_id
                    && after.credential_version == before.credential_version
                    && after.key_cipher == before.key_cipher
                    && after.ready == before.ready
                    && after.binding_enabled == before.binding_enabled
                    && after.binding_id == before.binding_id
                    && after.authorization_connection_id == before.authorization_connection_id
                    && after.grants == before.grants
            })
        })
    {
        return Err(V3ApiError::conflict_at(
            &state,
            "destination or credential changed during model discovery",
        ));
    }
    let available: Vec<_> = ocg_domain::destination::http_protocol_routes(&destination)
        .iter()
        .map(|route| route.protocol)
        .collect();
    let catalog = merge_discovered_models(&destination.catalog, &models, &available);
    let added_count = catalog.len() - destination.catalog.len();
    state
        .commit_configuration_update(|db| {
            crate::db::destination_commands::replace_http_catalog_on(
                &db.conn,
                &destination,
                &catalog,
            )?;
            let mut updated = destination.clone();
            updated.catalog = catalog.clone();
            crate::model_metadata::observe(db, &updated, &metadata)
        })
        .map_err(V3ApiError::internal)?;
    let mut updated = destination;
    updated.catalog = catalog;
    Ok(Json(DestinationCatalogRefreshResult {
        revision: ControlRevision::from_state(&state),
        destination: DestinationDto::from(&updated),
        added_count,
        truncated: discovered.truncated,
    }))
}

fn discovery_credential_allowed(
    credential: &ExecutionCredential,
    destination: &Destination,
    protocol: Protocol,
    models_url: &str,
) -> bool {
    if credential.destination_id != destination.id
        || !credential.ready
        || !credential.binding_enabled
        || credential.key_cipher.is_empty()
        || credential.authorization_connection_id.is_empty()
    {
        return false;
    }
    let Ok(connection): Result<ConnectionId, _> = serde_json::from_value(
        serde_json::Value::String(credential.authorization_connection_id.clone()),
    ) else {
        return false;
    };
    let endpoint = endpoint_id_for(&connection, EndpointOperation::from(protocol)).to_string();
    credential.grants.allowed_endpoint_ids.contains(&endpoint)
        && crate::custom_http::ensure_secret_origin_granted(
            models_url,
            &credential.grants.allowed_origins,
        )
        .is_ok()
}

/// Add exact upstream ids without replacing aliases, per-model routes or switches.
/// Omitted ids survive empty/partial inventories. Saved switches survive; new models start on.
fn merge_discovered_models(
    existing: &[CatalogModel],
    discovered: &[String],
    protocols: &[Protocol],
) -> Vec<CatalogModel> {
    let mut catalog = existing.to_vec();
    let mut known: std::collections::HashSet<_> = existing
        .iter()
        .flat_map(|m| {
            [
                m.public_model.to_ascii_lowercase(),
                m.upstream_model.to_ascii_lowercase(),
            ]
        })
        .collect();
    for model in discovered {
        if known.insert(model.to_ascii_lowercase()) {
            catalog.push(CatalogModel {
                public_model: model.clone(),
                upstream_model: model.clone(),
                protocols: protocols.to_vec(),
                preferred: protocols.first().copied(),
                enabled: !protocols.is_empty(),
                upstream_override: None,
            });
        }
    }
    catalog
}

pub(super) async fn update(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<DestinationPatchResult>, super::destinations::DestinationsError> {
    let input = parse_mutation_json::<DestinationCatalogUpdate>(&body)?;
    if input.updates.is_empty() && input.remove_models.is_empty() {
        return Err(V3ApiError::invalid_request_at(&state, "catalog update is empty").into());
    }
    let _settings = state.settings_update.lock();
    check_expectation(&state, &input.expectation)?;
    let snapshot = RoutingSnapshot::load(&state.db.lock()).map_err(V3ApiError::internal)?;
    let destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|row| row.id == id)
        .ok_or_else(|| V3ApiError::not_found_at(&state, "destination not found"))?;
    let catalog = apply_updates(destination, &input.updates, &input.remove_models)
        .map_err(|error| V3ApiError::invalid_request_at(&state, error))?;
    state
        .commit_configuration_update(|db| {
            crate::db::destination_commands::replace_http_catalog_on(
                &db.conn,
                destination,
                &catalog,
            )
        })
        .map_err(V3ApiError::internal)?;
    super::destinations::mutation_result_locked(&state, &id).map(Json)
}

pub(super) fn apply_updates(
    destination: &Destination,
    updates: &[DestinationCatalogModelUpdate],
    remove: &[String],
) -> Result<Vec<CatalogModel>, String> {
    if destination.adapter != AdapterKind::Http || destination.capabilities.observer {
        return Err("only configurable HTTP catalogs are editable here".into());
    }
    if updates.len() + remove.len() > 2000 {
        return Err("catalog update is too large".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut catalog = destination.catalog.clone();
    for update in updates {
        let key = update.public_model.trim().to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err("duplicate catalog model".into());
        }
        let model = catalog
            .iter_mut()
            .find(|row| row.public_model.to_ascii_lowercase() == key)
            .ok_or_else(|| "catalog model not found".to_string())?;
        let available = ocg_domain::destination::http_model_protocols(destination, model);
        if let Some(protocols) = &update.protocols {
            let selected: Vec<Protocol> = protocols.iter().copied().map(Into::into).collect();
            let unique: std::collections::HashSet<_> = selected.iter().collect();
            if unique.len() != selected.len() || selected.iter().any(|p| !available.contains(p)) {
                return Err("model protocols must be distinct configured routes".into());
            }
            model.protocols = selected;
        }
        if let Some(preferred) = update.preferred {
            let preferred = preferred.into();
            if !available.contains(&preferred) {
                return Err("preferred protocol has no configured route".into());
            }
            model.preferred = Some(preferred);
        }
        if let Some(enabled) = update.enabled {
            model.enabled = enabled;
            if enabled && model.protocols.is_empty() && update.protocols.is_none() {
                model.protocols = available.clone();
            }
        }
        if model.enabled && model.protocols.is_empty() {
            return Err("enabled model requires an enabled protocol".into());
        }
        if model.enabled
            && model
                .preferred
                .is_none_or(|p| !model.protocols.contains(&p))
        {
            if update.preferred.is_some() {
                return Err("preferred protocol must be enabled".into());
            }
            model.preferred = model.protocols.first().copied();
        }
    }
    for name in remove {
        let key = name.trim().to_ascii_lowercase();
        if !seen.insert(key.clone()) {
            return Err("duplicate or conflicting catalog model".into());
        }
        let index = catalog
            .iter()
            .position(|row| row.public_model.to_ascii_lowercase() == key)
            .ok_or_else(|| "catalog model not found".to_string())?;
        catalog.remove(index);
    }
    Ok(catalog)
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
