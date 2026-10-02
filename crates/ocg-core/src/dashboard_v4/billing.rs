//! One account billing view; provider observers retain their existing adapters.

use crate::account_control::{AccountControlError, CreditMutation, MutationCas};
use crate::billing::{billing_model_for_destination, stepfun_plan_credits};
use crate::billing_types::{
    BillingModel, BillingSource, BillingStatus, CreditCalibrationRequest, CreditConfigureRequest,
    CreditGrantRequest,
};
use crate::dashboard_v3::{MutationExpectation, V3ApiError, parse_mutation_json};
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
    status(&state, &id).map(Json)
}

fn status(state: &CoreState, id: &str) -> Result<BillingStatus, V3ApiError> {
    // Existing observer projections acquire their own locks. Check the revision
    // around composition rather than recursively acquiring the settings mutex.
    for _ in 0..3 {
        let (revision, provider_id, adapter, endpoint, configurable, credits, official_cash) = {
            let _settings = state.settings_update.lock();
            let db = state.db.lock();
            let account = db
                .get_account(id)
                .map_err(V3ApiError::internal)?
                .ok_or_else(|| V3ApiError::not_found_at(state, "account not found"))?;
            let (adapter, endpoint, legacy_kind) = destination(&db.conn, id)
                .map_err(V3ApiError::internal)?
                .ok_or_else(|| V3ApiError::not_found_at(state, "account destination not found"))?;
            let configurable = adapter == AdapterKind::Http
                && matches!(legacy_kind.as_str(), "dynamic" | "custom_account");
            let credits =
                storage::read_view_on(&db.conn, id, Utc::now()).map_err(V3ApiError::internal)?;
            let official_cash = db
                .get_dynamic_provider(&account.provider_id)
                .map_err(V3ApiError::internal)?
                .as_ref()
                .and_then(crate::official_api::kind_for_runtime)
                .is_some();
            (
                state.settings_revision(),
                account.provider_id,
                adapter,
                endpoint,
                configurable,
                credits,
                official_cash,
            )
        };
        let usage = crate::dashboard_v3::usage::load_provider_usage(state, id)?;
        let model = if credits.is_some() {
            BillingModel::Credits
        } else {
            billing_model_for_destination(adapter, &endpoint)
        };
        let cash = if official_cash && model == BillingModel::Cash {
            Some(super::official_api::status(state, id)?)
        } else {
            None
        };
        if revision != state.settings_revision() {
            continue;
        }
        return Ok(project_billing(
            state,
            id,
            revision,
            &provider_id,
            adapter,
            &endpoint,
            configurable,
            credits,
            usage,
            cash,
        ));
    }
    Err(V3ApiError::conflict_at(
        state,
        "billing configuration changed during read",
    ))
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
    mutation: CreditMutation<'_>,
) -> Result<BillingStatus, V3ApiError> {
    let applied = crate::account_control::apply_credit_mutation(
        state,
        id,
        MutationCas {
            expected_revision: expectation.expected_revision,
            process_generation: expectation.process_generation,
        },
        mutation,
    )
    .map_err(|error| map_credit_error(state, error))?;
    let mut projected = project_billing(
        state,
        id,
        applied.revision,
        &applied.provider_id,
        applied.adapter,
        &applied.endpoint,
        true,
        applied.credits,
        applied.usage,
        applied.cash,
    );
    projected.revision = applied.revision;
    if let Some(usage) = projected.usage.as_mut() {
        usage.revision = applied.revision;
    }
    if let Some(cash) = projected.cash.as_mut() {
        cash.revision = applied.revision;
    }
    Ok(projected)
}

fn map_credit_error(state: &CoreState, error: AccountControlError) -> V3ApiError {
    match error {
        AccountControlError::NotFound => V3ApiError::not_found_at(state, "account not found"),
        AccountControlError::Invalid(message) => V3ApiError::invalid_request_at(state, message),
        AccountControlError::RevisionConflict => V3ApiError::revision_conflict(state),
        AccountControlError::Conflict(message) => V3ApiError::conflict_at(state, message),
        AccountControlError::Unavailable(message) => {
            V3ApiError::precondition_failed_at(state, message)
        }
        AccountControlError::Internal(error) => V3ApiError::internal(error),
    }
}

pub(super) async fn configure(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<BillingStatus>, V3ApiError> {
    let input = parse_mutation_json::<CreditConfigureRequest>(&body)?;
    mutate(
        &state,
        &id,
        &input.expectation,
        CreditMutation::Configure {
            configuration: input.configuration,
            initial_buckets: input.initial_buckets,
        },
    )
    .map(Json)
}

pub(super) async fn calibrate(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<BillingStatus>, V3ApiError> {
    let input = parse_mutation_json::<CreditCalibrationRequest>(&body)?;
    mutate(
        &state,
        &id,
        &input.expectation,
        CreditMutation::Calibrate {
            balances: &input.balances,
        },
    )
    .map(Json)
}

pub(super) async fn grant(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<BillingStatus>, V3ApiError> {
    let input = parse_mutation_json::<CreditGrantRequest>(&body)?;
    mutate(
        &state,
        &id,
        &input.expectation,
        CreditMutation::Grant {
            label: input.label,
            amount: input.amount,
            expires_at: input.expires_at,
        },
    )
    .map(Json)
}

pub(super) async fn disable(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<BillingStatus>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    mutate(&state, &id, &expectation, CreditMutation::Disable).map(Json)
}

#[cfg(test)]
mod tests;
