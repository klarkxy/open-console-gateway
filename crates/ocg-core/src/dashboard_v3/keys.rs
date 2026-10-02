//! Access-key lifecycle: create, patch, delete, and rotate.
//!
//! Mutation responses never carry plaintext. `ConnectionInfo` remains the only
//! V3 DTO allowed to expose primary or sub Key values; clients refetch
//! `GET /connection` after a successful acknowledgement.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::Utc;

use crate::gateway_keys::{self, KeyError, PRIMARY_KEY_ID};
use crate::log_types::OperationMetadata;
use crate::state::CoreState;

use super::accounts::DashboardAttempt;
use super::types::{KeyCreate, KeyUpdate, MutationAck, MutationExpectation};
use super::{V3ApiError, check_expectation, parse_mutation_json};

pub(super) async fn regenerate_primary_key(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let mut attempt =
        DashboardAttempt::open(&state, "key.regenerate", "key", Some(PRIMARY_KEY_ID.into()));
    let result = parse_mutation_json::<MutationExpectation>(&body)
        .and_then(|expectation| regenerate_primary_locked(&state, &expectation, &mut attempt));
    attempt.finish(result).map(Json)
}

pub(super) async fn create_key(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let mut attempt = DashboardAttempt::open(&state, "key.create", "key", None);
    let result = parse_mutation_json::<KeyCreate>(&body)
        .and_then(|input| create_key_locked(&state, input, &mut attempt));
    attempt.finish(result).map(Json)
}

pub(super) async fn update_key(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let mut attempt = DashboardAttempt::open(&state, "key.update", "key", Some(id.clone()));
    let result = parse_mutation_json::<KeyUpdate>(&body)
        .and_then(|input| update_key_locked(&state, &id, input, &mut attempt));
    attempt.finish(result).map(Json)
}

pub(super) async fn delete_key(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let mut attempt = DashboardAttempt::open(&state, "key.delete", "key", Some(id.clone()));
    let result = parse_mutation_json::<MutationExpectation>(&body)
        .and_then(|expectation| delete_key_locked(&state, &id, &expectation, &mut attempt));
    attempt.finish(result).map(Json)
}

pub(super) async fn regenerate_key(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<MutationAck>, V3ApiError> {
    let mut attempt = DashboardAttempt::open(&state, "key.regenerate", "key", Some(id.clone()));
    let result = parse_mutation_json::<MutationExpectation>(&body)
        .and_then(|expectation| regenerate_key_locked(&state, &id, &expectation, &mut attempt));
    attempt.finish(result).map(Json)
}

fn regenerate_primary_locked(
    state: &CoreState,
    expectation: &MutationExpectation,
    attempt: &mut DashboardAttempt,
) -> Result<MutationAck, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)?;
    let new_value = {
        let db = state.db.lock();
        gateway_keys::generate_primary_value(&db, &state.config().gateway_key)
            .map_err(|error| key_error(state, error))?
    };
    let mut config = state.config();
    config.gateway_key = new_value;
    // `set_config` bumps the shared revision once and resets sticky routing
    // because the previous primary value stops authenticating. The receipt
    // records that bump only after this call's own config row commits.
    if let Err(error) = state.set_config_recorded(config, |revision| {
        attempt.note_own_commit(revision);
    }) {
        return Err(V3ApiError::internal(error));
    }
    let ack = ack(state);
    attempt.note_success(OperationMetadata {
        revision: Some(ack.revision),
        ..OperationMetadata::default()
    });
    Ok(ack)
}

fn create_key_locked(
    state: &CoreState,
    input: KeyCreate,
    attempt: &mut DashboardAttempt,
) -> Result<MutationAck, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let key = gateway_keys::create_sub_key(state, &input.name)
        .map_err(|error| key_error(state, error))?;
    attempt.subject(key.id);
    state.bump_settings_revision();
    Ok(ack(state))
}

fn update_key_locked(
    state: &CoreState,
    id: &str,
    input: KeyUpdate,
    attempt: &mut DashboardAttempt,
) -> Result<MutationAck, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    // Unknown (or soft-deleted, or primary) ids are rejected up front, even
    // for empty-body patches.
    if !state
        .db
        .lock()
        .get_sub_gateway_key(id)
        .map_err(V3ApiError::internal)?
        .is_some_and(|key| key.is_active())
    {
        return Err(V3ApiError::invalid_request_at(state, "key not found"));
    }
    let mut reset_routing = false;
    let mut mutated = false;
    let mut changed_fields = Vec::new();
    if let Some(name) = input.name.as_deref() {
        // No-op renames (same trimmed name) neither audit nor bump, matching
        // the no-op-toggle handling below.
        let current_name = state
            .db
            .lock()
            .get_sub_gateway_key(id)
            .map_err(V3ApiError::internal)?
            .map(|key| key.name)
            .unwrap_or_else(|| id.to_string());
        if current_name != name.trim() {
            gateway_keys::rename_sub_key(state, id, name)
                .map_err(|error| key_error(state, error))?;
            mutated = true;
            changed_fields.push("name".to_string());
        }
    }
    if let Some(enabled) = input.enabled {
        let current = state
            .db
            .lock()
            .get_sub_gateway_key(id)
            .map_err(V3ApiError::internal)?
            .filter(|key| key.is_active())
            .ok_or_else(|| V3ApiError::invalid_request_at(state, "key not found"))?;
        // No-op toggles (already in the target state) neither audit nor
        // reset routing: nothing about the authenticating credential set
        // changed.
        if current.enabled != enabled {
            // The endpoint drives the explicit routing reset for revocations:
            // disabling a sub key invalidates credentials its sticky
            // sessions were pinned to. Renames, creates, and enables never
            // reset.
            if let Err(error) = gateway_keys::set_sub_key_enabled(state, id, enabled) {
                // A committed rename in the same request already changed
                // state: bump before failing so the revision never lags a
                // committed mutation. The receipt stays partial.
                if mutated {
                    state.bump_settings_revision();
                    attempt.note_partial(OperationMetadata {
                        changed_fields,
                        requested_count: Some(2),
                        completed_count: Some(1),
                        failed_count: Some(1),
                        revision: Some(state.settings_revision()),
                        ..OperationMetadata::default()
                    });
                }
                return Err(key_error(state, error));
            }
            reset_routing = !enabled;
            mutated = true;
            changed_fields.push("enabled".to_string());
        }
    }
    if reset_routing {
        state.routing.reset();
    }
    if mutated {
        state.bump_settings_revision();
    }
    let ack = ack(state);
    attempt.note_success(OperationMetadata {
        changed_fields,
        revision: Some(ack.revision),
        ..OperationMetadata::default()
    });
    Ok(ack)
}

fn delete_key_locked(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    _attempt: &mut DashboardAttempt,
) -> Result<MutationAck, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)?;
    let existing = state
        .db
        .lock()
        .get_sub_gateway_key(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "key not found"))?;
    let was_authenticating = existing.authenticates();
    gateway_keys::delete_sub_key(state, id, Utc::now()).map_err(|error| key_error(state, error))?;
    // A disabled key's value never authenticated; its removal changes no
    // live sticky sessions.
    if was_authenticating {
        state.routing.reset();
    }
    state.bump_settings_revision();
    Ok(ack(state))
}

fn regenerate_key_locked(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    _attempt: &mut DashboardAttempt,
) -> Result<MutationAck, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)?;
    let updated =
        gateway_keys::regenerate_sub_key(state, id).map_err(|error| key_error(state, error))?;
    // Only an authenticating key's rotation invalidates live sessions; a
    // disabled key's fresh value never entered the snapshot.
    if updated.authenticates() {
        state.routing.reset();
    }
    state.bump_settings_revision();
    Ok(ack(state))
}

fn ack(state: &CoreState) -> MutationAck {
    MutationAck {
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    }
}

fn key_error(state: &CoreState, error: KeyError) -> V3ApiError {
    match error {
        KeyError::BadRequest(message) => V3ApiError::invalid_request_at(state, message),
        KeyError::Internal(message) => V3ApiError::internal(message),
    }
}

#[cfg(test)]
mod tests;
