use crate::alias;
use crate::auth;
use crate::browser::{
    BrowserCapabilities, BrowserOpenResult, BrowserProfileOperationKind, StagedBrowserProfiles,
};
use crate::db::{ForwardLogQueryOptions, ReorderAccountsError};
use crate::gateway::{
    diagnostics::{
        api_format_name, redact_known_secret, sanitize_upstream_error_value_with_known_secret,
    },
    limit::{parse_reset, parse_usage_limit_window},
};
use crate::go_usage::GoUsageError;
use crate::kernel::pricing::PricingSnapshot;
use crate::kernel::protocol::{
    ApiFormat, supported_model_protocol_profiles, supported_model_protocols,
};
use crate::models::*;
use crate::pricing::{
    OfficialPricingRefresh, PricingRefreshConfirmPolicy, evaluate_official_pricing_refresh,
    fetch_official_snapshot, prepare_multiplier_update, stamp_pricing_activation,
};
use crate::state::{CoreState, DesktopUpdateStartError, DesktopUpdateStatus};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        Path, Query, Request, State, WebSocketUpgrade,
        rejection::JsonRejection,
        ws::{Message as AxumWsMessage, WebSocket},
    },
    http::{HeaderMap, HeaderValue, Response as HttpResponse, StatusCode, header, uri::Authority},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path as FsPath, PathBuf};
use std::str::FromStr;

const MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES: usize = 64 * 1024;

pub fn api_router(state: CoreState) -> Router<CoreState> {
    let protected = Router::new()
        .route("/accounts", get(list_accounts).post(create_account_route))
        .route("/providers", get(provider_catalog))
        .route("/providers/catalog", get(provider_catalog))
        .route("/provider-contracts", get(list_provider_contracts))
        .route(
            "/provider-contracts/{scope_kind}/{scope_id}/protocols/{protocol}",
            put(update_provider_contract_protocol),
        )
        .route(
            "/providers/model-capabilities",
            get(provider_model_capabilities),
        )
        .route("/models/capabilities", get(provider_model_capabilities))
        .route(
            "/custom/models/discover",
            post(discover_custom_models_route),
        )
        .route(
            "/providers/{provider_id}/{offering_id}/pricing",
            get(provider_pricing),
        )
        .route(
            "/providers/accounts/{id}/usage",
            get(provider_account_usage),
        )
        .route("/accounts/{id}/provider-usage", get(provider_account_usage))
        .route("/providers/zen-free", patch(update_zen_free_settings))
        .route("/accounts/managed", post(create_managed_account))
        .route("/accounts/order", put(reorder_accounts))
        .route(
            "/accounts/{id}",
            patch(update_account).delete(delete_account),
        )
        .route(
            "/accounts/{id}/provider-settings",
            patch(update_zen_free_settings_for_account),
        )
        .route("/accounts/{id}/provider-models", get(get_zen_free_models))
        .route(
            "/accounts/{id}/provider-models/refresh",
            post(refresh_provider_models),
        )
        .route(
            "/accounts/{id}/protocol-probes",
            post(run_account_protocol_probes),
        )
        .route("/accounts/{id}/toggle", post(toggle_account))
        .route("/accounts/{id}/verify", post(verify_account_connection))
        .route(
            "/accounts/{id}/custom-config",
            get(get_account_custom_config).put(put_account_custom_config),
        )
        .route(
            "/accounts/{id}/model-capabilities",
            get(get_account_model_capabilities).put(put_account_model_capabilities),
        )
        .route(
            "/accounts/{id}/acknowledgements",
            get(list_account_acknowledgements).post(create_account_acknowledgement),
        )
        .route("/accounts/{id}/test", post(test_account))
        .route("/accounts/{id}/setup", patch(advance_account_setup))
        .route(
            "/accounts/{id}/setup/verify-key",
            post(verify_managed_account_key),
        )
        .route("/browser/capabilities", get(browser_capabilities))
        .route("/accounts/{id}/browser", post(open_account_browser))
        .route(
            "/accounts/{id}/browser-profile",
            delete(reset_account_browser_profile),
        )
        .route(
            "/browser/sessions/{token}/ws",
            get(browser_session_websocket),
        )
        .route(
            "/accounts/{id}/usage",
            get(account_usage).patch(update_account_usage),
        )
        .route(
            "/accounts/{id}/usage/refresh",
            post(refresh_account_usage_from_official_go),
        )
        .route(
            "/accounts/{id}/reset-cooldown",
            post(reset_account_cooldown),
        )
        .route("/settings", get(get_settings).post(update_settings_route))
        .route("/settings/test-proxy", post(test_proxy))
        .route(
            "/claude-desktop/models",
            get(get_claude_desktop_models).put(update_claude_desktop_models),
        )
        .route("/settings/check-update", get(check_update))
        .route("/settings/update-status", get(get_update_status))
        .route("/settings/install-update", post(install_update))
        .route("/pricing", get(get_pricing))
        .route("/pricing/refresh", post(refresh_pricing))
        .route("/pricing/multipliers", put(update_pricing_multipliers))
        .route(
            "/settings/regenerate-gateway-key",
            post(regenerate_gateway_key),
        )
        .route("/settings/keys", post(create_gateway_key))
        .route(
            "/settings/keys/{id}",
            patch(update_gateway_key).delete(delete_gateway_key),
        )
        .route(
            "/settings/keys/{id}/regenerate",
            post(regenerate_gateway_key_entry),
        )
        .route("/connection", get(connection_info))
        .route("/gateway/status", get(gateway_status))
        .route("/application-models", get(application_models))
        .route("/logs/gateway", get(gateway_logs))
        .route("/logs/forward", get(forward_logs))
        .route("/logs/forward/models", get(forward_log_models))
        .route("/logs/forward/keys", get(forward_log_keys))
        .route("/dashboard/summary", get(dashboard_summary))
        .route("/dashboard/daily-cost-by-model", get(daily_cost_by_model))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_dashboard_session,
        ));

    Router::new()
        .route("/auth/status", get(auth_status))
        .route("/auth/register", post(register_admin))
        .route("/auth/login", post(login_admin))
        .route("/auth/logout", post(logout_admin))
        .merge(protected)
}

pub fn dashboard_dir(state: &CoreState) -> PathBuf {
    if let Some(dir) = state.dashboard_dir() {
        return dir;
    }
    if let Ok(dir) = std::env::var("OCG_DASHBOARD_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.join("dist");
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("dist")
}

pub async fn serve_index(State(state): State<CoreState>) -> impl IntoResponse {
    serve_file(dashboard_dir(&state).join("index.html")).await
}

pub async fn serve_asset(
    State(state): State<CoreState>,
    Path(path): Path<String>,
) -> impl IntoResponse {
    match asset_path(&dashboard_dir(&state), &path) {
        Some(path) => serve_file(path).await,
        None => StatusCode::BAD_REQUEST.into_response(),
    }
}

fn asset_path(dashboard_dir: &FsPath, raw: &str) -> Option<PathBuf> {
    if raw.contains('\\') || raw.contains(':') {
        return None;
    }
    let mut path = dashboard_dir.join("assets");
    for component in FsPath::new(raw).components() {
        match component {
            Component::Normal(part) => path.push(part),
            _ => return None,
        }
    }
    Some(path)
}

async fn serve_file(path: PathBuf) -> Response {
    match tokio::fs::read(&path).await {
        Ok(bytes) => HttpResponse::builder()
            .status(StatusCode::OK)
            .header(
                header::CONTENT_TYPE,
                content_type(path.extension().and_then(|s| s.to_str())),
            )
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(_) => (
            StatusCode::NOT_FOUND,
            format!("dashboard file not found: {}", path.display()),
        )
            .into_response(),
    }
}

fn content_type(ext: Option<&str>) -> &'static str {
    match ext.unwrap_or_default() {
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

const SESSION_COOKIE: &str = "ocg_dashboard_session";

#[derive(Serialize)]
struct AuthStatus {
    local: bool,
    initialized: bool,
    authenticated: bool,
}

#[derive(Deserialize)]
struct AdminCredentials {
    username: String,
    password: String,
}

async fn auth_status(
    State(state): State<CoreState>,
    headers: HeaderMap,
) -> Result<Json<AuthStatus>, ApiError> {
    let local = is_local_dashboard_request(&state, &headers);
    let initialized = {
        let db = state.db.lock();
        auth::load_admin(&db).map_err(ApiError::internal)?.is_some()
    };
    Ok(Json(AuthStatus {
        local,
        initialized,
        authenticated: local || has_dashboard_session(&state, &headers),
    }))
}

async fn register_admin(
    State(state): State<CoreState>,
    headers: HeaderMap,
    Json(input): Json<AdminCredentials>,
) -> Result<Response, ApiError> {
    let admin = auth::build_admin(&input.username, &input.password)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    {
        let db = state.db.lock();
        if auth::load_admin(&db).map_err(ApiError::internal)?.is_some() {
            return Err(ApiError::status(
                StatusCode::CONFLICT,
                "管理员已经创建，请直接登录",
            ));
        }
        auth::save_admin(&db, &admin).map_err(ApiError::internal)?;
    }
    session_response(&state, &headers, StatusCode::CREATED).await
}

async fn login_admin(
    State(state): State<CoreState>,
    headers: HeaderMap,
    Json(input): Json<AdminCredentials>,
) -> Result<Response, ApiError> {
    let admin = {
        let db = state.db.lock();
        auth::load_admin(&db).map_err(ApiError::internal)?
    };
    let valid = admin
        .as_ref()
        .map(|admin| auth::verify_admin(admin, &input.username, &input.password))
        .unwrap_or(false);
    if !valid {
        return Err(ApiError::status(
            StatusCode::UNAUTHORIZED,
            "用户名或密码错误",
        ));
    }
    session_response(&state, &headers, StatusCode::OK).await
}

async fn logout_admin(
    State(state): State<CoreState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !is_local_dashboard_request(&state, &headers) && !has_dashboard_session(&state, &headers) {
        return Err(ApiError::status(
            StatusCode::UNAUTHORIZED,
            "dashboard session is required",
        ));
    }
    let _browser_operation = state.browser.operation().await;
    if !is_local_dashboard_request(&state, &headers) && !has_dashboard_session(&state, &headers) {
        return Err(ApiError::status(
            StatusCode::UNAUTHORIZED,
            "dashboard session is required",
        ));
    }
    state.browser.invalidate_remote_sessions();
    *state.dashboard_session_token.lock() = uuid::Uuid::new_v4().simple().to_string();
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie_header("", &headers, true)?);
    Ok(response)
}

async fn require_dashboard_session(
    State(state): State<CoreState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if is_local_dashboard_request(&state, req.headers())
        || has_dashboard_session(&state, req.headers())
    {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

fn is_local_dashboard_request(state: &CoreState, headers: &HeaderMap) -> bool {
    state.dashboard_local_mode()
        && [
            "forwarded",
            "x-forwarded-for",
            "x-forwarded-proto",
            "x-real-ip",
        ]
        .iter()
        .all(|name| !headers.contains_key(*name))
}

fn has_dashboard_session(state: &CoreState, headers: &HeaderMap) -> bool {
    let current = state.dashboard_session_token.lock();
    dashboard_session_value(headers)
        .map(|value| value == current.as_str())
        .unwrap_or(false)
}

fn dashboard_session_value(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|cookie| {
                let (name, value) = cookie.trim().split_once('=')?;
                (name == SESSION_COOKIE).then_some(value)
            })
        })
}

async fn session_response(
    state: &CoreState,
    headers: &HeaderMap,
    status: StatusCode,
) -> Result<Response, ApiError> {
    let _browser_operation = state.browser.operation().await;
    state.browser.invalidate_remote_sessions();
    let session_token = uuid::Uuid::new_v4().simple().to_string();
    *state.dashboard_session_token.lock() = session_token.clone();
    let mut response = (status, Json(serde_json::json!({ "ok": true }))).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        cookie_header(&session_token, headers, false)?,
    );
    Ok(response)
}

fn cookie_header(
    value: &str,
    request_headers: &HeaderMap,
    clear: bool,
) -> Result<HeaderValue, ApiError> {
    let mut cookie =
        format!("{SESSION_COOKIE}={value}; HttpOnly; SameSite=Strict; Path=/dashboard");
    if clear {
        cookie.push_str("; Max-Age=0");
    }
    if request_headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("https"))
    {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).map_err(ApiError::internal)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
    retry_after_secs: Option<u64>,
    next_allowed_at: Option<String>,
}

impl ApiError {
    fn status(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after_secs: None,
            next_allowed_at: None,
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            retry_after_secs: None,
            next_allowed_at: None,
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
            retry_after_secs: None,
            next_allowed_at: None,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            retry_after_secs: None,
            next_allowed_at: None,
        }
    }

    fn throttled(
        message: impl Into<String>,
        next_allowed_at: DateTime<Utc>,
        retry_after_secs: u64,
    ) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: message.into(),
            retry_after_secs: Some(retry_after_secs.max(1)),
            next_allowed_at: Some(next_allowed_at.to_rfc3339()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = serde_json::json!({ "error": self.message });
        if let Some(next_allowed_at) = &self.next_allowed_at {
            body["next_allowed_at"] = serde_json::json!(next_allowed_at);
        }
        let mut response = (self.status, Json(body)).into_response();
        if let Some(secs) = self.retry_after_secs {
            if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

#[derive(Debug, Serialize)]
struct DashboardAccount {
    id: String,
    provider_id: String,
    offering_id: String,
    credential_kind: crate::provider::CredentialKind,
    quota_scope: crate::provider::QuotaScope,
    name: String,
    username: String,
    password: String,
    key: String,
    enabled: bool,
    account_type: AccountType,
    setup_step: AccountSetupStep,
    purchase_date: String,
    expires_on: String,
    cooldown_until: Option<String>,
    cooldown_generic_until: Option<String>,
    cooldown_5h_until: Option<String>,
    cooldown_week_until: Option<String>,
    cooldown_month_until: Option<String>,
    cooldown_free_until: Option<String>,
    last_error: Option<String>,
    auth_error: Option<String>,
    notes: String,
    /// Last successful official Go usage calibration, if any.
    usage_sync_last_success_at: Option<String>,
    /// When a manual refresh may be attempted again (15s throttle), if blocked.
    usage_sync_next_allowed_at: Option<String>,
    created_at: String,
    updated_at: String,
    /// Shared control-plane revision used by account/settings CAS mutations.
    revision: u64,
    verification_status: crate::provider::ConnectionVerificationStatus,
    connection_verified_at: Option<String>,
    verification_error: Option<String>,
    plan_routable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_config: Option<AccountCustomConfig>,
    model_capabilities: Vec<AccountModelCapability>,
    acknowledgements: Vec<AccountAcknowledgement>,
}

fn dashboard_account(state: &CoreState, account: Account) -> DashboardAccount {
    let ((usage_sync_last_success_at, usage_sync_next_allowed_at), contract) = {
        let db = state.db.lock();
        let sync = db.account_usage_sync_state(&account.id).ok().flatten();
        let contract = db.load_account_contract(&account.id).unwrap_or_default();
        (
            crate::usage_sync::dashboard_sync_fields(sync.as_ref(), state.usage_sync.now()),
            contract,
        )
    };
    // The decrypted key is only needed to redact secrets inside persisted
    // error text; skip the (Windows DPAPI-backed) decrypt for accounts
    // without errors, which is the common case on every list call.
    let known_secret = if account.last_error.is_some()
        || account.auth_error.is_some()
        || contract.verification.verification_error.is_some()
    {
        if account.key_cipher.is_empty() {
            Some(String::new())
        } else {
            state.decrypt_key(&account.key_cipher).ok()
        }
    } else {
        None
    };
    let sanitize_persisted_error = |error: Option<String>| {
        error.and_then(|error| {
            known_secret
                .as_deref()
                .map(|secret| redact_known_secret(&error, secret))
        })
    };
    let plan = crate::provider::builtin_plan(&account.provider_id, &account.offering_id);
    DashboardAccount {
        id: account.id.clone(),
        provider_id: account.provider_id.clone(),
        offering_id: account.offering_id.clone(),
        credential_kind: account.credential_kind,
        quota_scope: account.quota_scope,
        name: account.name,
        username: account.username.unwrap_or_default(),
        password: String::new(),
        key: String::new(),
        enabled: account.enabled,
        account_type: account.account_type,
        setup_step: account.setup_step,
        purchase_date: account.purchase_date,
        expires_on: account.expires_on,
        cooldown_until: account.cooldown_until.map(|t| t.to_rfc3339()),
        cooldown_generic_until: account.cooldown_generic_until.map(|t| t.to_rfc3339()),
        cooldown_5h_until: account.cooldown_5h_until.map(|t| t.to_rfc3339()),
        cooldown_week_until: account.cooldown_week_until.map(|t| t.to_rfc3339()),
        cooldown_month_until: account.cooldown_month_until.map(|t| t.to_rfc3339()),
        cooldown_free_until: account.cooldown_free_until.map(|t| t.to_rfc3339()),
        last_error: sanitize_persisted_error(account.last_error),
        auth_error: sanitize_persisted_error(account.auth_error),
        notes: account.notes.unwrap_or_default(),
        usage_sync_last_success_at,
        usage_sync_next_allowed_at,
        created_at: account.created_at.to_rfc3339(),
        updated_at: account.updated_at.to_rfc3339(),
        revision: state.settings_revision(),
        verification_status: contract.verification.status,
        connection_verified_at: contract
            .verification
            .connection_verified_at
            .map(|value| value.to_rfc3339()),
        verification_error: sanitize_persisted_error(contract.verification.verification_error),
        plan_routable: plan.is_some_and(|plan| plan.routable),
        custom_config: contract.custom_config,
        model_capabilities: contract.model_capabilities,
        acknowledgements: contract.acknowledgements,
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value.and_then(|s| {
        let trimmed = s.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

async fn get_pricing(State(state): State<CoreState>) -> Result<Json<PricingSnapshot>, ApiError> {
    Ok(Json(state.pricing_snapshot().as_ref().clone()))
}

#[derive(Debug, Serialize)]
struct PricingRefreshResponse {
    #[serde(flatten)]
    snapshot: PricingSnapshot,
    refresh_status: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    multiplier_changes: Vec<PricingMultiplierChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    official_content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct PricingMultiplierChange {
    model_id: String,
    current_multiplier: f64,
    official_multiplier: f64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PricingRefreshPolicy {
    KeepCurrent,
    UseOfficial,
}

#[derive(Debug, Default, Deserialize)]
struct PricingRefreshRequest {
    policy: Option<PricingRefreshPolicy>,
    expected_revision: Option<String>,
    expected_official_content_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PricingMultiplierInput {
    model_id: String,
    multiplier: f64,
}

#[derive(Debug, Deserialize)]
struct PricingMultiplierUpdate {
    expected_revision: String,
    multipliers: Vec<PricingMultiplierInput>,
}

async fn refresh_pricing(
    State(state): State<CoreState>,
    request: Option<Json<PricingRefreshRequest>>,
) -> Result<Json<PricingRefreshResponse>, ApiError> {
    let _guard = state.pricing_refresh.try_lock().map_err(|_| {
        ApiError::status(
            StatusCode::CONFLICT,
            "OpenCode Go pricing refresh is already running",
        )
    })?;

    let request = request.map(|Json(request)| request).unwrap_or_default();
    if let Some(expected_revision) = request.expected_revision.as_deref()
        && state.pricing_snapshot().revision != expected_revision
    {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "pricing revision changed; refresh and try again",
        ));
    }

    let config = state.config();
    apply_pricing_refresh(
        &state,
        fetch_official_snapshot(&config).await,
        request.policy,
        request.expected_official_content_hash.as_deref(),
    )
    .map(Json)
}

fn apply_pricing_refresh(
    state: &CoreState,
    result: crate::Result<PricingSnapshot>,
    policy: Option<PricingRefreshPolicy>,
    expected_official_content_hash: Option<&str>,
) -> Result<PricingRefreshResponse, ApiError> {
    let policy = policy.map(|policy| match policy {
        PricingRefreshPolicy::KeepCurrent => PricingRefreshConfirmPolicy::KeepCurrent,
        PricingRefreshPolicy::UseOfficial => PricingRefreshConfirmPolicy::UseOfficial,
    });
    match evaluate_official_pricing_refresh(
        state.pricing_snapshot().as_ref(),
        result,
        policy,
        expected_official_content_hash,
    ) {
        OfficialPricingRefresh::NeedsConfirmation {
            multiplier_changes,
            official_content_hash,
        } => Ok(PricingRefreshResponse {
            snapshot: state.pricing_snapshot().as_ref().clone(),
            refresh_status: "needs_confirmation",
            multiplier_changes: v2_multiplier_changes(multiplier_changes),
            official_content_hash: Some(official_content_hash),
            error: None,
        }),
        OfficialPricingRefresh::Unchanged { multiplier_changes } => Ok(PricingRefreshResponse {
            snapshot: state.pricing_snapshot().as_ref().clone(),
            refresh_status: "unchanged",
            multiplier_changes: v2_multiplier_changes(multiplier_changes),
            official_content_hash: None,
            error: None,
        }),
        OfficialPricingRefresh::Activate {
            candidate,
            multiplier_changes,
        } => {
            let snapshot = stamp_pricing_activation(candidate);
            state
                .activate_pricing_snapshot(snapshot.clone())
                .map_err(ApiError::internal)?;
            let _ = state.db.lock().log_gateway(
                "info",
                "pricing",
                &format!("activated OpenCode Go pricing {}", snapshot.revision),
            );
            Ok(PricingRefreshResponse {
                snapshot,
                refresh_status: "success",
                multiplier_changes: v2_multiplier_changes(multiplier_changes),
                official_content_hash: None,
                error: None,
            })
        }
        OfficialPricingRefresh::Failed { error } => {
            let _ = state.db.lock().log_gateway(
                "warn",
                "pricing",
                &format!("OpenCode Go pricing refresh failed: {error}"),
            );
            Ok(PricingRefreshResponse {
                snapshot: state.pricing_snapshot().as_ref().clone(),
                refresh_status: "failed_no_change",
                multiplier_changes: Vec::new(),
                official_content_hash: None,
                error: Some(error),
            })
        }
    }
}

fn v2_multiplier_changes(
    changes: Vec<crate::pricing::PricingMultiplierDelta>,
) -> Vec<PricingMultiplierChange> {
    changes
        .into_iter()
        .map(|change| PricingMultiplierChange {
            model_id: change.model_id,
            current_multiplier: change.current_multiplier,
            official_multiplier: change.official_multiplier,
        })
        .collect()
}

async fn update_pricing_multipliers(
    State(state): State<CoreState>,
    Json(update): Json<PricingMultiplierUpdate>,
) -> Result<Json<PricingSnapshot>, ApiError> {
    let _guard = state
        .pricing_refresh
        .try_lock()
        .map_err(|_| ApiError::status(StatusCode::CONFLICT, "pricing update is already running"))?;
    let active = state.pricing_snapshot();
    if active.revision != update.expected_revision {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "pricing revision changed; refresh and try again",
        ));
    }
    let writes = update
        .multipliers
        .into_iter()
        .map(|input| (input.model_id, input.multiplier))
        .collect::<Vec<_>>();
    match prepare_multiplier_update(&active, &writes) {
        Err(message) => Err(ApiError::bad_request(message)),
        Ok(None) => Ok(Json(active.as_ref().clone())),
        Ok(Some(snapshot)) => {
            let snapshot = stamp_pricing_activation(snapshot);
            state
                .activate_pricing_snapshot(snapshot.clone())
                .map_err(ApiError::internal)?;
            let _ = state.db.lock().log_gateway(
                "info",
                "pricing",
                &format!("updated pricing multipliers in {}", snapshot.revision),
            );
            Ok(Json(snapshot))
        }
    }
}

fn encrypted_optional(
    state: &CoreState,
    value: &Option<String>,
) -> Result<Option<String>, ApiError> {
    match value.as_deref().map(str::trim) {
        Some("") | None => Ok(None),
        Some(v) => state.encrypt_key(v).map(Some).map_err(ApiError::internal),
    }
}

#[derive(Debug, Serialize)]
struct ProviderCatalogFormField {
    id: &'static str,
    kind: &'static str,
    required: bool,
    immutable_after_create: bool,
}

#[derive(Debug, Serialize)]
struct ProviderCatalogRiskNotice {
    acknowledgement_id: &'static str,
    version: &'static str,
    source_url: &'static str,
    body: &'static str,
    content_hash: String,
}

#[derive(Debug, Serialize)]
struct ProviderCatalogEntry {
    provider_id: String,
    offering_id: String,
    display_name: &'static str,
    display_family: &'static str,
    credential_kind: crate::provider::CredentialKind,
    quota_scope: crate::provider::QuotaScope,
    singleton: bool,
    creation_availability: crate::provider::CreationAvailability,
    #[serde(skip_serializing_if = "Option::is_none")]
    creation_unavailable_reason: Option<&'static str>,
    verification_policy: crate::provider::VerificationPolicy,
    verification_runtime_availability: &'static str,
    routable: bool,
    managed_registration: bool,
    pricing_availability: &'static str,
    usage_availability: &'static str,
    manual_usage_calibration: bool,
    quota_unit: &'static str,
    model_source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_prefix: Option<&'static str>,
    auth_schemes: Vec<&'static str>,
    upstream_protocols: Vec<&'static str>,
    form_fields: Vec<ProviderCatalogFormField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    risk_notice: Option<ProviderCatalogRiskNotice>,
    model_aliases: Vec<String>,
}

/// Built-in provider/offering pairs only. Command Code GOAT and SCNet remain
/// unroutable. Custom API is routable after explicit verification.
async fn provider_catalog(State(state): State<CoreState>) -> Json<Vec<ProviderCatalogEntry>> {
    let zen_catalog = state.zen_free_model_catalog();
    Json(
        crate::provider::BUILTIN_PLANS
            .iter()
            .map(|plan| ProviderCatalogEntry {
                provider_id: plan.offering.provider_id.to_string(),
                offering_id: plan.offering.offering_id.to_string(),
                display_name: plan.display_name,
                display_family: plan.display_family,
                credential_kind: plan.offering.credential_kind,
                quota_scope: plan.offering.quota_scope,
                singleton: plan.offering.singleton_account_id.is_some(),
                creation_availability: plan.creation_availability,
                creation_unavailable_reason: plan.creation_unavailable_reason,
                verification_policy: plan.verification_policy,
                verification_runtime_availability: plan.verification_runtime_availability,
                routable: plan.routable,
                managed_registration: plan.managed_registration,
                pricing_availability: plan.pricing_availability,
                usage_availability: plan.usage_availability,
                manual_usage_calibration: plan.manual_usage_calibration,
                quota_unit: plan.quota_unit,
                model_source: plan.model_source,
                key_prefix: plan.key_prefix,
                auth_schemes: plan
                    .auth_schemes
                    .iter()
                    .map(|value| value.as_str())
                    .collect(),
                upstream_protocols: plan
                    .upstream_protocols
                    .iter()
                    .map(|value| value.as_str())
                    .collect(),
                form_fields: plan
                    .form_fields
                    .iter()
                    .map(|field| ProviderCatalogFormField {
                        id: field.id,
                        kind: field.kind,
                        required: field.required,
                        immutable_after_create: field.immutable_after_create,
                    })
                    .collect(),
                risk_notice: plan.risk_notice.map(|notice| ProviderCatalogRiskNotice {
                    acknowledgement_id: notice.acknowledgement_id,
                    version: notice.version,
                    source_url: notice.source_url,
                    body: notice.body,
                    content_hash: notice.content_hash(),
                }),
                model_aliases: alias::routeable_aliases_for_with_zen(
                    plan.offering.provider_id,
                    plan.offering.offering_id,
                    &zen_catalog.models,
                ),
            })
            .collect(),
    )
}

#[derive(Debug, Serialize)]
struct ZenFreeModelsResponse {
    account_id: &'static str,
    models: Vec<crate::kernel::zen::ZenFreeModelView>,
    refreshed_at: Option<DateTime<Utc>>,
    source_url: String,
}

fn zen_free_models_response(
    catalog: &crate::kernel::zen::ZenFreeModelCatalog,
) -> ZenFreeModelsResponse {
    ZenFreeModelsResponse {
        account_id: crate::provider::ZEN_FREE_ACCOUNT_ID,
        models: crate::kernel::zen::model_views(catalog),
        refreshed_at: catalog.refreshed_at,
        source_url: catalog.source_url.clone(),
    }
}

fn require_zen_free_account(id: &str) -> Result<(), ApiError> {
    if id != crate::provider::ZEN_FREE_ACCOUNT_ID {
        return Err(ApiError::bad_request(
            "provider model refresh is available only for the Zen Free account",
        ));
    }
    Ok(())
}

async fn get_zen_free_models(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<ZenFreeModelsResponse>, ApiError> {
    require_zen_free_account(&id)?;
    let catalog = state.zen_free_model_catalog();
    Ok(Json(zen_free_models_response(&catalog)))
}

async fn refresh_zen_free_catalog(
    state: &CoreState,
) -> Result<crate::kernel::zen::ZenFreeModelCatalog, ApiError> {
    let _guard = state.zen_free_models_refresh.try_lock().map_err(|_| {
        ApiError::status(
            StatusCode::CONFLICT,
            "Zen Free model refresh is already running",
        )
    })?;
    let config = state.config();
    let catalog = crate::zen_models::fetch_catalog(&config)
        .await
        .map_err(|message| ApiError::status(StatusCode::BAD_GATEWAY, message))?;
    state
        .activate_zen_free_model_catalog(catalog.clone())
        .map_err(ApiError::internal)?;
    Ok(catalog)
}

async fn refresh_provider_models(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    let adapter = crate::provider::ProviderAdapterKind::from_offering(
        &account.provider_id,
        &account.offering_id,
    )
    .ok_or_else(|| ApiError::bad_request("unknown provider offering"))?;
    match adapter {
        crate::provider::ProviderAdapterKind::ZenFree => {
            require_zen_free_account(&id)?;
            let catalog = refresh_zen_free_catalog(&state).await?;
            Ok(Json(zen_free_models_response(&catalog)).into_response())
        }
        crate::provider::ProviderAdapterKind::ConfigurableHttp => {
            let refreshed = refresh_custom_catalog(&state, &account).await?;
            Ok(Json(refreshed).into_response())
        }
        crate::provider::ProviderAdapterKind::CommandCodeGoat
        | crate::provider::ProviderAdapterKind::Scnet => Err(ApiError::status(
            StatusCode::NOT_IMPLEMENTED,
            "model catalog refresh is not available for this Plan in this slice",
        )),
        crate::provider::ProviderAdapterKind::OpenCodeGo => Err(ApiError::status(
            StatusCode::CONFLICT,
            "OpenCode Go uses the static protocol catalog and does not refresh models from upstream",
        )),
    }
}

#[derive(Debug, Serialize)]
struct CustomCatalogRefreshResponse {
    scope_kind: &'static str,
    scope_id: String,
    models: Vec<String>,
    truncated: bool,
    refreshed_at: DateTime<Utc>,
    source: &'static str,
    declared_capabilities_unchanged: bool,
}

async fn refresh_custom_catalog(
    state: &CoreState,
    account: &Account,
) -> Result<CustomCatalogRefreshResponse, ApiError> {
    require_custom_plan(
        account,
        "model catalog refresh is only available for Custom API accounts",
    )?;
    let config = state
        .db
        .lock()
        .account_custom_config(&account.id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::bad_request(
                "Custom API accounts require a persisted base URL, protocol, and auth scheme",
            )
        })?;
    if account.key_cipher.is_empty() {
        return Err(ApiError::bad_request(
            "Custom model discovery requires a stored API key",
        ));
    }
    let api_key = state
        .decrypt_key(&account.key_cipher)
        .map_err(ApiError::internal)?;
    let discovery = crate::custom::discover_custom_models(
        &state.config(),
        &AccountCustomConfigInput {
            base_url: config.base_url.clone(),
            upstream_protocol: config.upstream_protocol,
            auth_scheme: config.auth_scheme,
        },
        &api_key,
    )
    .await
    .map_err(|failure| ApiError::bad_request(failure.message))?;
    let now = Utc::now();
    let scope = crate::provider_contracts::ContractScope::custom_endpoint(&account.id);
    state
        .db
        .lock()
        .set_contract_catalog(
            &scope,
            &discovery.models,
            Some(now),
            crate::provider_contracts::CATALOG_SOURCE_CUSTOM_DISCOVERY,
            "",
            now,
        )
        .map_err(ApiError::internal)?;
    state
        .reload_provider_contracts()
        .map_err(ApiError::internal)?;
    Ok(CustomCatalogRefreshResponse {
        scope_kind: crate::provider_contracts::SCOPE_KIND_CUSTOM_ENDPOINT,
        scope_id: account.id.clone(),
        models: discovery.models,
        truncated: discovery.truncated,
        refreshed_at: now,
        source: crate::provider_contracts::CATALOG_SOURCE_CUSTOM_DISCOVERY,
        declared_capabilities_unchanged: true,
    })
}

#[derive(Debug, Serialize)]
struct ProviderContractsResponse {
    /// Shared settings revision used by PUT `expected_revision`. Distinct
    /// from each scope's own `revision`.
    revision: u64,
    providers: Vec<ProviderContractGroupView>,
    custom_endpoints: Vec<CustomEndpointContractView>,
}

#[derive(Debug, Serialize)]
struct ProviderContractGroupView {
    scope_kind: &'static str,
    scope_id: String,
    provider_id: String,
    offerings: Vec<ProviderOfferingChoiceView>,
    catalog: crate::provider_contracts::EffectiveCatalog,
    models: Vec<crate::provider_contracts::EffectiveModelContract>,
    protocols: crate::provider_contracts::ProtocolSwitches,
    pricing: CapabilitySummary,
    usage: CapabilitySummary,
    card: CardCapabilitySummary,
    catalog_routable: bool,
    production_inference: bool,
    disabled_reasons: Vec<String>,
    revision: u64,
}

#[derive(Debug, Serialize)]
struct ProviderOfferingChoiceView {
    offering_id: String,
    display_name: &'static str,
    routable: bool,
    accounts: Vec<ProviderAccountChoiceView>,
}

#[derive(Debug, Serialize)]
struct ProviderAccountChoiceView {
    id: String,
    name: String,
    enabled: bool,
    verification_status: crate::provider::ConnectionVerificationStatus,
}

#[derive(Debug, Serialize)]
struct CustomEndpointContractView {
    scope_kind: &'static str,
    scope_id: String,
    provider_id: String,
    account: ProviderAccountChoiceView,
    catalog: crate::provider_contracts::EffectiveCatalog,
    models: Vec<crate::provider_contracts::EffectiveModelContract>,
    protocols: crate::provider_contracts::ProtocolSwitches,
    pricing: CapabilitySummary,
    usage: CapabilitySummary,
    card: CardCapabilitySummary,
    catalog_routable: bool,
    production_inference: bool,
    disabled_reasons: Vec<String>,
    revision: u64,
}

#[derive(Debug, Serialize)]
struct CapabilitySummary {
    availability: &'static str,
}

#[derive(Debug, Serialize)]
struct CardCapabilitySummary {
    fetch_zen_models: bool,
    discover_models: bool,
    protocol_probe: bool,
    catalog_refresh: bool,
}

async fn list_provider_contracts(
    State(state): State<CoreState>,
) -> Result<Json<ProviderContractsResponse>, ApiError> {
    let contracts = state.provider_contracts();
    let (accounts, statuses) = load_accounts_with_verification(&state)?;
    Ok(Json(build_provider_contracts_response(
        &contracts,
        &accounts,
        &statuses,
        state.settings_revision(),
    )))
}

fn load_accounts_with_verification(
    state: &CoreState,
) -> Result<
    (
        Vec<Account>,
        std::collections::HashMap<String, crate::provider::ConnectionVerificationStatus>,
    ),
    ApiError,
> {
    let db = state.db.lock();
    let accounts = db.list_accounts().map_err(ApiError::internal)?;
    let mut statuses = std::collections::HashMap::new();
    for account in &accounts {
        if let Some(state) = db
            .account_verification_state(&account.id)
            .map_err(ApiError::internal)?
        {
            statuses.insert(account.id.clone(), state.status);
        }
    }
    Ok((accounts, statuses))
}

fn build_provider_contracts_response(
    contracts: &crate::provider_contracts::EffectiveContractSet,
    accounts: &[Account],
    statuses: &std::collections::HashMap<String, crate::provider::ConnectionVerificationStatus>,
    settings_revision: u64,
) -> ProviderContractsResponse {
    let mut providers = Vec::new();
    for provider_id in crate::provider_contracts::builtin_provider_scope_ids() {
        let Some(contract) = contracts.providers.get(provider_id) else {
            continue;
        };
        let descriptor = crate::provider::ProviderRegistry::iter()
            .find(|item| item.kind == contract.adapter_kind)
            .expect("adapter has a catalog offering");
        let offerings = crate::provider::BUILTIN_PLANS
            .iter()
            .filter(|plan| plan.offering.provider_id == provider_id)
            .map(|plan| ProviderOfferingChoiceView {
                offering_id: plan.offering.offering_id.to_string(),
                display_name: plan.display_name,
                routable: plan.routable,
                accounts: accounts
                    .iter()
                    .filter(|account| {
                        account.provider_id == plan.offering.provider_id
                            && account.offering_id == plan.offering.offering_id
                    })
                    .map(|account| account_choice_view(account, statuses))
                    .collect(),
            })
            .collect();
        providers.push(ProviderContractGroupView {
            scope_kind: crate::provider_contracts::SCOPE_KIND_PROVIDER,
            scope_id: provider_id.to_string(),
            provider_id: provider_id.to_string(),
            offerings,
            catalog: contract.catalog.clone(),
            models: contract.models.values().cloned().collect(),
            protocols: contract.switches,
            pricing: CapabilitySummary {
                availability: descriptor.pricing.availability,
            },
            usage: CapabilitySummary {
                availability: descriptor.usage.catalog_availability,
            },
            card: card_summary(descriptor),
            catalog_routable: contract.catalog_routable,
            production_inference: contract.production_inference,
            disabled_reasons: contract.disabled_reasons.clone(),
            revision: contract.revision,
        });
    }
    let custom_endpoints = contracts
        .custom_endpoints
        .values()
        .map(|contract| {
            let descriptor = crate::provider::ProviderRegistry::get(
                crate::provider::CUSTOM_PROVIDER_ID,
                crate::provider::CUSTOM_API_OFFERING_ID,
            )
            .expect("custom offering is registered");
            let account = accounts
                .iter()
                .find(|account| account.id == contract.scope.id())
                .map(|account| account_choice_view(account, statuses))
                .unwrap_or(ProviderAccountChoiceView {
                    id: contract.scope.id().to_string(),
                    name: contract.scope.id().to_string(),
                    enabled: false,
                    verification_status: crate::provider::ConnectionVerificationStatus::Pending,
                });
            CustomEndpointContractView {
                scope_kind: crate::provider_contracts::SCOPE_KIND_CUSTOM_ENDPOINT,
                scope_id: contract.scope.id().to_string(),
                provider_id: crate::provider::CUSTOM_PROVIDER_ID.to_string(),
                account,
                catalog: contract.catalog.clone(),
                models: contract.models.values().cloned().collect(),
                protocols: contract.switches,
                pricing: CapabilitySummary {
                    availability: descriptor.pricing.availability,
                },
                usage: CapabilitySummary {
                    availability: descriptor.usage.catalog_availability,
                },
                card: card_summary(descriptor),
                catalog_routable: contract.catalog_routable,
                production_inference: contract.production_inference,
                disabled_reasons: contract.disabled_reasons.clone(),
                revision: contract.revision,
            }
        })
        .collect();
    ProviderContractsResponse {
        revision: settings_revision,
        providers,
        custom_endpoints,
    }
}

fn account_choice_view(
    account: &Account,
    statuses: &std::collections::HashMap<String, crate::provider::ConnectionVerificationStatus>,
) -> ProviderAccountChoiceView {
    let verification_status = statuses.get(&account.id).copied().unwrap_or_else(|| {
        crate::provider::builtin_plan(&account.provider_id, &account.offering_id)
            .map(crate::provider::default_verification_status)
            .unwrap_or(crate::provider::ConnectionVerificationStatus::NotRequired)
    });
    ProviderAccountChoiceView {
        id: account.id.clone(),
        name: account.name.clone(),
        enabled: account.enabled,
        verification_status,
    }
}

fn card_summary(descriptor: crate::provider::ProviderDescriptor) -> CardCapabilitySummary {
    CardCapabilitySummary {
        fetch_zen_models: descriptor.card_actions.fetch_zen_models,
        discover_models: descriptor.card_actions.discover_models,
        protocol_probe: descriptor.card_actions.protocol_probe,
        catalog_refresh: descriptor.card_actions.catalog_refresh,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolSwitchUpdate {
    enabled: bool,
    #[serde(default)]
    expected_revision: Option<u64>,
}

async fn update_provider_contract_protocol(
    State(state): State<CoreState>,
    Path((scope_kind, scope_id, protocol)): Path<(String, String, String)>,
    Json(input): Json<ProtocolSwitchUpdate>,
) -> Result<Json<ProviderContractsResponse>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    let scope = crate::provider_contracts::ContractScope::parse(&scope_kind, &scope_id)
        .map_err(ApiError::bad_request)?;
    let protocol = crate::provider_contracts::parse_upstream_protocol(&protocol)
        .map_err(ApiError::bad_request)?;
    validate_contract_scope_ownership(&state, &scope)?;
    let now = Utc::now();
    {
        let db = state.db.lock();
        db.set_protocol_switch(&scope, protocol, input.enabled, now)
            .map_err(ApiError::internal)?;
        state
            .reload_provider_contracts_locked(&db)
            .map_err(ApiError::internal)?;
    }
    state.routing.reset();
    let revision = state.bump_settings_revision();
    let (accounts, statuses) = load_accounts_with_verification(&state)?;
    Ok(Json(build_provider_contracts_response(
        &state.provider_contracts(),
        &accounts,
        &statuses,
        revision,
    )))
}

fn validate_contract_scope_ownership(
    state: &CoreState,
    scope: &crate::provider_contracts::ContractScope,
) -> Result<(), ApiError> {
    match scope {
        crate::provider_contracts::ContractScope::Provider(provider_id) => {
            if crate::provider_contracts::builtin_provider_scope_ids()
                .contains(&provider_id.as_str())
            {
                Ok(())
            } else {
                Err(ApiError::not_found("provider contract scope not found"))
            }
        }
        crate::provider_contracts::ContractScope::CustomEndpoint(account_id) => {
            let account = state
                .db
                .lock()
                .get_account(account_id)
                .map_err(ApiError::internal)?
                .ok_or_else(|| ApiError::not_found("custom endpoint contract scope not found"))?;
            if crate::provider::ProviderAdapterKind::from_offering(
                &account.provider_id,
                &account.offering_id,
            ) == Some(crate::provider::ProviderAdapterKind::ConfigurableHttp)
            {
                Ok(())
            } else {
                Err(ApiError::bad_request(
                    "custom_endpoint scopes are only valid for Custom API accounts",
                ))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtocolProbeRequest {
    model_id: String,
    protocols: Vec<crate::provider::UpstreamProtocolKind>,
}

#[derive(Debug, Serialize)]
struct ProtocolProbeResponse {
    account_id: String,
    model_id: String,
    results: Vec<ProtocolProbeResultView>,
    contract: Option<crate::provider_contracts::EffectiveModelContract>,
}

#[derive(Debug, Serialize)]
struct ProtocolProbeResultView {
    protocol: &'static str,
    success: bool,
    skipped: bool,
    error: Option<String>,
}

async fn run_account_protocol_probes(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(input): Json<ProtocolProbeRequest>,
) -> Result<Json<ProtocolProbeResponse>, ApiError> {
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    let descriptor =
        crate::provider::ProviderRegistry::get(&account.provider_id, &account.offering_id)
            .ok_or_else(|| ApiError::bad_request("unknown provider offering"))?;
    if !descriptor.protocol_probe.explicit_probe {
        return Err(ApiError::status(
            StatusCode::NOT_IMPLEMENTED,
            "protocol probes are not available for this Plan in this slice",
        ));
    }
    let model_id = input.model_id.trim();
    if model_id.is_empty() {
        return Err(ApiError::bad_request("model_id is required"));
    }
    if input.protocols.is_empty() {
        return Err(ApiError::bad_request(
            "at least one explicit upstream protocol is required",
        ));
    }
    require_unique_probe_protocols(&input.protocols)?;
    let adapter = descriptor.kind;
    let scope = crate::provider_contracts::ContractScope::from_account(&account)
        .ok_or_else(|| ApiError::bad_request("account does not own a provider contract scope"))?;
    let declared = if adapter == crate::provider::ProviderAdapterKind::ConfigurableHttp {
        state
            .db
            .lock()
            .list_account_model_capabilities_declared(&account.id)
            .map_err(ApiError::internal)?
            .into_iter()
            .map(|capability| (capability.model_id, capability.protocol))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut results = Vec::new();
    let now = Utc::now();
    for protocol in &input.protocols {
        if !crate::provider_contracts::probe_may_add(adapter, model_id, *protocol, &declared) {
            results.push(ProtocolProbeResultView {
                protocol: protocol.as_str(),
                success: false,
                skipped: true,
                error: Some("probe combination is outside the adapter safety ceiling".to_string()),
            });
            continue;
        }
        let existing = state
            .db
            .lock()
            .load_model_protocol(&scope, model_id, *protocol)
            .map_err(ApiError::internal)?;
        let outcome = execute_protocol_probe(&state, &account, adapter, model_id, *protocol).await;
        let (success, error) = match &outcome {
            Ok(()) => (true, None),
            Err(message) => (false, Some(message.clone())),
        };
        let persisted = crate::provider_contracts::apply_probe_observation(
            existing.as_ref(),
            scope.clone(),
            model_id,
            *protocol,
            success,
            error.clone(),
            now,
            true,
        )
        .map_err(ApiError::bad_request)?;
        state
            .db
            .lock()
            .upsert_model_protocol(&persisted)
            .map_err(ApiError::internal)?;
        results.push(ProtocolProbeResultView {
            protocol: protocol.as_str(),
            success,
            skipped: false,
            error,
        });
    }
    state
        .reload_provider_contracts()
        .map_err(ApiError::internal)?;
    let contract = state
        .provider_contracts()
        .scope(&scope)
        .and_then(|scope| scope.model(model_id).cloned());
    Ok(Json(ProtocolProbeResponse {
        account_id: account.id,
        model_id: model_id.to_string(),
        results,
        contract,
    }))
}

fn require_unique_probe_protocols(
    protocols: &[crate::provider::UpstreamProtocolKind],
) -> Result<(), ApiError> {
    let mut seen = HashSet::new();
    for protocol in protocols {
        if !seen.insert(*protocol) {
            return Err(ApiError::bad_request("duplicate upstream protocol"));
        }
    }
    Ok(())
}

async fn execute_protocol_probe(
    state: &CoreState,
    account: &Account,
    adapter: crate::provider::ProviderAdapterKind,
    model_id: &str,
    protocol: crate::provider::UpstreamProtocolKind,
) -> Result<(), String> {
    use crate::custom_http::{
        HttpInferenceTransport, HttpInferenceTransportSpec, InferenceHttpRequest,
        json_content_headers,
    };
    use crate::gateway::protocol::RequestPlan;
    use crate::models::UpstreamChannel;
    use crate::provider_contracts::protocol_to_api;

    let format = protocol_to_api(protocol);
    let config = state.config();
    let body = crate::custom::minimal_verification_body(protocol, model_id)
        .map_err(|error| error.message)?;
    let mut plan = RequestPlan {
        client: format,
        upstream: format,
        model: model_id.to_string(),
        client_model: model_id.to_string(),
        stream: false,
        body: bytes::Bytes::from(body.clone()),
        channel: if adapter == crate::provider::ProviderAdapterKind::ZenFree {
            UpstreamChannel::Free
        } else {
            UpstreamChannel::Go
        },
        upstream_base_override: None,
        original_model: None,
        allow_go_fallback: false,
        resolved_alias: None,
        custom_route: None,
        service_tier: None,
        custom_tools: Vec::new(),
        namespace_tools: Vec::new(),
        response_parallel_tool_calls: true,
        response_tool_choice: serde_json::json!("auto"),
        response_tools: Vec::new(),
    };
    if adapter == crate::provider::ProviderAdapterKind::ConfigurableHttp {
        let custom = state
            .db
            .lock()
            .account_custom_config(&account.id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                "Custom API accounts require a persisted base URL, protocol, and auth scheme"
                    .to_string()
            })?;
        plan.custom_route = Some(crate::gateway::protocol::CustomRouteSpec {
            base_url: custom.base_url,
            auth_scheme: custom.auth_scheme,
        });
    }
    let route = crate::gateway::provider_adapter::resolve_probe_route(account, &config, &plan)?;
    let secret = if matches!(
        route.auth,
        crate::gateway::provider_adapter::UpstreamAuth::None
    ) {
        None
    } else {
        Some(
            state
                .decrypt_key(&account.key_cipher)
                .map_err(|error| error.to_string())?,
        )
    };
    let spec = if route.follow_redirects {
        HttpInferenceTransportSpec::follow_redirects()
    } else {
        HttpInferenceTransportSpec::no_redirects()
    };
    let transport =
        HttpInferenceTransport::build(&config, spec).map_err(|error| error.to_string())?;
    let url = HttpInferenceTransport::join_endpoint(&route.base_url, &route.path)
        .map_err(|error| error.to_string())?;
    let extra = json_content_headers(protocol == crate::provider::UpstreamProtocolKind::Messages)
        .map_err(|error| error.to_string())?;
    let timeout = std::time::Duration::from_secs(config.non_stream_timeout_secs.clamp(5, 30));
    let auth = match (route.auth, secret.as_deref()) {
        (crate::gateway::provider_adapter::UpstreamAuth::None, _) => None,
        (crate::gateway::provider_adapter::UpstreamAuth::XApiKey, Some(key)) => {
            Some((crate::provider::UpstreamAuthScheme::XApiKey, key))
        }
        (crate::gateway::provider_adapter::UpstreamAuth::Bearer, Some(key)) => {
            Some((crate::provider::UpstreamAuthScheme::Bearer, key))
        }
        (crate::gateway::provider_adapter::UpstreamAuth::OpenCodeProtocolDefault, Some(key))
            if format == ApiFormat::Messages =>
        {
            Some((crate::provider::UpstreamAuthScheme::XApiKey, key))
        }
        (crate::gateway::provider_adapter::UpstreamAuth::OpenCodeProtocolDefault, Some(key)) => {
            Some((crate::provider::UpstreamAuthScheme::Bearer, key))
        }
        (_, None) => {
            return Err("account is missing a decrypted credential for this probe".to_string());
        }
    };
    let response = transport
        .send(InferenceHttpRequest {
            method: reqwest::Method::POST,
            url,
            auth,
            extra_headers: extra,
            body: Some(body),
            request_timeout: Some(timeout),
        })
        .await
        .map_err(|error| {
            crate::provider_contracts::sanitize_probe_error(&error.to_string(), secret.as_deref())
        })?;
    let status = response.status();
    let bytes = HttpInferenceTransport::read_body_limited(
        response,
        crate::custom::MAX_CUSTOM_VERIFICATION_BODY_BYTES,
    )
    .await
    .map_err(|error| {
        crate::provider_contracts::sanitize_probe_error(&error.to_string(), secret.as_deref())
    })?;
    if !status.is_success() {
        let raw = String::from_utf8_lossy(&bytes);
        return Err(crate::provider_contracts::sanitize_probe_error(
            &format!("upstream returned {} {raw}", status.as_u16()),
            secret.as_deref(),
        ));
    }
    let parsed: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| "protocol probe did not return a JSON object".to_string())?;
    if !parsed.is_object() {
        return Err("protocol probe did not return a JSON object".to_string());
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct ProviderModelCapability {
    model_id: String,
    provider_id: String,
    offering_id: String,
    preferred_protocol: &'static str,
    supported_protocols: Vec<&'static str>,
}

/// The gateway model set is currently backed only by OpenCode Go. Do not
/// advertise those models as GOAT capabilities merely because GOAT is a
/// selectable account binding.
async fn provider_model_capabilities() -> Json<Vec<ProviderModelCapability>> {
    Json(
        supported_model_protocol_profiles()
            .map(|(model_id, preferred, supported)| ProviderModelCapability {
                model_id: model_id.to_string(),
                provider_id: crate::provider::OPENCODE_PROVIDER_ID.to_string(),
                offering_id: crate::provider::GO_OFFERING_ID.to_string(),
                preferred_protocol: api_format_name(preferred),
                supported_protocols: supported.iter().copied().map(api_format_name).collect(),
            })
            .collect(),
    )
}

#[derive(Debug, Serialize)]
struct ProviderPricingResponse {
    provider_id: String,
    offering_id: String,
    availability: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot: Option<crate::provider::ProviderPricingSnapshot>,
}

async fn provider_pricing(
    State(state): State<CoreState>,
    Path((provider_id, offering_id)): Path<(String, String)>,
) -> Result<Json<ProviderPricingResponse>, ApiError> {
    let descriptor = crate::provider::ProviderRegistry::get(&provider_id, &offering_id)
        .ok_or_else(|| ApiError::not_found("provider offering not found"))?;
    let snapshot = if descriptor.pricing.availability == "available" {
        state
            .db
            .lock()
            .latest_provider_pricing_snapshot(descriptor.provider_id, descriptor.offering_id)
            .map_err(ApiError::internal)?
    } else {
        None
    };
    Ok(Json(ProviderPricingResponse {
        provider_id,
        offering_id,
        availability: descriptor.pricing.availability,
        snapshot,
    }))
}

#[derive(Debug, Serialize)]
struct ProviderUsageResponse {
    account_id: String,
    provider_id: String,
    offering_id: String,
    availability: &'static str,
    experimental: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    free_cooldown_until: Option<DateTime<Utc>>,
    quota_windows: Vec<crate::provider::QuotaWindow>,
    credit_balances: Vec<crate::provider::CreditBalance>,
    sync_state: Option<crate::db::AccountUsageSyncState>,
}

async fn provider_account_usage(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<ProviderUsageResponse>, ApiError> {
    let db = state.db.lock();
    let account = db
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    let descriptor =
        crate::provider::ProviderRegistry::get(&account.provider_id, &account.offering_id)
            .ok_or_else(|| ApiError::bad_request("unknown provider offering"))?;
    if descriptor.usage.catalog_availability == "unavailable" {
        return Ok(Json(ProviderUsageResponse {
            account_id: id,
            provider_id: account.provider_id,
            offering_id: account.offering_id,
            availability: descriptor.usage.catalog_availability,
            experimental: descriptor.usage.experimental,
            free_cooldown_until: None,
            quota_windows: Vec::new(),
            credit_balances: Vec::new(),
            sync_state: db
                .account_usage_sync_state(&account.id)
                .map_err(ApiError::internal)?,
        }));
    }
    let free_cooldown_until = if descriptor.error_cooldown.egress_ip_shared_free_cooldown {
        db.free_channel_cooldown_until()
            .map_err(ApiError::internal)?
    } else {
        None
    };
    let quota_windows = if descriptor.usage.authoritative_for_quota {
        db.live_opencode_go_quota_windows(&account.id, &state.pricing_snapshot().limits)
            .map_err(ApiError::internal)?
    } else if descriptor.error_cooldown.egress_ip_shared_free_cooldown {
        vec![crate::provider::QuotaWindow {
            account_id: account.id.clone(),
            window_kind: crate::provider::QUOTA_WINDOW_FREE.to_string(),
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
        }]
    } else {
        db.list_quota_windows(&account.id)
            .map_err(ApiError::internal)?
    };
    Ok(Json(ProviderUsageResponse {
        account_id: id,
        provider_id: account.provider_id,
        offering_id: account.offering_id,
        availability: descriptor.usage.catalog_availability,
        experimental: descriptor.usage.experimental,
        free_cooldown_until,
        quota_windows,
        credit_balances: db
            .list_credit_balances(&account.id)
            .map_err(ApiError::internal)?,
        sync_state: db
            .account_usage_sync_state(&account.id)
            .map_err(ApiError::internal)?,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ZenFreeSettingsInput {
    enabled: bool,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Serialize)]
struct ZenFreeSettingsResponse {
    account: DashboardAccount,
    revision: u64,
}

async fn apply_zen_free_settings(
    state: &CoreState,
    input: ZenFreeSettingsInput,
) -> Result<Json<ZenFreeSettingsResponse>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(state, input.expected_revision)?;
    {
        let db = state.db.lock();
        db.set_zen_free_enabled(input.enabled)
            .map_err(ApiError::internal)?;
    }
    let revision = state.bump_settings_revision();
    let account = state
        .db
        .lock()
        .get_account(crate::provider::ZEN_FREE_ACCOUNT_ID)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::internal("Zen Free singleton is missing"))?;
    Ok(Json(ZenFreeSettingsResponse {
        account: dashboard_account(state, account),
        revision,
    }))
}

async fn update_zen_free_settings(
    State(state): State<CoreState>,
    Json(input): Json<ZenFreeSettingsInput>,
) -> Result<Json<ZenFreeSettingsResponse>, ApiError> {
    apply_zen_free_settings(&state, input).await
}

async fn update_zen_free_settings_for_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(input): Json<ZenFreeSettingsInput>,
) -> Result<Json<ZenFreeSettingsResponse>, ApiError> {
    if id != crate::provider::ZEN_FREE_ACCOUNT_ID {
        return Err(ApiError::bad_request(
            "provider settings are only available for the Zen Free singleton",
        ));
    }
    apply_zen_free_settings(&state, input).await
}

async fn list_accounts(
    State(state): State<CoreState>,
) -> Result<Json<Vec<DashboardAccount>>, ApiError> {
    let accounts = state
        .db
        .lock()
        .list_accounts()
        .map_err(ApiError::internal)?;
    Ok(Json(
        accounts
            .into_iter()
            .map(|account| dashboard_account(&state, account))
            .collect::<Vec<_>>(),
    ))
}

#[derive(Debug, Deserialize)]
struct DashboardAccountInput {
    #[serde(flatten)]
    account: AccountInput,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    custom_config: Option<AccountCustomConfigInput>,
    #[serde(default)]
    acknowledgements: Vec<AccountAcknowledgementInput>,
    #[serde(default)]
    model_capabilities: Vec<AccountModelCapabilityInput>,
}

async fn create_account_route(
    State(state): State<CoreState>,
    Json(input): Json<DashboardAccountInput>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    let mut account = create_account_inner(
        state.clone(),
        input.account,
        input.custom_config,
        input.acknowledgements,
        input.model_capabilities,
    )?;
    // Account creation is a shared control-plane mutation; stamp the returned
    // legacy account DTO with the same revision used by following CAS writes.
    account.0.revision = state.bump_settings_revision();
    Ok(account)
}

#[cfg(test)]
async fn create_account(
    State(state): State<CoreState>,
    Json(input): Json<AccountInput>,
) -> Result<Json<DashboardAccount>, ApiError> {
    create_account_inner(state, input, None, Vec::new(), Vec::new())
}

fn create_account_inner(
    state: CoreState,
    input: AccountInput,
    custom_config: Option<AccountCustomConfigInput>,
    acknowledgements: Vec<AccountAcknowledgementInput>,
    model_capabilities: Vec<AccountModelCapabilityInput>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("name is required"));
    }
    let provider_id = input.provider_id.trim();
    let offering_id = input.offering_id.trim();
    let plan = crate::provider::builtin_plan(provider_id, offering_id).ok_or_else(|| {
        ApiError::bad_request(format!(
            "unknown provider offering `{provider_id}/{offering_id}`"
        ))
    })?;
    let offering = plan.offering;
    if plan.creation_availability == crate::provider::CreationAvailability::Unavailable {
        return Err(ApiError::bad_request(
            plan.creation_unavailable_reason
                .unwrap_or("this Plan cannot be created through the generic account API")
                .to_string(),
        ));
    }
    if offering.singleton_account_id.is_some() {
        return Err(ApiError::bad_request(
            "Zen Free is a built-in singleton and cannot be created through the generic account API",
        ));
    }
    crate::provider::validate_plan_key(plan, &input.key)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let requires_custom = crate::provider::plan_requires_custom_config(plan);
    if requires_custom {
        match custom_config.as_ref() {
            Some(config) => {
                crate::provider::validate_custom_base_url(&config.base_url)
                    .map_err(|error| ApiError::bad_request(error.to_string()))?;
            }
            None => {
                return Err(ApiError::bad_request(
                    "Custom API accounts require a base URL, upstream protocol, and auth scheme",
                ));
            }
        }
        if model_capabilities.is_empty() {
            return Err(ApiError::bad_request(
                "Custom API accounts require at least one model capability",
            ));
        }
        for capability in &model_capabilities {
            crate::provider::validate_custom_model_id(&capability.model_id)
                .map_err(|error| ApiError::bad_request(error.to_string()))?;
            if let Some(config) = custom_config.as_ref() {
                if capability.protocol != config.upstream_protocol {
                    return Err(ApiError::bad_request(
                        "model capability protocol must match account custom_config.upstream_protocol",
                    ));
                }
            }
        }
    } else {
        if custom_config.is_some() {
            return Err(ApiError::bad_request(
                "custom config is only available for Custom API accounts",
            ));
        }
        if !model_capabilities.is_empty() {
            return Err(ApiError::bad_request(
                "model capabilities are only available for Custom API accounts",
            ));
        }
    }
    if let Some(notice) = plan.risk_notice {
        let accepted = acknowledgements.iter().any(|item| {
            item.acknowledgement_id == notice.acknowledgement_id && item.version == notice.version
        });
        if !accepted {
            return Err(ApiError::bad_request(
                "this Plan requires a matching versioned risk acknowledgement before create",
            ));
        }
    }
    let requires_verification =
        plan.verification_policy == crate::provider::VerificationPolicy::Required;
    let enabled =
        crate::provider::offering_allows_enablement(offering.provider_id, offering.offering_id)
            && !requires_verification;
    let purchase_date = match input.purchase_date {
        Some(value) if !value.trim().is_empty() => normalize_purchase_date(&value)
            .map_err(|error| ApiError::bad_request(error.to_string()))?,
        _ => String::new(),
    };
    let notes = match input.notes.as_deref() {
        Some(value) => normalize_account_notes(value)
            .map_err(|error| ApiError::bad_request(error.to_string()))?,
        None => None,
    };
    let now = Utc::now();
    let id = uuid::Uuid::new_v4().to_string();
    let account = Account {
        id: id.clone(),
        provider_id: offering.provider_id.to_string(),
        offering_id: offering.offering_id.to_string(),
        credential_kind: offering.credential_kind,
        quota_scope: offering.quota_scope,
        name,
        username: clean_optional(input.username),
        password_cipher: encrypted_optional(&state, &input.password)?,
        key_cipher: state
            .encrypt_key(input.key.trim())
            .map_err(ApiError::internal)?,
        enabled,
        account_type: AccountType::Key,
        setup_step: AccountSetupStep::Ready,
        referral_code: clean_optional(input.referral_code),
        purchase_date,
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes,
        created_at: now,
        updated_at: now,
    };
    let account = {
        let db = state.db.lock();
        db.create_account_with_contract(
            &account,
            custom_config.as_ref(),
            &model_capabilities,
            plan.risk_notice,
        )
        .map_err(map_account_write_error)?;
        let _ = db.log_gateway(
            "info",
            "account",
            &format!("created account {}", account.name),
        );
        db.get_account(&id)
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::internal("created account not found"))?
    };
    state
        .reload_provider_contracts()
        .map_err(ApiError::internal)?;
    Ok(Json(dashboard_account(&state, account)))
}

fn map_account_write_error(error: anyhow::Error) -> ApiError {
    if let Some(binding) = error.downcast_ref::<crate::provider::ProviderBindingError>() {
        return map_provider_binding_error(binding.clone());
    }
    let message = error.to_string();
    if message.contains("not routable") {
        ApiError::status(StatusCode::CONFLICT, message)
    } else if message.contains("Custom API accounts require")
        || message.contains("only available for Custom")
        || message.contains("risk acknowledgement")
        || message.contains("base URL")
        || message.contains("model id")
        || message.contains("model capability")
        || message.contains("protocol and auth")
        || message.contains("duplicate model")
    {
        ApiError::bad_request(message)
    } else {
        ApiError::internal(error)
    }
}

fn map_provider_binding_error(error: crate::provider::ProviderBindingError) -> ApiError {
    match error {
        crate::provider::ProviderBindingError::EnablementNotRoutable { .. } => {
            ApiError::status(StatusCode::CONFLICT, error.to_string())
        }
        other => ApiError::bad_request(other.to_string()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedAccountInput {
    name: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

async fn create_managed_account(
    State(state): State<CoreState>,
    Json(input): Json<ManagedAccountInput>,
) -> Result<(StatusCode, Json<DashboardAccount>), ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    if state.config().opencode_invite_url.is_empty() {
        return Err(ApiError::status(
            StatusCode::PRECONDITION_FAILED,
            "configure an OpenCode invite URL before registering a managed account",
        ));
    }
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err(ApiError::bad_request("name is required"));
    }
    if name.chars().count() > 200 {
        return Err(ApiError::bad_request("name must be at most 200 characters"));
    }
    let notes = match input.notes.as_deref() {
        Some(value) => normalize_account_notes(value)
            .map_err(|error| ApiError::bad_request(error.to_string()))?,
        None => None,
    };

    let now = Utc::now();
    let id = uuid::Uuid::new_v4().to_string();
    let account = Account {
        id: id.clone(),
        provider_id: crate::provider::default_provider_id(),
        offering_id: crate::provider::default_offering_id(),
        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name,
        username: clean_optional(input.username),
        password_cipher: None,
        key_cipher: String::new(),
        enabled: false,
        account_type: AccountType::Managed,
        setup_step: AccountSetupStep::GoogleAccount,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes,
        created_at: now,
        updated_at: now,
    };
    let account = {
        let db = state.db.lock();
        db.create_account(&account).map_err(ApiError::internal)?;
        let _ = db.log_gateway(
            "info",
            "account",
            &format!("created managed account draft {}", account.name),
        );
        db.get_account(&id)
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::internal("created managed account not found"))?
    };
    state.bump_settings_revision();
    Ok((
        StatusCode::CREATED,
        Json(dashboard_account(&state, account)),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountSetupUpdate {
    setup_step: AccountSetupStep,
    #[serde(default)]
    expected_revision: Option<u64>,
}

async fn advance_account_setup(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(input): Json<AccountSetupUpdate>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    let current = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    if current.account_type != AccountType::Managed {
        return Err(ApiError::bad_request(
            "setup steps are only available for managed accounts",
        ));
    }
    if current.setup_step == input.setup_step {
        return Ok(Json(dashboard_account(&state, current)));
    }
    if !current.setup_step.can_transition_to(input.setup_step) {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            format!(
                "setup cannot move from {} to {}",
                current.setup_step.as_str(),
                input.setup_step.as_str()
            ),
        ));
    }
    if !state
        .db
        .lock()
        .advance_managed_setup(&id, current.setup_step, input.setup_step)
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "setup changed; reload the account and try again",
        ));
    }
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    state.bump_settings_revision();
    Ok(Json(dashboard_account(&state, account)))
}

const GOOGLE_SIGNUP_URL: &str = "https://accounts.google.com/signup";
const GOOGLE_LOGIN_URL: &str = "https://accounts.google.com/ServiceLogin";
const GITHUB_SIGNUP_URL: &str = "https://github.com/signup";
const GITHUB_LOGIN_URL: &str = "https://github.com/login";
const OPENCODE_CONSOLE_URL: &str = "https://opencode.ai/auth";

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BrowserTarget {
    GoogleSignup,
    GoogleLogin,
    GithubSignup,
    GithubLogin,
    Invite,
    Console,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenBrowserInput {
    target: BrowserTarget,
}

async fn browser_capabilities(State(state): State<CoreState>) -> Json<BrowserCapabilities> {
    Json(state.browser.capabilities().await)
}

async fn open_account_browser(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<OpenBrowserInput>,
) -> Result<Json<BrowserOpenResult>, ApiError> {
    let browser_operation = state.browser.operation().await;
    state
        .recover_browser_profiles_for_account(&id)
        .map_err(ApiError::internal)?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    if account.account_type != AccountType::Managed
        && !matches!(input.target, BrowserTarget::Console)
    {
        return Err(ApiError::bad_request(
            "imported key accounts can only open the OpenCode console",
        ));
    }
    let url = match input.target {
        BrowserTarget::GoogleSignup => GOOGLE_SIGNUP_URL.to_string(),
        BrowserTarget::GoogleLogin => GOOGLE_LOGIN_URL.to_string(),
        BrowserTarget::GithubSignup => GITHUB_SIGNUP_URL.to_string(),
        BrowserTarget::GithubLogin => GITHUB_LOGIN_URL.to_string(),
        BrowserTarget::Invite => {
            let invite = state.config().opencode_invite_url;
            if invite.is_empty() {
                return Err(ApiError::status(
                    StatusCode::PRECONDITION_FAILED,
                    "configure an OpenCode invite URL before opening this step",
                ));
            }
            invite
        }
        BrowserTarget::Console => OPENCODE_CONSOLE_URL.to_string(),
    };
    let binding = dashboard_session_binding(&state, &headers)?;
    browser_operation
        .open(&id, &url, &binding)
        .await
        .map(Json)
        .map_err(|error| ApiError::status(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))
}

async fn reset_account_browser_profile(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    expectation: Option<Json<AccountRevisionExpectation>>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let expected_revision = expectation.and_then(|Json(body)| body.expected_revision);
    {
        let _settings_update = state.settings_update.lock();
        check_key_revision(&state, expected_revision)?;
        state
            .recover_browser_profiles_for_account(&id)
            .map_err(ApiError::internal)?;
        state
            .db
            .lock()
            .get_account(&id)
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::not_found("account not found"))?;
    }
    let browser_operation = state.browser.operation().await;
    browser_operation
        .stop_account(&id)
        .await
        .map_err(|error| ApiError::status(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, expected_revision)?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    let staged = StagedBrowserProfiles::stage(
        &state.data_dir(),
        &id,
        BrowserProfileOperationKind::ResetProfile,
    )
    .map_err(ApiError::internal)?;
    if account.account_type == AccountType::Managed && !account.setup_step.is_ready() {
        if let Err(error) = state.db.lock().reset_pending_managed_setup(&id) {
            let purge_error = staged.purge().err();
            return Err(ApiError::internal(match purge_error {
                Some(purge) => format!(
                    "failed to reset managed setup: {error}; failed to finish browser profile reset: {purge}"
                ),
                None => format!("failed to reset managed setup: {error}"),
            }));
        }
    }
    staged.purge().map_err(ApiError::internal)?;
    state.bump_settings_revision();
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    Ok(Json(dashboard_account(&state, account)))
}

async fn browser_session_websocket(
    State(state): State<CoreState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    validate_websocket_origin(&headers)?;
    let binding = dashboard_session_binding(&state, &headers)?;
    let mut remote_session = state
        .browser
        .remote_websocket_session(&token, &binding)
        .map_err(|error| ApiError::status(StatusCode::GONE, error.to_string()))?;
    let worker = tokio::select! {
        _ = remote_session.cancellation.changed() => {
            return Err(ApiError::status(StatusCode::GONE, "browser session was replaced"));
        }
        result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio_tungstenite::connect_async(&remote_session.worker_ws_url),
        ) => {
            let (worker, _) = result
                .map_err(|_| ApiError::status(
                    StatusCode::GATEWAY_TIMEOUT,
                    "timed out connecting to remote browser display",
                ))?
                .map_err(|error| ApiError::status(
                    StatusCode::BAD_GATEWAY,
                    format!("failed to connect to remote browser display: {error}"),
                ))?;
            worker
        }
    };
    Ok(websocket.on_upgrade(move |client| {
        proxy_browser_websocket(state, token, remote_session.cancellation, client, worker)
    }))
}

async fn proxy_browser_websocket(
    state: CoreState,
    token: String,
    mut cancellation: tokio::sync::watch::Receiver<bool>,
    client: WebSocket,
    worker: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    use tokio_tungstenite::tungstenite::Message as WorkerWsMessage;

    let (mut client_tx, mut client_rx) = client.split();
    let (mut worker_tx, mut worker_rx) = worker.split();
    let mut expiry_check = tokio::time::interval(std::time::Duration::from_secs(1));
    expiry_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancellation.changed() => { break; }
            _ = expiry_check.tick() => {
                if !state.browser.remote_session_active(&token) { break; }
            }
            message = client_rx.next() => {
                let Some(Ok(message)) = message else { break };
                if !state.browser.touch_remote_session(&token) { break; }
                let message = match message {
                    AxumWsMessage::Text(value) => WorkerWsMessage::Text(value.as_str().into()),
                    AxumWsMessage::Binary(value) => WorkerWsMessage::Binary(value),
                    AxumWsMessage::Ping(value) => WorkerWsMessage::Ping(value),
                    AxumWsMessage::Pong(value) => WorkerWsMessage::Pong(value),
                    AxumWsMessage::Close(_) => break,
                };
                if worker_tx.send(message).await.is_err() { break; }
            }
            message = worker_rx.next() => {
                let Some(Ok(message)) = message else { break };
                if !state.browser.remote_session_active(&token) { break; }
                let message = match message {
                    WorkerWsMessage::Text(value) => AxumWsMessage::Text(value.as_str().into()),
                    WorkerWsMessage::Binary(value) => AxumWsMessage::Binary(value),
                    WorkerWsMessage::Ping(value) => AxumWsMessage::Ping(value),
                    WorkerWsMessage::Pong(value) => AxumWsMessage::Pong(value),
                    WorkerWsMessage::Close(_) => break,
                    WorkerWsMessage::Frame(_) => continue,
                };
                if client_tx.send(message).await.is_err() { break; }
            }
        }
    }
    let _ = worker_tx.close().await;
    let _ = client_tx.close().await;
}

fn dashboard_session_binding(state: &CoreState, headers: &HeaderMap) -> Result<String, ApiError> {
    if is_local_dashboard_request(state, headers) {
        return Ok("local-dashboard".to_string());
    }
    if has_dashboard_session(state, headers) {
        return dashboard_session_value(headers)
            .map(str::to_string)
            .ok_or_else(|| {
                ApiError::status(StatusCode::UNAUTHORIZED, "dashboard session is required")
            });
    }
    Err(ApiError::status(
        StatusCode::UNAUTHORIZED,
        "dashboard session is required",
    ))
}

fn validate_websocket_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("browser WebSocket Origin is required"))?;
    let origin = reqwest::Url::parse(origin)
        .map_err(|_| ApiError::bad_request("browser WebSocket Origin is invalid"))?;
    if !matches!(origin.scheme(), "http" | "https") {
        return Err(ApiError::bad_request(
            "browser WebSocket Origin must use http or https",
        ));
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("browser WebSocket Host is required"))?;
    let authority = Authority::from_str(host)
        .map_err(|_| ApiError::bad_request("browser WebSocket Host is invalid"))?;
    let default_port = if origin.scheme() == "https" { 443 } else { 80 };
    if !origin
        .host_str()
        .is_some_and(|value| value.eq_ignore_ascii_case(authority.host()))
        || origin.port_or_known_default() != Some(authority.port_u16().unwrap_or(default_port))
    {
        return Err(ApiError::status(
            StatusCode::FORBIDDEN,
            "browser WebSocket Origin does not match Host",
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DashboardAccountUpdate {
    name: Option<String>,
    username: Option<String>,
    password: Option<String>,
    key: Option<String>,
    enabled: Option<bool>,
    referral_code: Option<String>,
    #[serde(alias = "recharge_date")]
    purchase_date: Option<String>,
    notes: Option<String>,
    // Binding fields are named explicitly so a generic update fails closed
    // instead of silently ignoring an attempted provider reassignment.
    provider_id: Option<String>,
    offering_id: Option<String>,
    credential_kind: Option<crate::provider::CredentialKind>,
    quota_scope: Option<crate::provider::QuotaScope>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

impl DashboardAccountUpdate {
    fn binding_change_requested(&self) -> bool {
        self.provider_id.is_some()
            || self.offering_id.is_some()
            || self.credential_kind.is_some()
            || self.quota_scope.is_some()
    }

    fn into_account_update(self) -> AccountUpdate {
        AccountUpdate {
            name: self.name,
            username: self.username,
            password: self.password,
            key: self.key,
            enabled: self.enabled,
            referral_code: self.referral_code,
            purchase_date: self.purchase_date,
            notes: self.notes,
        }
    }
}

impl From<AccountUpdate> for DashboardAccountUpdate {
    fn from(update: AccountUpdate) -> Self {
        Self {
            name: update.name,
            username: update.username,
            password: update.password,
            key: update.key,
            enabled: update.enabled,
            referral_code: update.referral_code,
            purchase_date: update.purchase_date,
            notes: update.notes,
            provider_id: None,
            offering_id: None,
            credential_kind: None,
            quota_scope: None,
            expected_revision: None,
        }
    }
}

async fn update_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(input): Json<DashboardAccountUpdate>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    if input.binding_change_requested() {
        return Err(ApiError::bad_request(
            "provider binding is immutable; create a new account for another provider offering",
        ));
    }
    let mut update = input.into_account_update();
    let existing = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    if existing.is_zen_free() {
        return Err(ApiError::bad_request(
            "Zen Free settings must use the dedicated provider-settings endpoint",
        ));
    }
    if !existing.setup_step.is_ready()
        && (update.enabled == Some(true)
            || update
                .key
                .as_deref()
                .is_some_and(|key| !key.trim().is_empty()))
    {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "finish managed-account key verification before enabling or replacing its key",
        ));
    }
    if update.enabled == Some(true) {
        ensure_account_can_enable(&state, &existing)?;
    }
    if let Some(value) = update.purchase_date.take() {
        update.purchase_date = Some(
            normalize_purchase_date(&value)
                .map_err(|error| ApiError::bad_request(error.to_string()))?,
        );
    }
    if let Some(value) = update.notes.take() {
        update.notes = Some(
            normalize_account_notes(&value)
                .map_err(|error| ApiError::bad_request(error.to_string()))?
                .unwrap_or_default(),
        );
    }
    if let Some(plan) = crate::provider::builtin_plan(&existing.provider_id, &existing.offering_id)
    {
        if let Some(key) = update.key.as_deref() {
            crate::provider::validate_plan_key(plan, key)
                .map_err(|error| ApiError::bad_request(error.to_string()))?;
        }
    }
    let key_cipher = match update.key.as_deref().map(str::trim) {
        Some("") | None => None,
        Some(key) => Some(state.encrypt_key(key).map_err(ApiError::internal)?),
    };
    let password_cipher = match update.password.as_deref().map(str::trim) {
        Some("") => Some(String::new()),
        None => None,
        Some(password) => Some(state.encrypt_key(password).map_err(ApiError::internal)?),
    };
    {
        let db = state.db.lock();
        db.update_account(
            &id,
            &update,
            key_cipher.as_deref(),
            password_cipher.as_deref(),
        )
        .map_err(map_account_write_error)?;
        let _ = db.log_gateway("info", "account", &format!("updated account {}", id));
    }
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    state.bump_settings_revision();
    Ok(Json(dashboard_account(&state, account)))
}

fn ensure_account_can_enable(state: &CoreState, account: &Account) -> Result<(), ApiError> {
    crate::provider::ensure_offering_can_enable(&account.provider_id, &account.offering_id)
        .map_err(map_provider_binding_error)?;
    let plan = crate::provider::builtin_plan(&account.provider_id, &account.offering_id)
        .ok_or_else(|| ApiError::bad_request("unknown provider offering"))?;
    if plan.verification_policy == crate::provider::VerificationPolicy::Required {
        let status = state
            .db
            .lock()
            .account_verification_state(&account.id)
            .map_err(ApiError::internal)?
            .map(|state| state.status)
            .unwrap_or(crate::provider::ConnectionVerificationStatus::Pending);
        if !status.allows_enablement() {
            return Err(ApiError::status(
                StatusCode::CONFLICT,
                "verify the account connection before enabling it",
            ));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct AccountOrderInput {
    account_ids: Vec<String>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Deserialize)]
struct AccountRevisionExpectation {
    #[serde(default)]
    expected_revision: Option<u64>,
}

async fn reorder_accounts(
    State(state): State<CoreState>,
    Json(input): Json<AccountOrderInput>,
) -> Result<Json<Vec<DashboardAccount>>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    let accounts = {
        let db = state.db.lock();
        db.reorder_accounts(&input.account_ids)
            .map_err(|error| match error {
                ReorderAccountsError::DuplicateAccountId => {
                    ApiError::bad_request("account_ids contains duplicates")
                }
                ReorderAccountsError::AccountSetMismatch => ApiError::status(
                    StatusCode::CONFLICT,
                    "account list changed; reload accounts and try again",
                ),
                ReorderAccountsError::Database(error) => ApiError::internal(error),
            })?;
        db.list_accounts().map_err(ApiError::internal)?
    };
    state.bump_settings_revision();
    Ok(Json(
        accounts
            .into_iter()
            .map(|account| dashboard_account(&state, account))
            .collect::<Vec<_>>(),
    ))
}

async fn delete_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    expectation: Option<Json<AccountRevisionExpectation>>,
) -> Result<Response, ApiError> {
    if id == crate::provider::ZEN_FREE_ACCOUNT_ID {
        return Err(ApiError::bad_request(
            "Zen Free is a built-in singleton and cannot be deleted",
        ));
    }
    let browser_operation = state.browser.operation().await;
    state
        .recover_browser_profiles_for_account(&id)
        .map_err(ApiError::internal)?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    browser_operation
        .stop_account(&id)
        .await
        .map_err(|error| ApiError::status(StatusCode::SERVICE_UNAVAILABLE, error.to_string()))?;
    let staged = StagedBrowserProfiles::stage(
        &state.data_dir(),
        &id,
        BrowserProfileOperationKind::DeleteAccount,
    )
    .map_err(ApiError::internal)?;
    let _settings_update = state.settings_update.lock();
    if let Err(error) = check_key_revision(
        &state,
        expectation.and_then(|Json(body)| body.expected_revision),
    ) {
        return match staged.restore() {
            Ok(()) => Err(error),
            Err(restore) => Err(ApiError::internal(format!(
                "{}; failed to restore browser profile: {restore}",
                error.message
            ))),
        };
    }
    let delete_result = {
        let mut db = state.db.lock();
        let result = db.delete_account(&id);
        if result.is_ok() {
            let _ = db.log_gateway(
                "info",
                "account",
                &format!("deleted account {} ({})", id, account.name),
            );
        }
        result
    };
    if let Err(error) = delete_result {
        let restore_error = staged.restore().err();
        return Err(ApiError::internal(match restore_error {
            Some(restore) => format!(
                "failed to delete account: {error}; failed to restore browser profile: {restore}"
            ),
            None => format!("failed to delete account: {error}"),
        }));
    }
    staged.purge().map_err(ApiError::internal)?;
    state
        .reload_provider_contracts()
        .map_err(ApiError::internal)?;
    let revision = state.bump_settings_revision();
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(
        "x-ocg-settings-revision",
        HeaderValue::from_str(&revision.to_string()).expect("revision is a valid header value"),
    );
    Ok(response)
}

async fn toggle_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    expectation: Option<Json<AccountRevisionExpectation>>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(
        &state,
        expectation.and_then(|Json(body)| body.expected_revision),
    )?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    if account.is_zen_free() {
        return Err(ApiError::bad_request(
            "Zen Free settings must use the dedicated provider-settings endpoint",
        ));
    }
    let next_enabled = !account.enabled;
    if next_enabled && (!account.setup_step.is_ready() || account.key_cipher.is_empty()) {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "account setup is not complete and cannot be enabled",
        ));
    }
    if next_enabled {
        ensure_account_can_enable(&state, &account)?;
    }
    let update = AccountUpdate {
        name: None,
        username: None,
        password: None,
        key: None,
        enabled: Some(next_enabled),
        referral_code: None,
        purchase_date: None,
        notes: None,
    };
    {
        let db = state.db.lock();
        db.update_account(&id, &update, None, None)
            .map_err(map_account_write_error)?;
    }
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    state.bump_settings_revision();
    Ok(Json(dashboard_account(&state, account)))
}

async fn verify_account_connection(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    expectation: Option<Json<AccountRevisionExpectation>>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(
        &state,
        expectation.and_then(|Json(body)| body.expected_revision),
    )?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    let plan = crate::provider::builtin_plan(&account.provider_id, &account.offering_id)
        .ok_or_else(|| ApiError::bad_request("unknown provider offering"))?;
    if plan.verification_policy == crate::provider::VerificationPolicy::NotRequired {
        return Ok(Json(dashboard_account(&state, account)));
    }
    if crate::provider::plan_requires_custom_config(plan)
        && state
            .db
            .lock()
            .account_custom_config(&id)
            .map_err(ApiError::internal)?
            .is_none()
    {
        return Err(ApiError::bad_request(
            "Custom API accounts require a persisted base URL, protocol, and auth scheme",
        ));
    }
    if let Some(notice) = plan.risk_notice {
        if !state
            .db
            .lock()
            .account_has_acknowledgement(&id, notice)
            .map_err(ApiError::internal)?
        {
            return Err(ApiError::bad_request(
                "this Plan requires a matching versioned risk acknowledgement before verification",
            ));
        }
    }
    if plan.verification_runtime_availability != "available"
        && plan.verification_runtime_availability != "optional"
    {
        return Err(ApiError::status(
            StatusCode::NOT_IMPLEMENTED,
            "connection verification runtime is not available for this Plan in this slice",
        ));
    }
    if crate::provider::is_custom_api(&account.provider_id, &account.offering_id) {
        let verification = state
            .db
            .lock()
            .account_verification_state(&id)
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::not_found("account not found"))?;
        if verification.status == crate::provider::ConnectionVerificationStatus::Verified {
            drop(_settings_update);
            return Ok(Json(dashboard_account(&state, account)));
        }
        let job = capture_custom_verification_job(&state, &account)?;
        drop(_settings_update);
        return complete_custom_verification(&state, job).await;
    }
    Ok(Json(dashboard_account(&state, account)))
}

async fn discover_custom_models_route(
    State(state): State<CoreState>,
    Json(input): Json<CustomModelDiscoveryInput>,
) -> Result<Json<CustomModelDiscoveryResult>, ApiError> {
    let supplied_key = input
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty());
    let stored_key = if supplied_key.is_none()
        && let Some(account_id) = input
            .account_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    {
        let account = state
            .db
            .lock()
            .get_account(account_id)
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::not_found("account not found"))?;
        require_custom_plan(
            &account,
            "model discovery is only available for Custom API accounts",
        )?;
        if account.key_cipher.is_empty() {
            None
        } else {
            Some(
                state
                    .decrypt_key(&account.key_cipher)
                    .map_err(ApiError::internal)?,
            )
        }
    } else {
        None
    };
    let api_key = supplied_key
        .map(str::to_owned)
        .or(stored_key)
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(
                "Custom model discovery requires an API key or an existing Custom account with a stored key",
            )
        })?;
    let config = AccountCustomConfigInput {
        base_url: input.base_url,
        upstream_protocol: input.upstream_protocol,
        auth_scheme: input.auth_scheme,
    };
    crate::custom::discover_custom_models(&state.config(), &config, &api_key)
        .await
        .map(Json)
        // In particular, upstream 401/403 stay a dashboard 400 rather than
        // looking like an expired dashboard session to the frontend.
        .map_err(|failure| ApiError::bad_request(failure.message))
}

struct CustomVerificationJob {
    account: Account,
    contract: crate::custom::CustomVerificationContract,
    custom_config: AccountCustomConfig,
    first_capability: AccountModelCapability,
    api_key: String,
}

fn capture_custom_verification_job(
    state: &CoreState,
    account: &Account,
) -> Result<CustomVerificationJob, ApiError> {
    let db = state.db.lock();
    let custom_config = db
        .account_custom_config(&account.id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::bad_request(
                "Custom API accounts require a persisted base URL, protocol, and auth scheme",
            )
        })?;
    let capabilities = db
        .list_account_model_capabilities_declared(&account.id)
        .map_err(ApiError::internal)?;
    let first_capability = crate::custom::first_declared_capability(&capabilities)
        .cloned()
        .ok_or_else(|| {
            ApiError::bad_request("Custom API accounts require at least one model capability")
        })?;
    let contract = db
        .capture_custom_verification_contract(&account.id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    drop(db);
    let api_key = state
        .decrypt_key(&account.key_cipher)
        .map_err(ApiError::internal)?;
    Ok(CustomVerificationJob {
        account: account.clone(),
        contract,
        custom_config,
        first_capability,
        api_key,
    })
}

async fn complete_custom_verification(
    state: &CoreState,
    job: CustomVerificationJob,
) -> Result<Json<DashboardAccount>, ApiError> {
    let config = state.config();
    let result = crate::custom::probe_custom_connection(
        &config,
        &job.custom_config,
        &job.first_capability,
        &job.api_key,
    )
    .await;
    let _settings_update = state.settings_update.lock();
    let (status, error) = match result {
        Ok(()) => (
            crate::provider::ConnectionVerificationStatus::Verified,
            None,
        ),
        Err(failure) => (
            crate::provider::ConnectionVerificationStatus::Failed,
            Some(failure.message),
        ),
    };
    let verified_at =
        (status == crate::provider::ConnectionVerificationStatus::Verified).then(Utc::now);
    let committed = state
        .db
        .lock()
        .commit_custom_verification_if_contract_matches(
            &job.contract,
            status,
            verified_at,
            error.as_deref(),
        )
        .map_err(ApiError::internal)?;
    if !committed {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            crate::custom::CUSTOM_VERIFICATION_CONFLICT_MESSAGE,
        ));
    }
    let account = state
        .db
        .lock()
        .get_account(&job.account.id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    state.bump_settings_revision();
    Ok(Json(dashboard_account(state, account)))
}

async fn get_account_custom_config(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<Option<AccountCustomConfig>>, ApiError> {
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    require_custom_config_plan(&account)?;
    state
        .db
        .lock()
        .account_custom_config(&id)
        .map(Json)
        .map_err(ApiError::internal)
}

fn require_custom_config_plan(account: &Account) -> Result<(), ApiError> {
    require_custom_plan(
        account,
        "custom config is only available for Custom API accounts",
    )
}

fn require_custom_capabilities_plan(account: &Account) -> Result<(), ApiError> {
    require_custom_plan(
        account,
        "model capabilities are only available for Custom API accounts",
    )
}

fn require_custom_plan(account: &Account, message: &'static str) -> Result<(), ApiError> {
    let plan = crate::provider::builtin_plan(&account.provider_id, &account.offering_id)
        .ok_or_else(|| ApiError::bad_request("unknown provider offering"))?;
    if crate::provider::plan_requires_custom_config(plan) {
        Ok(())
    } else {
        Err(ApiError::bad_request(message))
    }
}

#[derive(Deserialize)]
struct DashboardCustomConfigUpdate {
    #[serde(flatten)]
    config: AccountCustomConfigInput,
    #[serde(default)]
    expected_revision: Option<u64>,
}

async fn put_account_custom_config(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(input): Json<DashboardCustomConfigUpdate>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    require_custom_config_plan(&account)?;
    state
        .db
        .lock()
        .upsert_account_custom_config(&id, &input.config, false)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    state
        .reload_provider_contracts()
        .map_err(ApiError::internal)?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    state.bump_settings_revision();
    Ok(Json(dashboard_account(&state, account)))
}

async fn get_account_model_capabilities(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<AccountModelCapability>>, ApiError> {
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    require_custom_capabilities_plan(&account)?;
    state
        .db
        .lock()
        .list_account_model_capabilities(&id)
        .map(Json)
        .map_err(ApiError::internal)
}

#[derive(Deserialize)]
struct DashboardModelCapabilitiesUpdate {
    capabilities: Vec<AccountModelCapabilityInput>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

async fn put_account_model_capabilities(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(input): Json<DashboardModelCapabilitiesUpdate>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    require_custom_capabilities_plan(&account)?;
    state
        .db
        .lock()
        .replace_account_model_capabilities(&id, &input.capabilities)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    state
        .reload_provider_contracts()
        .map_err(ApiError::internal)?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    state.bump_settings_revision();
    Ok(Json(dashboard_account(&state, account)))
}

async fn list_account_acknowledgements(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<AccountAcknowledgement>>, ApiError> {
    if state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .is_none()
    {
        return Err(ApiError::not_found("account not found"));
    }
    state
        .db
        .lock()
        .list_account_acknowledgements(&id)
        .map(Json)
        .map_err(ApiError::internal)
}

#[derive(Deserialize)]
struct DashboardAcknowledgementCreate {
    acknowledgement_id: String,
    version: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

async fn create_account_acknowledgement(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(input): Json<DashboardAcknowledgementCreate>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    let plan = crate::provider::builtin_plan(&account.provider_id, &account.offering_id)
        .ok_or_else(|| ApiError::bad_request("unknown provider offering"))?;
    let notice = plan.risk_notice.ok_or_else(|| {
        ApiError::bad_request("this Plan does not require a risk acknowledgement")
    })?;
    if notice.acknowledgement_id != input.acknowledgement_id || notice.version != input.version {
        return Err(ApiError::bad_request(
            "acknowledgement id and version must match the current catalog notice",
        ));
    }
    state
        .db
        .lock()
        .record_account_acknowledgement(&id, notice, Utc::now())
        .map_err(ApiError::internal)?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    state.bump_settings_revision();
    Ok(Json(dashboard_account(&state, account)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountTestRequest {
    #[serde(default = "default_account_test_model")]
    model_id: String,
    #[serde(default)]
    protocol: crate::provider::UpstreamProtocolKind,
}

fn default_account_test_model() -> String {
    crate::models::DEFAULT_ACCOUNT_TEST_MODEL.to_string()
}

#[derive(Debug, Serialize)]
struct AccountTestResponse {
    message: String,
    model_id: String,
    protocol: &'static str,
}

async fn test_account(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    input: Option<Json<AccountTestRequest>>,
) -> Result<Json<AccountTestResponse>, ApiError> {
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    if account.is_zen_free() {
        return Err(ApiError::bad_request("Zen Free has no credential to test"));
    }
    let adapter = crate::provider::ProviderAdapterKind::from_offering(
        &account.provider_id,
        &account.offering_id,
    );
    if adapter != Some(crate::provider::ProviderAdapterKind::OpenCodeGo) {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "provider account testing is unavailable until its upstream contract is configured",
        ));
    }
    if !account.setup_step.is_ready() || account.key_cipher.is_empty() {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "account setup is not complete",
        ));
    }
    let (model_id, protocol) = account_test_case(input.map(|Json(value)| value))?;
    let format = crate::custom::api_format_for_custom_protocol(protocol);
    let upstream_path = format
        .upstream_path()
        .ok_or_else(|| ApiError::bad_request("unsupported account test protocol"))?;
    let key = state
        .decrypt_key(&account.key_cipher)
        .map_err(ApiError::internal)?;
    let (config, client) = state.upstream_context();
    validate_upstream_url(&config.upstream_base_url)?;
    let response = client
        .post(format!(
            "{}{}",
            config.upstream_base_url.trim_end_matches('/'),
            upstream_path,
        ))
        .bearer_auth(&key)
        .header(header::CONTENT_TYPE, "application/json")
        .headers(
            if protocol == crate::provider::UpstreamProtocolKind::Messages {
                let mut headers = HeaderMap::new();
                headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
                headers
            } else {
                HeaderMap::new()
            },
        )
        .json(&account_test_payload(protocol, &model_id))
        .timeout(std::time::Duration::from_secs(
            config.non_stream_timeout_secs,
        ))
        .send()
        .await
        .map_err(ApiError::internal)?;
    let status = response.status();
    let body = response.text().await.map_err(|error| {
        if error.is_timeout() {
            ApiError::internal("upstream response body timed out")
        } else {
            ApiError::internal(error)
        }
    })?;
    if status == StatusCode::UNAUTHORIZED {
        let sanitized = sanitize_upstream_error_value_with_known_secret(&body, &key).to_string();
        let auth_error = format!("upstream auth error 401: {}", short_body(&sanitized));
        {
            let db = state.db.lock();
            db.set_account_auth_error_if_key_matches(
                &account.id,
                &account.key_cipher,
                Some(&auth_error),
            )
            .map_err(ApiError::internal)?;
            let _ = db.log_gateway(
                "warn",
                "account",
                &format!("ping authentication failed for account {}", account.name),
            );
        }
        // Dashboard HTTP 401 is reserved for the administrator session. Keep
        // this upstream account failure as a normal API validation error so the
        // frontend does not treat it as a logout signal.
        return Err(ApiError::bad_request(format!("Ping failed: {auth_error}")));
    }
    if status == StatusCode::TOO_MANY_REQUESTS {
        let cooldown = parse_reset(&body).unwrap_or_else(|| Duration::minutes(5));
        let until = Utc::now() + cooldown;
        let sanitized_body =
            sanitize_upstream_error_value_with_known_secret(&body, &key).to_string();
        {
            let db = state.db.lock();
            let credential_matches = db
                .set_account_auth_error_if_key_matches(&account.id, &account.key_cipher, None)
                .map_err(ApiError::internal)?;
            if credential_matches {
                db.set_account_rate_limit(
                    &account.id,
                    until,
                    &sanitized_body,
                    parse_usage_limit_window(&body),
                )
                .map_err(ApiError::internal)?;
            }
            let _ = db.log_gateway(
                "warn",
                "account",
                &format!("ping quota reached for account {}", account.name),
            );
        }
        return Err(ApiError::status(
            StatusCode::TOO_MANY_REQUESTS,
            format!(
                "Ping 到达额度或限流，已熔断到 {}",
                until.format("%Y-%m-%d %H:%M:%S UTC")
            ),
        ));
    }
    if !status.is_success() {
        let sanitized = sanitize_upstream_error_value_with_known_secret(&body, &key).to_string();
        return Err(ApiError::bad_request(format!(
            "Ping failed: upstream returned {}: {}",
            status,
            short_body(&sanitized)
        )));
    }
    state
        .db
        .lock()
        .set_account_auth_error_if_key_matches(&account.id, &account.key_cipher, None)
        .map_err(ApiError::internal)?;
    let masked = if key.len() > 8 && key.is_char_boundary(4) && key.is_char_boundary(key.len() - 4)
    {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    } else {
        "***".to_string()
    };
    Ok(Json(AccountTestResponse {
        message: format!("Ping OK: {} ({})", account.name, masked),
        model_id,
        protocol: api_format_name(format),
    }))
}

fn account_test_case(
    input: Option<AccountTestRequest>,
) -> Result<(String, crate::provider::UpstreamProtocolKind), ApiError> {
    let input = input.unwrap_or_else(|| AccountTestRequest {
        model_id: crate::models::DEFAULT_ACCOUNT_TEST_MODEL.to_string(),
        protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
    });
    let requested = input.model_id.trim();
    let Some((canonical, _, supported)) = supported_model_protocol_profiles()
        .find(|(model_id, _, _)| model_id.eq_ignore_ascii_case(requested))
    else {
        return Err(ApiError::bad_request("unknown OpenCode Go test model"));
    };
    let format = crate::custom::api_format_for_custom_protocol(input.protocol);
    if !supported.contains(&format) {
        return Err(ApiError::bad_request(format!(
            "model `{canonical}` does not support the selected upstream protocol"
        )));
    }
    Ok((canonical.to_string(), input.protocol))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyManagedKeyInput {
    key: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

async fn verify_managed_account_key(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(input): Json<VerifyManagedKeyInput>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let key = input.key.trim().to_string();
    if key.is_empty() {
        return Err(ApiError::bad_request("key is required"));
    }
    if key.len() > 4096 {
        return Err(ApiError::bad_request("key is too long"));
    }
    let key_cipher = state.encrypt_key(&key).map_err(ApiError::internal)?;
    let account = {
        let _settings_update = state.settings_update.lock();
        check_key_revision(&state, input.expected_revision)?;
        let db = state.db.lock();
        let account = db
            .get_account(&id)
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::not_found("account not found"))?;
        if account.account_type != AccountType::Managed
            || account.setup_step != AccountSetupStep::KeyVerification
        {
            return Err(ApiError::status(
                StatusCode::CONFLICT,
                "managed account is not waiting for key verification",
            ));
        }
        if !db
            .save_managed_key_for_verification(&id, &key_cipher)
            .map_err(ApiError::internal)?
        {
            return Err(ApiError::status(
                StatusCode::CONFLICT,
                "account setup changed; reload and try again",
            ));
        }
        account
    };

    let (config, client) = state.upstream_context();
    validate_upstream_url(&config.upstream_base_url)?;
    let response = client
        .post(format!(
            "{}/v1/chat/completions",
            config.upstream_base_url.trim_end_matches('/')
        ))
        .bearer_auth(&key)
        .json(&account_ping_payload())
        .timeout(std::time::Duration::from_secs(
            config.non_stream_timeout_secs,
        ))
        .send()
        .await
        .map_err(|error| {
            ApiError::status(
                StatusCode::BAD_GATEWAY,
                if error.is_timeout() {
                    "key verification timed out; the account remains pending".to_string()
                } else {
                    format!("key verification request failed; the account remains pending: {error}")
                },
            )
        })?;
    let status = response.status();
    let body = read_managed_key_verification_response(response)
        .await
        .map_err(|error| {
            ApiError::status(
                StatusCode::BAD_GATEWAY,
                if error.is_timeout() {
                    "key verification response timed out; the account remains pending".to_string()
                } else {
                    format!("failed to read key verification response: {error}")
                },
            )
        })?;

    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        let sanitized = sanitize_upstream_error_value_with_known_secret(&body, &key).to_string();
        let auth_error = format!(
            "upstream auth error {}: {}",
            status.as_u16(),
            short_body(&sanitized)
        );
        state
            .db
            .lock()
            .set_account_auth_error_if_key_matches(&id, &key_cipher, Some(&auth_error))
            .map_err(ApiError::internal)?;
        return Err(ApiError::bad_request(format!(
            "Key verification failed: {auth_error}"
        )));
    }

    if status.is_server_error() {
        return Err(ApiError::status(
            StatusCode::BAD_GATEWAY,
            format!(
                "key verification upstream returned {}; the account remains pending",
                status
            ),
        ));
    }

    if status != StatusCode::TOO_MANY_REQUESTS && !status.is_success() {
        let sanitized = sanitize_upstream_error_value_with_known_secret(&body, &key).to_string();
        return Err(ApiError::bad_request(format!(
            "Key verification failed: upstream returned {}: {}",
            status,
            short_body(&sanitized)
        )));
    }

    {
        let _settings_update = state.settings_update.lock();
        check_key_revision(&state, input.expected_revision)?;
        let db = state.db.lock();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let cooldown = parse_reset(&body).unwrap_or_else(|| Duration::minutes(5));
            let sanitized_body =
                sanitize_upstream_error_value_with_known_secret(&body, &key).to_string();
            if !db
                .set_account_rate_limit_if_key_matches(
                    &id,
                    &key_cipher,
                    Utc::now() + cooldown,
                    &sanitized_body,
                    parse_usage_limit_window(&body),
                )
                .map_err(ApiError::internal)?
            {
                return Err(ApiError::status(
                    StatusCode::CONFLICT,
                    "the key changed while it was being verified; retry verification",
                ));
            }
        }
        if !db
            .complete_managed_setup_if_key_matches(&id, &key_cipher)
            .map_err(map_account_write_error)?
        {
            return Err(ApiError::status(
                StatusCode::CONFLICT,
                "the key changed while it was being verified; retry verification",
            ));
        }
        if status != StatusCode::TOO_MANY_REQUESTS {
            db.clear_account_cooldown(&id).map_err(ApiError::internal)?;
        }
        let _ = db.log_gateway(
            "info",
            "account",
            &format!("verified managed account {}", account.name),
        );
        state.bump_settings_revision();
    }

    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    Ok(Json(dashboard_account(&state, account)))
}

async fn read_managed_key_verification_response(
    response: reqwest::Response,
) -> Result<String, reqwest::Error> {
    let read_limit = MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES.saturating_add(1);
    let capacity = response
        .content_length()
        .and_then(|length| usize::try_from(length).ok())
        .map_or(read_limit, |length| length.min(read_limit));
    let mut body = Vec::with_capacity(capacity);
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let remaining = read_limit.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        if body.len() == read_limit {
            break;
        }
    }

    let truncated = body.len() > MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES;
    body.truncate(MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES);
    let mut text = String::from_utf8_lossy(&body).into_owned();
    if truncated {
        text.push_str("\n<key verification response truncated>");
    }
    Ok(text)
}

fn account_test_payload(
    protocol: crate::provider::UpstreamProtocolKind,
    model_id: &str,
) -> serde_json::Value {
    match protocol {
        crate::provider::UpstreamProtocolKind::ChatCompletions => serde_json::json!({
            "model": model_id,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1,
            "stream": false
        }),
        crate::provider::UpstreamProtocolKind::Responses => serde_json::json!({
            "model": model_id,
            "input": "ping",
            "max_output_tokens": 1,
            "store": false,
            "stream": false
        }),
        crate::provider::UpstreamProtocolKind::Messages => serde_json::json!({
            "model": model_id,
            "messages": [{ "role": "user", "content": "ping" }],
            "max_tokens": 1,
            "stream": false
        }),
    }
}

fn account_ping_payload() -> serde_json::Value {
    account_test_payload(
        crate::provider::UpstreamProtocolKind::ChatCompletions,
        crate::models::DEFAULT_ACCOUNT_TEST_MODEL,
    )
}

fn short_body(body: &str) -> String {
    body.split_whitespace()
        .take(40)
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect()
}

async fn account_usage(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<UsageWindow>, ApiError> {
    let limits = account_usage_limits(&state, &id, false)?;
    state
        .db
        .lock()
        .account_usage_with_limits(&id, &limits)
        .map(Json)
        .map_err(ApiError::internal)
}

#[derive(Deserialize)]
struct AccountUsageUpdate {
    window: String,
    percent: f64,
    /// 距上游重置还剩多少分钟。None 表示从 now 起算满窗口时长（5h/7d）。
    /// 月窗口忽略此字段（固定到 purchase_expires_on）。
    #[serde(default)]
    resets_in_minutes: Option<i64>,
}

async fn refresh_account_usage_from_official_go(
    State(state): State<CoreState>,
    Path(id): Path<String>,
) -> Result<Json<crate::usage_sync::OfficialUsageRefreshSuccess>, ApiError> {
    if id == crate::provider::ZEN_FREE_ACCOUNT_ID {
        return Err(ApiError::bad_request(
            "Zen Free usage cannot be refreshed through the OpenCode Go API",
        ));
    }
    match crate::usage_sync::refresh_official_usage(
        &state,
        &id,
        crate::usage_sync::UsageSyncTrigger::Manual,
    )
    .await
    {
        Ok(success) => Ok(Json(success)),
        Err(error) => Err(map_official_usage_refresh_error(error)),
    }
}

fn map_official_usage_refresh_error(
    error: crate::usage_sync::OfficialUsageRefreshError,
) -> ApiError {
    use crate::usage_sync::OfficialUsageRefreshError;
    match error {
        OfficialUsageRefreshError::NotFound => ApiError::not_found(error.to_string()),
        OfficialUsageRefreshError::NotEligible(message) => ApiError::bad_request(message),
        OfficialUsageRefreshError::Conflict(message) => {
            ApiError::status(StatusCode::CONFLICT, message)
        }
        OfficialUsageRefreshError::Throttled {
            next_allowed_at,
            retry_after_secs,
        } => ApiError::throttled(error.to_string(), next_allowed_at, retry_after_secs),
        OfficialUsageRefreshError::Upstream(GoUsageError::Unauthorized)
        | OfficialUsageRefreshError::Upstream(GoUsageError::Forbidden) => {
            ApiError::bad_request("official Go usage rejected this account key")
        }
        OfficialUsageRefreshError::Upstream(upstream) => {
            ApiError::status(StatusCode::BAD_GATEWAY, upstream.to_string())
        }
        OfficialUsageRefreshError::Internal(message) => ApiError::internal(message),
    }
}

// Thin helpers retained for unit tests of eligibility/CAS without network.
#[cfg(test)]
fn load_ready_account_for_official_go_usage(
    db: &crate::db::Database,
    account_id: &str,
) -> Result<String, ApiError> {
    let account = db
        .get_account(account_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    if !account.setup_step.is_ready() || account.key_cipher.is_empty() {
        return Err(ApiError::bad_request(
            "only ready accounts with a stored key can refresh official Go usage",
        ));
    }
    Ok(account.key_cipher)
}

#[cfg(test)]
fn apply_official_go_usage_snapshot(
    db: &crate::db::Database,
    account_id: &str,
    expected_key_cipher: &str,
    snapshot: &crate::go_usage::GoUsageSnapshot,
    limits: &crate::kernel::pricing::PricingLimits,
) -> Result<UsageWindow, ApiError> {
    let account = db
        .get_account(account_id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    if !account.setup_step.is_ready()
        || account.key_cipher.is_empty()
        || account.key_cipher != expected_key_cipher
    {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "account key or setup changed while refreshing official Go usage",
        ));
    }
    db.calibrate_account_usage_snapshot(
        account_id,
        &crate::db::AccountUsageCalibrationSnapshot {
            rolling_percent: snapshot.rolling_percent,
            weekly_percent: snapshot.weekly_percent,
            monthly_percent: snapshot.monthly_percent,
            rolling_resets_in_minutes: snapshot.rolling_resets_in_minutes,
            weekly_resets_in_minutes: snapshot.weekly_resets_in_minutes,
        },
        limits,
    )
    .map_err(ApiError::internal)
}

async fn update_account_usage(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(update): Json<AccountUsageUpdate>,
) -> Result<Json<UsageWindow>, ApiError> {
    let limits = account_usage_limits(&state, &id, true)?;
    let window = match update.window.as_str() {
        "window_5h" => UsageWindowKind::FiveHours,
        "window_week" => UsageWindowKind::Week,
        "window_month" => UsageWindowKind::Month,
        _ => return Err(ApiError::bad_request("invalid usage window")),
    };
    if !update.percent.is_finite() || !(0.0..=100.0).contains(&update.percent) {
        return Err(ApiError::bad_request(
            "usage percent must be between 0 and 100",
        ));
    }
    let percent = (update.percent * 10.0).round() / 10.0;
    if let Some(mins) = update.resets_in_minutes {
        let max = match window {
            UsageWindowKind::FiveHours => Some(5 * 60),
            UsageWindowKind::Week => Some(7 * 24 * 60),
            UsageWindowKind::Month | UsageWindowKind::Free => None,
        };
        if mins < 0 || max.is_some_and(|max| mins > max) {
            return Err(ApiError::bad_request(match max {
                Some(max) => format!("resets_in_minutes must be between 0 and {max}"),
                None => "resets_in_minutes must be >= 0".to_string(),
            }));
        }
    }

    let limit = match window {
        UsageWindowKind::FiveHours => limits.window_5h,
        UsageWindowKind::Week => limits.window_week,
        UsageWindowKind::Month => limits.window_month,
        UsageWindowKind::Free => {
            return Err(ApiError::bad_request(
                "free promo quota cannot be calibrated as a Go usage window",
            ));
        }
    };
    let db = state.db.lock();
    if !db
        .calibrate_account_usage(&id, window, percent, update.resets_in_minutes, limit)
        .map_err(ApiError::internal)?
    {
        return Err(ApiError::not_found("account not found"));
    }
    db.account_usage_with_limits(&id, &limits)
        .map(Json)
        .map_err(ApiError::internal)
}

fn account_usage_limits(
    state: &CoreState,
    id: &str,
    _require_manual_calibration: bool,
) -> Result<crate::kernel::pricing::PricingLimits, ApiError> {
    let account = state
        .db
        .lock()
        .get_account(id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    let adapter = crate::provider::ProviderAdapterKind::from_offering(
        &account.provider_id,
        &account.offering_id,
    );
    if adapter == Some(crate::provider::ProviderAdapterKind::OpenCodeGo) {
        return Ok(state.pricing_snapshot().limits.clone());
    }
    let plan = crate::provider::builtin_plan(&account.provider_id, &account.offering_id)
        .ok_or_else(|| ApiError::bad_request("account usage capability is unknown"))?;
    if !plan.manual_usage_calibration {
        return Err(ApiError::bad_request(
            "manual usage calibration is unavailable for this account",
        ));
    }
    Ok(crate::kernel::pricing::PricingLimits {
        window_5h: crate::provider::COMMAND_CODE_GOAT_QUOTA_5H,
        window_week: crate::provider::COMMAND_CODE_GOAT_QUOTA_WEEK,
        window_month: crate::provider::COMMAND_CODE_GOAT_QUOTA_MONTH,
    })
}

async fn reset_account_cooldown(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    expectation: Option<Json<AccountRevisionExpectation>>,
) -> Result<Json<DashboardAccount>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(
        &state,
        expectation.and_then(|Json(body)| body.expected_revision),
    )?;
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    if account.is_zen_free() {
        return Err(ApiError::bad_request(
            "Zen Free uses an egress-wide cooldown that cannot be cleared from an account",
        ));
    }
    {
        let db = state.db.lock();
        db.clear_account_cooldown(&id).map_err(ApiError::internal)?;
    }
    state.bump_settings_revision();
    let account = state
        .db
        .lock()
        .get_account(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("account not found"))?;
    Ok(Json(dashboard_account(&state, account)))
}

/// One known model entry backing the list-mode checkbox grid. `zen_free`
/// follows the registered Zen promo allowlist (not a `-free` suffix — Go
/// catalog ids may contain `free` without being on the free channel).
#[derive(Serialize)]
struct ProxySupportedModel {
    id: String,
    preferred_protocol: &'static str,
    zen_free: bool,
}

#[derive(Serialize)]
struct SettingsResponse {
    #[serde(flatten)]
    config: AppConfig,
    revision: u64,
    auto_start_supported: bool,
    dock_visibility_supported: bool,
    client_root_url_from_env: bool,
    proxy_supported_models: Vec<ProxySupportedModel>,
}

async fn get_settings(State(state): State<CoreState>) -> Json<SettingsResponse> {
    let _settings_update = state.settings_update.lock();
    let auto_start_supported = state.auto_start_supported();
    let config = state.settings_config();
    Json(SettingsResponse {
        config,
        revision: state.settings_revision(),
        auto_start_supported,
        dock_visibility_supported: state.dock_visibility_supported(),
        client_root_url_from_env: state.client_root_url_from_env(),
        proxy_supported_models: proxy_supported_models(&state),
    })
}

fn proxy_supported_models(state: &CoreState) -> Vec<ProxySupportedModel> {
    let zen_catalog = state.zen_free_model_catalog();
    let zen_ids = zen_catalog
        .models
        .iter()
        .cloned()
        .collect::<std::collections::HashSet<_>>();
    let mut models = supported_model_protocols()
        .filter_map(|(id, preferred)| {
            let legacy_zen = id == "big-pickle" || crate::gateway::free_models::is_free_model(id);
            (!legacy_zen || zen_ids.contains(id)).then(|| ProxySupportedModel {
                id: id.to_string(),
                preferred_protocol: api_format_name(preferred),
                zen_free: legacy_zen,
            })
        })
        .collect::<Vec<_>>();
    let mut known = models
        .iter()
        .map(|model| model.id.clone())
        .collect::<std::collections::HashSet<_>>();
    for id in &zen_catalog.models {
        if known.insert(id.clone()) {
            models.push(ProxySupportedModel {
                id: id.clone(),
                preferred_protocol: api_format_name(ApiFormat::ChatCompletions),
                zen_free: true,
            });
        }
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models
}

#[derive(Deserialize)]
struct ProxyTestRequest {
    proxy_mode: ProxyMode,
    #[serde(default)]
    proxy_url: String,
    /// Optional list-mode direction override; omitted means "use the
    /// direction currently persisted in config". Only affects which leg the
    /// test builds — URL validation treats list mode like manual mode.
    #[serde(default)]
    proxy_list_direction: Option<ProxyListDirection>,
    upstream_base_url: String,
}

#[derive(Debug, Serialize)]
struct ProxyTestResponse {
    proxy_mode: ProxyMode,
    status: u16,
    latency_ms: u64,
}

async fn test_proxy(
    State(state): State<CoreState>,
    Json(input): Json<ProxyTestRequest>,
) -> Result<Json<ProxyTestResponse>, ApiError> {
    let mut config = state.config();
    config.proxy_mode = input.proxy_mode;
    if let Some(direction) = input.proxy_list_direction {
        config.proxy_list_direction = direction;
    }
    config.proxy_url =
        normalize_proxy_url(config.proxy_mode, &input.proxy_url).map_err(ApiError::bad_request)?;
    config.upstream_base_url = input
        .upstream_base_url
        .trim()
        .trim_end_matches('/')
        .to_string();
    validate_upstream_url(&config.upstream_base_url)?;

    let client = crate::http_client::configured_builder(&config)
        .map_err(ApiError::internal)?
        .connect_timeout(std::time::Duration::from_secs(config.connect_timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(ApiError::internal)?;
    let started = std::time::Instant::now();
    let response = client
        .get(&config.upstream_base_url)
        .timeout(std::time::Duration::from_secs(
            config.connect_timeout_secs.min(30),
        ))
        .send()
        .await
        .map_err(|error| proxy_test_error(&error))?;
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(Json(ProxyTestResponse {
        proxy_mode: config.proxy_mode,
        status: response.status().as_u16(),
        latency_ms,
    }))
}

fn proxy_test_error(error: &reqwest::Error) -> ApiError {
    let category = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "connection failed"
    } else {
        "request failed"
    };
    ApiError::status(
        StatusCode::BAD_GATEWAY,
        format!(
            "outbound connection test {category}: {}",
            format_error_chain(error)
        ),
    )
}

async fn get_claude_desktop_models(State(state): State<CoreState>) -> Json<ClaudeDesktopModels> {
    Json(state.config().claude_desktop_models.resolved())
}

async fn update_claude_desktop_models(
    State(state): State<CoreState>,
    Json(mut models): Json<ClaudeDesktopModels>,
) -> Result<Json<ClaudeDesktopModels>, ApiError> {
    let _settings_update = state.settings_update.lock();
    models.normalize();
    models.validate().map_err(ApiError::bad_request)?;
    let response = models.resolved();
    let mut config = state.config();
    config.claude_desktop_models = models;
    state.set_config(config).map_err(ApiError::internal)?;
    Ok(Json(response))
}

const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/klarkxy/opencode-go-mgr/releases/latest";
const GITHUB_LATEST_RELEASE_URL: &str =
    "https://github.com/klarkxy/opencode-go-mgr/releases/latest";
const UPDATE_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
}

#[derive(Serialize)]
struct UpdateCheckResponse {
    current_version: String,
    latest_version: String,
    update_available: bool,
    release_url: &'static str,
    install_supported: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallUpdateRequest {
    expected_version: String,
}

async fn check_update(
    State(state): State<CoreState>,
) -> Result<Json<UpdateCheckResponse>, ApiError> {
    let (_, client) = state.upstream_context();
    let release = client
        .get(GITHUB_LATEST_RELEASE_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header(
            reqwest::header::USER_AGENT,
            concat!("ocg-manager/", env!("CARGO_PKG_VERSION")),
        )
        .timeout(UPDATE_CHECK_TIMEOUT)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(update_check_error)?
        .json::<GithubRelease>()
        .await
        .map_err(update_check_error)?;

    let current_version = env!("CARGO_PKG_VERSION");
    let (current_version_parts, current_version) = parse_semver_version(current_version)
        .ok_or_else(|| ApiError::internal("application version is not valid SemVer"))?;
    let (latest_version_parts, latest_version) = parse_semver_version(&release.tag_name)
        .ok_or_else(|| {
            ApiError::status(
                StatusCode::BAD_GATEWAY,
                "GitHub latest release has an invalid SemVer tag",
            )
        })?;

    Ok(Json(UpdateCheckResponse {
        current_version: current_version.to_string(),
        latest_version: latest_version.to_string(),
        update_available: is_update_available(&current_version_parts, &latest_version_parts),
        release_url: GITHUB_LATEST_RELEASE_URL,
        install_supported: state.desktop_update_supported(),
    }))
}

async fn get_update_status(State(state): State<CoreState>) -> Json<DesktopUpdateStatus> {
    Json(state.desktop_update_status())
}

async fn install_update(
    State(state): State<CoreState>,
    Json(input): Json<InstallUpdateRequest>,
) -> Result<(StatusCode, Json<DesktopUpdateStatus>), ApiError> {
    let status = state.desktop_update_status();
    let (current_version_parts, _) = parse_semver_version(&status.current_version)
        .ok_or_else(|| ApiError::internal("application version is not valid SemVer"))?;
    let (expected_version_parts, expected_version) = parse_semver_version(&input.expected_version)
        .ok_or_else(|| ApiError::bad_request("expected_version must be a valid SemVer version"))?;
    if !is_update_available(&current_version_parts, &expected_version_parts) {
        return Err(ApiError::bad_request(
            "expected_version must be newer than the current version",
        ));
    }

    match state.start_desktop_update(expected_version.to_string()) {
        Ok(()) => Ok((StatusCode::ACCEPTED, Json(state.desktop_update_status()))),
        Err(DesktopUpdateStartError::Unsupported) => Err(ApiError::bad_request(
            "desktop update installation is unavailable in this runtime",
        )),
        Err(DesktopUpdateStartError::Busy) => Err(ApiError::status(
            StatusCode::CONFLICT,
            "a desktop update is already in progress",
        )),
        Err(DesktopUpdateStartError::Starter(error)) => Err(ApiError::internal(error)),
    }
}

fn update_check_error(error: reqwest::Error) -> ApiError {
    let category = if error.is_timeout() {
        format!(
            "request timed out after {} seconds",
            UPDATE_CHECK_TIMEOUT.as_secs()
        )
    } else if error.is_connect() {
        "connection failed".to_string()
    } else if let Some(status) = error.status() {
        format!("GitHub returned HTTP {status}")
    } else if error.is_decode() {
        "GitHub returned an invalid response".to_string()
    } else {
        "request failed".to_string()
    };
    ApiError::status(
        StatusCode::BAD_GATEWAY,
        format!(
            "failed to check GitHub releases ({category}): {}",
            format_error_chain(&error)
        ),
    )
}

fn format_error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[derive(Debug)]
struct SemverVersion<'a> {
    core: [u64; 3],
    prerelease: Option<Vec<PrereleaseIdentifier<'a>>>,
}

#[derive(Debug)]
struct PrereleaseIdentifier<'a> {
    value: &'a str,
    numeric: Option<u64>,
}

fn parse_semver_version(version: &str) -> Option<(SemverVersion<'_>, &str)> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let display_version = version;
    let (version, build) = match version.split_once('+') {
        Some((version, build)) => (version, Some(build)),
        None => (version, None),
    };
    build
        .is_none_or(|build| build.split('.').all(is_semver_identifier))
        .then_some(())?;

    let (core, prerelease) = match version.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (version, None),
    };
    let mut core_parts = core.split('.');
    let core = [
        parse_semver_number(core_parts.next()?)?,
        parse_semver_number(core_parts.next()?)?,
        parse_semver_number(core_parts.next()?)?,
    ];
    core_parts.next().is_none().then_some(())?;

    let prerelease = match prerelease {
        Some(prerelease) => Some(
            prerelease
                .split('.')
                .map(parse_prerelease_identifier)
                .collect::<Option<Vec<_>>>()?,
        ),
        None => None,
    };
    Some((SemverVersion { core, prerelease }, display_version))
}

fn is_semver_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn parse_prerelease_identifier(value: &str) -> Option<PrereleaseIdentifier<'_>> {
    is_semver_identifier(value).then_some(())?;
    let numeric = if value.bytes().all(|byte| byte.is_ascii_digit()) {
        Some(parse_semver_number(value)?)
    } else {
        None
    };
    Some(PrereleaseIdentifier { value, numeric })
}

fn parse_semver_number(value: &str) -> Option<u64> {
    (!value.is_empty()
        && (value == "0" || !value.starts_with('0'))
        && value.bytes().all(|byte| byte.is_ascii_digit()))
    .then(|| value.parse().ok())
    .flatten()
}

fn is_update_available(current: &SemverVersion<'_>, latest: &SemverVersion<'_>) -> bool {
    use std::cmp::Ordering;

    let core_ordering = latest.core.cmp(&current.core);
    if core_ordering != Ordering::Equal {
        return core_ordering == Ordering::Greater;
    }
    match (&current.prerelease, &latest.prerelease) {
        (None, None) => false,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (Some(current), Some(latest)) => {
            latest
                .iter()
                .zip(current)
                .map(
                    |(latest, current)| match (latest.numeric, current.numeric) {
                        (Some(latest), Some(current)) => latest.cmp(&current),
                        (Some(_), None) => Ordering::Less,
                        (None, Some(_)) => Ordering::Greater,
                        (None, None) => latest.value.cmp(current.value),
                    },
                )
                .find(|ordering| *ordering != Ordering::Equal)
                .unwrap_or_else(|| latest.len().cmp(&current.len()))
                == Ordering::Greater
        }
    }
}

#[derive(Deserialize)]
struct SettingsUpdateRequest {
    #[serde(flatten)]
    config: AppConfig,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SettingsRevisionResponse {
    revision: u64,
}

async fn update_settings(
    State(state): State<CoreState>,
    Json(input): Json<SettingsUpdateRequest>,
) -> Result<Json<SettingsRevisionResponse>, ApiError> {
    let _settings_update = state.settings_update.lock();
    if input
        .expected_revision
        .is_some_and(|revision| revision != state.settings_revision())
    {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "settings changed since they were loaded; reload and try again",
        ));
    }
    let mut config = input.config;
    config.gateway_key = config.gateway_key.trim().to_string();
    if config.gateway_key.is_empty() {
        // Minimal payload sanity guard: an empty/blank gateway_key means a
        // truncated or hand-written settings body, which must not silently
        // reset every other field to its default.
        return Err(ApiError::bad_request(PRIMARY_KEY_REQUIRED_MESSAGE));
    }
    // Unified cross-tier gate (shared with the Tauri update path and the sub
    // key enable path): the primary value must differ from every non-deleted
    // sub key's value, enabled or disabled.
    {
        let db = state.db.lock();
        crate::gateway_keys::ensure_primary_value_allowed(&db, &config.gateway_key)
            .map_err(key_api_error)?;
    }
    let previous_config = state.config();
    config.claude_desktop_models = previous_config.claude_desktop_models.clone();
    config.validate().map_err(ApiError::bad_request)?;
    validate_proxy_list(&state, &mut config).map_err(ApiError::bad_request)?;
    validate_upstream_url(&config.upstream_base_url)?;
    config.client_root_url =
        normalize_client_root_url(&config.client_root_url).map_err(ApiError::bad_request)?;
    let next_auto_start = config.auto_start;
    let next_show_dock_icon = config.show_dock_icon;
    let auto_start_supported = state.auto_start_supported();
    let dock_visibility_supported = state.dock_visibility_supported();
    if !auto_start_supported && next_auto_start != previous_config.auto_start {
        return Err(ApiError::bad_request(
            "auto-start is unavailable in this runtime",
        ));
    }
    if !dock_visibility_supported && next_show_dock_icon != previous_config.show_dock_icon {
        return Err(ApiError::bad_request(
            "Dock visibility is unavailable in this runtime",
        ));
    }
    state.set_config(config).map_err(ApiError::internal)?;
    let runtime_sync = (|| -> anyhow::Result<()> {
        if auto_start_supported {
            state.sync_auto_start(next_auto_start)?;
        }
        if dock_visibility_supported {
            state.sync_dock_visibility(next_show_dock_icon)?;
        }
        Ok(())
    })();
    if let Err(sync_error) = runtime_sync {
        let config_rollback_error = state.set_config(previous_config.clone()).err();
        let auto_start_rollback_error = auto_start_supported
            .then(|| state.sync_auto_start(previous_config.auto_start).err())
            .flatten();
        let dock_rollback_error = dock_visibility_supported
            .then(|| {
                state
                    .sync_dock_visibility(previous_config.show_dock_icon)
                    .err()
            })
            .flatten();
        let mut message = format!("failed to synchronize desktop settings: {sync_error}");
        if let Some(error) = config_rollback_error {
            message.push_str(&format!("; failed to restore settings: {error}"));
        }
        if let Some(error) = auto_start_rollback_error {
            message.push_str(&format!("; failed to restore auto-start state: {error}"));
        }
        if let Some(error) = dock_rollback_error {
            message.push_str(&format!("; failed to restore Dock visibility: {error}"));
        }
        return Err(ApiError::internal(message));
    }
    Ok(Json(SettingsRevisionResponse {
        revision: state.settings_revision(),
    }))
}

/// Write-gate validation for list proxy mode. Only the dashboard save path
/// enforces list contents (non-empty, exact known registry ids, deduped);
/// `AppConfig::validate` deliberately stays registry-free so future registry
/// shrinks never brick the load path of persisted configs.
fn validate_proxy_list(state: &CoreState, config: &mut AppConfig) -> Result<(), String> {
    if config.proxy_mode != ProxyMode::List {
        return Ok(());
    }
    if config.proxy_list_models.is_empty() {
        return Err("list proxy mode requires at least one model".to_string());
    }
    let known = proxy_supported_models(state)
        .into_iter()
        .map(|model| model.id)
        .collect::<std::collections::HashSet<_>>();
    let mut deduped: Vec<String> = Vec::new();
    for model in config.proxy_list_models.iter() {
        let model = model.trim();
        if !known.contains(model) {
            return Err(format!("unknown model in proxy list: `{model}`"));
        }
        if !deduped.iter().any(|existing| existing == model) {
            deduped.push(model.to_string());
        }
    }
    config.proxy_list_models = deduped;
    Ok(())
}

async fn update_settings_route(
    State(state): State<CoreState>,
    input: Result<Json<SettingsUpdateRequest>, JsonRejection>,
) -> Result<Json<SettingsRevisionResponse>, ApiError> {
    let input = input.map_err(|error| ApiError::bad_request(error.body_text()))?;
    update_settings(State(state), input).await
}

/// Legacy entry point kept for older dashboards; converges on rotating the
/// primary key so there is exactly one implementation.
async fn regenerate_gateway_key(
    State(state): State<CoreState>,
    expectation: Option<Json<GatewayKeyRevisionExpectation>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(
        &state,
        expectation.and_then(|Json(body)| body.expected_revision),
    )?;
    let new_value = {
        let db = state.db.lock();
        crate::gateway_keys::generate_primary_value(&db, &state.config().gateway_key)
            .map_err(key_api_error)?
    };
    let mut config = state.config();
    config.gateway_key = new_value;
    state.set_config(config).map_err(ApiError::internal)?;
    audit_key_event(
        &state,
        &format!(
            "regenerated primary key `{}`",
            crate::gateway_keys::PRIMARY_KEY_NAME
        ),
    );
    Ok(Json(serde_json::json!({
        "key": state.config().gateway_key,
        "revision": state.settings_revision(),
    })))
}

#[derive(Deserialize)]
struct GatewayKeyCreateRequest {
    name: String,
    #[serde(default)]
    expected_revision: Option<u64>,
}

/// Optional body shared by the key endpoints without dedicated payloads;
/// absent body means the client skips the settings-revision check.
#[derive(Deserialize)]
struct GatewayKeyRevisionExpectation {
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Serialize)]
struct GatewayKeyEntryResponse {
    #[serde(flatten)]
    key: SubGatewayKey,
    revision: u64,
}

#[derive(Deserialize)]
struct GatewayKeyUpdateRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    expected_revision: Option<u64>,
}

#[derive(Serialize)]
struct GatewayKeyRevisionResponse {
    revision: u64,
    keys: Vec<SubKeySummary>,
}

/// Sub key summary for list-shaped lifecycle responses. Plaintext never
/// rides along here: create/regenerate responses return the full value
/// exactly once, and `/connection` is the persistent session-guarded
/// exposure that feeds copy actions and masked previews.
#[derive(Serialize)]
struct SubKeySummary {
    id: String,
    name: String,
    enabled: bool,
}

fn check_key_revision(state: &CoreState, expected_revision: Option<u64>) -> Result<(), ApiError> {
    if expected_revision.is_some_and(|revision| revision != state.settings_revision()) {
        return Err(ApiError::status(
            StatusCode::CONFLICT,
            "settings changed since they were loaded; reload and try again",
        ));
    }
    Ok(())
}

fn audit_key_event(state: &CoreState, message: &str) {
    // The key mutation has already committed; a failed audit row must not
    // report the whole operation as failed. Surface it locally instead.
    if let Err(error) = state.db.lock().log_gateway("info", "keys", message) {
        eprintln!("warning: failed to audit key event: {error}");
    }
}

/// Maps the lifecycle facade's error tier: user-correctable rejections stay
/// 400, internal failures (including snapshot rollbacks that could not be
/// restored) surface 500 so clients do not retry blindly.
fn key_api_error(error: crate::gateway_keys::KeyError) -> ApiError {
    match error {
        crate::gateway_keys::KeyError::BadRequest(message) => ApiError::bad_request(message),
        crate::gateway_keys::KeyError::Internal(message) => {
            ApiError::status(StatusCode::INTERNAL_SERVER_ERROR, message)
        }
    }
}

/// Non-deleted sub key summaries for lifecycle responses; see
/// [`SubKeySummary`] for why plaintext is omitted.
fn sub_key_list(state: &CoreState) -> Result<Vec<SubKeySummary>, ApiError> {
    Ok(state
        .db
        .lock()
        .list_active_sub_gateway_keys()
        .map_err(ApiError::internal)?
        .into_iter()
        .map(|key| SubKeySummary {
            id: key.id,
            name: key.name,
            enabled: key.enabled,
        })
        .collect())
}

async fn create_gateway_key(
    State(state): State<CoreState>,
    Json(input): Json<GatewayKeyCreateRequest>,
) -> Result<Json<GatewayKeyEntryResponse>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    let key = crate::gateway_keys::create_sub_key(&state, &input.name).map_err(key_api_error)?;
    audit_key_event(&state, &format!("created key `{}`", key.name));
    let revision = state.bump_settings_revision();
    Ok(Json(GatewayKeyEntryResponse { key, revision }))
}

async fn update_gateway_key(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    Json(input): Json<GatewayKeyUpdateRequest>,
) -> Result<Json<GatewayKeyRevisionResponse>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(&state, input.expected_revision)?;
    // Unknown (or soft-deleted, or primary) ids are rejected up front, even
    // for empty-body patches.
    if !state
        .db
        .lock()
        .get_sub_gateway_key(&id)
        .map_err(ApiError::internal)?
        .is_some_and(|key| key.is_active())
    {
        return Err(ApiError::bad_request("key not found"));
    }
    let mut reset_routing = false;
    let mut mutated = false;
    if let Some(name) = input.name.as_deref() {
        // No-op renames (same trimmed name) neither audit nor bump, matching
        // the no-op-toggle handling below.
        let current_name = state
            .db
            .lock()
            .get_sub_gateway_key(&id)
            .map_err(ApiError::internal)?
            .map(|key| key.name)
            .unwrap_or_else(|| id.clone());
        if current_name != name.trim() {
            crate::gateway_keys::rename_sub_key(&state, &id, name).map_err(key_api_error)?;
            audit_key_event(
                &state,
                &format!("renamed key `{current_name}` to `{}`", name.trim()),
            );
            mutated = true;
        }
    }
    if let Some(enabled) = input.enabled {
        let current = state
            .db
            .lock()
            .get_sub_gateway_key(&id)
            .map_err(ApiError::internal)?
            .filter(|key| key.is_active())
            .ok_or_else(|| ApiError::bad_request("key not found"))?;
        // No-op toggles (already in the target state) neither audit nor
        // reset routing: nothing about the authenticating credential set
        // changed.
        if current.enabled != enabled {
            // The endpoint drives the explicit routing reset for revocations:
            // disabling a sub key invalidates credentials its sticky
            // sessions were pinned to. Renames, creates, and enables never
            // reset.
            if let Err(error) = crate::gateway_keys::set_sub_key_enabled(&state, &id, enabled) {
                // A committed rename in the same request already changed
                // state: bump before failing so the revision never lags a
                // committed mutation.
                if mutated {
                    state.bump_settings_revision();
                }
                return Err(key_api_error(error));
            }
            let display_name = state
                .db
                .lock()
                .get_sub_gateway_key(&id)
                .map_err(ApiError::internal)?
                .map(|key| key.name)
                .unwrap_or_else(|| id.clone());
            audit_key_event(
                &state,
                &format!(
                    "{} key `{display_name}`",
                    if enabled { "enabled" } else { "disabled" }
                ),
            );
            reset_routing = !enabled;
            mutated = true;
        }
    }
    if reset_routing {
        state.routing.reset();
    }
    let revision = mutated.then(|| state.bump_settings_revision());
    Ok(Json(GatewayKeyRevisionResponse {
        revision: revision.unwrap_or_else(|| state.settings_revision()),
        keys: sub_key_list(&state)?,
    }))
}

async fn delete_gateway_key(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    expectation: Option<Json<GatewayKeyRevisionExpectation>>,
) -> Result<Json<GatewayKeyRevisionResponse>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(
        &state,
        expectation.and_then(|Json(body)| body.expected_revision),
    )?;
    let existing = state
        .db
        .lock()
        .get_sub_gateway_key(&id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::bad_request("key not found"))?;
    let (name, was_authenticating) = (existing.name.clone(), existing.authenticates());
    crate::gateway_keys::delete_sub_key(&state, &id, Utc::now()).map_err(key_api_error)?;
    audit_key_event(&state, &format!("deleted key `{name}`"));
    // A disabled key's value never authenticated; its removal changes no
    // live sticky sessions.
    if was_authenticating {
        state.routing.reset();
    }
    let revision = state.bump_settings_revision();
    Ok(Json(GatewayKeyRevisionResponse {
        revision,
        keys: sub_key_list(&state)?,
    }))
}

async fn regenerate_gateway_key_entry(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    expectation: Option<Json<GatewayKeyRevisionExpectation>>,
) -> Result<Json<GatewayKeyEntryResponse>, ApiError> {
    let _settings_update = state.settings_update.lock();
    check_key_revision(
        &state,
        expectation.and_then(|Json(body)| body.expected_revision),
    )?;
    let updated = crate::gateway_keys::regenerate_sub_key(&state, &id).map_err(key_api_error)?;
    audit_key_event(&state, &format!("regenerated key `{}`", updated.name));
    // Only an authenticating key's rotation invalidates live sessions; a
    // disabled key's fresh value never entered the snapshot.
    if updated.authenticates() {
        state.routing.reset();
    }
    let revision = state.bump_settings_revision();
    Ok(Json(GatewayKeyEntryResponse {
        key: updated,
        revision,
    }))
}

/// Lightweight connection view for the dashboard connection center: the
/// primary key value, non-deleted sub keys with values, the settings
/// revision, and the fields the client needs to derive URLs. Served behind
/// the same dashboard session layer as `/settings`.
async fn connection_info(State(state): State<CoreState>) -> Result<Json<ConnectionInfo>, ApiError> {
    let settings = state.settings_config();
    let sub_keys = state
        .db
        .lock()
        .list_active_sub_gateway_keys()
        .map_err(ApiError::internal)?
        .into_iter()
        .map(|key| ConnectionSubKey {
            id: key.id,
            name: key.name,
            enabled: key.enabled,
            value: key.key,
        })
        .collect();
    Ok(Json(ConnectionInfo {
        gateway_port: settings.gateway_port,
        client_root_url: settings.client_root_url,
        upstream_base_url: settings.upstream_base_url,
        primary_key: settings.gateway_key,
        sub_keys,
        revision: state.settings_revision(),
    }))
}

async fn gateway_status(State(state): State<CoreState>) -> Json<GatewayStatus> {
    Json(status_from_state(&state))
}

/// Local Applications picker: currently routeable OpenCode Go aliases
/// intersected with the active Go pricing table. Highspeed variants inherit
/// the base row. Empty intersection is `[]`, not an error. Never selects an
/// account, calls upstream, writes logs, or advances routing state.
#[cfg(test)]
fn local_application_models(snapshot: &crate::kernel::pricing::PricingSnapshot) -> Vec<String> {
    local_application_models_with_contracts(snapshot, None)
}

fn local_application_models_with_contracts(
    snapshot: &crate::kernel::pricing::PricingSnapshot,
    contracts: Option<&crate::provider_contracts::EffectiveContractSet>,
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

fn go_alias_has_enabled_protocol(
    alias: &str,
    contracts: &crate::provider_contracts::EffectiveContractSet,
) -> bool {
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

async fn application_models(State(state): State<CoreState>) -> Json<Vec<String>> {
    Json(local_application_models_with_contracts(
        &state.pricing_snapshot(),
        Some(&state.provider_contracts()),
    ))
}

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<i64>,
    days: Option<i64>,
    request_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct ForwardLogQuery {
    limit: Option<i64>,
    offset: Option<i64>,
    status: Option<String>,
    account_id: Option<String>,
    provider_id: Option<String>,
    offering_id: Option<String>,
    route_account_id: Option<String>,
    credential_account_id: Option<String>,
    model: Option<String>,
    request_id: Option<String>,
    key_id: Option<String>,
    start_time: Option<String>,
    end_time: Option<String>,
    sort_by: Option<String>,
    sort_order: Option<String>,
}

fn validate_forward_log_query(
    query: &ForwardLogQuery,
) -> Result<(Option<String>, Option<String>), ApiError> {
    if query.sort_by.as_deref().is_some_and(|value| {
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
        return Err(ApiError::bad_request("invalid sort_by"));
    }
    if query
        .sort_order
        .as_deref()
        .is_some_and(|value| !matches!(value, "asc" | "desc"))
    {
        return Err(ApiError::bad_request("invalid sort_order"));
    }

    let parse_time = |value: Option<&str>, name: &str| -> Result<_, ApiError> {
        value
            .map(|value| {
                DateTime::parse_from_rfc3339(value)
                    .map(|time| {
                        time.with_timezone(&Utc)
                            .to_rfc3339_opts(SecondsFormat::Millis, true)
                    })
                    .map_err(|_| ApiError::bad_request(format!("invalid {name}")))
            })
            .transpose()
    };
    let start_time = parse_time(query.start_time.as_deref(), "start_time")?;
    let end_time = parse_time(query.end_time.as_deref(), "end_time")?;
    if start_time
        .as_ref()
        .zip(end_time.as_ref())
        .is_some_and(|(start, end)| start > end)
    {
        return Err(ApiError::bad_request(
            "start_time must not be after end_time",
        ));
    }
    Ok((start_time, end_time))
}

async fn gateway_logs(
    State(state): State<CoreState>,
    Query(q): Query<LimitQuery>,
) -> Result<Json<Vec<GatewayLog>>, ApiError> {
    let mut logs = state
        .db
        .lock()
        .query_gateway_logs(q.limit.unwrap_or(100), q.request_id.as_deref())
        .map_err(ApiError::internal)?;
    let secrets = dashboard_account_secrets(&state)?;
    for log in &mut logs {
        log.message = redact_known_secrets(&log.message, &secrets);
        log.diagnostic = redact_diagnostic(log.diagnostic.take(), secrets.values());
    }
    Ok(Json(logs))
}

#[derive(Serialize)]
struct DashboardForwardLog {
    #[serde(flatten)]
    log: ForwardLog,
    requested_model: Option<String>,
    resolved_alias: Option<String>,
    upstream_model: Option<String>,
    native_cost_value: Option<f64>,
    native_cost_unit: Option<String>,
    native_cost_currency: Option<String>,
}

#[derive(Serialize)]
struct DashboardForwardLogPage {
    items: Vec<DashboardForwardLog>,
    summary: ForwardLogSummary,
}

async fn forward_logs(
    State(state): State<CoreState>,
    Query(q): Query<ForwardLogQuery>,
) -> Result<Json<DashboardForwardLogPage>, ApiError> {
    let (start_time, end_time) = validate_forward_log_query(&q)?;
    let mut page = state
        .db
        .lock()
        .query_forward_logs(ForwardLogQueryOptions {
            limit: q.limit.unwrap_or(100),
            offset: q.offset.unwrap_or(0),
            status: q.status.as_deref(),
            account_id: q.account_id.as_deref(),
            provider_id: q.provider_id.as_deref().filter(|value| !value.is_empty()),
            offering_id: q.offering_id.as_deref().filter(|value| !value.is_empty()),
            route_account_id: q
                .route_account_id
                .as_deref()
                .filter(|value| !value.is_empty()),
            credential_account_id: q
                .credential_account_id
                .as_deref()
                .filter(|value| !value.is_empty()),
            model: q.model.as_deref(),
            request_id: q.request_id.as_deref(),
            start_time: start_time.as_deref(),
            end_time: end_time.as_deref(),
            sort_by: q.sort_by.as_deref(),
            sort_order: q.sort_order.as_deref(),
            key_id: q.key_id.as_deref().filter(|value| !value.is_empty()),
        })
        .map_err(ApiError::internal)?;
    let secrets = dashboard_account_secrets(&state)?;
    for log in &mut page.items {
        if let Some(secret) = secrets.get(&log.account_id) {
            log.error_message = log
                .error_message
                .take()
                .map(|error| redact_known_secret(&error, secret));
            log.diagnostic = redact_diagnostic(log.diagnostic.take(), std::slice::from_ref(secret));
        }
    }
    let attributions = state
        .db
        .lock()
        .query_forward_log_native_attributions(
            &page.items.iter().map(|log| log.id).collect::<Vec<_>>(),
        )
        .map_err(ApiError::internal)?;
    Ok(Json(DashboardForwardLogPage {
        items: page
            .items
            .into_iter()
            .map(|log| {
                let attribution = attributions.get(&log.id).cloned().unwrap_or_default();
                DashboardForwardLog {
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
    }))
}

fn dashboard_account_secrets(state: &CoreState) -> Result<BTreeMap<String, String>, ApiError> {
    let accounts = state
        .db
        .lock()
        .list_accounts()
        .map_err(ApiError::internal)?;
    Ok(accounts
        .into_iter()
        .filter(|account| !account.key_cipher.is_empty())
        .filter_map(|account| {
            state
                .decrypt_key(&account.key_cipher)
                .ok()
                .map(|secret| (account.id, secret))
        })
        .collect())
}

fn redact_known_secrets(text: &str, secrets: &BTreeMap<String, String>) -> String {
    secrets.values().fold(text.to_string(), |text, secret| {
        redact_known_secret(&text, secret)
    })
}

fn redact_diagnostic(
    diagnostic: Option<serde_json::Value>,
    secrets: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<serde_json::Value> {
    let mut encoded = diagnostic?.to_string();
    for secret in secrets {
        encoded = redact_known_secret(&encoded, secret.as_ref());
    }
    serde_json::from_str(&encoded).ok()
}

async fn forward_log_models(State(state): State<CoreState>) -> Result<Json<Vec<String>>, ApiError> {
    state
        .db
        .lock()
        .list_forward_log_models()
        .map(Json)
        .map_err(ApiError::internal)
}

/// Distinct client keys observed in forward logs — including disabled,
/// soft-deleted, and dangling ids — so the Logs filter matches exactly the
/// `client_key_id` values stored on rows.
async fn forward_log_keys(
    State(state): State<CoreState>,
) -> Result<Json<Vec<ForwardLogClientKey>>, ApiError> {
    state
        .db
        .lock()
        .list_forward_log_keys()
        .map(Json)
        .map_err(ApiError::internal)
}

async fn dashboard_summary(
    State(state): State<CoreState>,
) -> Result<Json<DashboardSummary>, ApiError> {
    let db = state.db.lock();
    let accounts = db.list_accounts().map_err(ApiError::internal)?;
    let total_accounts = accounts.len();
    let now = Utc::now();
    let free_channel_cooling = db
        .free_channel_cooldown_until()
        .map_err(ApiError::internal)?
        .is_some();
    let available_accounts = accounts
        .iter()
        .filter(|account| {
            dashboard_account_is_available(&state, account, now, free_channel_cooling)
        })
        .count();
    let (today_cost, week_cost, month_cost) = db.total_usage().map_err(ApiError::internal)?;
    Ok(Json(DashboardSummary {
        total_accounts,
        available_accounts,
        gateway_running: state.gateway.lock().is_some(),
        today_cost,
        week_cost,
        month_cost,
    }))
}

fn dashboard_account_is_available(
    state: &CoreState,
    account: &Account,
    now: DateTime<Utc>,
    free_channel_cooling: bool,
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
                && state
                    .decrypt_key(&account.key_cipher)
                    .is_ok_and(|key| !key.trim().is_empty())
        }
        (
            crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID,
            crate::provider::ANONYMOUS_FREE_OFFERING_ID,
        ) => !free_channel_cooling && !account.is_cooling_for(UpstreamChannel::Free, now),
        // Command Code GOAT is intentionally unavailable in production even
        // while a loopback-only integration seam exists in provider_adapter.
        _ => false,
    }
}

async fn daily_cost_by_model(
    State(state): State<CoreState>,
    Query(q): Query<LimitQuery>,
) -> Result<Json<Vec<DailyModelCost>>, ApiError> {
    state
        .db
        .lock()
        .daily_cost_by_model(q.days.unwrap_or(30))
        .map(Json)
        .map_err(ApiError::internal)
}

fn status_from_state(state: &CoreState) -> GatewayStatus {
    let config = state.config();
    let running = state.gateway.lock().is_some();
    let last_error = if running {
        None
    } else {
        state.db.lock().latest_gateway_error().ok().flatten()
    };
    GatewayStatus {
        running,
        port: state.active_gateway_port(),
        key: config.gateway_key.clone(),
        upstream_base_url: config.upstream_base_url,
        last_error,
    }
}

fn validate_upstream_url(url: &str) -> Result<(), ApiError> {
    validate_http_url(url, "upstream")
}

fn validate_http_url(url: &str, label: &str) -> Result<(), ApiError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| ApiError::bad_request(format!("invalid {} URL: {}", label, e)))?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(&parsed) => Ok(()),
        _ => Err(ApiError::bad_request(format!(
            "{} must use https, except loopback http",
            label
        ))),
    }
}

fn is_loopback(url: &reqwest::Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("[::1]")
    )
}

#[cfg(test)]
mod tests {
    use super::{
        AccountOrderInput, AccountSetupUpdate, AccountTestRequest, AccountUsageUpdate,
        BrowserTarget, DashboardAccountUpdate, DashboardCustomConfigUpdate,
        DashboardModelCapabilitiesUpdate, ForwardLogQuery, MAX_ACCOUNT_NOTES_CHARS,
        MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES, ManagedAccountInput, OpenBrowserInput,
        PricingMultiplierInput, PricingMultiplierUpdate, PricingRefreshPolicy,
        ProtocolProbeRequest, ProxyTestRequest, SemverVersion, SettingsUpdateRequest,
        VerifyManagedKeyInput, account_test_case, account_test_payload, advance_account_setup,
        application_models, apply_official_go_usage_snapshot, apply_pricing_refresh, asset_path,
        card_summary, create_account, create_account_inner, create_managed_account,
        dashboard_account, dashboard_summary, format_error_chain, forward_logs, get_settings,
        is_update_available, load_ready_account_for_official_go_usage, local_application_models,
        map_official_usage_refresh_error, open_account_browser, parse_semver_version,
        provider_account_usage, put_account_custom_config, put_account_model_capabilities,
        read_managed_key_verification_response, redact_diagnostic, redact_known_secrets,
        reorder_accounts, require_unique_probe_protocols, run_account_protocol_probes, test_proxy,
        toggle_account, update_account, update_account_usage, update_pricing_multipliers,
        update_settings, validate_forward_log_query, validate_websocket_origin,
        verify_managed_account_key,
    };
    use crate::browser::{BrowserProfileOperationKind, StagedBrowserProfiles};
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::{AccountUsageCalibrationSnapshot, Database};
    use crate::gateway::diagnostics::api_format_name;
    use crate::go_usage::{GoUsageError, GoUsageSnapshot, GoUsageWindowStatus};
    use crate::kernel::protocol::supported_model_protocols;
    use crate::models::{
        Account, AccountAcknowledgementInput, AccountCustomConfigInput, AccountInput,
        AccountModelCapabilityInput, AccountSetupStep, AccountType, AccountUpdate, AppConfig,
        ClaudeDesktopModels, ForwardLog, ProxyListDirection, ProxyMode, UsageWindow,
        normalize_purchase_date, purchase_expires_on,
    };
    use crate::pricing::{
        MAX_PRICING_MULTIPLIER, pricing_multiplier_deltas, pricing_semantically_equal,
        stamp_pricing_activation,
    };
    use crate::state::CoreStateInner;
    use axum::Json;
    use axum::extract::{Path as AxumPath, Query, State};
    use axum::http::{HeaderMap, StatusCode, header};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::fs;
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_data_dir(label: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        dir.push(format!("ocg-dashboard-test-{}-{}", label, nanos));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn account_test_cases_share_the_verified_model_protocol_matrix() {
        let (model, protocol) = account_test_case(Some(AccountTestRequest {
            model_id: "GPT-5.6-LUNA".into(),
            protocol: crate::provider::UpstreamProtocolKind::Responses,
        }))
        .expect("known supported pair should resolve");
        assert_eq!(model, "gpt-5.6-luna");
        assert_eq!(protocol, crate::provider::UpstreamProtocolKind::Responses);
        assert_eq!(
            account_test_payload(protocol, &model)["max_output_tokens"],
            1
        );

        let unsupported = account_test_case(Some(AccountTestRequest {
            model_id: "grok-4.5".into(),
            protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
        }))
        .expect_err("Responses-only model must reject Chat testing");
        assert_eq!(unsupported.status, StatusCode::BAD_REQUEST);

        let unknown = account_test_case(Some(AccountTestRequest {
            model_id: "unknown-model".into(),
            protocol: crate::provider::UpstreamProtocolKind::Responses,
        }))
        .expect_err("unknown models must fail without an upstream request");
        assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn protocol_probe_rejects_duplicates_before_any_upstream_call() {
        let unique = [
            crate::provider::UpstreamProtocolKind::ChatCompletions,
            crate::provider::UpstreamProtocolKind::Responses,
        ];
        require_unique_probe_protocols(&unique).expect("unique caller order is preserved");
        let error = require_unique_probe_protocols(&[
            crate::provider::UpstreamProtocolKind::ChatCompletions,
            crate::provider::UpstreamProtocolKind::Responses,
            crate::provider::UpstreamProtocolKind::ChatCompletions,
        ])
        .expect_err("duplicates must 400 locally");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("duplicate"));
    }

    fn raw_http_header_value(request: &[u8], name: &str) -> Option<String> {
        let text = String::from_utf8_lossy(request);
        let headers = text.split("\r\n\r\n").next().unwrap_or(text.as_ref());
        let needle = name.to_ascii_lowercase();
        headers.lines().find_map(|line| {
            let (header_name, value) = line.split_once(':')?;
            (header_name.trim().eq_ignore_ascii_case(&needle)).then(|| value.trim().to_string())
        })
    }

    async fn spawn_capturing_json_upstream() -> (
        SocketAddr,
        Arc<StdMutex<Vec<u8>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("protocol-probe upstream should bind");
        let address = listener.local_addr().unwrap();
        let captured_for_task = captured.clone();
        let body = r#"{"id":"ok","object":"json"}"#;
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 16 * 1024];
            let n = stream.read(&mut request).await.unwrap_or(0);
            *captured_for_task.lock().unwrap() = request[..n].to_vec();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        (address, captured, task)
    }

    #[tokio::test]
    async fn custom_x_api_key_protocol_probe_sends_x_api_key_not_authorization() {
        const CUSTOM_KEY: &str = "custom-x-api-key";
        for protocol in [
            crate::provider::UpstreamProtocolKind::ChatCompletions,
            crate::provider::UpstreamProtocolKind::Responses,
        ] {
            let (address, captured, upstream) = spawn_capturing_json_upstream().await;
            let dir = temp_data_dir(&format!("probe-x-api-key-{}", protocol.as_str()));
            let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
            let state = Arc::new(
                CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher)
                    .unwrap(),
            );
            let mut config = state.config();
            config.proxy_mode = ProxyMode::Direct;
            config.non_stream_timeout_secs = 5;
            state.set_config(config).unwrap();

            let custom = create_account_inner(
                state.clone(),
                AccountInput {
                    provider_id: crate::provider::CUSTOM_PROVIDER_ID.into(),
                    offering_id: crate::provider::CUSTOM_API_OFFERING_ID.into(),
                    name: format!("Custom {}", protocol.as_str()),
                    username: None,
                    password: None,
                    key: CUSTOM_KEY.into(),
                    referral_code: None,
                    purchase_date: None,
                    notes: None,
                },
                Some(AccountCustomConfigInput {
                    base_url: format!("http://{address}"),
                    upstream_protocol: protocol,
                    auth_scheme: crate::provider::UpstreamAuthScheme::XApiKey,
                }),
                Vec::new(),
                vec![AccountModelCapabilityInput {
                    model_id: "org/model".into(),
                    protocol,
                    source: None,
                }],
            )
            .expect("Custom x-api-key account should save")
            .0;

            let response = run_account_protocol_probes(
                State(state.clone()),
                AxumPath(custom.id.clone()),
                Json(ProtocolProbeRequest {
                    model_id: "org/model".into(),
                    protocols: vec![protocol],
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("protocol probe should reach the loopback: {error:?}"));
            assert_eq!(response.0.results.len(), 1, "{protocol:?}");
            assert!(
                response.0.results[0].success,
                "probe should succeed for {protocol:?}: {:?}",
                response.0.results[0].error
            );

            upstream.await.unwrap();
            let raw = captured.lock().unwrap().clone();
            let x_api_key = raw_http_header_value(&raw, "x-api-key");
            let authorization = raw_http_header_value(&raw, "authorization");
            assert_eq!(
                x_api_key.as_deref(),
                Some(CUSTOM_KEY),
                "Custom x-api-key {protocol:?} probe must send x-api-key: {}",
                String::from_utf8_lossy(&raw)
            );
            assert!(
                authorization.is_none(),
                "Custom x-api-key {protocol:?} probe must not send Authorization: {}",
                String::from_utf8_lossy(&raw)
            );

            drop(state);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn provider_contract_card_summary_uses_descriptor_capabilities() {
        let go = crate::provider::ProviderRegistry::get(
            crate::provider::OPENCODE_PROVIDER_ID,
            crate::provider::GO_OFFERING_ID,
        )
        .unwrap();
        let go_card = card_summary(go);
        assert!(!go_card.fetch_zen_models);
        assert!(!go_card.discover_models);
        assert!(go_card.protocol_probe);
        assert!(!go_card.catalog_refresh);

        let zen = crate::provider::ProviderRegistry::get(
            crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID,
            crate::provider::ANONYMOUS_FREE_OFFERING_ID,
        )
        .unwrap();
        let zen_card = card_summary(zen);
        assert!(zen_card.fetch_zen_models);
        assert!(zen_card.catalog_refresh);
        assert!(zen_card.protocol_probe);

        let custom = crate::provider::ProviderRegistry::get(
            crate::provider::CUSTOM_PROVIDER_ID,
            crate::provider::CUSTOM_API_OFFERING_ID,
        )
        .unwrap();
        let custom_card = card_summary(custom);
        assert!(custom_card.discover_models);
        assert!(custom_card.catalog_refresh);
        assert!(custom_card.protocol_probe);

        let goat = crate::provider::ProviderRegistry::get(
            crate::provider::COMMAND_CODE_PROVIDER_ID,
            crate::provider::GOAT_OFFERING_ID,
        )
        .unwrap();
        let goat_card = card_summary(goat);
        assert!(!goat_card.protocol_probe);
        assert!(!goat_card.catalog_refresh);
    }

    async fn spawn_key_verification_upstream(
        status: StatusCode,
        body: impl Into<String>,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let body = body.into();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test upstream should bind");
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0_u8; 8192];
            let _ = stream.read(&mut request).await;
            let reason = status.canonical_reason().unwrap_or("Test");
            let response = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status.as_u16(),
                reason,
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        (address, task)
    }

    fn managed_key_verification_account(id: &str) -> Account {
        let mut account = test_account(id);
        account.account_type = AccountType::Managed;
        account.setup_step = AccountSetupStep::KeyVerification;
        account.key_cipher.clear();
        account.enabled = false;
        account
    }

    fn test_account(id: &str) -> Account {
        let now = Utc::now();
        Account {
            id: id.into(),
            provider_id: crate::provider::default_provider_id(),
            offering_id: crate::provider::default_offering_id(),
            credential_kind: crate::provider::default_credential_kind(),
            quota_scope: crate::provider::default_quota_scope(),
            name: id.into(),
            username: None,
            password_cipher: None,
            key_cipher: format!("cipher-{id}"),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: "2026-06-15".into(),
            expires_on: "2026-07-15".into(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn asset_path_rejects_escape_components() {
        let root = Path::new("dist");

        assert_eq!(
            asset_path(root, "index.js").unwrap(),
            root.join("assets").join("index.js")
        );
        assert_eq!(
            asset_path(root, "nested/index.js").unwrap(),
            root.join("assets").join("nested").join("index.js")
        );

        assert!(asset_path(root, "../secret.txt").is_none());
        assert!(asset_path(root, "/secret.txt").is_none());
        assert!(asset_path(root, r"nested\secret.txt").is_none());
        assert!(asset_path(root, "C:/secret.txt").is_none());
    }

    #[test]
    fn browser_websocket_origin_must_match_request_host() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "manager.example:9443".parse().unwrap());
        headers.insert(
            header::ORIGIN,
            "https://manager.example:9443".parse().unwrap(),
        );
        validate_websocket_origin(&headers).expect("same origin should pass");

        headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        let error = validate_websocket_origin(&headers).expect_err("cross origin must fail");
        assert_eq!(error.status, StatusCode::FORBIDDEN);

        headers.remove(header::ORIGIN);
        assert!(validate_websocket_origin(&headers).is_err());
    }

    #[tokio::test]
    async fn proxy_test_accepts_any_upstream_status_and_manual_mode_never_falls_back() {
        let dir = temp_data_dir("proxy-test");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );

        let (address, upstream) =
            spawn_key_verification_upstream(StatusCode::UNAUTHORIZED, "unauthorized").await;
        let direct = test_proxy(
            State(state.clone()),
            Json(ProxyTestRequest {
                proxy_mode: ProxyMode::Direct,
                proxy_list_direction: None,
                proxy_url: String::new(),
                upstream_base_url: format!("http://{address}"),
            }),
        )
        .await
        .expect("an HTTP response should prove direct reachability")
        .0;
        assert_eq!(direct.status, StatusCode::UNAUTHORIZED.as_u16());
        upstream.await.unwrap();

        let closed_proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_proxy_address = closed_proxy.local_addr().unwrap();
        drop(closed_proxy);
        let (address, upstream) =
            spawn_key_verification_upstream(StatusCode::OK, "direct fallback must not happen")
                .await;
        let error = test_proxy(
            State(state.clone()),
            Json(ProxyTestRequest {
                proxy_mode: ProxyMode::Manual,
                proxy_list_direction: None,
                proxy_url: format!("http://{closed_proxy_address}"),
                upstream_base_url: format!("http://{address}"),
            }),
        )
        .await
        .expect_err("a failed manual proxy must not fall back to the reachable upstream");
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        upstream.abort();

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn dashboard_open_recovers_staged_profile_before_native_launch() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = temp_data_dir("browser-open-recovery");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("browser-open-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        state.set_dashboard_local_mode(true);
        let account = test_account("account-1");
        state.db.lock().create_account(&account).unwrap();
        let profile = dir.join("browser-profiles").join(&account.id);
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("Cookies"), b"recover-me").unwrap();
        let staged = StagedBrowserProfiles::stage(
            &dir,
            &account.id,
            BrowserProfileOperationKind::DeleteAccount,
        )
        .unwrap();
        assert!(!profile.exists());
        drop(staged);

        let launched = Arc::new(AtomicBool::new(false));
        let launched_flag = launched.clone();
        let expected_profile = profile.clone();
        state
            .browser
            .register_native_hooks(
                Arc::new(move |_, _| {
                    if !expected_profile.join("Cookies").is_file() {
                        anyhow::bail!("profile was not recovered before launch");
                    }
                    launched_flag.store(true, Ordering::SeqCst);
                    Ok(())
                }),
                Arc::new(|_| Ok(())),
            )
            .unwrap();

        let response = open_account_browser(
            State(state.clone()),
            AxumPath(account.id.clone()),
            HeaderMap::new(),
            Json(OpenBrowserInput {
                target: BrowserTarget::Console,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.0.mode, crate::browser::BrowserMode::Native);
        assert!(launched.load(Ordering::SeqCst));
        assert!(profile.join("Cookies").is_file());
        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn failed_pricing_refresh_preserves_last_known_good_snapshot() {
        let dir = temp_data_dir("pricing-lkg");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("pricing-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        let before = state.pricing_snapshot();

        let response = apply_pricing_refresh(
            &state,
            Err(anyhow::anyhow!("fixture parser rejected the document")),
            None,
            None,
        )
        .unwrap();

        assert_eq!(response.refresh_status, "failed_no_change");
        assert_eq!(response.snapshot.revision, before.revision);
        assert_eq!(state.pricing_snapshot().revision, before.revision);
        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pricing_refresh_requires_one_confirmation_per_changed_model() {
        let dir = temp_data_dir("pricing-confirmation");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("pricing-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        let before = state.pricing_snapshot();
        let mut official = before.as_ref().clone();
        official.content_hash = "official-price-change".into();
        for model in &mut official.models {
            if model.model_id == "qwen3.7-plus" {
                model.quota_multiplier = 2.0;
            }
        }

        let changes = pricing_multiplier_deltas(&before, &official);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].model_id, "qwen3.7-plus");
        let response = apply_pricing_refresh(&state, Ok(official), None, None).unwrap();
        assert_eq!(response.refresh_status, "needs_confirmation");
        assert_eq!(response.multiplier_changes.len(), 1);
        assert_eq!(
            response.official_content_hash.as_deref(),
            Some("official-price-change")
        );
        assert_eq!(state.pricing_snapshot().revision, before.revision);

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pricing_refresh_reconfirms_when_the_official_candidate_changes() {
        let dir = temp_data_dir("pricing-confirmation-candidate");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("pricing-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        let before = state.pricing_snapshot();
        let mut first = before.as_ref().clone();
        first.content_hash = "official-candidate-a".into();
        for model in &mut first.models {
            if model.model_id == "qwen3.7-plus" {
                model.quota_multiplier = 2.0;
            }
        }
        let mut second = first.clone();
        second.content_hash = "official-candidate-b".into();
        second.models[0].input += 0.25;

        let preview = apply_pricing_refresh(&state, Ok(first), None, None).unwrap();
        assert_eq!(preview.refresh_status, "needs_confirmation");
        assert_eq!(
            preview.official_content_hash.as_deref(),
            Some("official-candidate-a")
        );

        let changed = apply_pricing_refresh(
            &state,
            Ok(second.clone()),
            Some(PricingRefreshPolicy::UseOfficial),
            preview.official_content_hash.as_deref(),
        )
        .unwrap();
        assert_eq!(changed.refresh_status, "needs_confirmation");
        assert_eq!(
            changed.official_content_hash.as_deref(),
            Some("official-candidate-b")
        );
        assert_eq!(state.pricing_snapshot().revision, before.revision);

        let confirmed = apply_pricing_refresh(
            &state,
            Ok(second),
            Some(PricingRefreshPolicy::UseOfficial),
            changed.official_content_hash.as_deref(),
        )
        .unwrap();
        assert_eq!(confirmed.refresh_status, "success");
        assert_eq!(confirmed.snapshot.content_hash, "official-candidate-b");

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keep_current_refresh_merges_multiplier_across_all_official_tiers() {
        let dir = temp_data_dir("pricing-keep-current");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("pricing-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        let before = state.pricing_snapshot();
        let current_multiplier = before
            .models
            .iter()
            .find(|model| model.model_id == "qwen3.7-plus")
            .unwrap()
            .quota_multiplier;
        let mut official = before.as_ref().clone();
        official.content_hash = "official-price-and-multiplier-change".into();
        official.models[0].input += 0.25;
        for model in &mut official.models {
            if model.model_id == "qwen3.7-plus" {
                model.quota_multiplier = 2.0;
            }
        }

        let response = apply_pricing_refresh(
            &state,
            Ok(official),
            Some(PricingRefreshPolicy::KeepCurrent),
            Some("official-price-and-multiplier-change"),
        )
        .unwrap();
        assert_eq!(response.refresh_status, "success");
        assert_ne!(response.snapshot.revision, before.revision);
        assert!(
            response
                .snapshot
                .models
                .iter()
                .filter(|model| model.model_id == "qwen3.7-plus")
                .all(|model| model.quota_multiplier == current_multiplier)
        );
        assert_eq!(
            state
                .db
                .lock()
                .latest_pricing_snapshot()
                .unwrap()
                .unwrap()
                .revision,
            response.snapshot.revision
        );

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_refresh_applies_changed_multiplier_and_source_metadata() {
        let dir = temp_data_dir("pricing-use-official");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("pricing-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        let before = state.pricing_snapshot();
        let mut official = before.as_ref().clone();
        official.content_hash = "new-official-document".into();
        official.document_updated_at = "2026-07-18T00:00:00Z".into();
        for model in &mut official.models {
            if model.model_id == "grok-4.5" {
                model.quota_multiplier = 3.0;
            }
        }
        assert!(!pricing_semantically_equal(&before, &official));

        let response = apply_pricing_refresh(
            &state,
            Ok(official),
            Some(PricingRefreshPolicy::UseOfficial),
            Some("new-official-document"),
        )
        .unwrap();
        assert_eq!(response.refresh_status, "success");
        assert_eq!(response.snapshot.content_hash, "new-official-document");
        assert_eq!(
            response
                .snapshot
                .models
                .iter()
                .find(|model| model.model_id == "grok-4.5")
                .unwrap()
                .quota_multiplier,
            3.0
        );

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn seed_only_muse_multiplier_requires_confirmation_and_honors_refresh_policy() {
        let dir = temp_data_dir("pricing-muse-seed-confirmation");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("pricing-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        let mut active = state.pricing_snapshot().as_ref().clone();
        let standard_multiplier = active
            .models
            .iter()
            .find(|model| model.model_id == "muse-spark-1.2")
            .unwrap()
            .quota_multiplier;
        for model in &mut active.models {
            if model.model_id == "muse-spark-1.2" {
                model.quota_multiplier = 2.5;
            }
        }
        let active = stamp_pricing_activation(active);
        state.activate_pricing_snapshot(active).unwrap();

        // The public table omits standard Muse. The refresh candidate must be
        // covered before its multiplier is compared with the active snapshot.
        let mut official = state.pricing_snapshot().as_ref().clone();
        official.content_hash = "official-without-muse".into();
        official
            .models
            .retain(|model| !model.model_id.starts_with("muse-spark-1.2"));

        let preview = apply_pricing_refresh(&state, Ok(official.clone()), None, None).unwrap();
        assert_eq!(preview.refresh_status, "needs_confirmation");
        assert_eq!(preview.multiplier_changes.len(), 1);
        assert_eq!(preview.multiplier_changes[0].model_id, "muse-spark-1.2");

        let kept = apply_pricing_refresh(
            &state,
            Ok(official.clone()),
            Some(PricingRefreshPolicy::KeepCurrent),
            preview.official_content_hash.as_deref(),
        )
        .unwrap();
        assert_eq!(kept.refresh_status, "success");
        assert_eq!(
            kept.snapshot
                .models
                .iter()
                .find(|model| model.model_id == "muse-spark-1.2")
                .unwrap()
                .quota_multiplier,
            2.5
        );

        let second_preview =
            apply_pricing_refresh(&state, Ok(official.clone()), None, None).unwrap();
        assert_eq!(second_preview.refresh_status, "needs_confirmation");
        let replaced = apply_pricing_refresh(
            &state,
            Ok(official),
            Some(PricingRefreshPolicy::UseOfficial),
            second_preview.official_content_hash.as_deref(),
        )
        .unwrap();
        assert_eq!(replaced.refresh_status, "success");
        assert_eq!(
            replaced
                .snapshot
                .models
                .iter()
                .find(|model| model.model_id == "muse-spark-1.2")
                .unwrap()
                .quota_multiplier,
            standard_multiplier
        );

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refresh_is_unchanged_when_only_volatile_activation_metadata_differs() {
        let dir = temp_data_dir("pricing-unchanged");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("pricing-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        let before = state.pricing_snapshot();
        let mut fetched = before.as_ref().clone();
        fetched.revision = "volatile-fetch-revision".into();
        fetched.activated_at = "2099-01-01T00:00:00Z".into();

        let response = apply_pricing_refresh(&state, Ok(fetched), None, None).unwrap();
        assert_eq!(response.refresh_status, "unchanged");
        assert_eq!(response.snapshot.revision, before.revision);
        assert_eq!(state.pricing_snapshot().revision, before.revision);

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn multiplier_batch_updates_every_tier_with_one_revision() {
        let dir = temp_data_dir("pricing-edit");
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("pricing-test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        let before = state.pricing_snapshot();
        let Json(updated) = update_pricing_multipliers(
            State(state.clone()),
            Json(PricingMultiplierUpdate {
                expected_revision: before.revision.clone(),
                multipliers: vec![PricingMultiplierInput {
                    model_id: "qwen3.7-plus".into(),
                    multiplier: 0.75,
                }],
            }),
        )
        .await
        .unwrap();
        assert_ne!(updated.revision, before.revision);
        assert!(
            updated
                .models
                .iter()
                .filter(|model| model.model_id == "qwen3.7-plus")
                .all(|model| model.quota_multiplier == 0.75)
        );
        assert_eq!(
            state
                .db
                .lock()
                .latest_pricing_snapshot()
                .unwrap()
                .unwrap()
                .revision,
            updated.revision
        );

        let Json(no_change) = update_pricing_multipliers(
            State(state.clone()),
            Json(PricingMultiplierUpdate {
                expected_revision: updated.revision.clone(),
                multipliers: vec![PricingMultiplierInput {
                    model_id: "qwen3.7-plus".into(),
                    multiplier: 0.75,
                }],
            }),
        )
        .await
        .unwrap();
        assert_eq!(no_change.revision, updated.revision);

        let stale = update_pricing_multipliers(
            State(state.clone()),
            Json(PricingMultiplierUpdate {
                expected_revision: before.revision.clone(),
                multipliers: vec![PricingMultiplierInput {
                    model_id: "grok-4.5".into(),
                    multiplier: 2.0,
                }],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(stale.status, StatusCode::CONFLICT);

        let too_large = update_pricing_multipliers(
            State(state.clone()),
            Json(PricingMultiplierUpdate {
                expected_revision: updated.revision.clone(),
                multipliers: vec![PricingMultiplierInput {
                    model_id: "grok-4.5".into(),
                    multiplier: MAX_PRICING_MULTIPLIER + 0.1,
                }],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(too_large.status, StatusCode::BAD_REQUEST);

        let zero = update_pricing_multipliers(
            State(state.clone()),
            Json(PricingMultiplierUpdate {
                expected_revision: updated.revision,
                multipliers: vec![PricingMultiplierInput {
                    model_id: "grok-4.5".into(),
                    multiplier: 0.0,
                }],
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(zero.status, StatusCode::BAD_REQUEST);

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn forward_log_query_normalizes_offsets_and_rejects_invalid_ordering() {
        let query = ForwardLogQuery {
            start_time: Some("2026-07-17T12:00:00+08:00".into()),
            end_time: Some("2026-07-17T05:00:00Z".into()),
            sort_by: Some("attempt".into()),
            sort_order: Some("asc".into()),
            ..ForwardLogQuery::default()
        };
        let (start, end) = validate_forward_log_query(&query).expect("valid query");
        assert_eq!(start.as_deref(), Some("2026-07-17T04:00:00.000Z"));
        assert_eq!(end.as_deref(), Some("2026-07-17T05:00:00.000Z"));

        for invalid in [
            ForwardLogQuery {
                sort_by: Some("costt".into()),
                ..ForwardLogQuery::default()
            },
            ForwardLogQuery {
                sort_order: Some("sideways".into()),
                ..ForwardLogQuery::default()
            },
            ForwardLogQuery {
                start_time: Some("not-a-time".into()),
                ..ForwardLogQuery::default()
            },
            ForwardLogQuery {
                start_time: Some("2026-07-17T06:00:00Z".into()),
                end_time: Some("2026-07-17T05:00:00Z".into()),
                ..ForwardLogQuery::default()
            },
        ] {
            assert!(validate_forward_log_query(&invalid).is_err());
        }
    }

    #[test]
    fn dashboard_account_does_not_export_secrets() {
        const OPAQUE_KEY: &str = "opaque/account+key=42";
        let dir = temp_data_dir("secret-list");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        let account = Account {
            id: "acct-1".into(),
            provider_id: crate::provider::default_provider_id(),
            offering_id: crate::provider::default_offering_id(),
            credential_kind: crate::provider::default_credential_kind(),
            quota_scope: crate::provider::default_quota_scope(),
            name: "main".into(),
            username: Some("user".into()),
            password_cipher: Some(state.encrypt_key("password-secret").unwrap()),
            key_cipher: state.encrypt_key(OPAQUE_KEY).unwrap(),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: "2026-01-31".into(),
            expires_on: "2026-02-28".into(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: Some(format!("legacy rate limit echoed {OPAQUE_KEY}")),
            auth_error: Some(format!("legacy auth failure echoed {OPAQUE_KEY}")),
            notes: Some("keep this secret-free".into()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let dto = dashboard_account(&state, account);

        assert_eq!(dto.username, "user");
        assert!(dto.password.is_empty());
        assert!(dto.key.is_empty());
        assert!(!dto.last_error.as_deref().unwrap().contains(OPAQUE_KEY));
        assert!(!dto.auth_error.as_deref().unwrap().contains(OPAQUE_KEY));
        assert_eq!(dto.purchase_date, "2026-01-31");
        assert_eq!(dto.expires_on, "2026-02-28");
        let json = serde_json::to_value(dto).expect("dashboard account should serialize");
        assert!(!json.to_string().contains(OPAQUE_KEY));
        assert!(json.get("recharge_date").is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn dashboard_account_redacts_verification_error_with_known_secret() {
        const OPAQUE_KEY: &str = "opaque/account+key=42";
        let dir = temp_data_dir("secret-verify-error");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        let account = Account {
            id: "acct-verify".into(),
            provider_id: crate::provider::default_provider_id(),
            offering_id: crate::provider::default_offering_id(),
            credential_kind: crate::provider::default_credential_kind(),
            quota_scope: crate::provider::default_quota_scope(),
            name: "verify".into(),
            username: None,
            password_cipher: None,
            key_cipher: state.encrypt_key(OPAQUE_KEY).unwrap(),
            enabled: true,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: String::new(),
            expires_on: String::new(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        state.db.lock().create_account(&account).unwrap();
        state
            .db
            .lock()
            .set_account_verification(
                "acct-verify",
                crate::provider::ConnectionVerificationStatus::Failed,
                None,
                Some(&format!("connection verify echoed {OPAQUE_KEY}")),
            )
            .unwrap();
        let stored = state.db.lock().get_account("acct-verify").unwrap().unwrap();
        let dto = dashboard_account(&state, stored);
        let error = dto
            .verification_error
            .as_deref()
            .expect("verification error should be exported");
        assert!(error.contains("connection verify echoed"));
        assert!(!error.contains(OPAQUE_KEY));
        assert!(dto.last_error.is_none());
        assert!(dto.auth_error.is_none());
        let json = serde_json::to_value(dto).expect("dashboard account should serialize");
        assert!(!json.to_string().contains(OPAQUE_KEY));
        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn historical_dashboard_logs_redact_current_account_secrets() {
        const OPAQUE_KEY: &str = "opaque/account+key=42";
        let secrets = BTreeMap::from([("acct-1".to_string(), OPAQUE_KEY.to_string())]);
        let message =
            redact_known_secrets(&format!("legacy gateway log echoed {OPAQUE_KEY}"), &secrets);
        assert!(!message.contains(OPAQUE_KEY));

        let diagnostic = redact_diagnostic(
            Some(serde_json::json!({
                "upstream_error": {"message": format!("rejected {OPAQUE_KEY}")}
            })),
            secrets.values(),
        )
        .unwrap();
        assert!(!diagnostic.to_string().contains(OPAQUE_KEY));
    }

    #[tokio::test]
    async fn create_account_defaults_purchase_date_and_returns_persisted_expiry() {
        let dir = temp_data_dir("create-default-purchase-date");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).expect("test database should open");
        let state = Arc::new(
            CoreStateInner::new(db, dir.clone(), cipher).expect("test state should initialize"),
        );

        let account = create_account(
            State(state.clone()),
            Json(AccountInput {
                provider_id: crate::provider::default_provider_id(),
                offering_id: crate::provider::default_offering_id(),
                name: "main".into(),
                username: None,
                password: None,
                key: "sk-test".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            }),
        )
        .await
        .expect("account should be created")
        .0;

        assert_eq!(
            normalize_purchase_date(&account.purchase_date)
                .expect("persisted purchase date should be valid"),
            account.purchase_date
        );
        assert_eq!(
            account.expires_on,
            purchase_expires_on(&account.purchase_date)
                .expect("persisted purchase date should have an expiry")
        );
        let persisted = state
            .db
            .lock()
            .get_account(&account.id)
            .expect("created account lookup should succeed")
            .expect("created account should exist");
        assert_eq!(persisted.purchase_date, account.purchase_date);
        assert_eq!(persisted.expires_on, account.expires_on);

        drop(state);
        fs::remove_dir_all(dir).expect("test directory should be removable");
    }

    #[tokio::test]
    async fn account_notes_accept_empty_and_reject_overlong() {
        let dir = temp_data_dir("account-notes");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).expect("test database should open");
        let state = Arc::new(
            CoreStateInner::new(db, dir.clone(), cipher).expect("test state should initialize"),
        );

        let created = create_account(
            State(state.clone()),
            Json(AccountInput {
                provider_id: crate::provider::default_provider_id(),
                offering_id: crate::provider::default_offering_id(),
                name: "noted".into(),
                username: None,
                password: None,
                key: "sk-test".into(),
                referral_code: None,
                purchase_date: None,
                notes: Some("  first note  ".into()),
            }),
        )
        .await
        .expect("account should be created")
        .0;
        assert_eq!(created.notes, "first note");

        let cleared = update_account(
            State(state.clone()),
            AxumPath(created.id.clone()),
            Json(DashboardAccountUpdate::from(AccountUpdate {
                notes: Some("   ".into()),
                ..AccountUpdate::default()
            })),
        )
        .await
        .expect("empty notes should clear")
        .0;
        assert!(cleared.notes.is_empty());

        let overlong = "n".repeat(MAX_ACCOUNT_NOTES_CHARS + 1);
        let error = update_account(
            State(state.clone()),
            AxumPath(created.id.clone()),
            Json(DashboardAccountUpdate::from(AccountUpdate {
                notes: Some(overlong),
                ..AccountUpdate::default()
            })),
        )
        .await
        .expect_err("overlong notes should fail");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        drop(state);
        fs::remove_dir_all(dir).expect("test directory should be removable");
    }

    #[tokio::test]
    async fn managed_draft_requires_invite_and_resumes_in_strict_order() {
        let dir = temp_data_dir("managed-draft");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).expect("test database should open");
        let state = Arc::new(
            CoreStateInner::new(db, dir.clone(), cipher).expect("test state should initialize"),
        );

        let mut config = state.config();
        config.opencode_invite_url.clear();
        state.set_config(config).unwrap();

        let missing_invite = create_managed_account(
            State(state.clone()),
            Json(ManagedAccountInput {
                name: "pending".into(),
                username: None,
                notes: None,
                expected_revision: None,
            }),
        )
        .await
        .expect_err("managed registration should require an invite URL");
        assert_eq!(missing_invite.status, StatusCode::PRECONDITION_FAILED);

        let mut config = state.config();
        config.opencode_invite_url = "https://opencode.ai/invite/test".into();
        state.set_config(config).unwrap();
        let (status, Json(draft)) = create_managed_account(
            State(state.clone()),
            Json(ManagedAccountInput {
                name: "  pending  ".into(),
                username: Some("  user@example.test  ".into()),
                notes: Some("  keep this note  ".into()),
                expected_revision: None,
            }),
        )
        .await
        .expect("managed draft should be created");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(draft.account_type, AccountType::Managed);
        assert_eq!(draft.setup_step, AccountSetupStep::GoogleAccount);
        assert!(!draft.enabled);
        assert_eq!(draft.notes, "keep this note");

        let skipped = advance_account_setup(
            State(state.clone()),
            AxumPath(draft.id.clone()),
            Json(AccountSetupUpdate {
                setup_step: AccountSetupStep::Payment,
                expected_revision: None,
            }),
        )
        .await
        .expect_err("setup must not skip steps forward");
        assert_eq!(skipped.status, StatusCode::CONFLICT);

        let advanced = advance_account_setup(
            State(state.clone()),
            AxumPath(draft.id.clone()),
            Json(AccountSetupUpdate {
                setup_step: AccountSetupStep::OpencodeRegistration,
                expected_revision: None,
            }),
        )
        .await
        .expect("next setup step should save")
        .0;
        assert_eq!(advanced.setup_step, AccountSetupStep::OpencodeRegistration);

        let paid = advance_account_setup(
            State(state.clone()),
            AxumPath(draft.id.clone()),
            Json(AccountSetupUpdate {
                setup_step: AccountSetupStep::Payment,
                expected_revision: None,
            }),
        )
        .await
        .expect("payment step should save")
        .0;
        assert_eq!(paid.setup_step, AccountSetupStep::Payment);

        let rewound = advance_account_setup(
            State(state.clone()),
            AxumPath(draft.id.clone()),
            Json(AccountSetupUpdate {
                setup_step: AccountSetupStep::GoogleAccount,
                expected_revision: None,
            }),
        )
        .await
        .expect("setup should allow rewinding to earlier steps")
        .0;
        assert_eq!(rewound.setup_step, AccountSetupStep::GoogleAccount);

        let persisted = state.db.lock().get_account(&draft.id).unwrap().unwrap();
        assert!(persisted.key_cipher.is_empty());
        assert!(!persisted.enabled);

        drop(state);
        fs::remove_dir_all(dir).expect("test directory should be removable");
    }

    #[tokio::test]
    async fn managed_key_verification_routes_only_2xx_and_429_accounts() {
        const OPAQUE_KEY: &str = "opaque/account+key=42";
        for (label, status, body, should_finish) in [
            ("ok", StatusCode::OK, r#"{"choices":[]}"#, true),
            (
                "rate-limited",
                StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":{"message":"weekly usage limit reached for opaque/account+key=42"}}"#,
                true,
            ),
            (
                "unauthorized",
                StatusCode::UNAUTHORIZED,
                r#"{"error":{"message":"invalid key opaque/account+key=42"}}"#,
                false,
            ),
            (
                "server-error",
                StatusCode::INTERNAL_SERVER_ERROR,
                r#"{"error":{"message":"temporary failure for opaque/account+key=42"}}"#,
                false,
            ),
        ] {
            let (address, upstream) = spawn_key_verification_upstream(status, body).await;
            let dir = temp_data_dir(&format!("verify-{label}"));
            let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
            let db = Database::open(dir.clone()).unwrap();
            db.create_account(&managed_key_verification_account(label))
                .unwrap();
            let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
            let mut config = state.config();
            config.upstream_base_url = format!("http://{address}");
            config.non_stream_timeout_secs = 5;
            state.set_config(config).unwrap();

            let result = verify_managed_account_key(
                State(state.clone()),
                AxumPath(label.to_string()),
                Json(VerifyManagedKeyInput {
                    key: OPAQUE_KEY.into(),
                    expected_revision: None,
                }),
            )
            .await;
            let stored = state.db.lock().get_account(label).unwrap().unwrap();
            assert_ne!(stored.key_cipher, OPAQUE_KEY);
            if should_finish {
                let dto = result.expect("2xx and 429 should complete setup").0;
                assert!(!serde_json::to_string(&dto).unwrap().contains(OPAQUE_KEY));
                assert_eq!(dto.setup_step, AccountSetupStep::Ready);
                assert!(dto.enabled);
                assert!(dto.key.is_empty());
                assert_eq!(stored.setup_step, AccountSetupStep::Ready);
                assert!(stored.enabled);
                if status == StatusCode::TOO_MANY_REQUESTS {
                    assert!(stored.cooldown_until.is_some());
                }
            } else {
                let error = result.expect_err("auth and server failures should stay pending");
                assert!(
                    !error.message.contains(OPAQUE_KEY),
                    "verification API error leaked key: {}",
                    error.message
                );
                assert!(matches!(
                    error.status,
                    StatusCode::BAD_REQUEST | StatusCode::BAD_GATEWAY
                ));
                assert_eq!(stored.setup_step, AccountSetupStep::KeyVerification);
                assert!(!stored.enabled);
            }
            assert!(
                stored
                    .last_error
                    .as_deref()
                    .is_none_or(|error| !error.contains(OPAQUE_KEY))
            );
            assert!(
                stored
                    .auth_error
                    .as_deref()
                    .is_none_or(|error| !error.contains(OPAQUE_KEY))
            );

            upstream.await.unwrap();
            drop(state);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[tokio::test]
    async fn managed_key_verification_response_read_is_bounded() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();

        let (address, upstream) =
            spawn_key_verification_upstream(StatusCode::OK, "normal body").await;
        let normal = read_managed_key_verification_response(
            client
                .get(format!("http://{address}"))
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(normal, "normal body");
        upstream.await.unwrap();

        let oversized = "x".repeat(MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES + 1024);
        let (address, upstream) = spawn_key_verification_upstream(StatusCode::OK, oversized).await;
        let body = read_managed_key_verification_response(
            client
                .get(format!("http://{address}"))
                .send()
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(body.ends_with("\n<key verification response truncated>"));
        assert!(
            body.len()
                <= MAX_MANAGED_KEY_VERIFICATION_RESPONSE_BYTES
                    + "\n<key verification response truncated>".len()
        );
        upstream.await.unwrap();
    }

    #[tokio::test]
    async fn update_account_rejects_invalid_purchase_date_as_bad_request() {
        let dir = temp_data_dir("invalid-purchase-date");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).expect("test database should open");
        db.create_account(&test_account("acct-1"))
            .expect("test account should be created");
        let state = Arc::new(
            CoreStateInner::new(db, dir.clone(), cipher).expect("test state should initialize"),
        );

        let error = update_account(
            State(state.clone()),
            AxumPath("acct-1".into()),
            Json(DashboardAccountUpdate::from(AccountUpdate {
                name: None,
                username: None,
                password: None,
                key: None,
                enabled: None,
                referral_code: None,
                purchase_date: Some("2026-02-30".into()),
                notes: None,
            })),
        )
        .await
        .expect_err("invalid purchase date should fail");

        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        let persisted = state
            .db
            .lock()
            .get_account("acct-1")
            .expect("account lookup should succeed")
            .expect("account should still exist");
        assert_eq!(persisted.purchase_date, "2026-06-15");

        drop(state);
        fs::remove_dir_all(dir).expect("test directory should be removable");
    }

    #[tokio::test]
    async fn reorder_accounts_maps_validation_errors_and_returns_saved_order() {
        let dir = temp_data_dir("reorder-accounts");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).expect("test database should open");
        for id in ["acct-1", "acct-2", "acct-3"] {
            db.create_account(&test_account(id))
                .expect("test account should be created");
        }
        let state = Arc::new(
            CoreStateInner::new(db, dir.clone(), cipher).expect("test state should initialize"),
        );

        let duplicate = reorder_accounts(
            State(state.clone()),
            Json(AccountOrderInput {
                account_ids: vec!["acct-1".into(), "acct-1".into(), "acct-3".into()],
                expected_revision: None,
            }),
        )
        .await
        .expect_err("duplicate ids should fail");
        assert_eq!(duplicate.status, StatusCode::BAD_REQUEST);

        for stale_ids in [
            vec!["acct-1".into(), "acct-2".into()],
            vec!["acct-1".into(), "acct-2".into(), "missing".into()],
            Vec::new(),
        ] {
            let stale = reorder_accounts(
                State(state.clone()),
                Json(AccountOrderInput {
                    account_ids: stale_ids,
                    expected_revision: None,
                }),
            )
            .await
            .expect_err("stale account set should fail");
            assert_eq!(stale.status, StatusCode::CONFLICT);
        }

        let unchanged = state
            .db
            .lock()
            .list_accounts()
            .expect("account order should load")
            .into_iter()
            .map(|account| account.id)
            .collect::<Vec<_>>();
        assert_eq!(
            unchanged,
            [
                crate::provider::ZEN_FREE_ACCOUNT_ID,
                "acct-1",
                "acct-2",
                "acct-3",
            ]
        );

        let reordered = reorder_accounts(
            State(state.clone()),
            Json(AccountOrderInput {
                account_ids: vec![
                    "acct-3".into(),
                    "acct-1".into(),
                    "acct-2".into(),
                    crate::provider::ZEN_FREE_ACCOUNT_ID.into(),
                ],
                expected_revision: None,
            }),
        )
        .await
        .expect("complete account set should be reordered")
        .0;
        assert_eq!(
            reordered
                .into_iter()
                .map(|account| account.id)
                .collect::<Vec<_>>(),
            [
                "acct-3",
                "acct-1",
                "acct-2",
                crate::provider::ZEN_FREE_ACCOUNT_ID,
            ]
        );

        drop(state);
        fs::remove_dir_all(dir).expect("test directory should be removable");
    }

    #[tokio::test]
    async fn manual_usage_update_validates_persists_and_keeps_account_available() {
        let dir = temp_data_dir("manual-usage");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        db.create_account(&Account {
            id: "acct-usage".into(),
            provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.into(),
            offering_id: crate::provider::GOAT_OFFERING_ID.into(),
            credential_kind: crate::provider::default_credential_kind(),
            quota_scope: crate::provider::default_quota_scope(),
            name: "usage".into(),
            username: None,
            password_cipher: None,
            key_cipher: cipher.encrypt("sk-test").unwrap(),
            enabled: false,
            account_type: AccountType::Key,
            setup_step: AccountSetupStep::Ready,
            referral_code: None,
            purchase_date: "2026-01-31".into(),
            expires_on: "2026-02-28".into(),
            cooldown_until: None,
            cooldown_generic_until: None,
            cooldown_5h_until: None,
            cooldown_week_until: None,
            cooldown_month_until: None,
            cooldown_free_until: None,
            last_error: None,
            auth_error: None,
            notes: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        })
        .unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let invalid = update_account_usage(
            State(state.clone()),
            AxumPath("acct-usage".into()),
            Json(AccountUsageUpdate {
                window: "invalid".into(),
                percent: 50.0,
                resets_in_minutes: None,
            }),
        )
        .await
        .expect_err("invalid window should fail");
        assert_eq!(invalid.status, StatusCode::BAD_REQUEST);

        let invalid = update_account_usage(
            State(state.clone()),
            AxumPath("acct-usage".into()),
            Json(AccountUsageUpdate {
                window: "window_5h".into(),
                percent: -0.1,
                resets_in_minutes: None,
            }),
        )
        .await
        .expect_err("invalid percent should fail");
        assert_eq!(invalid.status, StatusCode::BAD_REQUEST);

        for (window, minutes) in [
            ("window_5h", 301),
            ("window_week", 10_081),
            ("window_5h", i64::MAX),
        ] {
            let invalid = update_account_usage(
                State(state.clone()),
                AxumPath("acct-usage".into()),
                Json(AccountUsageUpdate {
                    window: window.into(),
                    percent: 50.0,
                    resets_in_minutes: Some(minutes),
                }),
            )
            .await
            .expect_err("reset outside the selected window should fail");
            assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
        }

        let missing = update_account_usage(
            State(state.clone()),
            AxumPath("missing".into()),
            Json(AccountUsageUpdate {
                window: "window_5h".into(),
                percent: 50.0,
                resets_in_minutes: None,
            }),
        )
        .await
        .expect_err("missing account should fail");
        assert_eq!(missing.status, StatusCode::NOT_FOUND);

        let usage = update_account_usage(
            State(state.clone()),
            AxumPath("acct-usage".into()),
            Json(AccountUsageUpdate {
                window: "window_5h".into(),
                percent: 50.04,
                resets_in_minutes: Some(180),
            }),
        )
        .await
        .expect("valid calibrate should save")
        .0;
        // GOAT 5h 限额 14.0，50% = 7.0
        assert!((usage.window_5h - 7.0).abs() < 1e-9);
        // 倒计时 ≈ 180min
        let reset = usage
            .resets_in_5h
            .expect("5h reset should be set after calibrate");
        let remaining_min = (reset - Utc::now()).num_minutes();
        assert!(
            (175..=185).contains(&remaining_min),
            "expected ~180min remaining, got {remaining_min}"
        );

        // Bug 2 修复：月窗口现在支持手动校准（之前会返回 BAD_REQUEST 拒绝）。
        // 月窗口的 resets_in_minutes 被忽略——窗口由 purchase_date/expires_on 决定。
        let usage = update_account_usage(
            State(state.clone()),
            AxumPath("acct-usage".into()),
            Json(AccountUsageUpdate {
                window: "window_month".into(),
                percent: 100.0,
                resets_in_minutes: None,
            }),
        )
        .await
        .expect("month window calibrate should save")
        .0;
        // GOAT 月限额 70.0，100% = 70.0
        assert!((usage.window_month - 70.0).abs() < 1e-9);
        // resets_in_month 仍是 purchase_date + 1 自然月（2026-01-31 → 2026-02-28）
        // UTC 日期可能比 Local 日期早一天（China UTC+8: 02-28 00:00 CST = 02-27 16:00 UTC），
        // 用 Local 比对避免时区 flake。
        let reset = usage
            .resets_in_month
            .expect("month reset should be set after calibrate");
        assert_eq!(
            reset
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string(),
            "2026-02-28"
        );
        state
            .db
            .lock()
            .set_account_auth_error("acct-usage", Some("upstream auth error 401"))
            .expect("auth error should save");
        let summary = dashboard_summary(State(state.clone()))
            .await
            .expect("summary should load")
            .0;
        assert_eq!(
            summary.available_accounts, 1,
            "Zen Free remains available without a credential"
        );
        state
            .db
            .lock()
            .set_account_auth_error("acct-usage", None)
            .expect("auth error should clear");
        let invalid_cipher = test_account("invalid-cipher-summary");
        state
            .db
            .lock()
            .create_account(&invalid_cipher)
            .expect("invalid ciphertext fixture should persist");
        let mut goat = test_account("goat-summary");
        goat.provider_id = crate::provider::COMMAND_CODE_PROVIDER_ID.to_string();
        goat.offering_id = crate::provider::GOAT_OFFERING_ID.to_string();
        goat.enabled = false;
        state
            .db
            .lock()
            .create_account(&goat)
            .expect("GOAT fixture should persist");
        let summary = dashboard_summary(State(state))
            .await
            .expect("summary should load")
            .0;
        assert_eq!(
            summary.available_accounts, 1,
            "invalid OpenCode ciphertext and both unconfigured GOAT fixtures must not count"
        );

        fs::remove_dir_all(dir).unwrap();
    }

    fn official_go_usage_test_state(label: &str) -> (PathBuf, crate::state::CoreState) {
        let dir = temp_data_dir(label);
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("official-go-usage"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        (dir, state)
    }

    fn official_go_usage_account(
        state: &crate::state::CoreState,
        id: &str,
        account_type: AccountType,
        setup_step: AccountSetupStep,
        plaintext_key: &str,
    ) -> Account {
        let mut account = test_account(id);
        account.account_type = account_type;
        account.setup_step = setup_step;
        account.enabled = setup_step.is_ready();
        account.key_cipher = if plaintext_key.is_empty() {
            String::new()
        } else {
            state.encrypt_key(plaintext_key).unwrap()
        };
        account
    }

    fn sample_official_go_usage_snapshot() -> GoUsageSnapshot {
        GoUsageSnapshot {
            rolling_status: GoUsageWindowStatus::RateLimited,
            weekly_status: GoUsageWindowStatus::Ok,
            monthly_status: GoUsageWindowStatus::Ok,
            rolling_percent: 50.0,
            weekly_percent: 20.0,
            monthly_percent: 10.0,
            rolling_resets_in_minutes: 180,
            weekly_resets_in_minutes: 1_440,
            earliest_resets_in_minutes: 180,
        }
    }

    fn assert_usage_windows_unchanged(before: &UsageWindow, after: &UsageWindow) {
        assert_eq!(after.window_5h, before.window_5h);
        assert_eq!(after.window_week, before.window_week);
        assert_eq!(after.window_month, before.window_month);
        assert_eq!(after.resets_in_5h, before.resets_in_5h);
        assert_eq!(after.resets_in_week, before.resets_in_week);
        assert_eq!(after.resets_in_month, before.resets_in_month);
    }

    fn assert_no_go_usage_cooldown(account: &Account) {
        assert!(account.cooldown_until.is_none());
        assert!(account.cooldown_generic_until.is_none());
        assert!(account.cooldown_5h_until.is_none());
        assert!(account.cooldown_week_until.is_none());
        assert!(account.cooldown_month_until.is_none());
        assert!(account.cooldown_free_until.is_none());
    }

    #[test]
    fn official_go_usage_refresh_accepts_ready_key_accounts() {
        let (dir, state) = official_go_usage_test_state("official-go-ready-key");
        let account = official_go_usage_account(
            &state,
            "ready-key",
            AccountType::Key,
            AccountSetupStep::Ready,
            "sk-ready-key",
        );
        state.db.lock().create_account(&account).unwrap();
        let loaded = {
            let db = state.db.lock();
            load_ready_account_for_official_go_usage(&db, "ready-key").unwrap()
        };
        assert_eq!(loaded, account.key_cipher);

        let limits = state.pricing_snapshot().limits.clone();
        let snapshot = sample_official_go_usage_snapshot();
        let usage = {
            let db = state.db.lock();
            apply_official_go_usage_snapshot(&db, "ready-key", &loaded, &snapshot, &limits).unwrap()
        };
        assert!((usage.window_5h - limits.window_5h * 0.5).abs() < 1e-9);
        assert!((usage.window_week - limits.window_week * 0.2).abs() < 1e-9);
        assert!((usage.window_month - limits.window_month * 0.1).abs() < 1e-9);
        let stored = state.db.lock().get_account("ready-key").unwrap().unwrap();
        assert_no_go_usage_cooldown(&stored);
        let json = serde_json::to_value(crate::usage_sync::OfficialUsageRefreshSuccess {
            usage,
            source: "official_go_usage",
            last_success_at: Utc::now().to_rfc3339(),
            next_allowed_at: (Utc::now() + chrono::Duration::seconds(60)).to_rfc3339(),
        })
        .unwrap();
        assert_eq!(json["source"], "official_go_usage");
        assert!(json.get("fetched_at").is_none());
        assert!(json.get("last_success_at").is_some());
        assert!(json.get("next_allowed_at").is_some());

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_go_usage_refresh_accepts_ready_managed_accounts() {
        let (dir, state) = official_go_usage_test_state("official-go-ready-managed");
        let account = official_go_usage_account(
            &state,
            "ready-managed",
            AccountType::Managed,
            AccountSetupStep::Ready,
            "sk-ready-managed",
        );
        state.db.lock().create_account(&account).unwrap();
        let loaded = {
            let db = state.db.lock();
            load_ready_account_for_official_go_usage(&db, "ready-managed").unwrap()
        };
        assert_eq!(loaded, account.key_cipher);

        let limits = state.pricing_snapshot().limits.clone();
        let snapshot = sample_official_go_usage_snapshot();
        let usage = {
            let db = state.db.lock();
            apply_official_go_usage_snapshot(&db, "ready-managed", &loaded, &snapshot, &limits)
                .unwrap()
        };
        assert!((usage.window_5h - limits.window_5h * 0.5).abs() < 1e-9);
        assert!((usage.window_week - limits.window_week * 0.2).abs() < 1e-9);
        assert!((usage.window_month - limits.window_month * 0.1).abs() < 1e-9);
        let stored = state
            .db
            .lock()
            .get_account("ready-managed")
            .unwrap()
            .unwrap();
        assert_no_go_usage_cooldown(&stored);

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_go_usage_refresh_rejects_non_ready_accounts() {
        let (dir, state) = official_go_usage_test_state("official-go-not-ready");
        let account = official_go_usage_account(
            &state,
            "pending",
            AccountType::Managed,
            AccountSetupStep::KeyVerification,
            "sk-pending",
        );
        state.db.lock().create_account(&account).unwrap();
        let error = {
            let db = state.db.lock();
            load_ready_account_for_official_go_usage(&db, "pending").unwrap_err()
        };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(
            !error.message.contains("sk-pending"),
            "plaintext key must not appear in the error"
        );

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_go_usage_refresh_rejects_empty_key_cipher() {
        let (dir, state) = official_go_usage_test_state("official-go-empty-key");
        let account = official_go_usage_account(
            &state,
            "empty-key",
            AccountType::Key,
            AccountSetupStep::Ready,
            "",
        );
        state.db.lock().create_account(&account).unwrap();
        let error = {
            let db = state.db.lock();
            load_ready_account_for_official_go_usage(&db, "empty-key").unwrap_err()
        };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_go_usage_refresh_returns_not_found_for_missing_accounts() {
        let (dir, state) = official_go_usage_test_state("official-go-missing");
        let error = {
            let db = state.db.lock();
            load_ready_account_for_official_go_usage(&db, "missing").unwrap_err()
        };
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        let apply_error = {
            let db = state.db.lock();
            apply_official_go_usage_snapshot(
                &db,
                "missing",
                "cipher-snapshot",
                &sample_official_go_usage_snapshot(),
                &state.pricing_snapshot().limits,
            )
            .unwrap_err()
        };
        assert_eq!(apply_error.status, StatusCode::NOT_FOUND);

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_go_usage_cas_leaves_all_three_windows_unchanged_when_key_changes() {
        let (dir, state) = official_go_usage_test_state("official-go-cas");
        let account = official_go_usage_account(
            &state,
            "cas-key",
            AccountType::Key,
            AccountSetupStep::Ready,
            "sk-original",
        );
        state.db.lock().create_account(&account).unwrap();
        let original_cipher = account.key_cipher.clone();
        let limits = state.pricing_snapshot().limits.clone();
        let before = {
            let db = state.db.lock();
            db.calibrate_account_usage_snapshot(
                "cas-key",
                &AccountUsageCalibrationSnapshot {
                    rolling_percent: 15.0,
                    weekly_percent: 25.0,
                    monthly_percent: 35.0,
                    rolling_resets_in_minutes: 90,
                    weekly_resets_in_minutes: 600,
                },
                &limits,
            )
            .unwrap()
        };

        let replacement = state.encrypt_key("sk-replaced").unwrap();
        state
            .db
            .lock()
            .update_account(
                "cas-key",
                &AccountUpdate::default(),
                Some(&replacement),
                None,
            )
            .unwrap();

        let error = {
            let db = state.db.lock();
            apply_official_go_usage_snapshot(
                &db,
                "cas-key",
                &original_cipher,
                &sample_official_go_usage_snapshot(),
                &limits,
            )
            .unwrap_err()
        };
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(!error.message.contains("sk-original"));
        assert!(!error.message.contains("sk-replaced"));

        let after = state
            .db
            .lock()
            .account_usage_with_limits("cas-key", &limits)
            .unwrap();
        assert_usage_windows_unchanged(&before, &after);
        assert!((after.window_5h - limits.window_5h * 0.15).abs() < 1e-9);
        assert!((after.window_week - limits.window_week * 0.25).abs() < 1e-9);
        assert!((after.window_month - limits.window_month * 0.35).abs() < 1e-9);

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn official_go_usage_errors_never_become_dashboard_unauthorized() {
        use crate::usage_sync::OfficialUsageRefreshError;
        let mapped = [
            (
                OfficialUsageRefreshError::Upstream(GoUsageError::Unauthorized),
                StatusCode::BAD_REQUEST,
            ),
            (
                OfficialUsageRefreshError::Upstream(GoUsageError::Forbidden),
                StatusCode::BAD_REQUEST,
            ),
            (
                OfficialUsageRefreshError::Upstream(GoUsageError::RateLimited),
                StatusCode::BAD_GATEWAY,
            ),
            (
                OfficialUsageRefreshError::Upstream(GoUsageError::Http(500)),
                StatusCode::BAD_GATEWAY,
            ),
            (
                OfficialUsageRefreshError::Upstream(GoUsageError::Timeout),
                StatusCode::BAD_GATEWAY,
            ),
            (
                OfficialUsageRefreshError::Upstream(GoUsageError::Network),
                StatusCode::BAD_GATEWAY,
            ),
            (
                OfficialUsageRefreshError::Upstream(GoUsageError::Oversize),
                StatusCode::BAD_GATEWAY,
            ),
            (
                OfficialUsageRefreshError::Upstream(GoUsageError::Schema),
                StatusCode::BAD_GATEWAY,
            ),
            (
                OfficialUsageRefreshError::Upstream(GoUsageError::Window),
                StatusCode::BAD_GATEWAY,
            ),
        ];
        for (error, expected) in mapped {
            let mapped = map_official_usage_refresh_error(error.clone());
            assert_eq!(mapped.status, expected, "{error}");
            assert_ne!(mapped.status, StatusCode::UNAUTHORIZED, "{error}");
            assert!(!mapped.message.contains("sk-"));
            assert!(!mapped.message.to_ascii_lowercase().contains("bearer"));
        }
        let unauthorized = map_official_usage_refresh_error(OfficialUsageRefreshError::Upstream(
            GoUsageError::Unauthorized,
        ));
        assert_eq!(unauthorized.status, StatusCode::BAD_REQUEST);
        assert!(!unauthorized.message.contains("401"));
    }

    #[tokio::test]
    async fn regular_settings_update_preserves_claude_desktop_models() {
        let dir = temp_data_dir("preserve-claude-desktop-models");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        let configured = ClaudeDesktopModels {
            sonnet: "glm-5.2".to_string(),
            opus: String::new(),
            haiku: "mimo-v2.5".to_string(),
        };
        let mut persisted = state.config();
        persisted.claude_desktop_models = configured.clone();
        state.set_config(persisted).unwrap();

        let _ = update_settings(
            State(state.clone()),
            Json(SettingsUpdateRequest {
                config: AppConfig {
                    gateway_key: "updated-gateway-key".to_string(),
                    connect_timeout_secs: 45,
                    ..AppConfig::default()
                },
                expected_revision: Some(state.settings_revision()),
            }),
        )
        .await
        .expect("regular settings should save");

        assert_eq!(state.config().claude_desktop_models, configured);
        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn list_proxy_settings_write_gate_validates_and_dedupes() {
        let dir = temp_data_dir("list-proxy-write-gate");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let empty_list = AppConfig {
            gateway_key: "k".to_string(),
            proxy_mode: ProxyMode::List,
            proxy_url: "http://127.0.0.1:7890".to_string(),
            ..AppConfig::default()
        };
        let error = update_settings(
            State(state.clone()),
            Json(SettingsUpdateRequest {
                config: empty_list,
                expected_revision: Some(state.settings_revision()),
            }),
        )
        .await
        .expect_err("empty list must be rejected");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        let unknown_id = AppConfig {
            gateway_key: "k".to_string(),
            proxy_mode: ProxyMode::List,
            proxy_url: "http://127.0.0.1:7890".to_string(),
            proxy_list_models: vec!["gpt-5.6-luna".to_string(), "wildcard-*".to_string()],
            ..AppConfig::default()
        };
        let error = update_settings(
            State(state.clone()),
            Json(SettingsUpdateRequest {
                config: unknown_id,
                expected_revision: Some(state.settings_revision()),
            }),
        )
        .await
        .expect_err("unknown ids must be rejected");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        // Missing URL under list mode fails the shared validate() gate.
        let missing_url = AppConfig {
            gateway_key: "k".to_string(),
            proxy_mode: ProxyMode::List,
            proxy_url: String::new(),
            proxy_list_models: vec!["gpt-5.6-luna".to_string()],
            ..AppConfig::default()
        };
        let error = update_settings(
            State(state.clone()),
            Json(SettingsUpdateRequest {
                config: missing_url,
                expected_revision: Some(state.settings_revision()),
            }),
        )
        .await
        .expect_err("list mode without a proxy URL must be rejected");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        // Previous config stays active after every rejection.
        assert_eq!(state.config().proxy_mode, ProxyMode::Auto);

        let revision_before = state.settings_revision();
        let duplicated = AppConfig {
            gateway_key: "k".to_string(),
            proxy_mode: ProxyMode::List,
            proxy_url: "http://127.0.0.1:7890".to_string(),
            proxy_list_direction: ProxyListDirection::Blacklist,
            proxy_list_models: vec![
                "  gpt-5.6-luna ".to_string(),
                "grok-4.5".to_string(),
                "gpt-5.6-luna".to_string(),
            ],
            ..AppConfig::default()
        };
        let saved = update_settings(
            State(state.clone()),
            Json(SettingsUpdateRequest {
                config: duplicated,
                expected_revision: Some(state.settings_revision()),
            }),
        )
        .await
        .expect("deduped known ids should save");
        assert!(saved.revision > revision_before);
        let persisted = state.config();
        assert_eq!(persisted.proxy_mode, ProxyMode::List);
        assert_eq!(
            persisted.proxy_list_direction,
            ProxyListDirection::Blacklist
        );
        assert_eq!(
            persisted.proxy_list_models,
            vec!["gpt-5.6-luna".to_string(), "grok-4.5".to_string()]
        );

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_response_lists_supported_models_with_protocol_hints() {
        let models = supported_model_protocols()
            .map(|(id, preferred)| (id, api_format_name(preferred)))
            .collect::<Vec<_>>();
        assert!(!models.is_empty());
        assert!(models.iter().any(|(id, _)| *id == "gpt-5.6-luna"));
        assert!(models.iter().all(|(_, protocol)| {
            matches!(
                *protocol,
                "chat_completions" | "responses" | "messages" | "gemini"
            )
        }));
        let unique: std::collections::HashSet<&str> = models.iter().map(|(id, _)| *id).collect();
        assert_eq!(unique.len(), models.len(), "ids must be unique");
    }

    #[tokio::test]
    async fn settings_get_response_includes_proxy_supported_models() {
        let dir = temp_data_dir("settings-get-shape");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
        state
            .activate_zen_free_model_catalog(crate::kernel::zen::ZenFreeModelCatalog {
                models: vec![
                    "mimo-v2.5-free".to_string(),
                    "brand-new-promo-free".to_string(),
                ],
                refreshed_at: Some(Utc::now()),
                source_url: crate::kernel::zen::ZEN_MODELS_SOURCE_URL.to_string(),
            })
            .unwrap();

        let Json(response) = get_settings(State(state.clone())).await;
        let encoded = serde_json::to_value(&response).unwrap();
        let models = encoded["proxy_supported_models"]
            .as_array()
            .expect("proxy_supported_models must serialize as an array");
        assert!(!models.is_empty());
        assert!(models.iter().all(|model| {
            model["id"].is_string()
                && model["preferred_protocol"].is_string()
                && model["zen_free"].as_bool().is_some()
        }));
        assert!(models.iter().any(|model| model["id"] == "gpt-5.6-luna"));
        // The free-channel hint follows the active Zen catalog. Go's
        // ox-alpha-free remains a Go model despite its suffix.
        assert!(
            models
                .iter()
                .any(|model| model["id"] == "mimo-v2.5-free" && model["zen_free"] == true)
        );
        assert!(
            models
                .iter()
                .any(|model| model["id"] == "ox-alpha-free" && model["zen_free"] == false)
        );
        assert!(
            models
                .iter()
                .any(|model| model["id"] == "brand-new-promo-free"
                    && model["preferred_protocol"] == "chat_completions"
                    && model["zen_free"] == true)
        );
        assert!(!models.iter().any(|model| model["id"] == "big-pickle"));
        // Flattened config fields stay at the top level next to the extras.
        assert!(encoded["proxy_mode"].is_string());
        assert!(encoded["proxy_list_direction"].is_string());
        assert!(encoded["proxy_list_models"].is_array());

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn forward_logs_handler_exposes_route_labels() {
        let dir = temp_data_dir("logs-route-api");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let mut labeled = forward_log_for("a1");
        labeled.model = "gpt-5.6-luna".into();
        labeled.route = "proxy".into();
        labeled.status = "success".into();
        let mut historical = forward_log_for("a1");
        historical.model = "glm-5.3".into();
        // Row written before v22: the route column keeps its empty default.
        {
            let db = state.db.lock();
            db.log_forward(&labeled).unwrap();
            db.log_forward(&historical).unwrap();
        }

        let Json(page) = forward_logs(
            State(state.clone()),
            Query(ForwardLogQuery {
                limit: Some(10),
                offset: None,
                status: None,
                account_id: None,
                provider_id: None,
                offering_id: None,
                route_account_id: None,
                credential_account_id: None,
                model: None,
                request_id: None,
                key_id: None,
                start_time: None,
                end_time: None,
                sort_by: None,
                sort_order: None,
            }),
        )
        .await
        .expect("logs page should load");
        let encoded = serde_json::to_value(&page).unwrap();
        let items = encoded["items"].as_array().unwrap();
        let luna = items
            .iter()
            .find(|row| row["model"] == "gpt-5.6-luna")
            .unwrap();
        assert_eq!(luna["route"], "proxy");
        let historical = items.iter().find(|row| row["model"] == "glm-5.3").unwrap();
        assert_eq!(historical["route"], "");

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    fn forward_log_for(account_id: &str) -> ForwardLog {
        ForwardLog {
            id: 0,
            timestamp: Utc::now(),
            model: "test".into(),
            account_id: account_id.into(),
            account_name: account_id.into(),
            route_account_id: None,
            provider_id: None,
            offering_id: None,
            credential_account_id: None,
            client_key_id: None,
            client_key_name: None,
            status: "success".into(),
            http_status: Some(200),
            route: String::new(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: Some(0.0),
            raw_cost_usd: None,
            quota_debit: None,
            effective_paid_cost_usd: None,
            pricing_revision_id: None,
            quota_multiplier: None,
            local_adjustment_multiplier: None,
            service_tier: None,
            cost_state: "legacy_estimate".into(),
            error_message: None,
            request_id: None,
            attempt: None,
            error_source: None,
            error_stage: None,
            duration_ms: None,
            diagnostic: None,
        }
    }

    #[tokio::test]
    async fn proxy_test_list_mode_follows_direction_default_leg() {
        let dir = temp_data_dir("proxy-test-list");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );

        // Whitelist default leg connects directly to the reachable upstream.
        let (address, upstream) =
            spawn_key_verification_upstream(StatusCode::OK, "direct leg").await;
        let direct = test_proxy(
            State(state.clone()),
            Json(ProxyTestRequest {
                proxy_mode: ProxyMode::List,
                proxy_url: "http://127.0.0.1:7890".to_string(),
                proxy_list_direction: Some(ProxyListDirection::Whitelist),
                upstream_base_url: format!("http://{address}"),
            }),
        )
        .await
        .expect("whitelist default leg must connect directly");
        assert_eq!(direct.status, StatusCode::OK.as_u16());
        upstream.await.unwrap();

        // Blacklist default leg routes through the (dead) proxy URL instead of
        // the reachable upstream.
        let closed_proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed_proxy_address = closed_proxy.local_addr().unwrap();
        drop(closed_proxy);
        let (address, upstream) =
            spawn_key_verification_upstream(StatusCode::OK, "must stay direct-free").await;
        let error = test_proxy(
            State(state.clone()),
            Json(ProxyTestRequest {
                proxy_mode: ProxyMode::List,
                proxy_url: format!("http://{closed_proxy_address}"),
                proxy_list_direction: Some(ProxyListDirection::Blacklist),
                upstream_base_url: format!("http://{address}"),
            }),
        )
        .await
        .expect_err("blacklist default leg must route through the proxy URL");
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        upstream.abort();

        // Missing direction falls back to the direction persisted in config.
        let mut persisted = state.config();
        persisted.proxy_list_direction = ProxyListDirection::Blacklist;
        persisted.proxy_url = format!("http://{closed_proxy_address}");
        state.set_config(persisted).unwrap();
        let (address, upstream) =
            spawn_key_verification_upstream(StatusCode::OK, "default direction").await;
        let error = test_proxy(
            State(state.clone()),
            Json(ProxyTestRequest {
                proxy_mode: ProxyMode::List,
                proxy_url: format!("http://{closed_proxy_address}"),
                proxy_list_direction: None,
                upstream_base_url: format!("http://{address}"),
            }),
        )
        .await
        .expect_err("omitted direction must adopt the persisted blacklist");
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        upstream.abort();

        // Empty URL is a 400 regardless of direction (list is manual-like).
        let error = test_proxy(
            State(state.clone()),
            Json(ProxyTestRequest {
                proxy_mode: ProxyMode::List,
                proxy_url: String::new(),
                proxy_list_direction: Some(ProxyListDirection::Whitelist),
                upstream_base_url: "https://opencode.ai/zen/go".to_string(),
            }),
        )
        .await
        .expect_err("empty URL must be rejected before any connection");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn dock_visibility_setting_is_runtime_gated_and_applied() {
        let dir = temp_data_dir("dock-visibility");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let mut unsupported = state.config();
        unsupported.show_dock_icon = false;
        let error = match update_settings(
            State(state.clone()),
            Json(SettingsUpdateRequest {
                config: unsupported,
                expected_revision: Some(state.settings_revision()),
            }),
        )
        .await
        {
            Err(error) => error,
            Ok(_) => panic!("non-desktop runtimes must reject Dock changes"),
        };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(state.config().show_dock_icon);

        let applied = Arc::new(StdMutex::new(Vec::new()));
        let captured = applied.clone();
        state.set_dock_visibility_sync(Arc::new(move |visible| {
            captured.lock().unwrap().push(visible);
            Ok(())
        }));
        let mut supported = state.config();
        supported.show_dock_icon = false;
        let _ = update_settings(
            State(state.clone()),
            Json(SettingsUpdateRequest {
                config: supported,
                expected_revision: Some(state.settings_revision()),
            }),
        )
        .await
        .expect("desktop runtime should apply Dock changes");
        assert!(!state.config().show_dock_icon);
        assert_eq!(*applied.lock().unwrap(), [false]);

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    fn parsed_version(version: &str) -> SemverVersion<'_> {
        parse_semver_version(version)
            .expect("test version should be valid")
            .0
    }

    #[test]
    fn error_chain_includes_transport_root_cause() {
        let error = anyhow::Error::msg("root cause").context("outer error");
        assert_eq!(
            format_error_chain(error.as_ref()),
            "outer error: root cause"
        );
    }

    #[test]
    fn update_availability_covers_stable_and_prerelease_comparisons() {
        for (current, latest, available) in [
            ("1.0.0", "1.1.0", true),
            ("1.1.0", "1.1.0", false),
            ("2.0.0", "1.9.9", false),
            ("1.5.8-beta.1", "1.5.7", false),
            ("1.5.8-beta.1", "1.5.8-beta.2", true),
            ("1.5.8-beta.1", "1.5.8", true),
            ("1.5.8-beta.2.9", "1.5.8-beta.11", true),
        ] {
            assert_eq!(
                is_update_available(&parsed_version(current), &parsed_version(latest)),
                available,
                "{current} vs {latest}"
            );
        }
    }

    #[test]
    fn semver_version_parser_accepts_valid_forms_and_rejects_malformed() {
        for (version, expected) in [
            ("v1.2.3", Some("1.2.3")),
            ("v1.2.3+build.1", Some("1.2.3+build.1")),
        ] {
            assert_eq!(
                parse_semver_version(version).map(|(_, value)| value),
                expected,
                "{version} should parse"
            );
        }
        for version in ["v1.1", "v01.1.0", "v1.1.0-beta.01", "v1.1.0+build..1"] {
            assert!(
                parse_semver_version(version).is_none(),
                "{version} should be rejected"
            );
        }
    }

    #[tokio::test]
    async fn provider_catalog_is_the_plan_source_and_keeps_unverified_plans_unroutable() {
        let dir = temp_data_dir("provider-catalog-source");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir, cipher).unwrap());
        let catalog = super::provider_catalog(State(state)).await.0;
        assert_eq!(catalog.len(), 7);
        let go = catalog
            .iter()
            .find(|entry| entry.provider_id == crate::provider::OPENCODE_PROVIDER_ID)
            .unwrap();
        assert!(go.routable);
        assert_eq!(go.pricing_availability, "available");
        assert_eq!(
            go.model_aliases,
            crate::alias::routeable_aliases_for(
                crate::provider::OPENCODE_PROVIDER_ID,
                crate::provider::GO_OFFERING_ID
            )
        );
        assert!(go.model_aliases.contains(&"glm-5.2".to_string()));
        assert!(!go.model_aliases.iter().any(|alias| alias.contains('/')));
        assert!(
            !go.model_aliases
                .iter()
                .any(|alias| alias == "deepseek-v4-flash-free")
        );
        assert!(!go.model_aliases.iter().any(|alias| {
            alias == crate::provider::COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM
        }));

        let zen = catalog
            .iter()
            .find(|entry| {
                entry.provider_id == crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID
                    && entry.offering_id == crate::provider::ANONYMOUS_FREE_OFFERING_ID
            })
            .unwrap();
        assert!(zen.routable);
        assert_eq!(
            zen.model_aliases,
            crate::alias::routeable_aliases_for(
                crate::provider::OPENCODE_ZEN_FREE_PROVIDER_ID,
                crate::provider::ANONYMOUS_FREE_OFFERING_ID
            )
        );
        assert!(zen.model_aliases.contains(&"mimo-v2.5-free".to_string()));
        assert!(
            zen.model_aliases
                .contains(&"deepseek-v4-flash-free".to_string())
        );
        assert!(zen.model_aliases.contains(&"deepseek-v4-flash".to_string()));
        assert!(!zen.model_aliases.iter().any(|alias| alias.contains('/')));

        let goat = catalog
            .iter()
            .find(|entry| {
                entry.provider_id == crate::provider::COMMAND_CODE_PROVIDER_ID
                    && entry.offering_id == crate::provider::GOAT_OFFERING_ID
            })
            .unwrap();
        assert!(!goat.routable);
        assert!(goat.model_aliases.is_empty());
        assert_eq!(goat.pricing_availability, "unavailable");
        assert_eq!(goat.usage_availability, "unavailable");
        assert_eq!(
            goat.verification_policy,
            crate::provider::VerificationPolicy::Required
        );

        let scnet = catalog
            .iter()
            .find(|entry| {
                entry.provider_id == crate::provider::SCNET_PROVIDER_ID
                    && entry.offering_id == crate::provider::SCNET_TOKEN_PLAN_BASIC_OFFERING_ID
            })
            .unwrap();
        assert!(!scnet.routable);
        assert!(scnet.model_aliases.is_empty());
        assert!(scnet.risk_notice.is_some());
        assert_eq!(
            scnet.key_prefix,
            Some(crate::provider::SCNET_TOKEN_PLAN_KEY_PREFIX)
        );

        for offering_id in crate::provider::SCNET_TOKEN_PLAN_OFFERING_IDS {
            let entry = catalog
                .iter()
                .find(|entry| {
                    entry.provider_id == crate::provider::SCNET_PROVIDER_ID
                        && entry.offering_id == offering_id
                })
                .unwrap();
            assert!(entry.model_aliases.is_empty());
        }
        let custom = catalog
            .iter()
            .find(|entry| {
                entry.provider_id == crate::provider::CUSTOM_PROVIDER_ID
                    && entry.offering_id == crate::provider::CUSTOM_API_OFFERING_ID
            })
            .unwrap();
        assert!(custom.routable);
        assert_eq!(custom.verification_runtime_availability, "available");
        assert!(custom.model_aliases.is_empty());
    }

    #[tokio::test]
    async fn application_models_are_local_priced_go_aliases() {
        let dir = temp_data_dir("application-models-local");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let default_list = application_models(State(state.clone())).await.0;
        let expected_default = local_application_models(&state.pricing_snapshot());
        assert_eq!(default_list, expected_default);
        assert!(
            !expected_default.is_empty(),
            "seed Go pricing should intersect published Go aliases"
        );
        assert!(expected_default.contains(&"deepseek-v4-flash".to_string()));
        assert!(expected_default.contains(&"minimax-m2.7-highspeed".to_string()));
        assert!(!expected_default.iter().any(|id| id.contains('/')));
        assert!(!expected_default.iter().any(|id| id.ends_with("-free")));
        assert_eq!(
            expected_default.iter().find(|id| *id == "glm-5"),
            None,
            "unpriced Go aliases must stay out of Applications"
        );
        let mut sorted = expected_default.clone();
        sorted.sort();
        assert_eq!(
            expected_default, sorted,
            "application-models must keep registry order"
        );

        let mut pricing = state.pricing_snapshot().as_ref().clone();
        let mut raw_row = pricing
            .models
            .iter()
            .find(|model| model.model_id == "grok-4.5")
            .cloned()
            .expect("seed snapshot includes grok-4.5");
        raw_row.model_id = "vendor-raw-not-an-alias".into();
        pricing
            .models
            .retain(|model| matches!(model.model_id.as_str(), "grok-4.5" | "minimax-m2.7"));
        pricing.models.push(raw_row);
        pricing.revision = format!("test-app-models-{}", Utc::now().timestamp_micros());
        pricing.activated_at = Utc::now().to_rfc3339();
        state.activate_pricing_snapshot(pricing).unwrap();
        assert_eq!(
            local_application_models(&state.pricing_snapshot()),
            vec![
                "grok-4.5".to_string(),
                "minimax-m2.7".to_string(),
                "minimax-m2.7-highspeed".to_string()
            ]
        );

        let mut empty = state.pricing_snapshot().as_ref().clone();
        empty.models.clear();
        empty.revision = format!("test-app-models-empty-{}", Utc::now().timestamp_micros());
        empty.activated_at = Utc::now().to_rfc3339();
        state.activate_pricing_snapshot(empty).unwrap();
        assert!(local_application_models(&state.pricing_snapshot()).is_empty());
        assert_eq!(
            application_models(State(state.clone())).await.0,
            Vec::<String>::new()
        );

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn create_account_saves_verification_required_plans_as_disabled_drafts() {
        let dir = temp_data_dir("v23-create-drafts");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let go = create_account(
            State(state.clone()),
            Json(AccountInput {
                provider_id: crate::provider::OPENCODE_PROVIDER_ID.into(),
                offering_id: crate::provider::GO_OFFERING_ID.into(),
                name: "Go".into(),
                username: None,
                password: None,
                key: "sk-go".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            }),
        )
        .await
        .expect("Go import should stay immediately usable")
        .0;
        assert!(go.enabled);
        assert_eq!(
            go.verification_status,
            crate::provider::ConnectionVerificationStatus::NotRequired
        );
        assert!(go.plan_routable);

        let goat = create_account(
            State(state.clone()),
            Json(AccountInput {
                provider_id: crate::provider::COMMAND_CODE_PROVIDER_ID.into(),
                offering_id: crate::provider::GOAT_OFFERING_ID.into(),
                name: "GOAT".into(),
                username: None,
                password: None,
                key: "goat-key".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            }),
        )
        .await
        .expect("GOAT should persist as a draft")
        .0;
        assert!(!goat.enabled);
        assert_eq!(
            goat.verification_status,
            crate::provider::ConnectionVerificationStatus::Pending
        );
        assert!(!goat.plan_routable);

        let scnet_err = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::SCNET_PROVIDER_ID.into(),
                offering_id: crate::provider::SCNET_TOKEN_PLAN_BASIC_OFFERING_ID.into(),
                name: "SCNet".into(),
                username: None,
                password: None,
                key: "sk-tp-basic".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect_err("SCNet create requires acknowledgement");
        assert_eq!(scnet_err.status, StatusCode::BAD_REQUEST);

        let notice = crate::provider::builtin_plan(
            crate::provider::SCNET_PROVIDER_ID,
            crate::provider::SCNET_TOKEN_PLAN_BASIC_OFFERING_ID,
        )
        .unwrap()
        .risk_notice
        .unwrap();
        let scnet = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::SCNET_PROVIDER_ID.into(),
                offering_id: crate::provider::SCNET_TOKEN_PLAN_BASIC_OFFERING_ID.into(),
                name: "SCNet".into(),
                username: None,
                password: None,
                key: "sk-tp-basic".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            None,
            vec![AccountAcknowledgementInput {
                acknowledgement_id: notice.acknowledgement_id.to_string(),
                version: notice.version.to_string(),
            }],
            Vec::new(),
        )
        .expect("acknowledged SCNet draft should save")
        .0;
        assert!(!scnet.enabled);
        assert_eq!(scnet.acknowledgements.len(), 1);

        let custom_err = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::CUSTOM_PROVIDER_ID.into(),
                offering_id: crate::provider::CUSTOM_API_OFFERING_ID.into(),
                name: "Custom".into(),
                username: None,
                password: None,
                key: "custom-key".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            None,
            Vec::new(),
            Vec::new(),
        )
        .expect_err("Custom create requires config");
        assert_eq!(custom_err.status, StatusCode::BAD_REQUEST);

        let custom_caps_err = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::CUSTOM_PROVIDER_ID.into(),
                offering_id: crate::provider::CUSTOM_API_OFFERING_ID.into(),
                name: "Custom".into(),
                username: None,
                password: None,
                key: "custom-key".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            Some(AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: crate::provider::UpstreamProtocolKind::Messages,
                auth_scheme: crate::provider::UpstreamAuthScheme::XApiKey,
            }),
            Vec::new(),
            Vec::new(),
        )
        .expect_err("Custom create requires model capabilities");
        assert_eq!(custom_caps_err.status, StatusCode::BAD_REQUEST);

        let go_custom_err = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::OPENCODE_PROVIDER_ID.into(),
                offering_id: crate::provider::GO_OFFERING_ID.into(),
                name: "Go custom".into(),
                username: None,
                password: None,
                key: "sk-go-custom".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            Some(AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
                auth_scheme: crate::provider::UpstreamAuthScheme::Bearer,
            }),
            Vec::new(),
            Vec::new(),
        )
        .expect_err("Go accounts must reject custom_config");
        assert_eq!(go_custom_err.status, StatusCode::BAD_REQUEST);

        let go_caps_err = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::OPENCODE_PROVIDER_ID.into(),
                offering_id: crate::provider::GO_OFFERING_ID.into(),
                name: "Go caps".into(),
                username: None,
                password: None,
                key: "sk-go-caps".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            None,
            Vec::new(),
            vec![AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .expect_err("Go accounts must reject capabilities");
        assert_eq!(go_caps_err.status, StatusCode::BAD_REQUEST);
        assert!(
            state
                .db
                .lock()
                .list_accounts()
                .unwrap()
                .iter()
                .all(|account| account.name != "Go custom" && account.name != "Go caps")
        );

        let invalid_url_err = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::CUSTOM_PROVIDER_ID.into(),
                offering_id: crate::provider::CUSTOM_API_OFFERING_ID.into(),
                name: "Custom orphan".into(),
                username: None,
                password: None,
                key: "custom-key".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            Some(AccountCustomConfigInput {
                base_url: "https://user:pass@api.example.com/v1".into(),
                upstream_protocol: crate::provider::UpstreamProtocolKind::Messages,
                auth_scheme: crate::provider::UpstreamAuthScheme::XApiKey,
            }),
            Vec::new(),
            vec![AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: crate::provider::UpstreamProtocolKind::Messages,
                source: None,
            }],
        )
        .expect_err("invalid Custom URL should fail closed");
        assert_eq!(invalid_url_err.status, StatusCode::BAD_REQUEST);
        assert!(
            state
                .db
                .lock()
                .list_accounts()
                .unwrap()
                .iter()
                .all(|account| account.name != "Custom orphan"),
            "failed Custom create must not leave an account row"
        );

        let custom = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::CUSTOM_PROVIDER_ID.into(),
                offering_id: crate::provider::CUSTOM_API_OFFERING_ID.into(),
                name: "Custom".into(),
                username: None,
                password: None,
                key: "custom-key".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            Some(AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: crate::provider::UpstreamProtocolKind::Messages,
                auth_scheme: crate::provider::UpstreamAuthScheme::XApiKey,
            }),
            Vec::new(),
            vec![AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: crate::provider::UpstreamProtocolKind::Messages,
                source: None,
            }],
        )
        .expect("Custom draft should save")
        .0;
        assert!(!custom.enabled);
        assert_eq!(
            custom.custom_config.as_ref().unwrap().base_url,
            "https://api.example.com/v1"
        );
        assert_eq!(custom.model_capabilities[0].model_id, "org/model");

        assert!(custom.plan_routable);
        assert_eq!(
            crate::provider::builtin_plan(
                crate::provider::CUSTOM_PROVIDER_ID,
                crate::provider::CUSTOM_API_OFFERING_ID
            )
            .unwrap()
            .verification_runtime_availability,
            "available"
        );

        for (account_id, experimental) in [
            (goat.id.as_str(), true),
            (scnet.id.as_str(), false),
            (custom.id.as_str(), false),
        ] {
            let usage = provider_account_usage(State(state.clone()), AxumPath(account_id.into()))
                .await
                .expect("unavailable catalog usage must not return a generic 400")
                .0;
            assert_eq!(usage.availability, "unavailable");
            assert_eq!(usage.experimental, experimental);
            assert!(usage.quota_windows.is_empty());
        }

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn custom_config_and_capability_mutations_repend_disable_and_reject_protocol_mismatch() {
        let dir = temp_data_dir("v23-custom-lifecycle");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let mismatch = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::CUSTOM_PROVIDER_ID.into(),
                offering_id: crate::provider::CUSTOM_API_OFFERING_ID.into(),
                name: "Custom mismatch".into(),
                username: None,
                password: None,
                key: "custom-key".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            Some(AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: crate::provider::UpstreamProtocolKind::Messages,
                auth_scheme: crate::provider::UpstreamAuthScheme::Bearer,
            }),
            Vec::new(),
            vec![AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .expect_err("capability protocol must match custom_config.upstream_protocol");
        assert_eq!(mismatch.status, StatusCode::BAD_REQUEST);
        assert!(
            state
                .db
                .lock()
                .list_accounts()
                .unwrap()
                .iter()
                .all(|account| account.name != "Custom mismatch")
        );

        let custom = create_account_inner(
            state.clone(),
            AccountInput {
                provider_id: crate::provider::CUSTOM_PROVIDER_ID.into(),
                offering_id: crate::provider::CUSTOM_API_OFFERING_ID.into(),
                name: "Custom".into(),
                username: None,
                password: None,
                key: "custom-key".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            Some(AccountCustomConfigInput {
                base_url: "https://api.example.com/v1".into(),
                upstream_protocol: crate::provider::UpstreamProtocolKind::Messages,
                auth_scheme: crate::provider::UpstreamAuthScheme::XApiKey,
            }),
            Vec::new(),
            vec![AccountModelCapabilityInput {
                model_id: "org/model".into(),
                protocol: crate::provider::UpstreamProtocolKind::Messages,
                source: None,
            }],
        )
        .expect("Custom draft should save")
        .0;
        state
            .db
            .lock()
            .set_account_verification(
                &custom.id,
                crate::provider::ConnectionVerificationStatus::Verified,
                Some(Utc::now()),
                Some("previous"),
            )
            .unwrap();

        let updated = put_account_custom_config(
            State(state.clone()),
            AxumPath(custom.id.clone()),
            Json(DashboardCustomConfigUpdate {
                config: AccountCustomConfigInput {
                    base_url: "https://api.example.net/v2".into(),
                    upstream_protocol: crate::provider::UpstreamProtocolKind::Messages,
                    auth_scheme: crate::provider::UpstreamAuthScheme::XApiKey,
                },
                expected_revision: Some(custom.revision),
            }),
        )
        .await
        .expect("base URL change should persist")
        .0;
        assert!(!updated.enabled);
        assert_eq!(
            updated.verification_status,
            crate::provider::ConnectionVerificationStatus::Pending
        );
        assert!(updated.connection_verified_at.is_none());
        assert!(updated.verification_error.is_none());
        assert_eq!(
            updated.custom_config.as_ref().unwrap().base_url,
            "https://api.example.net/v2"
        );

        let protocol_change = put_account_custom_config(
            State(state.clone()),
            AxumPath(custom.id.clone()),
            Json(DashboardCustomConfigUpdate {
                config: AccountCustomConfigInput {
                    base_url: "https://api.example.net/v2".into(),
                    upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
                    auth_scheme: crate::provider::UpstreamAuthScheme::XApiKey,
                },
                expected_revision: Some(updated.revision),
            }),
        )
        .await
        .expect_err("protocol must stay immutable");
        assert_eq!(protocol_change.status, StatusCode::BAD_REQUEST);

        let cap_mismatch = put_account_model_capabilities(
            State(state.clone()),
            AxumPath(custom.id.clone()),
            Json(DashboardModelCapabilitiesUpdate {
                capabilities: vec![AccountModelCapabilityInput {
                    model_id: "org/other".into(),
                    protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
                    source: None,
                }],
                expected_revision: Some(updated.revision),
            }),
        )
        .await
        .expect_err("capability protocol must match config");
        assert_eq!(cap_mismatch.status, StatusCode::BAD_REQUEST);

        state
            .db
            .lock()
            .set_account_verification(
                &custom.id,
                crate::provider::ConnectionVerificationStatus::Verified,
                Some(Utc::now()),
                None,
            )
            .unwrap();
        let caps = put_account_model_capabilities(
            State(state.clone()),
            AxumPath(custom.id.clone()),
            Json(DashboardModelCapabilitiesUpdate {
                capabilities: vec![AccountModelCapabilityInput {
                    model_id: "org/other".into(),
                    protocol: crate::provider::UpstreamProtocolKind::Messages,
                    source: None,
                }],
                expected_revision: Some(updated.revision),
            }),
        )
        .await
        .expect("matching capability protocol should persist")
        .0;
        assert!(!caps.enabled);
        assert_eq!(
            caps.verification_status,
            crate::provider::ConnectionVerificationStatus::Pending
        );
        assert_eq!(caps.model_capabilities[0].model_id, "org/other");

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn unroutable_plans_reject_enablement_without_mutating_revision() {
        let dir = temp_data_dir("enablement-gate");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let db = Database::open(dir.clone()).unwrap();
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let go = create_account(
            State(state.clone()),
            Json(AccountInput {
                provider_id: crate::provider::OPENCODE_PROVIDER_ID.into(),
                offering_id: crate::provider::GO_OFFERING_ID.into(),
                name: "Go".into(),
                username: None,
                password: None,
                key: "sk-go".into(),
                referral_code: None,
                purchase_date: None,
                notes: None,
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(go.enabled);
        let go_revision = state.settings_revision();
        let disabled_go = toggle_account(State(state.clone()), AxumPath(go.id.clone()), None)
            .await
            .unwrap()
            .0;
        assert!(!disabled_go.enabled);
        assert_ne!(state.settings_revision(), go_revision);
        let restored = toggle_account(State(state.clone()), AxumPath(go.id.clone()), None)
            .await
            .unwrap()
            .0;
        assert!(restored.enabled);

        for plan in crate::provider::BUILTIN_PLANS
            .iter()
            .copied()
            .filter(|plan| !plan.routable && plan.offering.singleton_account_id.is_none())
        {
            let custom_config = crate::provider::plan_requires_custom_config(plan).then_some(
                AccountCustomConfigInput {
                    base_url: "https://api.example.com/v1".into(),
                    upstream_protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
                    auth_scheme: crate::provider::UpstreamAuthScheme::Bearer,
                },
            );
            let capabilities = if crate::provider::plan_requires_custom_config(plan) {
                vec![AccountModelCapabilityInput {
                    model_id: "org/model".into(),
                    protocol: crate::provider::UpstreamProtocolKind::ChatCompletions,
                    source: None,
                }]
            } else {
                Vec::new()
            };
            let acknowledgements = plan
                .risk_notice
                .map(|notice| {
                    vec![AccountAcknowledgementInput {
                        acknowledgement_id: notice.acknowledgement_id.to_string(),
                        version: notice.version.to_string(),
                    }]
                })
                .unwrap_or_default();
            let draft = create_account_inner(
                state.clone(),
                AccountInput {
                    provider_id: plan.offering.provider_id.into(),
                    offering_id: plan.offering.offering_id.into(),
                    name: format!("{} draft", plan.display_name),
                    username: None,
                    password: None,
                    key: if plan.key_prefix == Some(crate::provider::SCNET_TOKEN_PLAN_KEY_PREFIX) {
                        "sk-tp-enablement".into()
                    } else {
                        "draft-key".into()
                    },
                    referral_code: None,
                    purchase_date: None,
                    notes: None,
                },
                custom_config,
                acknowledgements,
                capabilities,
            )
            .unwrap()
            .0;
            assert!(!draft.enabled, "{} must save disabled", plan.display_name);
            let stored_before = state.db.lock().get_account(&draft.id).unwrap().unwrap();
            let revision_before = state.settings_revision();

            let toggle_err = toggle_account(State(state.clone()), AxumPath(draft.id.clone()), None)
                .await
                .expect_err("toggle enable must fail closed");
            assert_eq!(toggle_err.status, StatusCode::CONFLICT);
            assert!(toggle_err.message.contains("not routable"));

            let update_err = update_account(
                State(state.clone()),
                AxumPath(draft.id.clone()),
                Json(DashboardAccountUpdate::from(AccountUpdate {
                    enabled: Some(true),
                    ..AccountUpdate::default()
                })),
            )
            .await
            .expect_err("patch enable must fail closed");
            assert_eq!(update_err.status, StatusCode::CONFLICT);
            assert!(update_err.message.contains("not routable"));

            assert_eq!(state.settings_revision(), revision_before);
            let stored_after = state.db.lock().get_account(&draft.id).unwrap().unwrap();
            assert!(!stored_after.enabled);
            assert_eq!(stored_after.updated_at, stored_before.updated_at);
            assert_eq!(stored_after.name, stored_before.name);

            let renamed = update_account(
                State(state.clone()),
                AxumPath(draft.id.clone()),
                Json(DashboardAccountUpdate::from(AccountUpdate {
                    name: Some(format!("{} edited", plan.display_name)),
                    enabled: Some(false),
                    ..AccountUpdate::default()
                })),
            )
            .await
            .unwrap()
            .0;
            assert!(!renamed.enabled);
            assert_eq!(renamed.name, format!("{} edited", plan.display_name));
            assert_ne!(state.settings_revision(), revision_before);
        }

        drop(state);
        fs::remove_dir_all(dir).unwrap();
    }
}
