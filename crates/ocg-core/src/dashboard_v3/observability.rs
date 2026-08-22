//! Read-only observability and application surfaces for Dashboard V3.

use axum::Json;
use axum::extract::State;

use crate::control::observability::{
    self, EnrichedForwardLog, ForwardLogReadQuery, GatewayLogReadQuery, ObservabilityError,
};
use crate::state::CoreState;

use super::V3ApiError;
use super::V3Query;
use super::types::{
    ApplicationModels, DailyCostByModel, DailyCostQuery, DailyModelCost, DashboardSummary,
    ForwardLog, ForwardLogClientKey, ForwardLogKeys, ForwardLogModels, ForwardLogQuery,
    ForwardLogSummary, ForwardLogs, GatewayLog, GatewayLogQuery, GatewayLogs, GatewayStatus,
};

pub(super) async fn get_gateway_status(State(state): State<CoreState>) -> Json<GatewayStatus> {
    let _settings_update = state.settings_update.lock();
    let config = state.config();
    let running = state.gateway.lock().is_some();
    let last_error = if running {
        None
    } else {
        observability::redacted_latest_gateway_error(&state.db.lock(), |cipher| {
            state.decrypt_key(cipher).ok()
        })
    };
    let runtime = observability::gateway_runtime_status(
        running,
        state.active_gateway_port(),
        config.upstream_base_url,
        last_error,
    );
    let (revision, process_generation, pricing_revision) = snapshot_tokens(&state);
    Json(GatewayStatus {
        running: runtime.running,
        port: runtime.port,
        upstream_base_url: runtime.upstream_base_url,
        last_error: runtime.last_error,
        revision,
        process_generation,
        pricing_revision,
    })
}

pub(super) async fn get_application_models(
    State(state): State<CoreState>,
) -> Json<ApplicationModels> {
    let _settings_update = state.settings_update.lock();
    let pricing = state.pricing_snapshot();
    Json(application_models_from_pricing_snapshot(&state, &pricing))
}

fn application_models_from_pricing_snapshot(
    state: &CoreState,
    pricing: &crate::kernel::pricing::PricingSnapshot,
) -> ApplicationModels {
    ApplicationModels {
        models: observability::application_models(pricing, Some(&state.provider_contracts())),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
        pricing_revision: pricing.revision.clone(),
    }
}

pub(super) async fn get_dashboard_summary(
    State(state): State<CoreState>,
) -> Result<Json<DashboardSummary>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let summary = {
        let db = state.db.lock();
        observability::dashboard_summary(&db, state.gateway.lock().is_some(), |cipher| {
            state.decrypt_key(cipher).ok()
        })
        .map_err(|error| map_error(&state, error))?
    };
    let (revision, process_generation, pricing_revision) = snapshot_tokens(&state);
    Ok(Json(DashboardSummary {
        total_accounts: summary.total_accounts as u64,
        available_accounts: summary.available_accounts as u64,
        gateway_running: summary.gateway_running,
        today_cost: summary.today_cost,
        week_cost: summary.week_cost,
        month_cost: summary.month_cost,
        revision,
        process_generation,
        pricing_revision,
    }))
}

pub(super) async fn get_daily_cost_by_model(
    State(state): State<CoreState>,
    V3Query(query): V3Query<DailyCostQuery>,
) -> Result<Json<DailyCostByModel>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let items = observability::daily_cost_by_model(&state.db.lock(), query.days)
        .map_err(|error| map_error(&state, error))?;
    let (revision, process_generation, pricing_revision) = snapshot_tokens(&state);
    Ok(Json(DailyCostByModel {
        items: items
            .into_iter()
            .map(|row| DailyModelCost {
                date: row.date,
                model: row.model,
                cost: row.cost,
            })
            .collect(),
        revision,
        process_generation,
        pricing_revision,
    }))
}

pub(super) async fn get_gateway_logs(
    State(state): State<CoreState>,
    V3Query(query): V3Query<GatewayLogQuery>,
) -> Result<Json<GatewayLogs>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let items = observability::gateway_logs(
        &state.db.lock(),
        GatewayLogReadQuery {
            limit: query.limit,
            request_id: query.request_id,
        },
        |cipher| state.decrypt_key(cipher).ok(),
    )
    .map_err(|error| map_error(&state, error))?;
    let (revision, process_generation, pricing_revision) = snapshot_tokens(&state);
    Ok(Json(GatewayLogs {
        items: items.into_iter().map(gateway_log_from_model).collect(),
        revision,
        process_generation,
        pricing_revision,
    }))
}

pub(super) async fn get_forward_logs(
    State(state): State<CoreState>,
    V3Query(query): V3Query<ForwardLogQuery>,
) -> Result<Json<ForwardLogs>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let page = observability::query_forward_logs(
        &state.db.lock(),
        forward_log_read_query(query),
        |cipher| state.decrypt_key(cipher).ok(),
    )
    .map_err(|error| map_error(&state, error))?;
    let (revision, process_generation, pricing_revision) = snapshot_tokens(&state);
    Ok(Json(ForwardLogs {
        items: page
            .items
            .into_iter()
            .map(forward_log_from_enriched)
            .collect(),
        summary: ForwardLogSummary {
            total_requests: page.summary.total_requests,
            prompt_tokens: page.summary.prompt_tokens,
            completion_tokens: page.summary.completion_tokens,
            cached_tokens: page.summary.cached_tokens,
            cost: page.summary.cost,
        },
        revision,
        process_generation,
        pricing_revision,
    }))
}

pub(super) async fn get_forward_log_models(
    State(state): State<CoreState>,
) -> Result<Json<ForwardLogModels>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let models = observability::forward_log_models(&state.db.lock())
        .map_err(|error| map_error(&state, error))?;
    let (revision, process_generation, pricing_revision) = snapshot_tokens(&state);
    Ok(Json(ForwardLogModels {
        models,
        revision,
        process_generation,
        pricing_revision,
    }))
}

pub(super) async fn get_forward_log_keys(
    State(state): State<CoreState>,
) -> Result<Json<ForwardLogKeys>, V3ApiError> {
    let _settings_update = state.settings_update.lock();
    let keys = observability::forward_log_keys(&state.db.lock())
        .map_err(|error| map_error(&state, error))?;
    let (revision, process_generation, pricing_revision) = snapshot_tokens(&state);
    Ok(Json(ForwardLogKeys {
        keys: keys
            .into_iter()
            .map(|key| ForwardLogClientKey {
                id: key.id,
                name: key.name,
            })
            .collect(),
        revision,
        process_generation,
        pricing_revision,
    }))
}

fn snapshot_tokens(state: &CoreState) -> (u64, u64, String) {
    (
        state.settings_revision(),
        state.process_generation(),
        state.pricing_snapshot().revision.clone(),
    )
}

fn map_error(state: &CoreState, error: ObservabilityError) -> V3ApiError {
    match error {
        ObservabilityError::InvalidQuery(message) => V3ApiError::invalid_request_at(state, message),
        ObservabilityError::Internal(error) => V3ApiError::internal(error),
    }
}

fn forward_log_read_query(query: ForwardLogQuery) -> ForwardLogReadQuery {
    ForwardLogReadQuery {
        limit: query.limit,
        offset: query.offset,
        status: query.status,
        account_id: query.account_id,
        provider_id: query.provider_id,
        offering_id: query.offering_id,
        route_account_id: query.route_account_id,
        credential_account_id: query.credential_account_id,
        model: query.model,
        request_id: query.request_id,
        key_id: query.key_id,
        start_time: query.start_time,
        end_time: query.end_time,
        sort_by: query.sort_by,
        sort_order: query.sort_order,
    }
}

fn gateway_log_from_model(log: crate::models::GatewayLog) -> GatewayLog {
    GatewayLog {
        id: log.id,
        level: log.level,
        category: log.category,
        message: log.message,
        created_at: log.created_at.to_rfc3339(),
        request_id: log.request_id,
        attempt: log.attempt,
        error_source: log.error_source,
        error_stage: log.error_stage,
        duration_ms: log.duration_ms,
        diagnostic: log.diagnostic,
    }
}

fn forward_log_from_enriched(item: EnrichedForwardLog) -> ForwardLog {
    let log = item.log;
    ForwardLog {
        id: log.id,
        timestamp: log.timestamp.to_rfc3339(),
        model: log.model,
        account_id: log.account_id,
        account_name: log.account_name,
        route_account_id: log.route_account_id,
        provider_id: log.provider_id,
        offering_id: log.offering_id,
        credential_account_id: log.credential_account_id,
        client_key_id: log.client_key_id,
        client_key_name: log.client_key_name,
        status: log.status,
        http_status: log.http_status,
        route: log.route,
        prompt_tokens: log.prompt_tokens,
        completion_tokens: log.completion_tokens,
        cached_tokens: log.cached_tokens,
        cache_creation_tokens: log.cache_creation_tokens,
        cost: log.cost,
        raw_cost_usd: log.raw_cost_usd,
        quota_debit: log.quota_debit,
        effective_paid_cost_usd: log.effective_paid_cost_usd,
        pricing_revision_id: log.pricing_revision_id,
        quota_multiplier: log.quota_multiplier,
        local_adjustment_multiplier: log.local_adjustment_multiplier,
        service_tier: log.service_tier,
        cost_state: log.cost_state,
        error_message: log.error_message,
        request_id: log.request_id,
        attempt: log.attempt,
        error_source: log.error_source,
        error_stage: log.error_stage,
        duration_ms: log.duration_ms,
        diagnostic: log.diagnostic,
        requested_model: item.requested_model,
        resolved_alias: item.resolved_alias,
        upstream_model: item.upstream_model,
        native_cost_value: item.native_cost_value,
        native_cost_unit: item.native_cost_unit,
        native_cost_currency: item.native_cost_currency,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use std::sync::Arc;

    #[test]
    fn application_models_keeps_revision_and_models_on_the_captured_pricing_snapshot() {
        let dir = std::env::temp_dir().join(format!(
            "ocg-v3-observability-pricing-snapshot-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(dir.clone()).unwrap();
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("v3-observability"));
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let captured = state.pricing_snapshot();
        let expected_models =
            observability::application_models(&captured, Some(&state.provider_contracts()));
        let mut concurrent = captured.as_ref().clone();
        concurrent
            .models
            .retain(|model| model.model_id == "grok-4.5");
        concurrent.revision = "concurrent-v2-activation".into();
        concurrent.activated_at = "2099-01-01T00:00:00Z".into();
        state.activate_pricing_snapshot(concurrent).unwrap();

        let response = application_models_from_pricing_snapshot(&state, &captured);
        assert_eq!(response.pricing_revision, captured.revision);
        assert_eq!(response.models, expected_models);
        assert_ne!(response.pricing_revision, state.pricing_snapshot().revision);

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }
}
