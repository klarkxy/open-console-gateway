//! Local account usage reads and live-calibration writes.
//!
//! GET/PATCH `/accounts/{id}/usage` and GET `/accounts/{id}/provider-usage`
//! reuse the current V2 Database/provider projections. There is no outbound
//! I/O, no `/providers/accounts/{id}/usage` alias, and no plugin/trait
//! hierarchy. Usage calibration does not bump `settings_revision`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::kernel::pricing::PricingLimits;
use crate::models::{Account as ModelAccount, UsageWindow as ModelUsageWindow, UsageWindowKind};
use crate::provider::{
    COMMAND_CODE_GOAT_QUOTA_5H, COMMAND_CODE_GOAT_QUOTA_MONTH, COMMAND_CODE_GOAT_QUOTA_WEEK,
    CreditBalance as ModelCreditBalance, ProviderAdapterKind, ProviderRegistry,
    ProviderUsageSyncState, QUOTA_WINDOW_FREE, QuotaWindow as ModelQuotaWindow,
};
use crate::state::CoreState;

use super::types::{
    AccountUsageUpdate, CreditBalance, ProviderUsage, QuotaWindow, UsageAvailability,
    UsageMutation, UsageSyncState, UsageWindow,
};
use super::{V3ApiError, check_expectation, parse_mutation_json};

struct CapturedPricing {
    limits: PricingLimits,
    revision: String,
}

pub(super) async fn get_account_usage(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<UsageWindow>, V3ApiError> {
    account_usage_locked(&state, &id).map(Json)
}

pub(super) async fn patch_account_usage(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<UsageMutation>, V3ApiError> {
    let input = parse_mutation_json::<AccountUsageUpdate>(&body)?;
    patch_account_usage_locked(&state, &id, input).map(Json)
}

pub(super) async fn get_provider_usage(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderUsage>, V3ApiError> {
    provider_usage_locked(&state, &id).map(Json)
}

fn account_usage_locked(state: &CoreState, id: &str) -> Result<UsageWindow, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let pricing = captured_pricing(state);
    let db = state.db.lock();
    let account = load_account(&db, state, id)?;
    let (limits, pricing_revision) = account_usage_limits(state, &account, &pricing)?;
    let usage = db
        .account_usage_with_limits(id, &limits)
        .map_err(V3ApiError::internal)?;
    Ok(usage_window_from_model(state, usage, pricing_revision))
}

fn patch_account_usage_locked(
    state: &CoreState,
    id: &str,
    input: AccountUsageUpdate,
) -> Result<UsageMutation, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    check_expectation(state, &input.expectation)?;
    let pricing = captured_pricing(state);
    let db = state.db.lock();
    let account = load_account(&db, state, id)?;
    let (limits, pricing_revision) = account_usage_limits(state, &account, &pricing)?;
    let window = parse_usage_window(state, &input.window)?;
    if !input.percent.is_finite() || !(0.0..=100.0).contains(&input.percent) {
        return Err(V3ApiError::invalid_request_at(
            state,
            "usage percent must be between 0 and 100",
        ));
    }
    let percent = (input.percent * 10.0).round() / 10.0;
    if let Some(mins) = input.resets_in_minutes {
        let max = match window {
            UsageWindowKind::FiveHours => Some(5 * 60),
            UsageWindowKind::Week => Some(7 * 24 * 60),
            UsageWindowKind::Month | UsageWindowKind::Free => None,
        };
        if mins < 0 || max.is_some_and(|max| mins > max) {
            return Err(V3ApiError::invalid_request_at(
                state,
                match max {
                    Some(max) => format!("resets_in_minutes must be between 0 and {max}"),
                    None => "resets_in_minutes must be >= 0".to_string(),
                },
            ));
        }
    }
    let limit = match window {
        UsageWindowKind::FiveHours => limits.window_5h,
        UsageWindowKind::Week => limits.window_week,
        UsageWindowKind::Month => limits.window_month,
        UsageWindowKind::Free => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "free promo quota cannot be calibrated as a Go usage window",
            ));
        }
    };
    if !db
        .calibrate_account_usage(id, window, percent, input.resets_in_minutes, limit)
        .map_err(V3ApiError::internal)?
    {
        return Err(V3ApiError::not_found(state));
    }
    let usage = db
        .account_usage_with_limits(id, &limits)
        .map_err(V3ApiError::internal)?;
    Ok(UsageMutation {
        usage: usage_window_from_model(state, usage, pricing_revision),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

fn provider_usage_locked(state: &CoreState, id: &str) -> Result<ProviderUsage, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let db = state.db.lock();
    let account = load_account(&db, state, id)?;
    let descriptor = ProviderRegistry::get(&account.provider_id, &account.offering_id)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "unknown provider offering"))?;
    let availability = map_usage_availability(descriptor.usage.catalog_availability)
        .map_err(V3ApiError::internal)?;
    if descriptor.usage.catalog_availability == "unavailable" {
        return Ok(provider_usage_from_parts(
            state,
            id,
            &account,
            availability,
            descriptor.usage.experimental,
            None,
            Vec::new(),
            Vec::new(),
            db.account_usage_sync_state(&account.id)
                .map_err(V3ApiError::internal)?,
            None,
        ));
    }
    let free_cooldown_until = if descriptor.error_cooldown.egress_ip_shared_free_cooldown {
        db.free_channel_cooldown_until()
            .map_err(V3ApiError::internal)?
    } else {
        None
    };
    let (quota_windows, pricing_revision) = if descriptor.usage.authoritative_for_quota {
        let pricing = captured_pricing(state);
        (
            db.live_opencode_go_quota_windows(&account.id, &pricing.limits)
                .map_err(V3ApiError::internal)?,
            Some(pricing.revision),
        )
    } else if descriptor.error_cooldown.egress_ip_shared_free_cooldown {
        (
            vec![ModelQuotaWindow {
                account_id: account.id.clone(),
                window_kind: QUOTA_WINDOW_FREE.to_string(),
                used: if free_cooldown_until.is_some() {
                    1.0
                } else {
                    0.0
                },
                limit_value: None,
                started_at: None,
                resets_at: free_cooldown_until,
                calibration_offset: 0.0,
                unit: "channel".to_string(),
                source: "egress-cooldown-live".to_string(),
                observed_at: None,
                updated_at: Utc::now(),
            }],
            None,
        )
    } else {
        (
            db.list_quota_windows(&account.id)
                .map_err(V3ApiError::internal)?,
            None,
        )
    };
    Ok(provider_usage_from_parts(
        state,
        id,
        &account,
        availability,
        descriptor.usage.experimental,
        free_cooldown_until,
        quota_windows,
        db.list_credit_balances(&account.id)
            .map_err(V3ApiError::internal)?,
        db.account_usage_sync_state(&account.id)
            .map_err(V3ApiError::internal)?,
        pricing_revision,
    ))
}

fn captured_pricing(state: &CoreState) -> CapturedPricing {
    let snapshot = state.pricing_snapshot();
    CapturedPricing {
        limits: snapshot.limits.clone(),
        revision: snapshot.revision.clone(),
    }
}

fn load_account(db: &Database, state: &CoreState, id: &str) -> Result<ModelAccount, V3ApiError> {
    db.get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found(state))
}

fn account_usage_limits(
    state: &CoreState,
    account: &ModelAccount,
    pricing: &CapturedPricing,
) -> Result<(PricingLimits, Option<String>), V3ApiError> {
    if ProviderAdapterKind::from_offering(&account.provider_id, &account.offering_id)
        == Some(ProviderAdapterKind::OpenCodeGo)
    {
        return Ok((pricing.limits.clone(), Some(pricing.revision.clone())));
    }
    let plan = crate::provider::builtin_plan(&account.provider_id, &account.offering_id)
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(state, "account usage capability is unknown")
        })?;
    if !plan.manual_usage_calibration {
        return Err(V3ApiError::invalid_request_at(
            state,
            "manual usage calibration is unavailable for this account",
        ));
    }
    Ok((
        PricingLimits {
            window_5h: COMMAND_CODE_GOAT_QUOTA_5H,
            window_week: COMMAND_CODE_GOAT_QUOTA_WEEK,
            window_month: COMMAND_CODE_GOAT_QUOTA_MONTH,
        },
        None,
    ))
}

fn parse_usage_window(state: &CoreState, window: &str) -> Result<UsageWindowKind, V3ApiError> {
    match window {
        "window_5h" => Ok(UsageWindowKind::FiveHours),
        "window_week" => Ok(UsageWindowKind::Week),
        "window_month" => Ok(UsageWindowKind::Month),
        _ => Err(V3ApiError::invalid_request_at(
            state,
            "invalid usage window",
        )),
    }
}

fn map_usage_availability(value: &str) -> Result<UsageAvailability, String> {
    match value {
        "available" => Ok(UsageAvailability::Available),
        "unavailable" => Ok(UsageAvailability::Unavailable),
        "local_state" => Ok(UsageAvailability::LocalState),
        other => Err(format!("unknown usage availability `{other}`")),
    }
}

fn usage_window_from_model(
    state: &CoreState,
    usage: ModelUsageWindow,
    pricing_revision: Option<String>,
) -> UsageWindow {
    UsageWindow {
        account_id: usage.account_id,
        window_5h: usage.window_5h,
        window_week: usage.window_week,
        window_month: usage.window_month,
        resets_in_5h: rfc3339_opt(usage.resets_in_5h),
        resets_in_week: rfc3339_opt(usage.resets_in_week),
        resets_in_month: rfc3339_opt(usage.resets_in_month),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        pricing_revision,
    }
}

#[allow(clippy::too_many_arguments)]
fn provider_usage_from_parts(
    state: &CoreState,
    id: &str,
    account: &ModelAccount,
    availability: UsageAvailability,
    experimental: bool,
    free_cooldown_until: Option<DateTime<Utc>>,
    quota_windows: Vec<ModelQuotaWindow>,
    credit_balances: Vec<ModelCreditBalance>,
    sync_state: Option<ProviderUsageSyncState>,
    pricing_revision: Option<String>,
) -> ProviderUsage {
    ProviderUsage {
        account_id: id.to_string(),
        provider_id: account.provider_id.clone(),
        offering_id: account.offering_id.clone(),
        availability,
        experimental,
        free_cooldown_until: rfc3339_opt(free_cooldown_until),
        quota_windows: quota_windows
            .into_iter()
            .map(quota_window_from_model)
            .collect(),
        credit_balances: credit_balances
            .into_iter()
            .map(credit_balance_from_model)
            .collect(),
        sync_state: sync_state.map(usage_sync_state_from_model),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        pricing_revision,
    }
}

fn quota_window_from_model(window: ModelQuotaWindow) -> QuotaWindow {
    QuotaWindow {
        account_id: window.account_id,
        window_kind: window.window_kind,
        used: window.used,
        limit_value: window.limit_value,
        started_at: rfc3339_opt(window.started_at),
        resets_at: rfc3339_opt(window.resets_at),
        calibration_offset: window.calibration_offset,
        unit: window.unit,
        source: window.source,
        observed_at: rfc3339_opt(window.observed_at),
        updated_at: window.updated_at.to_rfc3339(),
    }
}

fn credit_balance_from_model(balance: ModelCreditBalance) -> CreditBalance {
    CreditBalance {
        account_id: balance.account_id,
        balance_kind: balance.balance_kind,
        amount: balance.amount,
        unit: balance.unit,
        source: balance.source,
        observed_at: rfc3339_opt(balance.observed_at),
        updated_at: balance.updated_at.to_rfc3339(),
    }
}

fn usage_sync_state_from_model(sync: ProviderUsageSyncState) -> UsageSyncState {
    UsageSyncState {
        account_id: sync.account_id,
        last_success_at: rfc3339_opt(sync.last_success_at),
        last_attempt_at: rfc3339_opt(sync.last_attempt_at),
        next_eligible_at: rfc3339_opt(sync.next_eligible_at),
        failure_streak: sync.failure_streak,
        last_expedited_at: rfc3339_opt(sync.last_expedited_at),
    }
}

fn rfc3339_opt(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|value| value.to_rfc3339())
}
