//! Explicit first-party financial refresh; GETs and inference never fetch.
use crate::dashboard_v3::{MutationExpectation, V3ApiError, parse_mutation_json};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::Account;
use crate::official_api::{self, OfficialApiKind, OfficialApiPrices, OfficialApiStatus};
use crate::state::CoreState;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
};
use chrono::{DateTime, Datelike, TimeZone, Utc};

fn runtime(
    db: &Database,
    id: &str,
    state: &CoreState,
) -> Result<(DynamicProviderRuntime, OfficialApiKind), V3ApiError> {
    let runtime = db
        .list_dynamic_providers()
        .map_err(V3ApiError::internal)?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| V3ApiError::not_found_at(state, "configured provider not found"))?;
    let kind = official_api::kind_for_runtime(&runtime).ok_or_else(|| {
        V3ApiError::invalid_request_at(
            state,
            "official financial evidence is unavailable for this preset or destination",
        )
    })?;
    Ok((runtime, kind))
}
fn account(db: &Database, id: &str, state: &CoreState) -> Result<Account, V3ApiError> {
    db.get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found_at(state, "account not found"))
}
pub(super) fn status(state: &CoreState, id: &str) -> Result<OfficialApiStatus, V3ApiError> {
    let _settings = state.settings_update.lock();
    let db = state.db.lock();
    status_locked(state, &db, id)
}

/// Local official-cash projection. Caller already holds `settings_update` and `db`.
pub(crate) fn status_locked(
    state: &CoreState,
    db: &Database,
    id: &str,
) -> Result<OfficialApiStatus, V3ApiError> {
    let account = account(db, id, state)?;
    let (runtime, kind) = runtime(db, &account.provider_id, state)?;
    let now = state.usage_sync.now();
    let since = Utc
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .single()
        .expect("valid UTC month");
    let (spend, unpriced) = db
        .official_api_spend(&account, since, now)
        .map_err(V3ApiError::internal)?;
    let (lifetime_spend, _) = db
        .official_api_spend(&account, DateTime::<Utc>::UNIX_EPOCH, now)
        .map_err(V3ApiError::internal)?;
    Ok(OfficialApiStatus {
        account_id: id.into(),
        provider_id: runtime.id.clone(),
        kind,
        balance_available: kind.balance_available(),
        balances: db
            .official_api_balances(&account, &runtime)
            .map_err(V3ApiError::internal)?,
        prices: db
            .official_api_prices(&runtime.id, kind)
            .map_err(V3ApiError::internal)?,
        month_started_at: since,
        month_spend: spend,
        lifetime_spend,
        unpriced_requests: unpriced,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}
fn prices(state: &CoreState, id: &str) -> Result<OfficialApiPrices, V3ApiError> {
    let _settings = state.settings_update.lock();
    let db = state.db.lock();
    let (_, kind) = runtime(&db, id, state)?;
    Ok(OfficialApiPrices {
        provider_id: id.into(),
        prices: db
            .official_api_prices(id, kind)
            .map_err(V3ApiError::internal)?,
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}
pub(super) async fn get_status(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<OfficialApiStatus>, V3ApiError> {
    status(&state, &id).map(Json)
}
pub(super) async fn get_prices(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<OfficialApiPrices>, V3ApiError> {
    prices(&state, &id).map(Json)
}

pub(super) async fn refresh_balance(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<OfficialApiStatus>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _refresh = state
        .provider_usage_refresh
        .exclusive(crate::usage_sync::ProviderUsageRefreshGate::balance_key(
            &id,
        ))
        .await;
    let prepared = state
        .prepare_official_balance_refresh(
            &id,
            expectation.expected_revision,
            expectation.process_generation,
        )
        .map_err(|error| map_official_refresh_error(&state, error))?;
    let fetched = official_api::balance::fetch(
        &prepared.config,
        &prepared.key,
        state.process_generation(),
        || state.usage_sync.now(),
    )
    .await;
    state
        .commit_official_balance_refresh(
            &id,
            expectation.expected_revision,
            expectation.process_generation,
            &prepared.account,
            &prepared.provider,
            fetched.map_err(|_| ()),
        )
        .map_err(|error| map_official_refresh_error(&state, error))?;
    status(&state, &id).map(Json)
}

pub(super) async fn refresh_prices(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<OfficialApiPrices>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    let _refresh = state
        .pricing_refresh
        .try_lock()
        .map_err(|_| V3ApiError::conflict_at(&state, "pricing refresh is already running"))?;
    let prepared = state
        .prepare_official_price_refresh(
            &id,
            expectation.expected_revision,
            expectation.process_generation,
        )
        .map_err(|error| map_official_refresh_error(&state, error))?;
    let fetched = official_api::pricing::fetch(
        &prepared.config,
        prepared.kind,
        state.process_generation(),
        || state.usage_sync.now(),
    )
    .await;
    state
        .commit_official_price_refresh(
            &id,
            expectation.expected_revision,
            expectation.process_generation,
            &prepared.provider,
            fetched.map_err(|_| ()),
        )
        .map_err(|error| map_official_refresh_error(&state, error))?;
    prices(&state, &id).map(Json)
}

fn map_official_refresh_error(
    state: &CoreState,
    error: crate::state::OfficialRefreshError,
) -> V3ApiError {
    use crate::state::OfficialRefreshError;
    match error {
        OfficialRefreshError::RevisionConflict => V3ApiError::revision_conflict(state),
        OfficialRefreshError::NotFound(message) => V3ApiError::not_found_at(state, message),
        OfficialRefreshError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        OfficialRefreshError::Conflict(message) => V3ApiError::conflict_at(state, message),
        OfficialRefreshError::Outbound(message) => V3ApiError::outbound_failed(state, message),
        OfficialRefreshError::Throttled(message) => V3ApiError::throttled_at(state, message),
        OfficialRefreshError::Internal(error) => V3ApiError::internal(error),
    }
}
