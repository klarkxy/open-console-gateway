//! POST `/credentials/{id}/rotate` — replace the Key on an existing Credential.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use ocg_domain::catalog::CredentialKind;
use ocg_domain::credential::observer_credential_id_for_platform_account;

use crate::dashboard_v3::dynamic_providers::first_account_key;
use crate::dashboard_v3::{
    ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::provider::{CPA_ACCOUNT_ID, builtin_provider, validate_plan_key};
use crate::state::CoreState;

use super::destinations::{DestinationsError, overlay_one_credential_dto, projection_refused};
use super::types::{CredentialRotateRequest, CredentialRotateResult, QuotaRetryResult};
use crate::destination_projection::read_v4_projection;
use crate::quota_recovery::QuotaEpisode;

/// Rotate the Key on an existing Credential.
///
/// CAS-only: there is no `operationId` in this stage. A lost-response retry
/// with the same CAS tokens returns `409` `revisionConflict`; the client
/// refreshes control tokens and the credential, then submits again.
pub(super) async fn rotate(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<CredentialRotateResult>, V3ApiError> {
    let input = parse_mutation_json::<CredentialRotateRequest>(&body)?;
    rotate_locked(&state, &id, input).map(Json)
}

fn rotate_locked(
    state: &CoreState,
    credential_id: &str,
    input: CredentialRotateRequest,
) -> Result<CredentialRotateResult, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;

    let secret = input.secret_input.trim();
    if secret.is_empty() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "secretInput is required",
        ));
    }

    let snapshot = {
        let db = state.db.lock();
        db.list_identity_model().map_err(V3ApiError::internal)?
    };
    if snapshot.platform_parents.iter().any(|parent| {
        observer_credential_id_for_platform_account(&parent.platform_id).as_str() == credential_id
    }) {
        return Err(V3ApiError::invalid_request_at(
            state,
            "platform observer credentials cannot be rotated here",
        ));
    }
    let record = snapshot
        .accounts
        .iter()
        .find(|record| record.credential_id == credential_id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "credential not found"))?;
    let account = &record.account;
    if account.id == CPA_ACCOUNT_ID {
        return Err(V3ApiError::invalid_request_at(
            state,
            "CPA Subscription Pool settings must use the external-integration endpoint",
        ));
    }
    if account.is_zen_free() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "Zen Free settings must use the dedicated provider-settings endpoint",
        ));
    }
    if account.credential_kind == CredentialKind::None {
        return Err(V3ApiError::invalid_request_at(
            state,
            "anonymous and no-auth credentials cannot be rotated",
        ));
    }

    let key_cipher = match state
        .dynamic_providers()
        .iter()
        .find(|runtime| runtime.id == account.provider_id)
    {
        Some(runtime) => first_account_key(state, runtime.auth_kind, Some(secret))?,
        None => {
            if let Some(plan) = builtin_provider(&account.provider_id) {
                validate_plan_key(plan, secret)
                    .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?;
            }
            state.encrypt_key(secret).map_err(V3ApiError::internal)?
        }
    };

    let rotated = {
        let db = state.db.lock();
        db.rotate_account_credential(&account.id, &key_cipher)
            .map_err(|error| V3ApiError::invalid_request_at(state, error.to_string()))?
    };
    state.bump_settings_revision();
    Ok(CredentialRotateResult {
        revision: ControlRevision::from_state(state),
        credential_id: rotated.credential_id,
        version: rotated.version,
        auth_state_version: rotated.auth_state_version,
        replayed: false,
    })
}

/// Permit one next normal selection for a confirmed exhausted Key.
///
/// Does not send, enable, or clear backoff. Idempotent while already ready
/// or probing.
pub(super) async fn quota_retry(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<QuotaRetryResult>, DestinationsError> {
    let input = parse_mutation_json::<MutationExpectation>(&body)?;
    quota_retry_locked(&state, &id, input).map(Json)
}

fn quota_retry_locked(
    state: &CoreState,
    credential_id: &str,
    expectation: MutationExpectation,
) -> Result<QuotaRetryResult, DestinationsError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &expectation)?;
    let now = state.sample_gateway_clock().0;
    let (projection, recovery, probing, revision) = {
        let db = state.db.lock();
        let projection = read_v4_projection(&db)
            .map_err(V3ApiError::internal)?
            .map_err(|refusals| DestinationsError::Refused(projection_refused(state, &refusals)))?;
        let account_id = projection
            .credentials
            .iter()
            .find(|credential| credential.id == credential_id)
            .ok_or_else(|| V3ApiError::not_found_at(state, "credential not found"))?
            .legacy_account_id
            .clone();
        let loaded = crate::db::quota_recovery::load_for_legacy_on(&db.conn, &account_id)
            .map_err(V3ApiError::internal)?;
        let Some((id, version, key_cipher, Some(recovery))) = loaded else {
            return Err(V3ApiError::invalid_request_at(
                state,
                "credential has no confirmed quota exhaustion",
            )
            .into());
        };
        let probing = state.is_quota_probing(&id, version, &key_cipher, recovery.epoch);
        let due = recovery.due_at(now);
        let episode = QuotaEpisode {
            credential_id: id,
            account_id,
            credential_version: version,
            epoch: recovery.epoch,
            key_cipher,
        };
        if probing || due {
            (
                projection,
                recovery,
                probing,
                ControlRevision::from_state(state),
            )
        } else {
            let mut updated = recovery.clone();
            updated.next_retry_at = now;
            let saved = crate::db::quota_recovery::save_on(&db.conn, &episode, &updated)
                .map_err(V3ApiError::internal)?;
            if !saved {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "credential has no confirmed quota exhaustion",
                )
                .into());
            }
            state.bump_settings_revision();
            (
                projection,
                updated,
                probing,
                ControlRevision::from_state(state),
            )
        }
    };
    let credential = projection
        .credentials
        .iter()
        .find(|credential| credential.id == credential_id)
        .expect("credential existed under settings lock");
    Ok(QuotaRetryResult {
        revision,
        credential: overlay_one_credential_dto(state, credential, Some(&recovery), probing),
    })
}

#[cfg(test)]
mod tests;
