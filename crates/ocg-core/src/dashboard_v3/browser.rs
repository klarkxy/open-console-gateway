//! Authenticated V3 browser runtime: capabilities, native/remote open, profile
//! reset, and the dashboard-bound remote display WebSocket.
//!
//! V2 `/dashboard/api` browser routes stay mounted and unchanged. This module
//! copies state-neutral helpers locally (Origin validation, session binding,
//! WS proxy) instead of importing `dashboard`. Account JSON uses the shared
//! accounts DTO mapper. DTOs never carry worker URLs or control tokens.
//! Mutations serialize on `BrowserRuntime::operation`, check CAS before side
//! effects, recheck after await, and do not hold settings/DB locks across await.

use axum::Json;
use axum::body::Bytes;
use axum::extract::ws::{Message as AxumWsMessage, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::{HeaderMap, header, uri::Authority};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use std::str::FromStr;

use crate::browser::{BrowserProfileOperationKind, StagedBrowserProfiles};
use crate::models::{Account as ModelAccount, AccountType as ModelAccountType};
use crate::state::CoreState;

use super::accounts::mutation_at;
use super::types::{
    AccountMutation, BrowserCapabilities, BrowserMode, BrowserOpen, BrowserOpenRequest,
    BrowserTarget, MutationExpectation,
};
use super::{
    V3ApiError, check_expectation, dashboard_session_value, has_dashboard_session,
    is_local_dashboard_request, parse_mutation_json,
};

#[cfg(debug_assertions)]
mod browser_profile_purge {
    use super::*;
    use parking_lot::Mutex;
    use std::collections::HashMap;
    use std::sync::OnceLock;

    static PURGE_ERROR_OVERRIDES: OnceLock<Mutex<HashMap<u64, String>>> = OnceLock::new();

    fn purge_error_overrides() -> &'static Mutex<HashMap<u64, String>> {
        PURGE_ERROR_OVERRIDES.get_or_init(|| Mutex::new(HashMap::new()))
    }

    pub struct BrowserProfilePurgeGuard {
        process_generation: u64,
    }

    impl Drop for BrowserProfilePurgeGuard {
        fn drop(&mut self) {
            purge_error_overrides()
                .lock()
                .remove(&self.process_generation);
        }
    }

    pub fn install_browser_profile_purge_error_for_tests(
        process_generation: u64,
        message: impl Into<String>,
    ) -> BrowserProfilePurgeGuard {
        purge_error_overrides()
            .lock()
            .insert(process_generation, message.into());
        BrowserProfilePurgeGuard { process_generation }
    }

    pub(super) fn injected_error(state: &CoreState) -> Option<String> {
        purge_error_overrides()
            .lock()
            .get(&state.process_generation())
            .cloned()
    }
}

#[cfg(debug_assertions)]
pub use browser_profile_purge::{
    BrowserProfilePurgeGuard, install_browser_profile_purge_error_for_tests,
};

const GOOGLE_SIGNUP_URL: &str = "https://accounts.google.com/signup";
const GOOGLE_LOGIN_URL: &str = "https://accounts.google.com/ServiceLogin";
const GITHUB_SIGNUP_URL: &str = "https://github.com/signup";
const GITHUB_LOGIN_URL: &str = "https://github.com/login";
const OPENCODE_CONSOLE_URL: &str = "https://opencode.ai/auth";
const LOCAL_DASHBOARD_BINDING: &str = "local-dashboard";

pub(super) async fn browser_capabilities(
    State(state): State<CoreState>,
) -> Json<BrowserCapabilities> {
    let capabilities = state.browser.capabilities().await;
    Json(BrowserCapabilities {
        mode: map_browser_mode(capabilities.mode),
        reason: capabilities.reason.map(sanitize_browser_runtime_message),
        revision: state.settings_revision(),
        process_generation: state.process_generation(),
    })
}

pub(super) async fn open_account_browser(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<BrowserOpen>, V3ApiError> {
    let input = parse_mutation_json::<BrowserOpenRequest>(&body)?;
    {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        resolve_browser_url(&state, &id, input.target)?;
    }

    let browser_operation = state.browser.operation().await;
    let url = {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &input.expectation)?;
        state
            .recover_browser_profiles_for_account(&id)
            .map_err(V3ApiError::internal)?;
        resolve_browser_url(&state, &id, input.target)?
    };
    let binding = dashboard_session_binding(&state, &headers)?;
    let opened = browser_operation
        .open(&id, &url, &binding)
        .await
        .map_err(|error| {
            V3ApiError::service_unavailable(&state, sanitize_browser_runtime_message(error))
        })?;
    let revision = {
        let _settings_update = state.settings_update.lock();
        check_open_still_valid(&state, &id, input.target, &input.expectation)
            .map(|()| state.settings_revision())
    };
    let revision = match revision {
        Ok(revision) => revision,
        Err(error) => {
            // Revoke the display token before the fallible worker/native stop.
            // A stale open must never remain usable merely because cleanup of
            // the underlying browser process failed.
            state.browser.invalidate_remote_sessions();
            let _ = browser_operation.stop_account(&id).await;
            return Err(error);
        }
    };
    Ok(Json(BrowserOpen {
        mode: map_browser_mode(opened.mode),
        session_token: opened.session_token,
        revision,
        process_generation: state.process_generation(),
    }))
}

pub(super) async fn reset_account_browser_profile(
    State(state): State<CoreState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<AccountMutation>, V3ApiError> {
    let expectation = parse_mutation_json::<MutationExpectation>(&body)?;
    {
        let _settings_update = state.settings_update.lock();
        check_expectation(&state, &expectation)?;
        state
            .recover_browser_profiles_for_account(&id)
            .map_err(V3ApiError::internal)?;
        load_model_account(&state, &id)?;
    }

    let browser_operation = state.browser.operation().await;
    browser_operation.stop_account(&id).await.map_err(|error| {
        V3ApiError::service_unavailable(&state, sanitize_browser_runtime_message(error))
    })?;

    let _settings_update = state.settings_update.lock();
    check_expectation(&state, &expectation)?;
    let account = load_model_account(&state, &id)?;
    let staged = StagedBrowserProfiles::stage(
        &state.data_dir(),
        &id,
        BrowserProfileOperationKind::ResetProfile,
    )
    .map_err(V3ApiError::internal)?;
    if account.account_type == ModelAccountType::Managed && !account.setup_step.is_ready() {
        if let Err(error) = state.db.lock().reset_pending_managed_setup(&id) {
            let purge_error = staged.purge().err();
            return Err(V3ApiError::internal(sanitize_browser_runtime_message(
                match purge_error {
                    Some(purge) => format!(
                        "failed to reset managed setup: {error}; failed to finish browser profile reset: {purge}"
                    ),
                    None => format!("failed to reset managed setup: {error}"),
                },
            )));
        }
        // The setup reset above is already committed and cannot be rolled
        // back. Publish that fact to the control plane before the fallible
        // profile purge/read so an error cannot leave clients holding a stale
        // revision for state that has changed on disk.
        let revision = state.bump_settings_revision();
        purge_staged_profiles(&state, staged)?;
        let account = load_model_account(&state, &id)?;
        return mutation_at(&state, account, revision).map(Json);
    }
    purge_staged_profiles(&state, staged)?;
    let revision = state.bump_settings_revision();
    let account = load_model_account(&state, &id)?;
    mutation_at(&state, account, revision).map(Json)
}

pub(super) async fn browser_session_websocket(
    State(state): State<CoreState>,
    Path(token): Path<String>,
    headers: HeaderMap,
    websocket: WebSocketUpgrade,
) -> Result<Response, V3ApiError> {
    validate_websocket_origin(&state, &headers)?;
    let binding = dashboard_session_binding(&state, &headers)?;
    let mut remote_session = state
        .browser
        .remote_websocket_session(&token, &binding)
        .map_err(|error| V3ApiError::gone(&state, sanitize_browser_runtime_message(error)))?;
    let worker = tokio::select! {
        _ = remote_session.cancellation.changed() => {
            return Err(V3ApiError::gone(&state, "browser session was replaced"));
        }
        result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio_tungstenite::connect_async(&remote_session.worker_ws_url),
        ) => {
            let (worker, _) = result
                .map_err(|_| V3ApiError::gateway_timeout(
                    &state,
                    "timed out connecting to remote browser display",
                ))?
                .map_err(|error| V3ApiError::outbound_failed(
                    &state,
                    sanitize_browser_runtime_message(format!(
                        "failed to connect to remote browser display: {error}"
                    )),
                ))?;
            worker
        }
    };
    Ok(websocket
        .on_upgrade(move |client| {
            proxy_browser_websocket(state, token, remote_session.cancellation, client, worker)
        })
        .into_response())
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

fn resolve_browser_url(
    state: &CoreState,
    id: &str,
    target: BrowserTarget,
) -> Result<String, V3ApiError> {
    let account = load_model_account(state, id)?;
    if account.account_type != ModelAccountType::Managed
        && !matches!(target, BrowserTarget::Console)
    {
        return Err(V3ApiError::invalid_request_at(
            state,
            "imported key accounts can only open the OpenCode console",
        ));
    }
    match target {
        BrowserTarget::GoogleSignup => Ok(GOOGLE_SIGNUP_URL.to_string()),
        BrowserTarget::GoogleLogin => Ok(GOOGLE_LOGIN_URL.to_string()),
        BrowserTarget::GithubSignup => Ok(GITHUB_SIGNUP_URL.to_string()),
        BrowserTarget::GithubLogin => Ok(GITHUB_LOGIN_URL.to_string()),
        BrowserTarget::Invite => {
            let invite = state.config().opencode_invite_url;
            if invite.is_empty() {
                Err(V3ApiError::precondition_failed_at(
                    state,
                    "configure an OpenCode invite URL before opening this step",
                ))
            } else {
                Ok(invite)
            }
        }
        BrowserTarget::Console => Ok(OPENCODE_CONSOLE_URL.to_string()),
    }
}

fn dashboard_session_binding(state: &CoreState, headers: &HeaderMap) -> Result<String, V3ApiError> {
    if is_local_dashboard_request(state, headers) {
        return Ok(LOCAL_DASHBOARD_BINDING.to_string());
    }
    if has_dashboard_session(state, headers) {
        return dashboard_session_value(headers)
            .map(str::to_string)
            .ok_or_else(V3ApiError::unauthorized);
    }
    Err(V3ApiError::unauthorized())
}

fn check_open_still_valid(
    state: &CoreState,
    id: &str,
    target: BrowserTarget,
    expectation: &MutationExpectation,
) -> Result<(), V3ApiError> {
    check_expectation(state, expectation)?;
    resolve_browser_url(state, id, target)?;
    Ok(())
}

fn purge_staged_profiles(
    state: &CoreState,
    staged: StagedBrowserProfiles,
) -> Result<(), V3ApiError> {
    let _ = state;
    #[cfg(debug_assertions)]
    if let Some(message) = browser_profile_purge::injected_error(state) {
        return Err(V3ApiError::internal(sanitize_browser_runtime_message(
            message,
        )));
    }
    staged
        .purge()
        .map_err(|error| V3ApiError::internal(sanitize_browser_runtime_message(error)))
}

fn validate_websocket_origin(state: &CoreState, headers: &HeaderMap) -> Result<(), V3ApiError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(state, "browser WebSocket Origin is required")
        })?;
    let origin = reqwest::Url::parse(origin).map_err(|_| {
        V3ApiError::invalid_request_at(state, "browser WebSocket Origin is invalid")
    })?;
    if !matches!(origin.scheme(), "http" | "https") {
        return Err(V3ApiError::invalid_request_at(
            state,
            "browser WebSocket Origin must use http or https",
        ));
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            V3ApiError::invalid_request_at(state, "browser WebSocket Host is required")
        })?;
    let authority = Authority::from_str(host)
        .map_err(|_| V3ApiError::invalid_request_at(state, "browser WebSocket Host is invalid"))?;
    let expected_scheme = dashboard_request_scheme(state, headers);
    let default_port = if expected_scheme == "https" { 443 } else { 80 };
    if origin.scheme() != expected_scheme
        || !origin
            .host_str()
            .is_some_and(|value| value.eq_ignore_ascii_case(authority.host()))
        || origin.port_or_known_default() != Some(authority.port_u16().unwrap_or(default_port))
    {
        return Err(V3ApiError::forbidden_at(
            state,
            "browser WebSocket Origin does not match Host",
        ));
    }
    Ok(())
}

/// Direct HTTP dashboard requests are `http`. `x-forwarded-proto` is honored
/// only when local-dashboard mode is off — the same boundary that already
/// treats forwarding headers as proxied, never a spoofable extra scheme
/// header, and never while loopback local mode is on.
fn dashboard_request_scheme(state: &CoreState, headers: &HeaderMap) -> &'static str {
    if trusted_forwarded_https(state, headers) {
        "https"
    } else {
        "http"
    }
}

fn trusted_forwarded_https(state: &CoreState, headers: &HeaderMap) -> bool {
    if state.dashboard_local_mode() {
        return false;
    }
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("https"))
}

fn load_model_account(state: &CoreState, id: &str) -> Result<ModelAccount, V3ApiError> {
    state
        .db
        .lock()
        .get_account(id)
        .map_err(V3ApiError::internal)?
        .ok_or_else(|| V3ApiError::not_found(state))
}

fn map_browser_mode(mode: crate::browser::BrowserMode) -> BrowserMode {
    match mode {
        crate::browser::BrowserMode::Native => BrowserMode::Native,
        crate::browser::BrowserMode::Remote => BrowserMode::Remote,
        crate::browser::BrowserMode::Unsupported => BrowserMode::Unsupported,
    }
}

fn sanitize_browser_runtime_message(message: impl std::fmt::Display) -> String {
    redact_embedded_urls(&crate::redaction::redact_text(&message.to_string()))
}

fn redact_embedded_urls(text: &str) -> String {
    let mut remaining = text;
    let mut output = String::with_capacity(text.len());
    while !remaining.is_empty() {
        let lower = remaining.to_ascii_lowercase();
        let next = ["https://", "http://", "wss://", "ws://"]
            .iter()
            .filter_map(|scheme| lower.find(scheme).map(|index| (index, scheme.len())))
            .min_by_key(|(index, _)| *index);
        let Some((start, scheme_len)) = next else {
            output.push_str(remaining);
            break;
        };
        output.push_str(&remaining[..start]);
        let after_scheme = &remaining[start + scheme_len..];
        let end = after_scheme
            .find(|ch: char| ch.is_whitespace() || matches!(ch, '"' | '\'' | '<' | '>' | ')' | ']'))
            .unwrap_or(after_scheme.len());
        output.push_str("<redacted-url>");
        remaining = &after_scheme[end..];
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        redact_embedded_urls, sanitize_browser_runtime_message, validate_websocket_origin,
    };
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use axum::http::{HeaderMap, header};
    use std::sync::Arc;

    fn temp_data_dir(label: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ocg-v3-browser-{}-{}", label, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn origin_state(label: &str, local: bool) -> (std::path::PathBuf, crate::state::CoreState) {
        let dir = temp_data_dir(label);
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("v3-browser-origin"));
        let state = Arc::new(
            CoreStateInner::new(Database::open(dir.clone()).unwrap(), dir.clone(), cipher).unwrap(),
        );
        state.set_dashboard_local_mode(local);
        (dir, state)
    }

    fn origin_headers(host: &str, origin: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, host.parse().unwrap());
        headers.insert(header::ORIGIN, origin.parse().unwrap());
        headers
    }

    #[test]
    fn websocket_origin_direct_http_rejects_https_and_matches_host() {
        let (dir, state) = origin_state("origin-direct", true);
        let mut headers = origin_headers("manager.example:9443", "http://manager.example:9443");
        assert!(
            validate_websocket_origin(&state, &headers).is_ok(),
            "direct HTTP origin should pass"
        );

        headers.insert(
            header::ORIGIN,
            "https://manager.example:9443".parse().unwrap(),
        );
        let error = validate_websocket_origin(&state, &headers)
            .expect_err("https origin must not match a direct HTTP request");
        assert_eq!(error.status, axum::http::StatusCode::FORBIDDEN);

        headers.insert(header::ORIGIN, "http://evil.example:9443".parse().unwrap());
        let error =
            validate_websocket_origin(&state, &headers).expect_err("cross origin must fail");
        assert_eq!(error.status, axum::http::StatusCode::FORBIDDEN);

        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert(
            header::ORIGIN,
            "https://manager.example:9443".parse().unwrap(),
        );
        let error = validate_websocket_origin(&state, &headers)
            .expect_err("local mode must not trust spoofed x-forwarded-proto");
        assert_eq!(error.status, axum::http::StatusCode::FORBIDDEN);

        headers.remove(header::ORIGIN);
        let error = validate_websocket_origin(&state, &headers).expect_err("missing origin");
        assert_eq!(error.status, axum::http::StatusCode::BAD_REQUEST);

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn websocket_origin_trusted_proxy_binds_forwarded_proto_only() {
        let (dir, state) = origin_state("origin-proxy", false);
        let mut headers = origin_headers("manager.example", "https://manager.example");
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        assert!(
            validate_websocket_origin(&state, &headers).is_ok(),
            "trusted proxy https origin should pass"
        );

        headers.insert(header::ORIGIN, "http://manager.example".parse().unwrap());
        let error = validate_websocket_origin(&state, &headers)
            .expect_err("http origin must not match an effective https request");
        assert_eq!(error.status, axum::http::StatusCode::FORBIDDEN);

        headers.remove("x-forwarded-proto");
        headers.insert("x-forwarded-scheme", "https".parse().unwrap());
        headers.insert(header::ORIGIN, "https://manager.example".parse().unwrap());
        let error = validate_websocket_origin(&state, &headers)
            .expect_err("arbitrary scheme headers must not change the direct HTTP request scheme");
        assert_eq!(error.status, axum::http::StatusCode::FORBIDDEN);

        headers.remove("x-forwarded-scheme");
        headers.insert("forwarded", "proto=https".parse().unwrap());
        let error = validate_websocket_origin(&state, &headers)
            .expect_err("Forwarded proto is not the trusted cookie scheme boundary");
        assert_eq!(error.status, axum::http::StatusCode::FORBIDDEN);

        headers.remove("forwarded");
        headers.insert(header::ORIGIN, "http://manager.example".parse().unwrap());
        assert!(
            validate_websocket_origin(&state, &headers).is_ok(),
            "direct public HTTP origin should pass without forwarded proto"
        );

        drop(state);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn production_source_reuses_accounts_dto_mapper() {
        let production = include_str!("browser.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source precedes tests");
        assert!(
            production.contains("use super::accounts::mutation_at;"),
            "browser reset/open account JSON must go through accounts.rs"
        );
        assert!(
            !production.contains("fn account_from_state"),
            "browser.rs must not duplicate the account DTO mapper"
        );
        assert!(
            !production.contains("fn mutation_at("),
            "browser.rs must reuse accounts::mutation_at"
        );
    }

    #[test]
    fn runtime_errors_redact_worker_urls_and_control_tokens() {
        let message = sanitize_browser_runtime_message(
            "failed to connect to remote browser display: ws://browser.internal:6080/websockify token=/run/ocg-browser/control-token http://127.0.0.1:9/session",
        );
        assert!(!message.contains("browser.internal"));
        assert!(!message.contains("127.0.0.1:9"));
        assert!(!message.contains("ws://"));
        assert!(!message.contains("http://"));
        assert!(message.contains("<redacted-url>"));
        assert!(!message.contains("/run/ocg-browser/control-token"));
        assert!(message.contains("<redacted>"));
        assert_eq!(redact_embedded_urls("plain error"), "plain error");
    }
}
