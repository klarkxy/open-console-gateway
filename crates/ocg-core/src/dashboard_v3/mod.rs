//! Dashboard V3 HTTP contract kernel.
//!
//! Handlers are remounted at `/dashboard/api/v4` (see `dashboard_v4::api_router`).
//! The `/dashboard/api/v3` prefix is a 410 tombstone. This module owns the
//! shared DTO / error / CAS envelope, process
//! generation, public auth/session issuance, connection/settings reads, the settings write path,
//! access-key lifecycle, the local accounts control plane including connection
//! verify, local account usage calibration, official Go usage refresh, and
//! provider-usage reads, the local/Zen provider catalog,
//! contracts, Zen Free control plane, pricing, the settings proxy diagnostic,
//! session-protected desktop update check/status/install, read-only
//! observability, Go/Zen protocol probes, the local/native/remote browser runtime, and managed-account Key
//! verification. Custom model discovery is an authenticated operational probe
//! (no `expectedRevision`, no revision bump).
//! `GET /settings/check-update` and `GET /settings/update-status` are reads
//! that capture revision/generation and never bump them.
//! `POST /settings/install-update` requires CAS under `settings_update`, starts
//! atomically, does not bump, and holds no network/DB lock. Account-page
//! operational tests use V3 `POST /accounts/{id}/model-tests`.

mod account_model_test;
mod account_transfer;
mod account_verify;
mod accounts;
mod auth;
mod browser;
mod connection;
mod cpa;
mod custom_discovery;
pub(crate) mod dynamic_providers;
mod keys;
mod managed_key_verify;
mod observability;
mod platforms;
mod pricing;
mod providers;
mod proxy_test;
mod settings;
mod types;
mod updater;
mod usage;
mod usage_refresh;

use axum::extract::{DefaultBodyLimit, FromRequestParts, Query, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::dashboard_session;
use crate::state::CoreState;

#[cfg(debug_assertions)]
pub use managed_key_verify::{
    ManagedKeyVerifyTargetGuard, install_managed_key_verify_target_for_tests,
};
#[cfg(debug_assertions)]
pub use providers::set_zen_models_source_url_override_for_tests;
pub use proxy_test::PROXY_TEST_TARGET;
#[cfg(debug_assertions)]
pub use proxy_test::{ProxyTestTargetGuard, install_proxy_test_target_for_tests};
pub use types::{
    Account, AccountAuthScheme, AccountCreate, AccountCredentialKind, AccountCustomConfig,
    AccountCustomConfigUpdate, AccountCustomConfigWrite, AccountExport, AccountExportRequest,
    AccountImportDisposition, AccountImportPreview, AccountImportPreviewItem,
    AccountImportPreviewRequest, AccountImportRequest, AccountImportResult, AccountList,
    AccountManagedCreate, AccountManagedKeyVerify, AccountModelCapabilitiesUpdate,
    AccountModelCapability, AccountModelCapabilityWrite, AccountModelTestRequest,
    AccountModelTestResponse, AccountMutation, AccountOrder, AccountQuotaScope, AccountSetupStep,
    AccountSetupUpdate, AccountType, AccountUpdate, AccountUpstreamProtocol, AccountUsageUpdate,
    AccountVerificationStatus, AccountVerify, ApplicationModels, AuthLogin, AuthLogout,
    AuthRegister, AuthStatus, BrowserCapabilities, BrowserMode, BrowserOpen, BrowserOpenRequest,
    BrowserTarget, CATALOG_TYPE_NAMES, CapabilitySummary, CardCapabilitySummary, ConnectionInfo,
    ConnectionSubKey, ContractScopeKind, ControlRevision, CpaAccount, CpaAccountDelete,
    CpaAccountStatusUpdate, CpaAccounts, CpaConnectionReport, CpaIntegration, CpaIntegrationUpdate,
    CpaModel, CpaModels, CpaOAuthProvider, CpaOAuthSessionDelete, CpaOAuthStart,
    CpaOAuthStartRequest, CpaOAuthStatus, CpaQuotaReset, CpaRuntime, CpaRuntimeCheck,
    CpaRuntimeInstall, CpaRuntimeKey, CpaRuntimeKeyCreated, CpaRuntimeKeys, CpaRuntimeLogs,
    CpaRuntimePhase, CpaTestRequest, CreditBalance, CustomEndpointContract,
    CustomModelDiscoveryRequest, CustomModelDiscoveryResponse, DailyModelTokens,
    DailyTokensByModel, DailyTokensQuery, DashboardSummary, DesktopUpdate, DesktopUpdatePhase,
    ERROR_BUILTIN_PROVIDER_IMMUTABLE, ERROR_CONFLICT, ERROR_FORBIDDEN, ERROR_GATEWAY_TIMEOUT,
    ERROR_GONE, ERROR_INTERNAL, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST,
    ERROR_MISSING_EXPECTED_REVISION, ERROR_NOT_FOUND, ERROR_NOT_IMPLEMENTED,
    ERROR_OPERATION_PAYLOAD_MISMATCH, ERROR_OUTBOUND_FAILED, ERROR_PRECONDITION_FAILED,
    ERROR_REVISION_CONFLICT, ERROR_SERVICE_UNAVAILABLE, ERROR_THROTTLED, ERROR_UNAUTHORIZED,
    EffectiveCatalog, EffectiveModelContract, EffectiveModelProtocols, EffectiveProtocolEvidence,
    ForwardLog, ForwardLogClientKey, ForwardLogKeys, ForwardLogModels, ForwardLogQuery,
    ForwardLogSummary, ForwardLogs, GatewayLog, GatewayLogQuery, GatewayLogs, GatewayStatus,
    InstallUpdate, KeyCreate, KeyUpdate, ModelProtocolOverride, ModelProtocolOverridesUpdate,
    MutationAck, MutationExpectation, OllamaBillingTier, PricingAdjustment, PricingAvailability,
    PricingLimits, PricingModel, PricingMultiplierChange, PricingMultiplierWrite,
    PricingMultipliersUpdate, PricingRefresh, PricingRefreshPolicy, PricingRefreshStatus,
    PricingRefreshUpdate, PricingRevision, PricingSnapshot, PricingTimeWindow,
    ProtocolOverrideState, ProtocolProbeRequest, ProtocolProbeResponse, ProtocolProbeResult,
    ProviderAccountChoice, ProviderCatalog, ProviderCatalogEntry, ProviderCatalogFormField,
    ProviderCatalogRiskNotice, ProviderContractGroup, ProviderContracts, ProviderDefinition,
    ProviderDefinitionAuthKind, ProviderDefinitionCreate, ProviderDefinitionDiscoverRequest,
    ProviderDefinitionDiscoverResponse, ProviderDefinitionModel, ProviderDefinitionMutation,
    ProviderDefinitionTestRequest, ProviderDefinitionTestResponse, ProviderDefinitionUpdate,
    ProviderModelCapability, ProviderModelUpstreamOverride, ProviderPricing,
    ProviderPricingRefresh, ProviderPricingRefreshUpdate, ProviderUsage, ProxyListDirection,
    ProxyMode, ProxySupportedModel, ProxyTestRequest, ProxyTestResponse, QuotaWindow, RoutingMode,
    Settings, SettingsUpdate, UpdateCheck, UsageAvailability, UsageMutation, UsageRefresh,
    UsageRefreshThrottleError, UsageRefreshUpdate, UsageSyncState, UsageWindow, V3Error,
    ZenFreeModel, ZenFreeModels, ZenFreeSettings, ZenFreeSettingsUpdate, contract_schema,
    contract_schema_pretty,
};
pub use updater::{GITHUB_LATEST_RELEASE_API, GITHUB_LATEST_RELEASE_URL};

pub use crate::official_protocols::OfficialProtocolBaseline;
#[cfg(debug_assertions)]
pub use crate::official_protocols::{
    OfficialProtocolFetchGuard, install_official_protocol_fetch_fallback_chat_for_tests,
    install_official_protocol_fetch_for_tests,
};
#[cfg(debug_assertions)]
pub use account_verify::{CustomVerifyProbeGuard, install_custom_verify_probe_for_tests};
#[cfg(debug_assertions)]
pub use browser::{BrowserProfilePurgeGuard, install_browser_profile_purge_error_for_tests};
#[cfg(debug_assertions)]
pub use pricing::{
    OfficialPricingFetchGuard, install_official_pricing_fetch_error_for_tests,
    install_official_pricing_fetch_for_tests,
};
#[cfg(debug_assertions)]
pub use updater::{UpdateCheckUrlGuard, install_update_check_url_for_tests};

pub fn api_router(state: CoreState) -> Router<CoreState> {
    let account_transfer = Router::new()
        .route(
            "/accounts/transfer/export",
            post(account_transfer::export_accounts),
        )
        .route(
            "/accounts/transfer/preview",
            post(account_transfer::preview_import),
        )
        .route(
            "/accounts/transfer/import",
            post(account_transfer::import_accounts),
        )
        .layer(DefaultBodyLimit::max(account_transfer::MAX_REQUEST_BYTES))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_v3_session,
        ))
        .layer(middleware::map_response(account_transfer::add_no_store));
    let protected = Router::new()
        .route(
            "/platform-accounts",
            get(platforms::list).post(platforms::create),
        )
        .route(
            "/platform-accounts/{id}",
            put(platforms::update).delete(platforms::delete),
        )
        .route("/platform-accounts/{id}/refresh", post(platforms::refresh))
        .route(
            "/accounts/{id}/platform-link",
            put(platforms::link).delete(platforms::unlink),
        )
        .route("/connection", get(connection::get_connection))
        .route(
            "/external-integrations/cpa",
            get(cpa::get_integration)
                .put(cpa::put_integration)
                .delete(cpa::delete_integration),
        )
        .route(
            "/external-integrations/cpa/test",
            post(cpa::test_connection),
        )
        .route("/external-integrations/cpa/models", get(cpa::get_models))
        .route(
            "/external-integrations/cpa/models/refresh",
            post(cpa::refresh_models),
        )
        .route(
            "/external-integrations/cpa/accounts",
            get(cpa::list_accounts).delete(cpa::delete_account),
        )
        .route(
            "/external-integrations/cpa/accounts/status",
            patch(cpa::set_account_status),
        )
        .route(
            "/external-integrations/cpa/accounts/reset-quota",
            post(cpa::reset_quota),
        )
        .route(
            "/external-integrations/cpa/cli-imports",
            get(cpa::cli_import_sources).post(cpa::import_cli_account),
        )
        .route(
            "/external-integrations/cpa/oauth/start",
            post(cpa::start_oauth),
        )
        .route(
            "/external-integrations/cpa/oauth/status",
            get(cpa::oauth_status),
        )
        .route(
            "/external-integrations/cpa/oauth/session",
            delete(cpa::cancel_oauth),
        )
        .route(
            "/external-integrations/cpa/runtime/check-update",
            post(cpa::check_runtime_update),
        )
        .route(
            "/external-integrations/cpa/runtime/install",
            post(cpa::install_runtime),
        )
        .route(
            "/external-integrations/cpa/runtime/update",
            post(cpa::update_runtime),
        )
        .route(
            "/external-integrations/cpa/runtime",
            get(cpa::get_runtime).delete(cpa::remove_runtime),
        )
        .route(
            "/external-integrations/cpa/runtime/start",
            post(cpa::start_runtime),
        )
        .route(
            "/external-integrations/cpa/runtime/stop",
            post(cpa::stop_runtime),
        )
        .route(
            "/external-integrations/cpa/runtime/rollback",
            post(cpa::rollback_runtime),
        )
        .route(
            "/external-integrations/cpa/runtime/logs",
            get(cpa::get_runtime_logs),
        )
        .route(
            "/external-integrations/cpa/client-keys",
            get(cpa::list_runtime_keys).post(cpa::create_runtime_key),
        )
        .route(
            "/external-integrations/cpa/client-keys/{fingerprint}/rotate",
            post(cpa::rotate_runtime_key),
        )
        .route(
            "/external-integrations/cpa/client-keys/{fingerprint}",
            delete(cpa::delete_runtime_key),
        )
        .route(
            "/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route("/settings/test-proxy", post(proxy_test::test_proxy))
        .route("/settings/check-update", get(updater::check_update))
        .route("/settings/update-status", get(updater::get_update_status))
        .route("/settings/install-update", post(updater::install_update))
        .route(
            "/providers/{provider_id}/pricing/refresh",
            post(pricing::refresh_provider_pricing),
        )
        .route(
            "/providers/{provider_id}/pricing/multipliers",
            put(pricing::put_pricing_multipliers),
        )
        .route(
            "/providers/{provider_id}/pricing",
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
        .route("/account-records", get(accounts::list_accounts))
        .route("/accounts", post(accounts::create_account))
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
            "/accounts/{id}/browser",
            post(browser::open_account_browser),
        )
        .route(
            "/accounts/{id}/browser-profile",
            delete(browser::reset_account_browser_profile),
        )
        .route(
            "/accounts/{id}/setup",
            patch(accounts::advance_account_setup),
        )
        .route(
            "/accounts/{id}/setup/verify-key",
            post(managed_key_verify::verify_managed_account_key),
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
            "/accounts/{id}/usage",
            get(usage::get_account_usage).patch(usage::patch_account_usage),
        )
        .route(
            "/accounts/{id}/usage/refresh",
            post(usage_refresh::refresh_account_usage),
        )
        .route(
            "/accounts/{id}/provider-usage",
            get(usage::get_provider_usage).post(usage::refresh_provider_usage),
        )
        .route(
            "/accounts/{id}/verify",
            post(account_verify::verify_account),
        )
        .route(
            "/accounts/{id}/model-tests",
            post(account_model_test::test_account_model),
        )
        .route(
            "/providers",
            get(providers::get_providers).post(dynamic_providers::create_provider),
        )
        .route(
            "/providers/models/discover",
            post(dynamic_providers::discover_models),
        )
        .route("/providers/test", post(dynamic_providers::test_provider))
        .route(
            "/providers/{provider_id}",
            get(dynamic_providers::get_provider)
                .patch(dynamic_providers::update_provider)
                .delete(dynamic_providers::delete_provider),
        )
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
            "/providers/{provider_id}/models/refresh",
            post(providers::refresh_provider_models),
        )
        .route(
            "/provider-contracts",
            get(providers::get_provider_contracts),
        )
        .route(
            "/provider-contracts/{scope_kind}/{scope_id}/catalog/refresh",
            post(providers::refresh_contract_catalog),
        )
        .route(
            "/provider-contracts/provider/{scope_id}/model-protocol-overrides",
            put(providers::put_provider_model_protocol_overrides),
        )
        .route(
            "/provider-contracts/provider/{scope_id}/model-protocols/reset-static",
            post(providers::reset_provider_model_protocols_to_static),
        )
        .route(
            "/provider-contracts/custom-endpoint/{scope_id}/model-protocol-overrides",
            put(providers::put_custom_endpoint_model_protocol_overrides),
        )
        .route(
            "/providers/{provider_id}/protocol-probes",
            post(providers::run_provider_protocol_probes),
        )
        .route("/browser/capabilities", get(browser::browser_capabilities))
        .route(
            "/browser/sessions/{token}/ws",
            get(browser::browser_session_websocket),
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
            "/dashboard/daily-tokens-by-model",
            get(observability::get_daily_tokens_by_model),
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
        .route(
            "/custom/models/discover",
            post(custom_discovery::discover_custom_models),
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
        .merge(account_transfer)
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

pub(crate) struct V3ApiError {
    status: StatusCode,
    body: V3Error,
}

impl V3ApiError {
    pub(crate) fn unauthorized() -> Self {
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

    pub(crate) fn revision_conflict(state: &CoreState) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: V3Error::revision_conflict(state.settings_revision(), state.process_generation()),
        }
    }

    pub(crate) fn invalid_request_at(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: V3Error::invalid_request_at(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn builtin_provider_immutable(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: V3Error::builtin_provider_immutable(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn not_found(state: &CoreState) -> Self {
        Self::not_found_at(state, "account not found")
    }

    pub(crate) fn not_found_at(state: &CoreState, message: impl Into<String>) -> Self {
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

    pub(crate) fn conflict_at(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: V3Error::conflict(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    pub(crate) fn operation_payload_mismatch(
        state: &CoreState,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            body: V3Error::operation_payload_mismatch(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    pub(crate) fn precondition_failed_at(state: &CoreState, message: impl Into<String>) -> Self {
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

    fn forbidden_at(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            body: V3Error::forbidden(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn gone(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GONE,
            body: V3Error::gone(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    fn gateway_timeout(state: &CoreState, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GATEWAY_TIMEOUT,
            body: V3Error::gateway_timeout(
                message,
                state.settings_revision(),
                state.process_generation(),
            ),
        }
    }

    pub(crate) fn internal(message: impl std::fmt::Display) -> Self {
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

pub(crate) async fn require_v3_session(
    State(state): State<CoreState>,
    req: Request,
    next: Next,
) -> Response {
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

/// Shared mutation-body parser: missing `expectedRevision` is a dedicated
/// 400; anything else that is not valid JSON for `T` is `invalidJson`.
pub(crate) fn parse_mutation_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, V3ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| V3ApiError::invalid_json())?;
    let Some(object) = value.as_object() else {
        return Err(V3ApiError::invalid_json());
    };
    if !object.contains_key("expectedRevision") {
        return Err(V3ApiError::missing_expected_revision());
    }
    serde_json::from_value(value).map_err(|_| V3ApiError::invalid_json())
}

/// Operational-body parser. Unknown fields and malformed JSON are
/// `invalidJson`. Unlike [`parse_mutation_json`], this does not require
/// `expectedRevision`.
fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, V3ApiError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| V3ApiError::invalid_json())?;
    if !value.is_object() {
        return Err(V3ApiError::invalid_json());
    }
    serde_json::from_value(value).map_err(|_| V3ApiError::invalid_json())
}

pub(crate) fn check_expectation(
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
