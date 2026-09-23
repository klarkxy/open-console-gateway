//! Local account usage reads and live-calibration writes.
//!
//! GET/PATCH `/accounts/{id}/usage` and GET `/accounts/{id}/provider-usage`
//! reuse the current Database/provider projections. POST on provider usage
//! refreshes MiniMax/Kimi snapshots under CAS and, for OpenCode Go, reuses
//! the official usage coordinator then returns `ProviderUsage`. There is no
//! legacy alias or plugin/trait hierarchy. Usage calibration does not bump
//! `settings_revision`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::kernel::pricing::PricingLimits;
use crate::models::{
    Account as ModelAccount, CreditBalance as ModelCreditBalance, ProviderUsageSyncState,
    QuotaWindow as ModelQuotaWindow, UsageWindow as ModelUsageWindow, UsageWindowKind,
};
use crate::provider::{
    OllamaBillingTier, ProviderAdapterKind, ProviderRegistry, QUOTA_WINDOW_FREE,
};
use crate::state::CoreState;
use crate::usage_sync::{
    CalibrationOutcome, ControlRevision, ProviderUsageRefreshGate, UsageSyncCommitAuthorization,
    UsageSyncTrigger, refresh_coalesced,
};

use super::types::{
    AccountUsageUpdate, CreditBalance, MutationExpectation, ProviderUsage, QuotaWindow,
    UsageAvailability, UsageMutation, UsageSyncState, UsageWindow,
};
use super::usage_refresh::{RefreshApiError, map_refresh_error};
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
    load_provider_usage(&state, &id).map(Json)
}

enum ProviderUsageRefreshKind {
    Go,
    Goat,
    Plan,
    Balance { endpoint_url: String },
}

fn classify_provider_usage_refresh(
    state: &CoreState,
    db: &Database,
    account: &ModelAccount,
) -> Result<ProviderUsageRefreshKind, V3ApiError> {
    match ProviderAdapterKind::from_provider_id(&account.provider_id) {
        Some(ProviderAdapterKind::OpenCodeGo) => Ok(ProviderUsageRefreshKind::Go),
        Some(ProviderAdapterKind::CommandCodeGoat) => Ok(ProviderUsageRefreshKind::Goat),
        Some(ProviderAdapterKind::MiniMaxCn | ProviderAdapterKind::KimiCn) => {
            Ok(ProviderUsageRefreshKind::Plan)
        }
        Some(ProviderAdapterKind::ConfigurableHttp) => {
            let endpoint = db
                .account_custom_config(&account.id)
                .map_err(V3ApiError::internal)?
                .map(|config| config.endpoint_url);
            balance_refresh_kind(state, endpoint)
        }
        None => {
            let endpoint =
                crate::dynamic::find_runtime(&state.dynamic_providers(), &account.provider_id)
                    .map(|runtime| runtime.endpoint_url.clone());
            if endpoint.is_none() {
                return Err(V3ApiError::invalid_request_at(
                    state,
                    "unknown provider offering",
                ));
            }
            balance_refresh_kind(state, endpoint)
        }
        Some(_) => Err(V3ApiError::invalid_request_at(
            state,
            "this Plan does not expose an official manual usage refresh",
        )),
    }
}

fn balance_refresh_kind(
    state: &CoreState,
    endpoint_url: Option<String>,
) -> Result<ProviderUsageRefreshKind, V3ApiError> {
    let Some(endpoint_url) = endpoint_url.filter(|value| !value.trim().is_empty()) else {
        return Err(V3ApiError::invalid_request_at(
            state,
            "this Plan does not expose an official manual usage refresh",
        ));
    };
    if crate::api_balance::probe_from_endpoint(&endpoint_url).is_none() {
        return Err(V3ApiError::invalid_request_at(
            state,
            "this destination does not expose an official balance endpoint",
        ));
    }
    Ok(ProviderUsageRefreshKind::Balance { endpoint_url })
}

pub(super) async fn refresh_provider_usage(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<ProviderUsage>, RefreshApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let kind = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        let db = state.db.lock();
        let account = load_account(&db, &state, &id)?;
        classify_provider_usage_refresh(&state, &db, &account)?
    };
    match kind {
        ProviderUsageRefreshKind::Go => {
            return refresh_go_provider_usage(&state, &id, &expectation).await;
        }
        ProviderUsageRefreshKind::Goat => {
            super::command_code_usage_refresh::refresh(&state, &id, &expectation).await?;
            load_provider_usage(&state, &id)
                .map(Json)
                .map_err(RefreshApiError::from)
        }
        ProviderUsageRefreshKind::Plan => {
            return refresh_plan_usage(&state, &id, &expectation).await;
        }
        ProviderUsageRefreshKind::Balance { endpoint_url } => {
            return refresh_official_balance(&state, &id, &expectation, endpoint_url).await;
        }
    }
}

async fn refresh_plan_usage(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
) -> Result<Json<ProviderUsage>, RefreshApiError> {
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
                "provider usage refresh",
            ));
        }
        CalibrationOutcome::FetchFailed(message) => {
            state.log_runtime_event(
                "warn",
                "usage_sync",
                &format!("event=provider_usage_refresh_failed account_id={id} stage=fetch"),
            );
            return Err(V3ApiError::outbound_failed(state, message).into());
        }
        CalibrationOutcome::Stale => {
            return Err(V3ApiError::conflict_at(
                state,
                "the account changed while provider usage was being refreshed",
            )
            .into());
        }
        CalibrationOutcome::RejectedKey | CalibrationOutcome::Skipped => {
            return Err(V3ApiError::invalid_request_at(
                state,
                "this Plan does not expose an official manual usage refresh",
            )
            .into());
        }
    }
    let usage = {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, expectation)?;
        let db = state.db.lock();
        provider_usage_from_db(state, &db, id)?
    };
    state.log_runtime_event(
        "info",
        "usage_sync",
        &format!("event=provider_usage_refresh_succeeded account_id={id}"),
    );
    Ok(Json(usage))
}

async fn refresh_official_balance(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    endpoint_url: String,
) -> Result<Json<ProviderUsage>, RefreshApiError> {
    let _refresh = state
        .provider_usage_refresh
        .exclusive(ProviderUsageRefreshGate::balance_key(id))
        .await;
    let (account_snapshot, config, key) = {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, expectation)?;
        let db = state.db.lock();
        let account = load_account(&db, state, id)?;
        if configured_balance_endpoint(&db, &account)?.as_deref() != Some(endpoint_url.as_str()) {
            return Err(V3ApiError::conflict_at(
                state,
                "the destination changed before balance refresh",
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
        let key = state
            .decrypt_key(&account.key_cipher)
            .map_err(V3ApiError::internal)?;
        (account, state.config(), key)
    };
    let rows = match crate::api_balance::fetch(&config, id, &key, &endpoint_url).await {
        Ok(rows) => rows,
        Err(message) => {
            state.log_runtime_event(
                "warn",
                "usage_sync",
                &format!(
                    "event=provider_usage_refresh_failed account_id={id} provider={} stage=balance",
                    account_snapshot.provider_id
                ),
            );
            return Err(V3ApiError::outbound_failed(state, message).into());
        }
    };
    let source = rows.first().map(|row| row.source.clone()).ok_or_else(|| {
        V3ApiError::outbound_failed(state, "balance endpoint returned no usable amount")
    })?;
    let row_count = rows.len();
    {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, expectation)?;
        let db = state.db.lock();
        let current = load_account(&db, state, id)?;
        if current.updated_at != account_snapshot.updated_at
            || current.key_cipher != account_snapshot.key_cipher
            || current.provider_id != account_snapshot.provider_id
            || configured_balance_endpoint(&db, &current)?.as_deref() != Some(endpoint_url.as_str())
        {
            return Err(V3ApiError::conflict_at(
                state,
                "the account changed while provider usage was being refreshed",
            )
            .into());
        }
        db.replace_credit_balances_by_source(id, &source, &rows)
            .map_err(V3ApiError::internal)?;
    }
    state.log_runtime_event(
        "info",
        "usage_sync",
        &format!(
            "event=provider_usage_refresh_succeeded account_id={id} provider={} window_count={row_count}",
            account_snapshot.provider_id
        ),
    );
    load_provider_usage(state, id)
        .map(Json)
        .map_err(RefreshApiError::from)
}

async fn refresh_go_provider_usage(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
) -> Result<Json<ProviderUsage>, RefreshApiError> {
    {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, expectation)?;
    }
    let authorization = UsageSyncCommitAuthorization::control_revision(
        expectation.expected_revision,
        expectation.process_generation,
    );
    let observation = crate::usage_sync::refresh_official_usage_with_authorization(
        state,
        id,
        UsageSyncTrigger::Manual,
        authorization,
    )
    .await;
    if observation.owner_authorization != authorization {
        let _settings_update = state.settings_update.lock();
        check_expectation(state, expectation)?;
    }
    match observation.result {
        Ok(_) => load_provider_usage(state, id)
            .map(Json)
            .map_err(RefreshApiError::from),
        Err(error) => Err(map_refresh_error(state, error)),
    }
}

fn account_usage_locked(state: &CoreState, id: &str) -> Result<UsageWindow, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let pricing = captured_pricing(state);
    let db = state.db.lock();
    let account = load_account(&db, state, id)?;
    if matches!(
        ProviderAdapterKind::from_provider_id(&account.provider_id),
        Some(ProviderAdapterKind::OllamaCloud)
    ) {
        let _limit = db
            .ollama_cloud_billing_tier(&account.id)
            .map_err(V3ApiError::internal)?
            .map(OllamaBillingTier::monthly_credit_limit)
            .ok_or_else(|| {
                V3ApiError::invalid_request_at(
                    state,
                    "manual usage calibration is unavailable for this account",
                )
            })?;
        let (used, reset) = db.ollama_month_usage(id).map_err(V3ApiError::internal)?;
        return Ok(usage_window_from_model(
            state,
            ModelUsageWindow {
                account_id: id.to_string(),
                window_5h: 0.0,
                window_week: 0.0,
                window_month: used,
                resets_in_5h: None,
                resets_in_week: None,
                resets_in_month: reset,
            },
            None,
        ));
    }
    let (limits, pricing_revision) = account_usage_limits(state, &db, &account, &pricing)?;
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
    let (limits, pricing_revision) = account_usage_limits(state, &db, &account, &pricing)?;
    let window = parse_usage_window(state, &input.window)?;
    if matches!(
        ProviderAdapterKind::from_provider_id(&account.provider_id),
        Some(ProviderAdapterKind::OllamaCloud)
    ) && window != UsageWindowKind::Month
    {
        return Err(V3ApiError::invalid_request_at(
            state,
            "Ollama Cloud publishes only a monthly credit window",
        ));
    }
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
    let ollama = matches!(
        ProviderAdapterKind::from_provider_id(&account.provider_id),
        Some(ProviderAdapterKind::OllamaCloud)
    );
    let calibrated = if ollama {
        db.calibrate_ollama_month_usage(id, percent, limit, Utc::now())
            .map_err(V3ApiError::internal)?
    } else {
        db.calibrate_account_usage(id, window, percent, input.resets_in_minutes, limit)
            .map_err(V3ApiError::internal)?
    };
    if !calibrated {
        return Err(V3ApiError::not_found(state));
    }
    if ollama {
        let (used, reset) = db.ollama_month_usage(id).map_err(V3ApiError::internal)?;
        return Ok(UsageMutation {
            usage: usage_window_from_model(
                state,
                ModelUsageWindow {
                    account_id: id.to_string(),
                    window_5h: 0.0,
                    window_week: 0.0,
                    window_month: used,
                    resets_in_5h: None,
                    resets_in_week: None,
                    resets_in_month: reset,
                },
                pricing_revision,
            ),
            revision: state.settings_revision(),
            process_generation: state.process_generation(),
        });
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

/// Read provider usage. Acquires `settings_update` and the database lock.
/// Callers must not already hold either lock.
pub(crate) fn load_provider_usage(
    state: &CoreState,
    id: &str,
) -> Result<ProviderUsage, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let db = state.db.lock();
    provider_usage_from_db(state, &db, id)
}

/// Project provider usage from a database the caller already locked.
/// Does not acquire `settings_update` or `db`.
fn provider_usage_from_db(
    state: &CoreState,
    db: &Database,
    id: &str,
) -> Result<ProviderUsage, V3ApiError> {
    let account = load_account(db, state, id)?;
    if crate::dynamic::find_runtime(&state.dynamic_providers(), &account.provider_id).is_some() {
        return Ok(provider_usage_from_parts(
            state,
            id,
            &account,
            UsageAvailability::Unavailable,
            false,
            None,
            Vec::new(),
            official_credit_balances(db, &account)?,
            None,
            None,
        ));
    }
    let descriptor = ProviderRegistry::get(&account.provider_id)
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
            official_credit_balances(db, &account)?,
            db.account_usage_sync_state(&account.id)
                .map_err(V3ApiError::internal)?,
            None,
        ));
    }
    let free_cooldown_until = if descriptor.usage.egress_ip_shared_cooldown_window {
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
    } else if descriptor.usage.egress_ip_shared_cooldown_window {
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
    } else if descriptor.kind == ProviderAdapterKind::CommandCodeGoat {
        let limits = crate::command_code_usage::goat_quota_limits();
        let observed_at = db
            .account_usage_sync_state(&account.id)
            .map_err(V3ApiError::internal)?
            .and_then(|sync| sync.last_success_at);
        let mut windows = db
            .live_local_quota_windows(&account.id, &limits, "command-code-goat-local")
            .map_err(V3ApiError::internal)?;
        for window in &mut windows {
            window.observed_at = observed_at;
        }
        (windows, None)
    } else if descriptor.kind == ProviderAdapterKind::OllamaCloud {
        let windows = match db
            .ollama_cloud_billing_tier(&account.id)
            .map_err(V3ApiError::internal)?
            .map(OllamaBillingTier::monthly_credit_limit)
        {
            Some(limit) => db
                .live_ollama_month_quota_window(&account.id, limit)
                .map_err(V3ApiError::internal)?,
            None => Vec::new(),
        };
        (windows, None)
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

fn official_credit_balances(
    db: &Database,
    account: &ModelAccount,
) -> Result<Vec<ModelCreditBalance>, V3ApiError> {
    let endpoint = configured_balance_endpoint(db, account)?;
    let stepfun_api = endpoint.as_deref().is_some_and(|endpoint| {
        crate::api_balance::probe_from_endpoint(endpoint).is_some()
            && reqwest::Url::parse(endpoint)
                .ok()
                .is_some_and(|url| url.host_str() == Some("api.stepfun.com"))
    });
    Ok(db
        .list_credit_balances(&account.id)
        .map_err(V3ApiError::internal)?
        .into_iter()
        .filter(|row| crate::api_balance::is_official_balance_source(&row.source))
        .filter(|row| row.source != "stepfun-api-official" || stepfun_api)
        .collect())
}

fn configured_balance_endpoint(
    db: &Database,
    account: &ModelAccount,
) -> Result<Option<String>, V3ApiError> {
    if account.provider_id == crate::provider::CUSTOM_PROVIDER_ID {
        return db
            .account_custom_config(&account.id)
            .map(|config| config.map(|config| config.endpoint_url))
            .map_err(V3ApiError::internal);
    }
    Ok(db
        .list_dynamic_providers()
        .map_err(V3ApiError::internal)?
        .into_iter()
        .find(|provider| provider.id == account.provider_id)
        .map(|provider| provider.endpoint_url))
}

fn load_account(db: &Database, state: &CoreState, id: &str) -> Result<ModelAccount, V3ApiError> {
    db.get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found(state))
}

fn account_usage_limits(
    state: &CoreState,
    db: &Database,
    account: &ModelAccount,
    pricing: &CapturedPricing,
) -> Result<(PricingLimits, Option<String>), V3ApiError> {
    match ProviderAdapterKind::from_provider_id(&account.provider_id) {
        Some(ProviderAdapterKind::OpenCodeGo) => {
            return Ok((pricing.limits.clone(), Some(pricing.revision.clone())));
        }
        Some(ProviderAdapterKind::CommandCodeGoat) => {
            return Ok((crate::command_code_usage::goat_quota_limits(), None));
        }
        Some(ProviderAdapterKind::OllamaCloud) => {
            let limit = db
                .ollama_cloud_billing_tier(&account.id)
                .map_err(V3ApiError::internal)?
                .map(OllamaBillingTier::monthly_credit_limit)
                .ok_or_else(|| {
                    V3ApiError::invalid_request_at(
                        state,
                        "manual usage calibration is unavailable for this account",
                    )
                })?;
            return Ok((
                PricingLimits {
                    window_5h: limit,
                    window_week: limit,
                    window_month: limit,
                },
                None,
            ));
        }
        _ => {}
    }
    Err(V3ApiError::invalid_request_at(
        state,
        "manual usage calibration is unavailable for this account",
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
