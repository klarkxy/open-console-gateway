//! POST `/credentials/{id}/rotate` — replace the Key on an existing Credential.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};

use crate::account_control::{AccountControlError, rotate_upstream_credential_locked};
use crate::dashboard_v3::{
    ControlRevision, MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::state::CoreState;

use super::destinations::{DestinationsError, projection_refused, with_policy_quota};
use super::types::{CredentialRotateRequest, CredentialRotateResult, QuotaRetryResult};
use crate::destination_projection::read_v4_projection;

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
    let saved = rotate_locked(&state, &id, input)?;
    crate::cpa_execution::note_product_apply(&state).await;
    Ok(Json(saved))
}

fn rotate_locked(
    state: &CoreState,
    credential_id: &str,
    input: CredentialRotateRequest,
) -> Result<CredentialRotateResult, V3ApiError> {
    let _settings = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let rotated = rotate_upstream_credential_locked(state, credential_id, &input.secret_input)
        .map_err(|error| map_rotation_error(state, error))?;
    Ok(CredentialRotateResult {
        revision: ControlRevision::from_state(state),
        credential_id: rotated.credential_id,
        version: rotated.version,
        auth_state_version: rotated.auth_state_version,
        replayed: false,
    })
}

fn map_rotation_error(state: &CoreState, error: AccountControlError) -> V3ApiError {
    match error {
        AccountControlError::NotFound => V3ApiError::not_found_at(state, "credential not found"),
        AccountControlError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        AccountControlError::Conflict(message) => V3ApiError::conflict_at(state, message),
        AccountControlError::RevisionConflict => V3ApiError::revision_conflict(state),
        AccountControlError::Unavailable(message) => {
            V3ApiError::precondition_failed_at(state, message)
        }
        AccountControlError::Internal(error) => V3ApiError::internal(error),
    }
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
    let mut db = state.db.lock();
    let projection = read_v4_projection(&db)
        .map_err(V3ApiError::internal)?
        .map_err(|refusals| DestinationsError::Refused(projection_refused(state, &refusals)))?;
    let credential = projection
        .credentials
        .iter()
        .find(|credential| credential.id == credential_id)
        .cloned()
        .ok_or_else(|| V3ApiError::not_found_at(state, "credential not found"))?;
    match crate::cpa_quota::mark_manual_opportunity(&mut db.conn, credential_id, now)
        .map_err(|_| V3ApiError::internal("official quota policy could not be updated"))?
    {
        crate::cpa_quota::ManualOpportunity::NotRestricted => {
            drop(db);
            return Err(V3ApiError::invalid_request_at(
                state,
                "credential has no confirmed quota exhaustion",
            )
            .into());
        }
        opened => {
            let plan = crate::cpa_quota::present_credential(&db.conn, credential_id, now)
                .map_err(|_| V3ApiError::internal("official quota policy could not be read"))?
                .ok_or_else(|| V3ApiError::internal("official quota policy could not be read"))?;
            if matches!(opened, crate::cpa_quota::ManualOpportunity::Opened) {
                state.bump_settings_revision();
            }
            let revision = ControlRevision::from_state(state);
            drop(db);
            return Ok(QuotaRetryResult {
                revision,
                credential: with_policy_quota(&credential, &plan),
            });
        }
    }
}

#[cfg(test)]
mod tests;
