//! One account billing view; provider observers retain their existing adapters.

use crate::billing::{billing_model_for_destination, stepfun_plan_credits};
use crate::billing_types::{
    BillingModel, BillingSource, BillingStatus, CreditCalibrationRequest, CreditConfigureRequest,
    CreditGrantRequest,
};
use crate::dashboard_v3::{
    MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::db::billing as storage;
use crate::provider::ProviderRegistry;
use crate::state::CoreState;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
};
use chrono::Utc;
use ocg_domain::destination::AdapterKind;
use rusqlite::{Connection, OptionalExtension};

pub(super) async fn get_status(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<BillingStatus>, V3ApiError> {
    tokio::task::spawn_blocking(move || status(&state, &id).map(Json))
        .await
        .map_err(V3ApiError::internal)?
}

fn status(state: &CoreState, id: &str) -> Result<BillingStatus, V3ApiError> {
    let _settings = state.settings_update.lock();
    let db = state.db.lock();
    cached_status(state, &db, id)
}

fn cached_status(
    state: &CoreState,
    db: &crate::db::Database,
    id: &str,
) -> Result<BillingStatus, V3ApiError> {
    use super::billing_cache::ReadVersion;
    for _ in 0..3 {
        let before = ReadVersion::capture(state, db).map_err(V3ApiError::internal)?;
        let now = Utc::now();
        if let Some(status) = state.billing_cache.lock().get(id, &before, now) {
            return Ok(status);
        }
        let status = compose_status(state, db, id)?;
        let next_credit_start = if status.credits.is_some() {
            storage::load_on(&db.conn, id)
                .map_err(V3ApiError::internal)?
                .and_then(|meter| {
                    meter
                        .buckets
                        .iter()
                        .map(|bucket| bucket.starts_at)
                        .filter(|at| *at > now)
                        .min()
                })
        } else {
            None
        };
        let after = ReadVersion::capture(state, db).map_err(V3ApiError::internal)?;
        // Read helpers may advance windows, and another SQLite connection may write.
        // Retry instead of caching a composition under a version it did not observe.
        if before != after {
            continue;
        }
        let mut cache = state.billing_cache.lock();
        cache.insert(&after, status.clone(), now);
        if let Some(at) = next_credit_start {
            cache.shorten_lifetime(id, at);
        }
        return Ok(status);
    }
    Err(V3ApiError::conflict_at(
        state,
        "billing data changed during read",
    ))
}

fn compose_status(
    state: &CoreState,
    db: &crate::db::Database,
    id: &str,
) -> Result<BillingStatus, V3ApiError> {
    let account = db
        .get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "account not found"))?;
    let (adapter, endpoint, legacy_kind) = destination(&db.conn, id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "account destination not found"))?;
    let configurable = adapter == AdapterKind::Http
        && matches!(legacy_kind.as_str(), "dynamic" | "custom_account");
    let credits = storage::read_view_on(&db.conn, id, Utc::now()).map_err(V3ApiError::internal)?;
    let official_cash = db
        .get_dynamic_provider(&account.provider_id)
        .map_err(V3ApiError::internal)?
        .as_ref()
        .and_then(crate::official_api::kind_for_runtime)
        .is_some();
    let usage = crate::dashboard_v3::usage::provider_usage_from_db(state, db, id)?;
    let model = if credits.is_some() {
        BillingModel::Credits
    } else {
        billing_model_for_destination(adapter, &endpoint)
    };
    let cash = if official_cash && model == BillingModel::Cash {
        Some(super::official_api::status_locked(state, db, id)?)
    } else {
        None
    };
    Ok(project_billing(
        state,
        id,
        state.settings_revision(),
        &account.provider_id,
        adapter,
        &endpoint,
        configurable,
        credits,
        usage,
        cash,
    ))
}

/// Local read only. Never fetches upstream and never starts background I/O.
/// Per-account errors cannot prevent other accounts from receiving snapshots.
pub(super) async fn snapshots(
    State(state): State<CoreState>,
    body: Bytes,
) -> Result<Json<crate::billing_types::BillingSnapshots>, V3ApiError> {
    let input =
        crate::dashboard_v3::parse_json::<crate::billing_types::BillingSnapshotRequest>(&body)?;
    if input.account_ids.len() > 64
        || input
            .account_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 256)
    {
        return Err(V3ApiError::invalid_request_at(
            &state,
            "at most 64 valid account ids are allowed",
        ));
    }
    tokio::task::spawn_blocking(move || {
        let _settings = state.settings_update.lock();
        let db = state.db.lock();
        let mut statuses = Vec::new();
        let mut errors = std::collections::BTreeMap::new();
        let mut seen = std::collections::HashSet::new();
        for id in input.account_ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            match cached_status(&state, &db, &id) {
                Ok(status) => statuses.push(status),
                Err(error) => {
                    errors.insert(id, error.envelope().clone());
                }
            }
        }
        Json(crate::billing_types::BillingSnapshots {
            statuses,
            errors,
            revision: state.settings_revision(),
            process_generation: state.process_generation(),
        })
    })
    .await
    .map_err(V3ApiError::internal)
}

fn destination(
    conn: &Connection,
    id: &str,
) -> anyhow::Result<Option<(AdapterKind, String, String)>> {
    let row: Option<(String, Option<String>, String)> = conn
        .query_row(
            "SELECT d.adapter, d.base_url, d.legacy_kind FROM credentials c
         JOIN destinations d ON d.id = c.destination_id
         WHERE c.legacy_account_id = ?1
           AND COALESCE(c.credential_purpose, 'inference') = 'inference'",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    row.map(|(adapter, endpoint, legacy)| {
        let kind = AdapterKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == adapter)
            .ok_or_else(|| anyhow::anyhow!("unknown billing adapter"))?;
        Ok((kind, endpoint.unwrap_or_default(), legacy))
    })
    .transpose()
}

// Compose the already-read receipt without reacquiring observer or database locks.
#[allow(clippy::too_many_arguments)]
fn project_billing(
    state: &CoreState,
    id: &str,
    revision: u64,
    provider_id: &str,
    adapter: AdapterKind,
    endpoint: &str,
    configurable: bool,
    credits: Option<crate::billing_types::CreditMeterView>,
    mut usage: crate::dashboard_v3::ProviderUsage,
    cash: Option<crate::official_api::OfficialApiStatus>,
) -> BillingStatus {
    let model = if credits.is_some() {
        BillingModel::Credits
    } else {
        billing_model_for_destination(adapter, endpoint)
    };
    let mut cash = if cash.is_some() && model == BillingModel::Cash {
        cash
    } else {
        None
    };
    let descriptor = ProviderRegistry::get(provider_id);
    let manual_calibration =
        credits.is_some() || descriptor.is_some_and(|entry| entry.usage.manual_calibration);
    let official_refresh = model != BillingModel::Credits
        && (descriptor.is_some_and(|entry| entry.card_actions.usage_refresh)
            || (configurable && crate::api_balance::probe_from_endpoint(endpoint).is_some()));
    let source = if credits.is_some()
        || matches!(
            adapter,
            AdapterKind::OpencodeGo | AdapterKind::Goat | AdapterKind::Ollama | AdapterKind::Zen
        ) {
        BillingSource::LocalEstimate
    } else if cash
        .as_ref()
        .is_some_and(|value| !value.balances.is_empty())
        || !usage.credit_balances.is_empty()
        || !usage.quota_windows.is_empty()
    {
        BillingSource::Official
    } else {
        BillingSource::Unavailable
    };
    let unit = if model == BillingModel::Credits && adapter != AdapterKind::Ollama {
        "credits".to_string()
    } else if let Some(window) = usage.quota_windows.first() {
        window.unit.clone()
    } else if let Some(balance) = usage.credit_balances.first() {
        balance.unit.clone()
    } else {
        "currency".to_string()
    };
    usage.revision = revision;
    if let Some(cash) = cash.as_mut() {
        cash.revision = revision;
    }
    BillingStatus {
        account_id: id.into(),
        model,
        source,
        unit,
        configurable_credits: configurable,
        manual_calibration,
        official_refresh,
        usage: Some(usage),
        cash,
        credits,
        presets: if configurable {
            stepfun_plan_credits(endpoint, Utc::now()).unwrap_or_default()
        } else {
            Vec::new()
        },
        revision,
        process_generation: state.process_generation(),
    }
}

fn mutate(
    state: &CoreState,
    id: &str,
    expectation: &MutationExpectation,
    apply: impl FnOnce(&Connection) -> anyhow::Result<()>,
) -> Result<BillingStatus, V3ApiError> {
    let _settings = state.settings_update.lock();
    check_expectation(state, expectation)?;
    let db = state.db.lock();
    let (adapter, endpoint, legacy) = destination(&db.conn, id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "account not found"))?;
    if adapter != AdapterKind::Http || !matches!(legacy.as_str(), "dynamic" | "custom_account") {
        return Err(V3ApiError::invalid_request_at(
            state,
            "this account uses its provider billing contract",
        ));
    }
    let account = db
        .get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "account not found"))?;
    let configurable = true;
    let official_cash = db
        .get_dynamic_provider(&account.provider_id)
        .map_err(V3ApiError::internal)?
        .as_ref()
        .and_then(crate::official_api::kind_for_runtime)
        .is_some();
    // Usage does not depend on the credit write. Failure here persists nothing.
    let usage = crate::dashboard_v3::usage::provider_usage_from_db(state, &db, id)?;
    let transaction = db
        .conn
        .unchecked_transaction()
        .map_err(V3ApiError::internal)?;
    apply(&transaction).map_err(|error| {
        if error.downcast_ref::<rusqlite::Error>().is_some() {
            V3ApiError::internal(error)
        } else {
            V3ApiError::invalid_request_at(state, error.to_string())
        }
    })?;
    // Clock must be after apply so a grant bucket that starts at `now` is active.
    let credits =
        storage::read_view_on(&transaction, id, Utc::now()).map_err(V3ApiError::internal)?;
    let model = if credits.is_some() {
        BillingModel::Credits
    } else {
        billing_model_for_destination(adapter, &endpoint)
    };
    // GET reads official cash only when that model is selected. Keep the read
    // inside this transaction so a cash failure rolls the credit write back.
    let cash = if official_cash && model == BillingModel::Cash {
        Some(super::official_api::status_locked(state, &db, id)?)
    } else {
        None
    };
    let mut projected = project_billing(
        state,
        id,
        state.settings_revision(),
        &account.provider_id,
        adapter,
        &endpoint,
        configurable,
        credits,
        usage,
        cash,
    );
    transaction.commit().map_err(V3ApiError::internal)?;
    let revision = state.bump_settings_revision();
    projected.revision = revision;
    if let Some(usage) = projected.usage.as_mut() {
        usage.revision = revision;
    }
    if let Some(cash) = projected.cash.as_mut() {
        cash.revision = revision;
    }
    Ok(projected)
}

pub(super) async fn configure(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<BillingStatus>, V3ApiError> {
    let input = parse_mutation_json::<CreditConfigureRequest>(&body)?;
    mutate(&state, &id, &input.expectation, |conn| {
        storage::configure_on(
            conn,
            &id,
            input.configuration,
            input.initial_buckets,
            Utc::now(),
        )
    })
    .map(Json)
}

pub(super) async fn calibrate(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<BillingStatus>, V3ApiError> {
    let input = parse_mutation_json::<CreditCalibrationRequest>(&body)?;
    mutate(&state, &id, &input.expectation, |conn| {
        storage::calibrate_on(conn, &id, &input.balances, Utc::now())
    })
    .map(Json)
}

pub(super) async fn grant(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<BillingStatus>, V3ApiError> {
    let input = parse_mutation_json::<CreditGrantRequest>(&body)?;
    mutate(&state, &id, &input.expectation, |conn| {
        storage::grant_on(
            conn,
            &id,
            input.label,
            input.amount,
            input.expires_at,
            Utc::now(),
        )
    })
    .map(Json)
}

pub(super) async fn disable(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<BillingStatus>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    mutate(&state, &id, &expectation, |conn| {
        storage::disable_on(conn, &id, Utc::now())
    })
    .map(Json)
}

#[cfg(test)]
mod tests;
