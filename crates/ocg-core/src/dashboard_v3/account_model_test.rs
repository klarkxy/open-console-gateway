//! Exact-account operational model test for the Accounts page.
//!
//! This intentionally differs from provider protocol probes: it never selects
//! a sibling account, writes protocol evidence, changes account state, or
//! requires the account to be enabled/available. It only verifies that the
//! requested account can currently serve one admitted model.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use std::time::Instant;

use crate::gateway::protocol::CustomRouteSpec;
use crate::kernel::protocol::ApiFormat;
use crate::models::Account as ModelAccount;
use crate::provider::{
    ProviderAdapterKind, UpstreamProtocolKind, builtin_provider, is_cpa_external_integration,
    plan_requires_custom_config,
};
use crate::provider_contracts::{ContractScope, protocol_from_api, select_upstream_protocol};
use crate::state::CoreState;
use ocg_domain::destination::{AdapterKind, AuthScheme, CatalogModel, Destination};

use super::accounts::load_model_account;
use super::types::{AccountModelTestRequest, AccountModelTestResponse, AccountUpstreamProtocol};
use super::{V3ApiError, parse_json};

pub(super) async fn test_account_model(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountModelTestResponse>, V3ApiError> {
    let input = parse_json::<AccountModelTestRequest>(&body)?;
    let prepared = prepare_account_model_test(&state, &id, input)?;
    let started = Instant::now();
    let (success, http_status, error) = match crate::protocol_probe::execute_account_model_test(
        crate::protocol_probe::AccountModelTestInput {
            state: &state,
            config: &prepared.config,
            account: &prepared.account,
            adapter: prepared.adapter,
            public_model: &prepared.public_model,
            model_id: &prepared.upstream_model,
            protocol: prepared.protocol,
            custom_route: prepared.custom_route,
        },
    )
    .await
    {
        Ok(status) => (true, Some(status), None),
        Err((status, message)) => (false, status, Some(message)),
    };
    Ok(Json(AccountModelTestResponse {
        account_id: prepared.account.id,
        model_id: prepared.public_model,
        protocol: AccountUpstreamProtocol::from(prepared.protocol),
        success,
        http_status,
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        error,
    }))
}

struct PreparedAccountModelTest {
    account: ModelAccount,
    config: crate::models::AppConfig,
    adapter: ProviderAdapterKind,
    public_model: String,
    upstream_model: String,
    protocol: UpstreamProtocolKind,
    /// Route chosen once here. Later transport construction consumes it.
    custom_route: Option<CustomRouteSpec>,
}

fn prepare_account_model_test(
    state: &CoreState,
    id: &str,
    input: AccountModelTestRequest,
) -> Result<PreparedAccountModelTest, V3ApiError> {
    let account = load_model_account(state, id)?;
    if !account.setup_step.is_ready() {
        return Err(V3ApiError::precondition_failed_at(
            state,
            "finish account setup before testing a model",
        ));
    }
    let model_id = input.model_id.trim();
    if model_id.is_empty() {
        return Err(V3ApiError::invalid_request_at(state, "modelId is required"));
    }
    let projection = crate::destination_projection::load_runtime(&state.db.lock())
        .map_err(V3ApiError::internal)?;
    let destination = projection
        .credentials
        .iter()
        .find(|credential| credential.legacy_account_id == id)
        .and_then(|credential| {
            projection
                .destinations
                .iter()
                .find(|destination| destination.id == credential.destination_id)
        });
    if let Some(destination) = destination.filter(|destination| {
        destination.adapter == AdapterKind::Http && !destination.capabilities.observer
    }) {
        return prepare_http_destination_model_test(state, account, destination, model_id);
    }
    let plan = builtin_provider(&account.provider_id)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "unknown provider offering"))?;
    let adapter = ProviderAdapterKind::from_provider_id(&account.provider_id)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "unknown provider offering"))?;
    if plan_requires_custom_config(plan) {
        return Err(V3ApiError::invalid_request_at(
            state,
            "Custom API accounts require a persisted endpoint URL and upstream protocol",
        ));
    }

    let scope = ContractScope::from_account(&account)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "unknown provider offering"))?;
    let contracts = state.provider_contracts();
    let contract = contracts
        .scope(&scope)
        .ok_or_else(|| V3ApiError::invalid_request_at(state, "unknown provider offering"))?;
    if !contract.model(model_id).is_some_and(|model| model.routable) {
        return Err(V3ApiError::invalid_request_at(
            state,
            "model is not routable for this provider",
        ));
    }
    let client = if is_cpa_external_integration(&account.provider_id) {
        ApiFormat::ChatCompletions
    } else {
        ApiFormat::Gemini
    };
    let selected = select_upstream_protocol(contract, client, model_id)
        .map_err(|error| V3ApiError::invalid_request_at(state, error.message))?;
    let protocol = protocol_from_api(selected).ok_or_else(|| {
        V3ApiError::invalid_request_at(state, "model is not routable for this provider")
    })?;

    Ok(PreparedAccountModelTest {
        account,
        config: state.config(),
        adapter,
        public_model: model_id.to_string(),
        upstream_model: model_id.to_string(),
        protocol,
        custom_route: None,
    })
}

fn prepare_http_destination_model_test(
    state: &CoreState,
    account: ModelAccount,
    destination: &Destination,
    model_id: &str,
) -> Result<PreparedAccountModelTest, V3ApiError> {
    let mapping = destination
        .catalog
        .iter()
        .find(|model| crate::custom::custom_model_id_matches(&model.public_model, model_id))
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(state, "model is not declared for this account")
        })?;
    let protocol = selected_http_test_protocol(destination, mapping).ok_or_else(|| {
        V3ApiError::invalid_request_at(state, "model is not routable for this provider")
    })?;
    let route = ocg_domain::destination::http_model_route(destination, mapping, protocol)
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(state, "model protocol has no configured route")
        })?;
    Ok(PreparedAccountModelTest {
        account,
        config: state.config(),
        adapter: ProviderAdapterKind::ConfigurableHttp,
        public_model: mapping.public_model.clone(),
        upstream_model: mapping.upstream_model.clone(),
        protocol,
        custom_route: Some(CustomRouteSpec {
            endpoint_url: route.endpoint_url,
            auth_kind: match route.auth_scheme {
                AuthScheme::Bearer => ocg_domain::dynamic::DynamicAuthKind::Bearer,
                AuthScheme::XApiKey => ocg_domain::dynamic::DynamicAuthKind::XApiKey,
                AuthScheme::ApiKey => ocg_domain::dynamic::DynamicAuthKind::ApiKey,
                AuthScheme::None => ocg_domain::dynamic::DynamicAuthKind::None,
            },
        }),
    })
}

fn selected_http_test_protocol(
    destination: &Destination,
    model: &CatalogModel,
) -> Option<UpstreamProtocolKind> {
    if let Some(route) = &model.upstream_override {
        return Some(route.protocol);
    }
    let available = ocg_domain::destination::http_model_protocols(destination, model);
    if let Some(preferred) = model
        .preferred
        .filter(|protocol| available.contains(protocol))
    {
        return Some(preferred);
    }
    available.into_iter().next()
}

#[cfg(test)]
mod tests;
