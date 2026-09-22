//! Immediate official usage refresh after a real upstream 429.
//!
//! Inference never waits on this path. Go remains the only automatic
//! quota-authoritative contract; GOAT/CN calibration must not become a
//! routing hard limit.

use super::provider_adapter::supports_reactive_usage_refresh;
use super::{
    MANUAL_THROTTLE, UsageSyncHost, UsageSyncTrigger, manual_next_allowed_at,
    refresh_official_usage,
};
use crate::command_code_usage::fetch_command_code_usage;
use crate::db::{AccountUsageCalibrationSnapshot, AccountUsageSyncSuccessMetadata};
use crate::go_usage::{GoUsageSnapshot, GoUsageWindowStatus};
use crate::kernel::pricing::PricingLimits;
use crate::models::AccountSetupStep;
use crate::provider::{
    COMMAND_CODE_GOAT_QUOTA_5H, COMMAND_CODE_GOAT_QUOTA_MONTH, COMMAND_CODE_GOAT_QUOTA_WEEK,
    ProviderAdapterKind,
};
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

/// Fire-and-forget official refresh so fallback is not delayed. Coalesces per
/// Key through the existing Go inflight map / 15s throttle, or the process
/// provider-usage lock for GOAT/CN. Does not create a scheduler.
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

/// Reuse the provider refresh gate. Waiting callers recheck the same per-Key
/// throttle after acquiring it, so one burst performs one fetch.
pub(crate) async fn calibrate_provider_if_current(state: &CoreState, account_id: &str) -> bool {
    let _guard = state.provider_usage_refresh.lock().await;
    let now = state.usage_sync.now();
    let (identity, adapter, config, key, revision) = {
        let _settings = state.settings_update.lock();
        let db = state.db.lock();
        let Ok(Some(account)) = db.get_account(account_id) else {
            return false;
        };
        let Some(adapter) = ProviderAdapterKind::from_provider_id(&account.provider_id) else {
            return false;
        };
        if !matches!(
            adapter,
            ProviderAdapterKind::CommandCodeGoat
                | ProviderAdapterKind::MiniMaxCn
                | ProviderAdapterKind::KimiCn
        ) || account.setup_step != AccountSetupStep::Ready
            || account.key_cipher.is_empty()
        {
            return false;
        }
        let sync = match db.account_usage_sync_state(account_id) {
            Ok(sync) => sync,
            Err(_) => return false,
        };
        if manual_next_allowed_at(sync.and_then(|row| row.last_attempt_at), now).is_some() {
            return false;
        }
        let Ok(Some(identity)) = super::UsageRefreshIdentity::capture(&db, account_id) else {
            return false;
        };
        let Ok(key) = state.decrypt_key(&account.key_cipher) else {
            return false;
        };
        // Record the attempt before I/O: success and failure share throttling,
        // and an old response never writes metadata onto a rotated account.
        if db
            .touch_account_usage_sync_attempt(account_id, now)
            .is_err()
        {
            return false;
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
            fetch_command_code_usage(&config, &key, state.process_generation(), || {
                state.usage_sync.now()
            })
            .await
            .map(Snapshot::Goat)
            .map_err(|_| ())
        }
        _ => crate::plan_usage::fetch(&config, adapter, account_id, &key)
            .await
            .map(Snapshot::Plan)
            .map_err(|_| ()),
    };
    drop(key);
    let Ok(snapshot) = fetched else {
        return false;
    };
    let _settings = state.settings_update.lock();
    if state.settings_revision() != revision {
        return false;
    }
    let db = state.db.lock();
    if !identity.is_current(&db).unwrap_or(false) {
        return false;
    }
    match snapshot {
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
                &goat_limits(),
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
                _ => return false,
            };
            db.replace_quota_windows_by_source(account_id, source, &windows)
                .is_ok()
        }
    }
}

fn goat_limits() -> PricingLimits {
    PricingLimits {
        window_5h: COMMAND_CODE_GOAT_QUOTA_5H,
        window_week: COMMAND_CODE_GOAT_QUOTA_WEEK,
        window_month: COMMAND_CODE_GOAT_QUOTA_MONTH,
    }
}

#[cfg(test)]
mod tests;
