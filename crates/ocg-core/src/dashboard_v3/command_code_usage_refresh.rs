//! Explicit Command Code GOAT account-usage calibration.
//!
//! Manual refresh keeps the control-plane revision check and then joins the
//! shared GOAT/CN calibration. Official values become the baseline for the
//! locally priced estimator and never affect routing or cooldowns.

use crate::command_code_usage::COMMAND_CODE_GOAT_USAGE_SOURCE;
use crate::models::AccountSetupStep;
use crate::provider::COMMAND_CODE_PROVIDER_ID;
use crate::state::CoreState;
use crate::usage_sync::{CalibrationOutcome, ControlRevision, MANUAL_THROTTLE, refresh_coalesced};

use super::types::{MutationExpectation, UsageRefresh};
use super::usage_refresh::{RefreshApiError, usage_window_from_model};
use super::{V3ApiError, check_expectation};

pub(super) async fn refresh(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
) -> Result<UsageRefresh, RefreshApiError> {
    {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, expectation)?;
        let db = state.db.lock();
        let account = db
            .get_account(id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| V3ApiError::not_found(state))?;
        if account.provider_id != COMMAND_CODE_PROVIDER_ID {
            return Err(V3ApiError::invalid_request_at(
                state,
                "official Command Code usage refresh is unavailable for this provider offering",
            )
            .into());
        }
        if account.setup_step != AccountSetupStep::Ready {
            return Err(V3ApiError::invalid_request_at(
                state,
                "official Command Code usage refresh requires a ready account",
            )
            .into());
        }
        if account.key_cipher.trim().is_empty() {
            return Err(V3ApiError::invalid_request_at(
                state,
                "the selected account has no stored Key",
            )
            .into());
        }
    }
    let outcome = refresh_coalesced(
        state,
        id,
        Some(ControlRevision {
            revision: expectation.expected_revision,
            process_generation: expectation.process_generation,
        }),
    )
    .await;
    match outcome {
        CalibrationOutcome::Applied => {}
        CalibrationOutcome::Throttled {
            next_allowed_at,
            retry_after_secs,
        } => {
            return Err(RefreshApiError::throttled(
                state,
                next_allowed_at,
                retry_after_secs,
                "official Command Code usage refresh",
            ));
        }
        CalibrationOutcome::RejectedKey => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "official Command Code usage rejected this account Key",
            )
            .into());
        }
        CalibrationOutcome::FetchFailed(message) => {
            state.log_runtime_event(
                "warn",
                "usage_sync",
                &format!("event=command_code_usage_refresh_failed account_id={id} stage=fetch"),
            );
            return Err(V3ApiError::outbound_failed(state, message).into());
        }
        CalibrationOutcome::Stale => {
            return Err(V3ApiError::conflict_at(
                state,
                "the account changed while Command Code usage was being refreshed",
            )
            .into());
        }
        CalibrationOutcome::Skipped => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "official Command Code usage refresh is unavailable for this provider offering",
            )
            .into());
        }
    }
    let refresh = {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, expectation)?;
        let db = state.db.lock();
        let usage = db
            .account_usage_with_limits(id, &crate::command_code_usage::goat_quota_limits())
            .map_err(V3ApiError::internal)?;
        let sync = db
            .account_usage_sync_state(id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| {
                V3ApiError::conflict_at(
                    state,
                    "the account changed while Command Code usage was being refreshed",
                )
            })?;
        let now = sync.last_success_at.ok_or_else(|| {
            V3ApiError::conflict_at(
                state,
                "the account changed while Command Code usage was being refreshed",
            )
        })?;
        let next_allowed_at = sync
            .next_eligible_at
            .unwrap_or_else(|| now + MANUAL_THROTTLE);
        UsageRefresh {
            usage: usage_window_from_model(state, usage, None),
            source: COMMAND_CODE_GOAT_USAGE_SOURCE.to_string(),
            last_success_at: now.to_rfc3339(),
            next_allowed_at: next_allowed_at.to_rfc3339(),
            revision: state.settings_revision(),
            process_generation: state.process_generation(),
        }
    };
    state.log_runtime_event(
        "info",
        "usage_sync",
        &format!("event=command_code_usage_refresh_succeeded account_id={id}"),
    );
    Ok(refresh)
}
