//! Explicit model discovery for saved configurable HTTP destinations.
//! Reuses the same bounded, no-redirect discovery transport as draft discovery.

use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
};
use ocg_domain::destination::{
    AdapterKind, AuthScheme, CatalogModel, Destination, HttpProtocolRoute, Protocol,
};

use super::types::{
    DestinationCatalogModelUpdate, DestinationCatalogRefreshResult, DestinationCatalogUpdate,
    DestinationDto, DestinationModelTestRequest, DestinationModelTestResult,
    DestinationPatchResult,
};
use crate::account_control::{CatalogRefreshError, MutationCas};
use crate::dashboard_v3::{
    ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::models::{AccountSetupStep, AccountType};
use crate::routing_snapshot::{ExecutionCredential, RoutingSnapshot};
use crate::state::CoreState;
use std::time::Duration;

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
    let saved = finish_catalog_refresh(&state, &id, cas, prepared, &models, &metadata, truncated)?;
    drop(_refresh);
    crate::cpa_execution::note_product_apply(&state).await;
    Ok(Json(saved))
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
    let receipt = {
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
        super::destinations::ensure_quota_presentation(&state)?;
        crate::account_control::update_http_catalog_locked(
            &state,
            &id,
            &edits,
            &input.remove_models,
        )
        .map_err(|error| map_catalog_update_error(&state, error))?;
        super::destinations::mutation_result_locked(&state, &id)
    };
    crate::cpa_execution::note_product_apply(&state).await;
    receipt.map(Json)
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
    let selected = {
        let _settings = state.settings_update.lock();
        prepare_destination_model_test(
            &state,
            &id,
            &input.expectation,
            input.public_model.trim(),
            protocol,
        )?
    };
    #[cfg(test)]
    assert_product_locks_released(&state);
    if let Err(error) = crate::cpa_execution::schedule_owned_apply(&state).await {
        return Err(V3ApiError::precondition_failed_at(
            &state,
            format!("owned apply is unavailable: {error}"),
        ));
    }
    let response = match crate::protocol_probe::send_validated_generation(
        &state,
        &crate::protocol_probe::ValidatedGeneration {
            credential_id: selected.credential.credential_id.clone(),
            credential_version: selected.credential.credential_version,
            binding_id: selected.credential.binding_id.clone(),
            public_model: selected.public_model.clone(),
            protocol,
        },
        selected.timeout,
    )
    .await
    {
        Ok(response) => response,
        Err(crate::protocol_probe::ValidatedSendError::NotSent(message)) => {
            return Err(V3ApiError::precondition_failed_at(
                &state,
                redact_model_test_detail(&state, &selected.credential.key_cipher, &message),
            ));
        }
    };
    let shaped =
        crate::protocol_probe::protocol_shaped_success(response.status, &response.body, protocol);
    let _settings = state.settings_update.lock();
    recheck_model_test_fence(&state, &id, &input.expectation, &selected)?;
    let error = shaped
        .err()
        .map(|message| redact_model_test_detail(&state, &selected.credential.key_cipher, &message));
    Ok(Json(DestinationModelTestResult {
        revision: ControlRevision::from_state(&state),
        public_model: selected.public_model,
        protocol: input.protocol,
        ok: error.is_none(),
        error,
    }))
}

struct DestinationModelTestSelection {
    destination: Destination,
    credential: ExecutionCredential,
    public_model: String,
    timeout: Duration,
}

fn prepare_destination_model_test(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    public_model: &str,
    protocol: Protocol,
) -> Result<DestinationModelTestSelection, V3ApiError> {
    check_expectation(state, expectation)?;
    let db = state.db.lock();
    let snapshot = RoutingSnapshot::load(&db).map_err(V3ApiError::internal)?;
    let destination = snapshot
        .projection
        .destinations
        .iter()
        .find(|row| row.id == id)
        .cloned()
        .ok_or_else(|| V3ApiError::not_found_at(state, "destination not found"))?;
    if destination.adapter != AdapterKind::Http || destination.capabilities.observer {
        return Err(V3ApiError::invalid_request_at(
            state,
            "destination is not a configurable HTTP connection",
        ));
    }
    if !destination.enabled {
        return Err(V3ApiError::precondition_failed_at(
            state,
            "destination is disabled",
        ));
    }
    let draft: i64 = db
        .conn
        .query_row(
            "SELECT COALESCE(onboarding_draft, 0) FROM destinations WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .map_err(V3ApiError::internal)?;
    if draft != 0 {
        return Err(V3ApiError::precondition_failed_at(
            state,
            "save the destination before testing a model",
        ));
    }
    let model = destination
        .catalog
        .iter()
        .find(|row| row.public_model.eq_ignore_ascii_case(public_model))
        .cloned()
        .ok_or_else(|| V3ApiError::not_found_at(state, "model not found"))?;
    let route = ocg_domain::destination::http_model_route(&destination, &model, protocol)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "model protocol is not configured"))?;
    let format = match protocol {
        Protocol::ChatCompletions => crate::gateway::protocol::ApiFormat::ChatCompletions,
        Protocol::Responses => crate::gateway::protocol::ApiFormat::Responses,
        Protocol::Messages => crate::gateway::protocol::ApiFormat::Messages,
    };
    let mut eligible = Vec::new();
    for credential in &snapshot.credentials {
        if !persisted_route_grant(credential, &destination, &model, &route, format) {
            continue;
        }
        let admission = credential_admission(&db, &credential.id)?;
        if admits_model_test(credential, &admission, draft != 0) {
            eligible.push(credential.clone());
        }
    }
    eligible.sort_by(|left, right| {
        right
            .ready
            .cmp(&left.ready)
            .then_with(|| left.auth_error.is_some().cmp(&right.auth_error.is_some()))
            .then_with(|| left.credential_id.cmp(&right.credential_id))
    });
    let Some(credential) = eligible.into_iter().next() else {
        let message = if route.auth_scheme == AuthScheme::None {
            "save an enabled keyless credential binding for this destination, then use its model test"
        } else {
            "save an enabled credential authorized for this model, endpoint, and protocol, then use its model test"
        };
        return Err(V3ApiError::precondition_failed_at(state, message));
    };
    drop(db);
    let timeout = Duration::from_secs(state.config().non_stream_timeout_secs.min(30));
    Ok(DestinationModelTestSelection {
        destination,
        credential,
        public_model: model.public_model,
        timeout,
    })
}

fn persisted_route_grant(
    credential: &ExecutionCredential,
    destination: &Destination,
    model: &CatalogModel,
    route: &HttpProtocolRoute,
    format: crate::gateway::protocol::ApiFormat,
) -> bool {
    if credential.destination_id != destination.id
        || credential.credential_id.trim().is_empty()
        || credential.credential_version == 0
        || credential.binding_id.trim().is_empty()
        || !credential.binding_enabled
    {
        return false;
    }
    // Empty ciphertext is the persisted AuthScheme::None shape. Whether the
    // validated pin admits that shape is ValidationRow.material_present in the
    // route projection. This caller does not invent a key and still sends
    // through send_validated_generation.
    let keyless = credential.key_cipher.is_empty();
    if (route.auth_scheme == AuthScheme::None) != keyless {
        return false;
    }
    let Ok(endpoint) =
        crate::gateway::materialize::endpoint_id_for_target(credential, destination, model, format)
    else {
        return false;
    };
    if !credential.grants.allowed_endpoint_ids.contains(&endpoint) {
        return false;
    }
    if crate::custom_http::ensure_secret_origin_granted(
        &route.endpoint_url,
        &credential.grants.allowed_origins,
    )
    .is_err()
    {
        return false;
    }
    crate::gateway::materialize::binding_allows_requested_model(
        &credential.scope,
        &model.public_model,
        &model.public_model,
        [&model.public_model, &model.upstream_model],
    )
}

struct CredentialAdmission {
    account_type: String,
    step: AccountSetupStep,
}

fn credential_admission(
    db: &crate::db::Database,
    legacy_account_id: &str,
) -> Result<CredentialAdmission, V3ApiError> {
    let (account_type, step): (String, String) = db
        .conn
        .query_row(
            "SELECT COALESCE(account_type, 'key'), setup_step FROM credentials
             WHERE legacy_account_id = ?1
               AND COALESCE(credential_purpose, 'inference') = 'inference'",
            [legacy_account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(V3ApiError::internal)?;
    Ok(CredentialAdmission {
        account_type,
        step: AccountSetupStep::try_from(step.as_str()).map_err(V3ApiError::internal)?,
    })
}

/// Enabled Ready rows use the model test. The only disabled row is the
/// accepted managed `key_verification` candidate: binding already matched,
/// destination is not a draft, and the staged ciphertext is nonempty.
/// Disabled Ready, revoked, incomplete, and enabled ordinary
/// `key_verification` rows are not candidates.
fn admits_model_test(
    credential: &ExecutionCredential,
    admission: &CredentialAdmission,
    destination_draft: bool,
) -> bool {
    if destination_draft {
        return false;
    }
    if credential.enabled && credential.ready {
        return true;
    }
    !credential.enabled
        && admission.account_type == AccountType::Managed.as_str()
        && admission.step == AccountSetupStep::KeyVerification
        && !credential.key_cipher.trim().is_empty()
}

fn recheck_model_test_fence(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    selected: &DestinationModelTestSelection,
) -> Result<(), V3ApiError> {
    check_expectation(state, expectation)?;
    let current = RoutingSnapshot::load(&state.db.lock()).map_err(V3ApiError::internal)?;
    let before = &selected.credential;
    if current
        .projection
        .destinations
        .iter()
        .find(|row| row.id == id)
        != Some(&selected.destination)
        || !current.credentials.iter().any(|after| {
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
    {
        return Err(V3ApiError::conflict_at(
            state,
            "connection or Key changed during model test",
        ));
    }
    Ok(())
}

fn redact_model_test_detail(state: &CoreState, key_cipher: &str, message: &str) -> String {
    if key_cipher.is_empty() {
        return message.to_string();
    }
    match state.decrypt_key(key_cipher) {
        Ok(secret) if !secret.is_empty() => crate::redaction::redact_known_secret(message, &secret),
        _ => message.to_string(),
    }
}

#[cfg(test)]
fn assert_product_locks_released(state: &CoreState) {
    assert!(
        state.settings_update.try_lock().is_some(),
        "settings revision lock held across await"
    );
    assert!(
        state.db.try_lock().is_some(),
        "database lock held across await"
    );
}

#[cfg(test)]
mod tests;
