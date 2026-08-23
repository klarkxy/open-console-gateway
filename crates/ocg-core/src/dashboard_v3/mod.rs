//! Dashboard V3 HTTP contract kernel.
//!
//! Mounted at `/dashboard/api/v3` beside the unchanged V2 `/dashboard/api`
//! router. This module owns the shared DTO / error / CAS envelope, process
//! generation, public auth/session issuance, connection/settings reads, the settings write path,
//! access-key lifecycle, the local accounts control plane, local account usage
//! calibration and provider-usage reads, the local/Zen provider catalog,
//! contracts, Zen Free control plane, pricing, the settings proxy diagnostic,
//! read-only observability, Go/Zen protocol probes, and the Claude Desktop
//! three-role model mapping. Custom protocol probes stay account-owned on V2.

mod accounts;
mod auth;
mod claude_desktop;
mod connection;
mod keys;
mod observability;
mod pricing;
mod providers;
mod proxy_test;
mod settings;
mod types;
mod usage;

use axum::extract::{FromRequestParts, Query, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::dashboard_session;
use crate::state::CoreState;

#[cfg(debug_assertions)]
pub use providers::set_zen_models_source_url_override_for_tests;
pub use proxy_test::PROXY_TEST_TARGET;
#[cfg(debug_assertions)]
pub use proxy_test::{ProxyTestTargetGuard, install_proxy_test_target_for_tests};
pub use types::{
    Account, AccountAcknowledgement, AccountAcknowledgementCreate, AccountAcknowledgementWrite,
    AccountAuthScheme, AccountCreate, AccountCredentialKind, AccountCustomConfig,
    AccountCustomConfigUpdate, AccountCustomConfigWrite, AccountList, AccountManagedCreate,
    AccountModelCapabilitiesUpdate, AccountModelCapability, AccountModelCapabilityWrite,
    AccountMutation, AccountOrder, AccountQuotaScope, AccountSetupStep, AccountSetupUpdate,
    AccountType, AccountUpdate, AccountUpstreamProtocol, AccountUsageUpdate,
    AccountVerificationStatus, ApplicationModels, AuthLogin, AuthLogout, AuthRegister, AuthStatus,
    CATALOG_TYPE_NAMES, CapabilitySummary, CardCapabilitySummary, ClaudeDesktopModels,
    ClaudeDesktopModelsUpdate, ConnectionInfo, ConnectionSubKey, ContractScopeKind,
    ControlRevision, CreditBalance, CustomEndpointContract, DailyCostByModel, DailyCostQuery,
    DailyModelCost, DashboardSummary, ERROR_CONFLICT, ERROR_INTERNAL, ERROR_INVALID_JSON,
    ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION, ERROR_NOT_FOUND, ERROR_NOT_IMPLEMENTED,
    ERROR_OUTBOUND_FAILED, ERROR_PRECONDITION_FAILED, ERROR_REVISION_CONFLICT,
    ERROR_SERVICE_UNAVAILABLE, ERROR_UNAUTHORIZED, EffectiveCatalog, EffectiveModelContract,
    EffectiveModelProtocols, EffectiveProtocolEvidence, ForwardLog, ForwardLogClientKey,
    ForwardLogKeys, ForwardLogModels, ForwardLogQuery, ForwardLogSummary, ForwardLogs, GatewayLog,
    GatewayLogQuery, GatewayLogs, GatewayStatus, KeyCreate, KeyUpdate, MutationAck,
    MutationExpectation, PricingAdjustment, PricingAvailability, PricingLimits, PricingModel,
    PricingMultiplierChange, PricingMultiplierWrite, PricingMultipliersUpdate, PricingRefresh,
    PricingRefreshPolicy, PricingRefreshStatus, PricingRefreshUpdate, PricingRevision,
    PricingSnapshot, PricingTimeWindow, ProtocolProbeRequest, ProtocolProbeResponse,
    ProtocolProbeResult, ProtocolSwitchUpdate, ProtocolSwitches, ProviderAccountChoice,
    ProviderCatalog, ProviderCatalogEntry, ProviderCatalogFormField, ProviderCatalogRiskNotice,
    ProviderContractGroup, ProviderContracts, ProviderModelCapability, ProviderOfferingChoice,
    ProviderPricing, ProviderUsage, ProxyListDirection, ProxyMode, ProxySupportedModel,
    ProxyTestRequest, ProxyTestResponse, QuotaWindow, RoutingMode, Settings, SettingsUpdate,
    UsageAvailability, UsageMutation, UsageSyncState, UsageWindow, V3Error, ZenFreeModel,
    ZenFreeModels, ZenFreeSettings, ZenFreeSettingsUpdate, contract_schema, contract_schema_pretty,
};

#[cfg(debug_assertions)]
pub use pricing::{
    OfficialPricingFetchGuard, install_official_pricing_fetch_error_for_tests,
    install_official_pricing_fetch_for_tests,
};

pub fn api_router(state: CoreState) -> Router<CoreState> {
    let protected = Router::new()
        .route("/contract", get(get_contract))
        .route("/connection", get(connection::get_connection))
        .route(
            "/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route("/settings/test-proxy", post(proxy_test::test_proxy))
        .route(
            "/claude-desktop/models",
            get(claude_desktop::get_claude_desktop_models)
                .put(claude_desktop::put_claude_desktop_models),
        )
        .route("/pricing", get(pricing::get_pricing))
        .route("/pricing/refresh", post(pricing::refresh_pricing))
        .route(
            "/pricing/multipliers",
            put(pricing::put_pricing_multipliers),
        )
        .route(
            "/providers/{provider_id}/{offering_id}/pricing",
            get(pricing::get_provider_pricing),
        )
        .route(
            "/keys/primary/regenerate",
            post(keys::regenerate_primary_key),
        )
        .route("/keys", post(keys::create_key))
        .route(
            "/keys/{id}",
            patch(keys::update_key).delete(keys::delete_key),
        )
        .route("/keys/{id}/regenerate", post(keys::regenerate_key))
        .route(
            "/accounts",
            get(accounts::list_accounts).post(accounts::create_account),
        )
        .route("/accounts/managed", post(accounts::create_managed_account))
        .route("/accounts/order", put(accounts::reorder_accounts))
        .route(
            "/accounts/{id}",
            get(accounts::get_account)
                .patch(accounts::update_account)
                .delete(accounts::delete_account),
        )
        .route("/accounts/{id}/toggle", post(accounts::toggle_account))
        .route(
            "/accounts/{id}/setup",
            patch(accounts::advance_account_setup),
        )
        .route(
            "/accounts/{id}/reset-cooldown",
            post(accounts::reset_account_cooldown),
        )
        .route(
            "/accounts/{id}/custom-config",
            put(accounts::put_account_custom_config),
        )
        .route(
            "/accounts/{id}/model-capabilities",
            put(accounts::put_account_model_capabilities),
        )
        .route(
            "/accounts/{id}/acknowledgements",
            post(accounts::create_account_acknowledgement),
        )
        .route(
            "/accounts/{id}/usage",
            get(usage::get_account_usage).patch(usage::patch_account_usage),
        )
        .route(
            "/accounts/{id}/provider-usage",
            get(usage::get_provider_usage),
        )
        .route("/providers", get(providers::get_providers))
        .route(
            "/providers/model-capabilities",
            get(providers::get_model_capabilities),
        )
        .route(
            "/providers/zen-free",
            get(providers::get_zen_free_settings).patch(providers::patch_zen_free_settings),
        )
        .route(
            "/providers/zen-free/models",
            get(providers::get_zen_free_models),
        )
        .route(
            "/providers/zen-free/models/refresh",
            post(providers::refresh_zen_free_models),
        )
        .route(
            "/provider-contracts",
            get(providers::get_provider_contracts),
        )
        .route(
            "/provider-contracts/provider/{scope_id}/protocols/{protocol}",
            put(providers::put_provider_protocol_switch),
        )
        .route(
            "/providers/{provider_id}/protocol-probes",
            post(providers::run_provider_protocol_probes),
        )
        .route("/gateway/status", get(observability::get_gateway_status))
        .route(
            "/application-models",
            get(observability::get_application_models),
        )
        .route(
            "/dashboard/summary",
            get(observability::get_dashboard_summary),
        )
        .route(
            "/dashboard/daily-cost-by-model",
            get(observability::get_daily_cost_by_model),
        )
        .route("/logs/gateway", get(observability::get_gateway_logs))
        .route("/logs/forward", get(observability::get_forward_logs))
        .route(
            "/logs/forward/models",
            get(observability::get_forward_log_models),
        )
        .route(
            "/logs/forward/keys",
            get(observability::get_forward_log_keys),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_v3_session,
        ));

    Router::new()
        .route("/auth/status", get(auth::auth_status))
        .route("/auth/register", post(auth::register_admin))
        .route("/auth/login", post(auth::login_admin))
        .route("/auth/logout", post(auth::logout_admin))
        .merge(protected)
}

struct V3Query<T>(T);

impl<T> FromRequestParts<CoreState> for V3Query<T>
where
    T: DeserializeOwned + Send,
{
    type Rejection = V3ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &CoreState,
    ) -> Result<Self, Self::Rejection> {
        Query::<T>::try_from_uri(&parts.uri)
            .map(|Query(value)| Self(value))
            .map_err(|_| V3ApiError::invalid_request_at(state, "invalid query"))
    }
}

struct V3ApiError {
    status: StatusCode,
    body: V3Error,
}

impl V3ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: V3Error::unauthorized(),
        }
    }

    fn unauthorized_credentials() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            body: V3Error {
                code: ERROR_UNAUTHORIZED.to_string(),
                message: "username or password is incorrect".to_string(),
                current_revision: None,
                process_generation: None,
            },
        }
    }

    fn invalid_json() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: V3Error::invalid_json(),
        }
    }

    fn missing_expected_revision() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: V3Error::missing_expected_revision(),
        }
    }

    fn revision_conflict(state: &CoreState) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: V3Error::revision_conflict(state.settings_revision(), state.process_generation()),
        }
    }

    fn invalid_request_at(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: V3Error::invalid_request_at(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn not_found(state: &CoreState) -> Self {
        Self::not_found_at(state, "account not found")
    }

    fn not_found_at(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            body: V3Error::not_found(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn outbound_failed(state: &CoreState, message: impl Into<String>) -> Self {
        Self::outbound_failed_at(
            state.settings_revision(),
            state.process_generation(),
            message,
        )
    }

    fn outbound_failed_at(
        current_revision: u64,
        process_generation: u64,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            body: V3Error {
                code: ERROR_OUTBOUND_FAILED.to_string(),
                message: message.into(),
                current_revision: Some(current_revision),
                process_generation: Some(process_generation),
            },
        }
    }

    fn conflict_at(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: V3Error::conflict(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn precondition_failed_at(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PRECONDITION_FAILED,
            body: V3Error::precondition_failed(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn service_unavailable(state: &CoreState, message: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: V3Error::service_unavailable(
                message.to_string(),
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn not_implemented(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            body: V3Error::not_implemented(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn internal(message: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: V3Error::internal(message.to_string()),
        }
    }
}

impl IntoResponse for V3ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

async fn require_v3_session(State(state): State<CoreState>, req: Request, next: Next) -> Response {
    let authorized = {
        let current = state.dashboard_session_token.lock();
        dashboard_session::is_authorized(
            state.dashboard_local_mode(),
            current.as_str(),
            req.headers(),
        )
    };
    if authorized {
        next.run(req).await
    } else {
        V3ApiError::unauthorized().into_response()
    }
}

async fn get_contract(State(state): State<CoreState>) -> Json<ControlRevision> {
    Json(ControlRevision::from_state(&state))
}

/// Shared mutation-body parser: missing `expectedRevision` is a dedicated
/// 400; anything else that is not valid JSON for `T` is `invalidJson`.
fn parse_mutation_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, V3ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| V3ApiError::invalid_json())?;
    let Some(object) = value.as_object() else {
        return Err(V3ApiError::invalid_json());
    };
    if !object.contains_key("expectedRevision") {
        return Err(V3ApiError::missing_expected_revision());
    }
    serde_json::from_value(value).map_err(|_| V3ApiError::invalid_json())
}

fn check_expectation(
    state: &CoreState,
    expectation: &MutationExpectation,
) -> Result<(), V3ApiError> {
    if expectation.expected_revision != state.settings_revision()
        || expectation.process_generation != state.process_generation()
    {
        Err(V3ApiError::revision_conflict(state))
    } else {
        Ok(())
    }
}

fn check_pricing_expectation(
    state: &CoreState,
    expectation: &MutationExpectation,
    expected_pricing_revision: &str,
) -> Result<(), V3ApiError> {
    check_expectation(state, expectation)?;
    if expected_pricing_revision != state.pricing_snapshot().revision {
        Err(V3ApiError::revision_conflict(state))
    } else {
        Ok(())
    }
}
