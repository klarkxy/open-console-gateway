//! Local observability reads shared by Dashboard V2 and V3.
//!
//! Every function is a runtime/SQLite read. None of these paths issue outbound
//! HTTP. Secret redaction matches the historical V2 diagnostic/account-secret
//! behavior; plaintext Keys stay off this API. Callers pass Database guards,
//! pricing/contract snapshots, primitive gateway status values, and a
//! decryption callback. This module must not import `state`, `gateway`,
//! `dashboard`, or `dashboard_v3`.

use crate::alias;
use crate::db::{Database, ForwardLogQueryOptions};
use crate::kernel::pricing::PricingSnapshot;
use crate::models::{
    Account, DailyModelCost, DashboardSummary, ForwardLog, ForwardLogClientKey, ForwardLogPage,
    ForwardLogSummary, GatewayLog, UpstreamChannel,
};
use crate::provider_contracts::EffectiveContractSet;
use crate::redaction::redact_known_secret;
use chrono::{DateTime, SecondsFormat, Utc};
use std::collections::{BTreeMap, HashSet};

pub struct GatewayRuntimeStatus {
    pub running: bool,
    pub port: u16,
    pub upstream_base_url: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GatewayLogReadQuery {
    pub limit: Option<i64>,
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ForwardLogReadQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    pub status: Option<String>,
    pub account_id: Option<String>,
    pub provider_id: Option<String>,
    pub offering_id: Option<String>,
    pub route_account_id: Option<String>,
    pub credential_account_id: Option<String>,
    pub model: Option<String>,
    pub request_id: Option<String>,
    pub key_id: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnrichedForwardLog {
    pub log: ForwardLog,
    pub requested_model: Option<String>,
    pub resolved_alias: Option<String>,
    pub upstream_model: Option<String>,
    pub native_cost_value: Option<f64>,
    pub native_cost_unit: Option<String>,
    pub native_cost_currency: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EnrichedForwardLogPage {
    pub items: Vec<EnrichedForwardLog>,
    pub summary: ForwardLogSummary,
}

#[derive(Debug)]
pub enum ObservabilityError {
    InvalidQuery(String),
    Internal(anyhow::Error),
}

impl std::fmt::Display for ObservabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidQuery(message) => f.write_str(message),
            Self::Internal(error) => write!(f, "{error}"),
        }
    }
}

impl From<anyhow::Error> for ObservabilityError {
    fn from(error: anyhow::Error) -> Self {
        Self::Internal(error)
    }
}

pub fn gateway_runtime_status(
    running: bool,
    port: u16,
    upstream_base_url: String,
    last_error: Option<String>,
) -> GatewayRuntimeStatus {
    GatewayRuntimeStatus {
        running,
        port,
        upstream_base_url,
        last_error: if running { None } else { last_error },
    }
}

/// Latest `error`/`gateway` log message, with every known decrypted account
/// secret redacted using the same policy as gateway log lists.
pub fn redacted_latest_gateway_error(
    db: &Database,
    decrypt_key: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let message = db.latest_gateway_error().ok().flatten()?;
    let secrets = dashboard_account_secrets(db, decrypt_key).ok()?;
    Some(redact_known_secrets(&message, &secrets))
}

pub fn application_models(
    snapshot: &PricingSnapshot,
    contracts: Option<&EffectiveContractSet>,
) -> Vec<String> {
    application_models_from_snapshot(snapshot, contracts)
}

pub fn application_models_from_snapshot(
    snapshot: &PricingSnapshot,
    contracts: Option<&EffectiveContractSet>,
) -> Vec<String> {
    let priced = snapshot
        .models
        .iter()
        .map(|model| model.model_id.as_str())
        .collect::<HashSet<_>>();
    alias::routeable_aliases_for(
        crate::provider::OPENCODE_PROVIDER_ID,
        crate::provider::GO_OFFERING_ID,
    )
    .into_iter()
    .filter(|alias| {
        application_alias_is_priced(alias, &priced)
            && contracts.is_none_or(|contracts| go_alias_has_enabled_protocol(alias, contracts))
    })
    .collect()
}

fn go_alias_has_enabled_protocol(alias: &str, contracts: &EffectiveContractSet) -> bool {
    match crate::alias::resolve(alias) {
        Ok(crate::alias::ResolvedModel::Alias { mappings, .. }) => mappings.iter().any(|mapping| {
            mapping.routeable
                && mapping.provider_id == crate::provider::OPENCODE_PROVIDER_ID
                && contracts.mapping_has_enabled_protocol(mapping)
        }),
        Ok(crate::alias::ResolvedModel::PinnedRaw { mapping, .. }) => {
            mapping.routeable
                && mapping.provider_id == crate::provider::OPENCODE_PROVIDER_ID
                && contracts.mapping_has_enabled_protocol(&mapping)
        }
        Err(_) => false,
    }
}

fn application_alias_is_priced(alias: &str, priced: &HashSet<&str>) -> bool {
    priced.contains(alias)
        || alias
            .strip_suffix("-highspeed")
            .is_some_and(|base| priced.contains(base))
}

pub fn dashboard_summary(
    db: &Database,
    gateway_running: bool,
    decrypt_key: impl Fn(&str) -> Option<String>,
) -> Result<DashboardSummary, ObservabilityError> {
    let accounts = db.list_accounts()?;
    let total_accounts = accounts.len();
    let now = Utc::now();
    let free_channel_cooling = db.free_channel_cooldown_until()?.is_some();
    let available_accounts = accounts
        .iter()
        .filter(|account| {
            dashboard_account_is_available(account, now, free_channel_cooling, &decrypt_key)
        })
        .count();
    let (today_cost, week_cost, month_cost) = db.total_usage()?;
    Ok(DashboardSummary {
        total_accounts,
        available_accounts,
        gateway_running,
        today_cost,
        week_cost,
        month_cost,
    })
}

fn dashboard_account_is_available(
    account: &Account,
    now: DateTime<Utc>,
    free_channel_cooling: bool,
    decrypt_key: &impl Fn(&str) -> Option<String>,
) -> bool {
    if !account.enabled
        || !account.setup_step.is_ready()
        || account.auth_error.is_some()
        || account.validate_provider_binding().is_err()
    {
        return false;
    }
    match (account.provider_id.as_str(), account.offering_id.as_str()) {
        (crate::provider::OPENCODE_PROVIDER_ID, crate::provider::GO_OFFERING_ID) => {
            !account.is_cooling_for(UpstreamChannel::Go, now)
                && !account.key_cipher.is_empty()
                && decrypt_key(&account.key_cipher).is_some_and(|key| !key.trim().is_empty())
        }
        (
            crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID,
            crate::provider::ANONYMOUS_FREE_OFFERING_ID,
        ) => !free_channel_cooling && !account.is_cooling_for(UpstreamChannel::Free, now),
        _ => false,
    }
}

pub fn daily_cost_by_model(
    db: &Database,
    days: Option<i64>,
) -> Result<Vec<DailyModelCost>, ObservabilityError> {
    db.daily_cost_by_model(days.unwrap_or(30))
        .map_err(ObservabilityError::from)
}

pub fn gateway_logs(
    db: &Database,
    query: GatewayLogReadQuery,
    decrypt_key: impl Fn(&str) -> Option<String>,
) -> Result<Vec<GatewayLog>, ObservabilityError> {
    let mut logs =
        db.query_gateway_logs(query.limit.unwrap_or(100), query.request_id.as_deref())?;
    let secrets = dashboard_account_secrets(db, decrypt_key)?;
    for log in &mut logs {
        log.message = redact_known_secrets(&log.message, &secrets);
        log.diagnostic = redact_diagnostic(log.diagnostic.take(), secrets.values());
    }
    Ok(logs)
}

pub fn query_forward_logs(
    db: &Database,
    query: ForwardLogReadQuery,
    decrypt_key: impl Fn(&str) -> Option<String>,
) -> Result<EnrichedForwardLogPage, ObservabilityError> {
    let (start_time, end_time) = normalize_forward_log_window(
        query.sort_by.as_deref(),
        query.sort_order.as_deref(),
        query.start_time.as_deref(),
        query.end_time.as_deref(),
    )
    .map_err(ObservabilityError::InvalidQuery)?;
    let mut page = db.query_forward_logs(ForwardLogQueryOptions {
        limit: query.limit.unwrap_or(100),
        offset: query.offset.unwrap_or(0),
        status: query.status.as_deref(),
        account_id: query.account_id.as_deref(),
        provider_id: query
            .provider_id
            .as_deref()
            .filter(|value| !value.is_empty()),
        offering_id: query
            .offering_id
            .as_deref()
            .filter(|value| !value.is_empty()),
        route_account_id: query
            .route_account_id
            .as_deref()
            .filter(|value| !value.is_empty()),
        credential_account_id: query
            .credential_account_id
            .as_deref()
            .filter(|value| !value.is_empty()),
        model: query.model.as_deref(),
        request_id: query.request_id.as_deref(),
        start_time: start_time.as_deref(),
        end_time: end_time.as_deref(),
        sort_by: query.sort_by.as_deref(),
        sort_order: query.sort_order.as_deref(),
        key_id: query.key_id.as_deref().filter(|value| !value.is_empty()),
    })?;
    let secrets = dashboard_account_secrets(db, decrypt_key)?;
    for log in &mut page.items {
        if let Some(secret) = secrets.get(&log.account_id) {
            log.error_message = log
                .error_message
                .take()
                .map(|error| redact_known_secret(&error, secret));
            log.diagnostic = redact_diagnostic(log.diagnostic.take(), std::slice::from_ref(secret));
        }
    }
    enrich_forward_log_page(db, page)
}

fn enrich_forward_log_page(
    db: &Database,
    page: ForwardLogPage,
) -> Result<EnrichedForwardLogPage, ObservabilityError> {
    let attributions = db.query_forward_log_native_attributions(
        &page.items.iter().map(|log| log.id).collect::<Vec<_>>(),
    )?;
    Ok(EnrichedForwardLogPage {
        items: page
            .items
            .into_iter()
            .map(|log| {
                let attribution = attributions.get(&log.id).cloned().unwrap_or_default();
                EnrichedForwardLog {
                    log,
                    requested_model: attribution.requested_model,
                    resolved_alias: attribution.resolved_alias,
                    upstream_model: attribution.upstream_model,
                    native_cost_value: attribution.native_cost_value,
                    native_cost_unit: attribution.native_cost_unit,
                    native_cost_currency: attribution.native_cost_currency,
                }
            })
            .collect(),
        summary: page.summary,
    })
}

pub fn forward_log_models(db: &Database) -> Result<Vec<String>, ObservabilityError> {
    db.list_forward_log_models()
        .map_err(ObservabilityError::from)
}

pub fn forward_log_keys(db: &Database) -> Result<Vec<ForwardLogClientKey>, ObservabilityError> {
    db.list_forward_log_keys().map_err(ObservabilityError::from)
}

pub fn normalize_forward_log_window(
    sort_by: Option<&str>,
    sort_order: Option<&str>,
    start_time: Option<&str>,
    end_time: Option<&str>,
) -> Result<(Option<String>, Option<String>), String> {
    if sort_by.is_some_and(|value| {
        !matches!(
            value,
            "timestamp"
                | "attempt"
                | "prompt_tokens"
                | "completion_tokens"
                | "cached_tokens"
                | "cost"
                | "model"
                | "status"
        )
    }) {
        return Err("invalid sort_by".into());
    }
    if sort_order.is_some_and(|value| !matches!(value, "asc" | "desc")) {
        return Err("invalid sort_order".into());
    }

    let parse_time = |value: Option<&str>, name: &str| -> Result<_, String> {
        value
            .map(|value| {
                DateTime::parse_from_rfc3339(value)
                    .map(|time| {
                        time.with_timezone(&Utc)
                            .to_rfc3339_opts(SecondsFormat::Millis, true)
                    })
                    .map_err(|_| format!("invalid {name}"))
            })
            .transpose()
    };
    let start_time = parse_time(start_time, "start_time")?;
    let end_time = parse_time(end_time, "end_time")?;
    if start_time
        .as_ref()
        .zip(end_time.as_ref())
        .is_some_and(|(start, end)| start > end)
    {
        return Err("start_time must not be after end_time".into());
    }
    Ok((start_time, end_time))
}

pub fn dashboard_account_secrets(
    db: &Database,
    decrypt_key: impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, ObservabilityError> {
    let accounts = db.list_accounts()?;
    Ok(accounts
        .into_iter()
        .filter(|account| !account.key_cipher.is_empty())
        .filter_map(|account| decrypt_key(&account.key_cipher).map(|secret| (account.id, secret)))
        .collect())
}

pub fn redact_known_secrets(text: &str, secrets: &BTreeMap<String, String>) -> String {
    secrets.values().fold(text.to_string(), |text, secret| {
        redact_known_secret(&text, secret)
    })
}

pub fn redact_diagnostic(
    diagnostic: Option<serde_json::Value>,
    secrets: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<serde_json::Value> {
    let mut encoded = diagnostic?.to_string();
    for secret in secrets {
        encoded = redact_known_secret(&encoded, secret.as_ref());
    }
    serde_json::from_str(&encoded).ok()
}
