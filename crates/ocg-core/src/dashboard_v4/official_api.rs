//! Explicit first-party financial refresh; GETs and inference never fetch.
use crate::dashboard_v3::{
    MutationExpectation, V3ApiError, check_expectation, parse_mutation_json,
};
use crate::db::Database;
use crate::dynamic::DynamicProviderRuntime;
use crate::models::{Account, AccountSetupStep};
use crate::official_api::{self, OfficialApiKind, OfficialApiPrices, OfficialApiStatus};
use crate::state::CoreState;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, State},
};
use chrono::{DateTime, Datelike, TimeZone, Utc};
use ocg_domain::connection::{
    EndpointOperation, LegacyConnectionKind, connection_id_for_legacy, endpoint_id_for,
};

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
    let account = account(&db, id, state)?;
    let (runtime, kind) = runtime(&db, &account.provider_id, state)?;
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

/// A billing read uses only a Key whose saved default endpoint and Origin are
/// both still granted. It does not grant a new billing destination on its own.
fn require_grant(
    db: &Database,
    account: &Account,
    runtime: &DynamicProviderRuntime,
    state: &CoreState,
) -> Result<(), V3ApiError> {
    let binding = db
        .list_inference_bindings()
        .map_err(V3ApiError::internal)?
        .into_iter()
        .find(|b| b.account_id == account.id)
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(state, "selected credential binding is unavailable")
        })?;
    let endpoint = endpoint_id_for(
        &connection_id_for_legacy(LegacyConnectionKind::DynamicProvider, &runtime.id),
        EndpointOperation::from(runtime.upstream_protocol),
    )
    .to_string();
    if !binding.enabled
        || !binding.allowed_endpoint_ids.contains(&endpoint)
        || crate::custom_http::ensure_secret_origin_granted(
            official_api::BALANCE_URL,
            &binding.allowed_origins,
        )
        .is_err()
    {
        return Err(V3ApiError::invalid_request_at(
            state,
            "the official destination is not authorized for this Key",
        ));
    }
    Ok(())
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
    let (snapshot, provider, config, key) = {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        let db = state.db.lock();
        let account = account(&db, &id, &state)?;
        let (runtime, kind) = runtime(&db, &account.provider_id, &state)?;
        if !kind.balance_available() {
            return Err(V3ApiError::invalid_request_at(
                &state,
                "no supported public balance API is configured for this provider",
            ));
        }
        if account.setup_step != AccountSetupStep::Ready || account.key_cipher.is_empty() {
            return Err(V3ApiError::invalid_request_at(
                &state,
                "a ready account with a stored Key is required",
            ));
        }
        require_grant(&db, &account, &runtime, &state)?;
        if crate::usage_sync::manual_next_allowed_at(
            db.account_usage_sync_state(&id)
                .map_err(V3ApiError::internal)?
                .and_then(|s| s.last_attempt_at),
            state.usage_sync.now(),
        )
        .is_some()
        {
            return Err(V3ApiError::throttled_at(
                &state,
                "official balance refresh is limited to once per 15 seconds",
            ));
        }
        let key = state
            .decrypt_key(&account.key_cipher)
            .map_err(V3ApiError::internal)?;
        (account, runtime, state.config(), key)
    };
    let fetched = official_api::balance::fetch(&config, &key, state.process_generation(), || {
        state.usage_sync.now()
    })
    .await;
    drop(key);
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        let db = state.db.lock();
        let current = account(&db, &id, &state)?;
        let (current_provider, _) = runtime(&db, &provider.id, &state)?;
        if current.key_cipher != snapshot.key_cipher
            || current.updated_at != snapshot.updated_at
            || current.provider_id != snapshot.provider_id
            || current_provider != provider
        {
            return Err(V3ApiError::conflict_at(
                &state,
                "account or provider changed during official balance refresh",
            ));
        }
        require_grant(&db, &current, &current_provider, &state)?;
        let now = state.usage_sync.now();
        match fetched {
            Ok(balances) => db
                .store_official_api_balances(&current, &provider, &balances, now)
                .map_err(V3ApiError::internal)?,
            Err(_) => {
                db.touch_account_usage_sync_attempt(&id, now)
                    .map_err(V3ApiError::internal)?;
                return Err(V3ApiError::outbound_failed(
                    &state,
                    "official balance refresh failed; previous evidence retained",
                ));
            }
        }
    }
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
    let (provider, kind, config) = {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        let db = state.db.lock();
        let (provider, kind) = runtime(&db, &id, &state)?;
        (provider, kind, state.config())
    };
    let fetched = official_api::pricing::fetch(&config, kind, state.process_generation(), || {
        state.usage_sync.now()
    })
    .await;
    {
        let _settings = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        let db = state.db.lock();
        let (current, _) = runtime(&db, &id, &state)?;
        if current != provider {
            return Err(V3ApiError::conflict_at(
                &state,
                "provider changed during official pricing refresh",
            ));
        }
        let sheet = fetched.map_err(|_| {
            V3ApiError::outbound_failed(
                &state,
                "official pricing refresh failed; previous evidence retained",
            )
        })?;
        db.store_official_api_prices(&current, &sheet)
            .map_err(V3ApiError::internal)?;
    }
    prices(&state, &id).map(Json)
}
