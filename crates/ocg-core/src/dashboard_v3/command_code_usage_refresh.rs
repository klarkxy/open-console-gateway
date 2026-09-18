//! Explicit Command Code GOAT account-usage calibration.
//!
//! This is deliberately separate from the automatic OpenCode Go coordinator:
//! GOAT refresh is manual-only and uses the fixed first-party account endpoint
//! used by Command Code's CLI. Official values become the baseline for the
//! existing locally priced estimator and never affect routing or cooldowns.

use chrono::Utc;

use crate::command_code_usage::{
    COMMAND_CODE_GOAT_USAGE_SOURCE, CommandCodeUsageError, fetch_command_code_usage,
};
use crate::db::{AccountUsageCalibrationSnapshot, AccountUsageSyncSuccessMetadata};
use crate::kernel::pricing::PricingLimits;
use crate::models::AccountSetupStep;
use crate::provider::{
    COMMAND_CODE_GOAT_QUOTA_5H, COMMAND_CODE_GOAT_QUOTA_MONTH, COMMAND_CODE_GOAT_QUOTA_WEEK,
    COMMAND_CODE_PROVIDER_ID,
};
use crate::state::CoreState;
use crate::usage_sync::{MANUAL_THROTTLE, manual_next_allowed_at};

use super::types::{MutationExpectation, UsageRefresh};
use super::usage_refresh::{RefreshApiError, usage_window_from_model};
use super::{V3ApiError, check_expectation};

pub(super) async fn refresh(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
) -> Result<UsageRefresh, RefreshApiError> {
    let _refresh = state
        .provider_usage_refresh
        .try_lock()
        .map_err(|_| V3ApiError::conflict_at(state, "provider usage refresh is already running"))?;
    let now = state.usage_sync.now();
    let (account_snapshot, config, key) = {
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
        let sync = db
            .account_usage_sync_state(id)
            .map_err(V3ApiError::internal)?;
        if let Some(next_allowed_at) =
            manual_next_allowed_at(sync.and_then(|value| value.last_attempt_at), now)
        {
            let retry_after_secs = (next_allowed_at - now).num_seconds().max(1) as u64;
            return Err(RefreshApiError::throttled(
                state,
                next_allowed_at,
                retry_after_secs,
                "official Command Code usage refresh",
            ));
        }
        let key = state
            .decrypt_key(&account.key_cipher)
            .map_err(V3ApiError::internal)?;
        (account, state.config(), key)
    };

    let fetched = fetch_command_code_usage(&config, &key, state.process_generation(), || {
        state.usage_sync.now()
    })
    .await;
    drop(key);
    let snapshot = match fetched {
        Ok(snapshot) => snapshot,
        Err(error) => {
            record_failed_attempt_if_current(
                state,
                id,
                expectation,
                &account_snapshot,
                state.usage_sync.now(),
            )?;
            state.log_runtime_event(
                "warn",
                "usage_sync",
                &format!(
                    "event=command_code_usage_refresh_failed account_id={id} provider={} stage=fetch",
                    account_snapshot.provider_id
                ),
            );
            return Err(map_upstream_error(state, error));
        }
    };

    let now = snapshot.observed_at;
    let next_allowed_at = now + MANUAL_THROTTLE;
    let limits = goat_limits();
    let usage = {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, expectation)?;
        let db = state.db.lock();
        let current = db
            .get_account(id)
            .map_err(V3ApiError::internal)?
            .ok_or_else(|| V3ApiError::not_found(state))?;
        if current.updated_at != account_snapshot.updated_at
            || current.key_cipher != account_snapshot.key_cipher
            || current.provider_id != account_snapshot.provider_id
        {
            return Err(V3ApiError::conflict_at(
                state,
                "the account changed while Command Code usage was being refreshed",
            )
            .into());
        }
        db.commit_official_usage_sync_success(
            id,
            &account_snapshot.key_cipher,
            &AccountUsageCalibrationSnapshot {
                rolling_percent: snapshot.rolling_percent,
                weekly_percent: snapshot.weekly_percent,
                monthly_percent: snapshot.monthly_percent,
                rolling_resets_in_minutes: snapshot.rolling_resets_in_minutes,
                weekly_resets_in_minutes: snapshot.weekly_resets_in_minutes,
            },
            &limits,
            AccountUsageSyncSuccessMetadata {
                now,
                next_eligible_at: next_allowed_at,
                mark_expedited: false,
            },
        )
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| {
            V3ApiError::conflict_at(
                state,
                "the account changed while Command Code usage was being refreshed",
            )
        })?
    };

    state.log_runtime_event(
        "info",
        "usage_sync",
        &format!(
            "event=command_code_usage_refresh_succeeded account_id={id} provider={}",
            account_snapshot.provider_id
        ),
    );
    Ok(UsageRefresh {
        usage: usage_window_from_model(state, usage, None),
        source: COMMAND_CODE_GOAT_USAGE_SOURCE.to_string(),
        last_success_at: now.to_rfc3339(),
        next_allowed_at: next_allowed_at.to_rfc3339(),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

fn record_failed_attempt_if_current(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    account_snapshot: &crate::models::Account,
    now: chrono::DateTime<Utc>,
) -> Result<(), RefreshApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, expectation)?;
    let db = state.db.lock();
    let current = db
        .get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found(state))?;
    if current.updated_at != account_snapshot.updated_at
        || current.key_cipher != account_snapshot.key_cipher
        || current.provider_id != account_snapshot.provider_id
    {
        return Err(V3ApiError::conflict_at(
            state,
            "the account changed while Command Code usage was being refreshed",
        )
        .into());
    }
    db.touch_account_usage_sync_attempt(id, now)
        .map_err(V3ApiError::internal)?;
    Ok(())
}

fn map_upstream_error(state: &CoreState, error: CommandCodeUsageError) -> RefreshApiError {
    match error {
        CommandCodeUsageError::Unauthorized | CommandCodeUsageError::Forbidden => {
            V3ApiError::invalid_request_at(
                state,
                "official Command Code usage rejected this account Key",
            )
            .into()
        }
        other => V3ApiError::outbound_failed(state, other.to_string()).into(),
    }
}

fn goat_limits() -> PricingLimits {
    PricingLimits {
        window_5h: COMMAND_CODE_GOAT_QUOTA_5H,
        window_week: COMMAND_CODE_GOAT_QUOTA_WEEK,
        window_month: COMMAND_CODE_GOAT_QUOTA_MONTH,
    }
}
