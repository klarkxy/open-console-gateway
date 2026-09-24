//! Immediate official usage refresh after a real upstream 429.
//!
//! Inference never waits on this path. Go remains the only automatic
//! quota-authoritative contract; GOAT/CN calibration must not become a
//! routing hard limit.

use super::provider_adapter::supports_reactive_usage_refresh;
use super::provider_refresh::{CalibrationOutcome, ControlRevision};
use super::{
    MANUAL_THROTTLE, UsageSyncHost, UsageSyncTrigger, manual_next_allowed_at,
    refresh_official_usage,
};
use crate::command_code_usage::fetch_command_code_usage;
use crate::db::{AccountUsageCalibrationSnapshot, AccountUsageSyncSuccessMetadata};
use crate::go_usage::{GoUsageSnapshot, GoUsageWindowStatus};
use crate::models::AccountSetupStep;
use crate::provider::ProviderAdapterKind;
use crate::state::CoreState;
use ocg_gateway::quota::{QuotaEvidence, QuotaReason, QuotaWindowKind};

/// Official Go `status=rate-limited` is exhaustion. Percent-only rows are not.
pub(crate) fn official_go_quota_evidence(
    snapshot: &GoUsageSnapshot,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> Vec<QuotaEvidence> {
    [
        (
            snapshot.rolling_status,
            snapshot.rolling_resets_in_minutes,
            QuotaWindowKind::FiveHours,
        ),
        (
            snapshot.weekly_status,
            snapshot.weekly_resets_in_minutes,
            QuotaWindowKind::Week,
        ),
        (
            snapshot.monthly_status,
            snapshot.monthly_resets_in_minutes,
            QuotaWindowKind::Month,
        ),
    ]
    .into_iter()
    .filter(|(status, _, _)| *status == GoUsageWindowStatus::RateLimited)
    .map(|(_, minutes, window)| QuotaEvidence {
        reason: QuotaReason::QuotaExhausted,
        window,
        resets_at_rfc3339: chrono::Duration::try_minutes(minutes.max(1))
            .and_then(|delay| observed_at.checked_add_signed(delay))
            .map(|at| at.to_rfc3339()),
        resets_in_text: None,
    })
    .collect()
}

/// Fire-and-forget official refresh so fallback is not delayed. Go keeps its
/// inflight map. GOAT/CN join the shared provider calibration for the same
/// credential version. Does not create a scheduler.
pub fn spawn_reactive_usage_refresh(state: &CoreState, account_id: &str) {
    if !state.usage_sync.reactive_refresh_enabled() {
        return;
    }
    let account = match state.with_sync_store(|store| store.get_account(account_id)) {
        Ok(Some(account)) => account,
        _ => return,
    };
    if !supports_reactive_usage_refresh(&account.provider_id) {
        return;
    }
    if account.setup_step != AccountSetupStep::Ready || account.key_cipher.is_empty() {
        return;
    }
    let state = state.clone();
    let account_id = account_id.to_string();
    let provider_id = account.provider_id;
    tokio::spawn(async move {
        match ProviderAdapterKind::from_provider_id(&provider_id) {
            Some(ProviderAdapterKind::OpenCodeGo) => {
                let _ = refresh_official_usage(&state, &account_id, UsageSyncTrigger::Inference429)
                    .await;
            }
            Some(ProviderAdapterKind::CommandCodeGoat) => {
                let _ = calibrate_provider_if_current(&state, &account_id).await;
            }
            Some(ProviderAdapterKind::MiniMaxCn | ProviderAdapterKind::KimiCn) => {
                let _ = calibrate_provider_if_current(&state, &account_id).await;
            }
            _ => {}
        }
    });
}

/// Same credential version joins one in-flight calibration. A manual caller
/// passes its control-plane revision; reactive refresh does not.
pub(crate) async fn refresh_coalesced(
    state: &CoreState,
    account_id: &str,
    control: Option<ControlRevision>,
) -> CalibrationOutcome {
    let Some(key) = usage_coalesce_key(state, account_id) else {
        return CalibrationOutcome::Skipped;
    };
    let state_for_work = state.clone();
    let account_id = account_id.to_string();
    state
        .provider_usage_refresh
        .run(key, move || {
            let state_for_work = state_for_work.clone();
            async move { calibrate_provider_usage(&state_for_work, &account_id, control).await }
        })
        .await
}

pub(crate) async fn calibrate_provider_if_current(state: &CoreState, account_id: &str) -> bool {
    matches!(
        refresh_coalesced(state, account_id, None).await,
        CalibrationOutcome::Applied
    )
}

fn usage_coalesce_key(state: &CoreState, account_id: &str) -> Option<String> {
    let db = state.db.lock();
    let identity = super::UsageRefreshIdentity::capture(&db, account_id)
        .ok()
        .flatten()?;
    Some(format!(
        "usage:{}:{}",
        identity.credential.credential_id, identity.credential.credential_version
    ))
}

async fn calibrate_provider_usage(
    state: &CoreState,
    account_id: &str,
    control: Option<ControlRevision>,
) -> CalibrationOutcome {
    let now = state.usage_sync.now();
    let (identity, adapter, config, key, revision) = {
        let _settings = state.settings_update.lock();
        let db = state.db.lock();
        let Ok(Some(account)) = db.get_account(account_id) else {
            return CalibrationOutcome::Skipped;
        };
        let Some(adapter) = ProviderAdapterKind::from_provider_id(&account.provider_id) else {
            return CalibrationOutcome::Skipped;
        };
        if !matches!(
            adapter,
            ProviderAdapterKind::CommandCodeGoat
                | ProviderAdapterKind::MiniMaxCn
                | ProviderAdapterKind::KimiCn
        ) || account.setup_step != AccountSetupStep::Ready
            || account.key_cipher.is_empty()
        {
            return CalibrationOutcome::Skipped;
        }
        let sync = match db.account_usage_sync_state(account_id) {
            Ok(sync) => sync,
            Err(_) => return CalibrationOutcome::Skipped,
        };
        if let Some(next_allowed_at) =
            manual_next_allowed_at(sync.and_then(|row| row.last_attempt_at), now)
        {
            let retry_after_secs = (next_allowed_at - now).num_seconds().max(1) as u64;
            return CalibrationOutcome::Throttled {
                next_allowed_at,
                retry_after_secs,
            };
        }
        let Ok(Some(identity)) = super::UsageRefreshIdentity::capture(&db, account_id) else {
            return CalibrationOutcome::Skipped;
        };
        let Ok(key) = state.decrypt_key(&account.key_cipher) else {
            return CalibrationOutcome::Skipped;
        };
        // Record the attempt before I/O: success and failure share throttling,
        // and an old response never writes metadata onto a rotated account.
        if db
            .touch_account_usage_sync_attempt(account_id, now)
            .is_err()
        {
            return CalibrationOutcome::Skipped;
        }
        (
            identity,
            adapter,
            state.config(),
            key,
            state.settings_revision(),
        )
    };
    enum Snapshot {
        Goat(crate::command_code_usage::CommandCodeUsageSnapshot),
        Plan(Vec<crate::models::QuotaWindow>),
    }
    let fetched = match adapter {
        ProviderAdapterKind::CommandCodeGoat => {
            match fetch_command_code_usage(&config, &key, state.process_generation(), || {
                state.usage_sync.now()
            })
            .await
            {
                Ok(usage) => Ok(Snapshot::Goat(usage)),
                Err(
                    crate::command_code_usage::CommandCodeUsageError::Unauthorized
                    | crate::command_code_usage::CommandCodeUsageError::Forbidden,
                ) => {
                    drop(key);
                    return CalibrationOutcome::RejectedKey;
                }
                Err(error) => Err(error.to_string()),
            }
        }
        _ => {
            crate::plan_usage::fetch(
                &config,
                adapter,
                account_id,
                &key,
                state.process_generation(),
            )
        }
        .await
        .map(Snapshot::Plan),
    };
    drop(key);
    let snapshot = match fetched {
        Ok(snapshot) => snapshot,
        Err(message) => return CalibrationOutcome::FetchFailed(message),
    };
    let _settings = state.settings_update.lock();
    if state.settings_revision() != revision {
        return CalibrationOutcome::Stale;
    }
    if let Some(control) = control
        && (state.settings_revision() != control.revision
            || state.process_generation() != control.process_generation)
    {
        return CalibrationOutcome::Stale;
    }
    let db = state.db.lock();
    if !identity.is_current(&db).unwrap_or(false) {
        return CalibrationOutcome::Stale;
    }
    let applied = match snapshot {
        Snapshot::Goat(usage) => db
            .commit_official_usage_sync_success(
                account_id,
                &identity.credential.key_cipher,
                &AccountUsageCalibrationSnapshot {
                    rolling_percent: usage.rolling_percent,
                    weekly_percent: usage.weekly_percent,
                    monthly_percent: usage.monthly_percent,
                    rolling_resets_in_minutes: usage.rolling_resets_in_minutes,
                    weekly_resets_in_minutes: usage.weekly_resets_in_minutes,
                },
                &crate::command_code_usage::goat_quota_limits(),
                AccountUsageSyncSuccessMetadata {
                    now: usage.observed_at,
                    next_eligible_at: usage.observed_at + MANUAL_THROTTLE,
                    mark_expedited: false,
                },
            )
            .ok()
            .flatten()
            .is_some(),
        Snapshot::Plan(windows) => {
            let source = match adapter {
                ProviderAdapterKind::MiniMaxCn => crate::plan_usage::MINIMAX_USAGE_SOURCE,
                ProviderAdapterKind::KimiCn => crate::plan_usage::KIMI_USAGE_SOURCE,
                _ => return CalibrationOutcome::Skipped,
            };
            db.replace_quota_windows_by_source(account_id, source, &windows)
                .is_ok()
        }
    };
    if applied {
        CalibrationOutcome::Applied
    } else {
        CalibrationOutcome::Stale
    }
}

#[cfg(test)]
mod tests;
