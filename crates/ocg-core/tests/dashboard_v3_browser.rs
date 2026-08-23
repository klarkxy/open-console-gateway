//! Dashboard V3 browser runtime: session protection, CAS, secrecy, Origin,
//! native/remote wire shape, reset lifecycle, and V2 coexistence.

use axum::Router;
use axum::extract::ws::{Message as AxumWsMessage, WebSocketUpgrade};
use axum::extract::{Json as AxumJson, State as AxumState};
use axum::http::HeaderMap as AxumHeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use ocg_core::browser::browser_profile_paths;
#[cfg(debug_assertions)]
use ocg_core::dashboard_v3::ERROR_INTERNAL;
use ocg_core::dashboard_v3::{
    Account, AccountMutation, AccountSetupStep, BrowserCapabilities, BrowserMode, BrowserOpen,
    ERROR_FORBIDDEN, ERROR_GONE, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST,
    ERROR_MISSING_EXPECTED_REVISION, ERROR_NOT_FOUND, ERROR_PRECONDITION_FAILED,
    ERROR_REVISION_CONFLICT, ERROR_SERVICE_UNAVAILABLE, ERROR_UNAUTHORIZED,
};
use ocg_core::db::CURRENT_SCHEMA_VERSION;
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as ClientWsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

const REMOTE_CHILD_ENV: &str = "OCG_V3_BROWSER_REMOTE_CHILD";

#[derive(Clone)]
struct FakeRemoteWorker {
    control_token: Arc<String>,
    display_url: Arc<String>,
    active_account: Arc<Mutex<Option<String>>>,
}

fn fake_worker_authorized(state: &FakeRemoteWorker, headers: &AxumHeaderMap) -> bool {
    headers
        .get(reqwest::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {}", state.control_token))
}

async fn fake_worker_health(
    AxumState(state): AxumState<FakeRemoteWorker>,
    headers: AxumHeaderMap,
) -> Response {
    if !fake_worker_authorized(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    AxumJson(json!({ "ok": true })).into_response()
}

async fn fake_worker_open(
    AxumState(state): AxumState<FakeRemoteWorker>,
    headers: AxumHeaderMap,
    AxumJson(body): AxumJson<Value>,
) -> Response {
    if !fake_worker_authorized(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(account_id) = body.get("account_id").and_then(Value::as_str) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    *state.active_account.lock().unwrap() = Some(account_id.to_string());
    AxumJson(json!({ "vnc_ws_url": state.display_url.as_str() })).into_response()
}

async fn fake_worker_status(
    AxumState(state): AxumState<FakeRemoteWorker>,
    headers: AxumHeaderMap,
) -> Response {
    if !fake_worker_authorized(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let account_id = state.active_account.lock().unwrap().clone();
    AxumJson(json!({
        "active": account_id.is_some(),
        "account_id": account_id,
    }))
    .into_response()
}

async fn fake_worker_stop(
    AxumState(state): AxumState<FakeRemoteWorker>,
    headers: AxumHeaderMap,
    AxumJson(body): AxumJson<Value>,
) -> Response {
    if !fake_worker_authorized(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let requested = body.get("account_id").and_then(Value::as_str);
    let mut active = state.active_account.lock().unwrap();
    if active.as_deref() == requested {
        *active = None;
    }
    AxumJson(json!({ "ok": true })).into_response()
}

async fn fake_worker_display(websocket: WebSocketUpgrade) -> Response {
    websocket
        .on_upgrade(|mut socket| async move {
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    AxumWsMessage::Text(_) | AxumWsMessage::Binary(_) => {
                        if socket.send(message).await.is_err() {
                            break;
                        }
                    }
                    AxumWsMessage::Ping(value) => {
                        if socket.send(AxumWsMessage::Pong(value)).await.is_err() {
                            break;
                        }
                    }
                    AxumWsMessage::Pong(_) => {}
                    AxumWsMessage::Close(_) => break,
                }
            }
        })
        .into_response()
}

async fn start_fake_remote_worker(
    control_token: String,
) -> (
    String,
    Arc<Mutex<Option<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let base_url = format!("http://{address}");
    let active_account = Arc::new(Mutex::new(None));
    let state = FakeRemoteWorker {
        control_token: Arc::new(control_token),
        display_url: Arc::new(format!("ws://{address}/display")),
        active_account: active_account.clone(),
    };
    let router = Router::new()
        .route("/health", get(fake_worker_health))
        .route(
            "/session",
            get(fake_worker_status)
                .post(fake_worker_open)
                .delete(fake_worker_stop),
        )
        .route("/display", get(fake_worker_display))
        .with_state(state);
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base_url, active_account, server)
}

async fn run_remote_browser_child() {
    let harness = start_loopback("browser-remote-round-trip-child").await;
    let (status, capabilities) = harness
        .get_json(&format!("{}/browser/capabilities", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{capabilities}");
    assert_eq!(parse_capabilities(&capabilities).mode, BrowserMode::Remote);
    assert_secret_free(
        &capabilities,
        &["v3-remote-control-token", "ws://", "http://"],
    );

    let account_id = create_go_account(&harness, "remote", "sk-remote-open").await;
    let before = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        &cas(&harness, json!({ "target": "console" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let opened = parse_open(&body);
    assert_eq!(opened.mode, BrowserMode::Remote);
    assert_eq!(opened.revision, before);
    let token = opened
        .session_token
        .expect("remote mode must return one opaque dashboard token");
    assert_eq!(body.as_object().unwrap().len(), 4);
    assert_secret_free(
        &body,
        &[
            "sk-remote-open",
            "v3-remote-control-token",
            "ws://",
            "http://",
        ],
    );
    assert!(
        harness
            .state
            .browser
            .remote_websocket_session(&token, "different-dashboard-session")
            .is_err(),
        "the opaque token must stay bound to the dashboard session that opened it"
    );

    let origin = format!("http://127.0.0.1:{}", harness.handle.port);
    let display_url = format!(
        "ws://127.0.0.1:{}/dashboard/api/v3/browser/sessions/{token}/ws",
        harness.handle.port
    );
    let mut request = display_url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert(reqwest::header::ORIGIN, origin.parse().unwrap());
    let (mut display, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    display
        .send(ClientWsMessage::Text("v3-round-trip".into()))
        .await
        .unwrap();
    let echoed = tokio::time::timeout(Duration::from_secs(5), display.next())
        .await
        .expect("remote display echo timed out")
        .expect("remote display closed before echo")
        .expect("remote display echo failed");
    match echoed {
        ClientWsMessage::Text(text) => assert_eq!(text.as_str(), "v3-round-trip"),
        other => panic!("unexpected remote display echo: {other:?}"),
    }
    display.close(None).await.unwrap();

    let (status, reset) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}/browser-profile"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    assert_eq!(harness.state.settings_revision(), before + 1);

    let (status, gone) = websocket_get(
        &harness,
        &format!("/browser/sessions/{token}/ws"),
        Some(&origin),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{gone}");
    assert_v3_error(&gone, ERROR_GONE);

    harness.stop();
}

const SECRET_FIELD_NAMES: &[&str] = &[
    "key",
    "password",
    "passwordCipher",
    "keyCipher",
    "gatewayKey",
    "gateway_key",
    "primaryKey",
    "primary_key",
    "referralCode",
    "referral_code",
    "cipher",
    "apiKey",
    "api_key",
    "token",
    "secret",
    "workerUrl",
    "vncWsUrl",
    "controlToken",
    "worker_url",
    "vnc_ws_url",
    "control_token",
];

fn cas(harness: &V3Harness, patch: Value) -> Value {
    let mut body = match patch {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    body.insert(
        "expectedRevision".into(),
        json!(harness.state.settings_revision()),
    );
    body.insert(
        "processGeneration".into(),
        json!(harness.state.process_generation()),
    );
    Value::Object(body)
}

async fn send_json(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v3_base))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn send_raw(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &str,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v3_base))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

fn json_field_names(value: &Value) -> Vec<&str> {
    match value {
        Value::Object(map) => {
            let mut names: Vec<&str> = map.keys().map(String::as_str).collect();
            names.extend(map.values().flat_map(json_field_names));
            names
        }
        Value::Array(items) => items.iter().flat_map(json_field_names).collect(),
        _ => Vec::new(),
    }
}

fn json_string_values(value: &Value) -> Vec<&str> {
    match value {
        Value::String(text) => vec![text.as_str()],
        Value::Array(items) => items.iter().flat_map(json_string_values).collect(),
        Value::Object(map) => map.values().flat_map(json_string_values).collect(),
        _ => Vec::new(),
    }
}

fn assert_v3_error(body: &Value, code: &str) {
    assert_eq!(body["code"], code, "{body}");
    assert!(body.get("message").and_then(Value::as_str).is_some());
    assert!(body.as_object().unwrap().contains_key("currentRevision"));
    assert!(body.as_object().unwrap().contains_key("processGeneration"));
    assert!(body.get("current_revision").is_none());
}

fn assert_secret_free(body: &Value, secrets: &[&str]) {
    for name in json_field_names(body) {
        assert!(
            !SECRET_FIELD_NAMES.contains(&name),
            "browser JSON leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in secrets {
            assert!(
                !value.contains(secret),
                "browser JSON leaked secret {secret}: {body}"
            );
        }
    }
    let encoded = body.to_string();
    for secret in secrets {
        assert!(
            !encoded.contains(secret),
            "browser JSON leaked secret {secret} in encoded JSON: {body}"
        );
    }
}

fn profile_tombstones_exist(data_dir: &Path, account_id: &str) -> bool {
    browser_profile_paths(data_dir, account_id)
        .unwrap()
        .into_iter()
        .any(|path| {
            path.parent()
                .and_then(|parent| std::fs::read_dir(parent).ok())
                .is_some_and(|entries| {
                    entries.filter_map(Result::ok).any(|entry| {
                        entry
                            .file_name()
                            .to_string_lossy()
                            .starts_with(&format!(".ocg-profile-delete-{account_id}"))
                    })
                })
        })
        || {
            let journal = data_dir.join("browser-profile-operations");
            journal.is_dir()
                && std::fs::read_dir(&journal)
                    .map(|entries| entries.filter_map(Result::ok).next().is_some())
                    .unwrap_or(false)
        }
}

fn parse_mutation(body: &Value) -> AccountMutation {
    serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("AccountMutation JSON: {body}"))
}

fn mutation_account(body: &Value) -> Account {
    parse_mutation(body)
        .account
        .expect("mutation should return an account")
}

fn parse_open(body: &Value) -> BrowserOpen {
    serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("BrowserOpen JSON: {body}"))
}

fn parse_capabilities(body: &Value) -> BrowserCapabilities {
    serde_json::from_value(body.clone())
        .unwrap_or_else(|_| panic!("BrowserCapabilities JSON: {body}"))
}

fn websocket_headers(origin: Option<&str>) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(reqwest::header::CONNECTION, "Upgrade".parse().unwrap());
    headers.insert(reqwest::header::UPGRADE, "websocket".parse().unwrap());
    headers.insert("Sec-WebSocket-Version", "13".parse().unwrap());
    headers.insert(
        "Sec-WebSocket-Key",
        "dGhlIHNhbXBsZSBub25jZQ==".parse().unwrap(),
    );
    if let Some(origin) = origin {
        headers.insert(reqwest::header::ORIGIN, origin.parse().unwrap());
    }
    headers
}

async fn websocket_get(
    harness: &V3Harness,
    path: &str,
    origin: Option<&str>,
    extra: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut request = harness
        .client
        .get(format!("{}{path}", harness.v3_base))
        .headers(websocket_headers(origin));
    for (name, value) in extra {
        request = request.header(*name, *value);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn create_go_account(harness: &V3Harness, name: &str, key: &str) -> String {
    let (status, created) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(harness, json!({ "name": name, "key": key })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    mutation_account(&created).id
}

fn register_native(
    harness: &V3Harness,
    launches: Arc<AtomicUsize>,
    stops: Arc<AtomicUsize>,
    stop_error: Option<&'static str>,
) {
    harness
        .state
        .browser
        .register_native_hooks(
            Arc::new(move |_, _| {
                launches.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            Arc::new(move |_| {
                stops.fetch_add(1, Ordering::SeqCst);
                if let Some(error) = stop_error {
                    Err(anyhow::anyhow!("{error}"))
                } else {
                    Ok(())
                }
            }),
        )
        .unwrap();
}

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
}

#[tokio::test]
async fn dashboard_v3_remote_browser_round_trip() {
    if std::env::var_os(REMOTE_CHILD_ENV).is_some() {
        run_remote_browser_child().await;
        return;
    }

    // BrowserRuntime intentionally freezes its worker configuration at state
    // construction. Run the configured-runtime half in an isolated child test
    // process so this regression never races other tests through process-global
    // environment variables.
    let control_token = "v3-remote-control-token-0123456789abcdef0123456789abcdef".to_string();
    let (worker_url, active_account, server) =
        start_fake_remote_worker(control_token.clone()).await;
    let token_dir = harness::temp_data_dir("browser-remote-token");
    let token_file = token_dir.join("control-token");
    std::fs::write(&token_file, &control_token).unwrap();
    let executable = std::env::current_exe().unwrap();
    let output = tokio::task::spawn_blocking(move || {
        Command::new(executable)
            .arg("--exact")
            .arg("dashboard_v3_remote_browser_round_trip")
            .arg("--nocapture")
            .env(REMOTE_CHILD_ENV, "1")
            .env("OCG_BROWSER_WORKER_URL", worker_url)
            .env("OCG_BROWSER_CONTROL_TOKEN_FILE", token_file)
            .env_remove("OCG_BROWSER_PROFILES_DIR")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    server.abort();
    let _ = std::fs::remove_dir_all(token_dir);
    assert!(
        output.status.success(),
        "isolated remote browser regression failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        active_account.lock().unwrap().is_none(),
        "profile reset must stop the worker account as well as revoke its dashboard token"
    );
}

#[tokio::test]
async fn dashboard_v3_browser_routes_require_the_v3_session() {
    let harness = start_public("browser-auth").await;
    let (status, body) = harness
        .get_json(&format!("{}/browser/capabilities", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts/missing/browser",
        &cas(&harness, json!({ "target": "console" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    let (status, body) = send_json(
        &harness,
        Method::DELETE,
        "/accounts/missing/browser-profile",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    let (status, body) = websocket_get(
        &harness,
        "/browser/sessions/opaque-token/ws",
        Some("http://evil.example"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_browser_rejects_forwarded_header_local_bypass() {
    let harness = start_loopback("browser-forwarded").await;
    let (status, body) = harness
        .get_json(&format!("{}/browser/capabilities", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let forwarded = harness
        .client
        .get(format!("{}/browser/capabilities", harness.v3_base))
        .header("x-forwarded-for", "203.0.113.10")
        .send()
        .await
        .unwrap();
    assert_eq!(forwarded.status(), StatusCode::UNAUTHORIZED);
    let body: Value = forwarded.json().await.unwrap();
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    let forwarded_ws = harness
        .client
        .get(format!(
            "{}/browser/sessions/opaque-token/ws",
            harness.v3_base
        ))
        .headers(websocket_headers(Some(&format!(
            "http://127.0.0.1:{}",
            harness.handle.port
        ))))
        .header("x-forwarded-for", "203.0.113.10")
        .send()
        .await
        .unwrap();
    assert_eq!(forwarded_ws.status(), StatusCode::UNAUTHORIZED);
    let body: Value = forwarded_ws.json().await.unwrap();
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    let forwarded_open = harness
        .client
        .post(format!("{}/accounts/missing/browser", harness.v3_base))
        .header("x-forwarded-for", "203.0.113.10")
        .json(&cas(&harness, json!({ "target": "console" })))
        .send()
        .await
        .unwrap();
    assert_eq!(forwarded_open.status(), StatusCode::UNAUTHORIZED);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_login_cookie_authorizes_browser_routes() {
    let harness = start_public("browser-cookie").await;
    let register = harness
        .client
        .post(format!("{}/auth/register", harness.v2_base))
        .json(&json!({ "username": "admin", "password": "password123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(register.status(), StatusCode::CREATED);
    let cookie = register
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let authorized = harness
        .client
        .get(format!("{}/browser/capabilities", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    let body: Value = authorized.json().await.unwrap();
    let parsed = parse_capabilities(&body);
    assert_eq!(
        parsed.process_generation,
        harness.state.process_generation()
    );
    assert_eq!(parsed.revision, harness.state.settings_revision());
    assert!(
        body["reason"].is_null() || body["reason"].as_str().is_some(),
        "capabilities reason must be T|null, got {body}"
    );
    assert!(body.get("workerUrl").is_none());
    assert_secret_free(&body, &["password123"]);

    let logout = harness
        .client
        .post(format!("{}/auth/logout", harness.v2_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);

    let after_logout = harness
        .client
        .get(format!("{}/browser/capabilities", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(after_logout.status(), StatusCode::UNAUTHORIZED);

    let origin = format!("http://127.0.0.1:{}", harness.handle.port);
    let (status, body) = {
        let response = harness
            .client
            .get(format!(
                "{}/browser/sessions/opaque-token/ws",
                harness.v3_base
            ))
            .headers(websocket_headers(Some(&origin)))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        (
            response.status(),
            response.json().await.unwrap_or(Value::Null),
        )
    };
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_browser_websocket_origin_and_token_binding() {
    let harness = start_loopback("browser-ws-origin").await;
    let origin = format!("http://127.0.0.1:{}", harness.handle.port);

    let (status, body) =
        websocket_get(&harness, "/browser/sessions/opaque-token/ws", None, &[]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("Origin is required"),
        "{body}"
    );

    let (status, body) = websocket_get(
        &harness,
        "/browser/sessions/opaque-token/ws",
        Some("https://evil.example"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_v3_error(&body, ERROR_FORBIDDEN);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("does not match Host"),
        "{body}"
    );

    let (status, body) = websocket_get(
        &harness,
        "/browser/sessions/opaque-token/ws",
        Some(&origin),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_v3_error(&body, ERROR_GONE);
    assert_eq!(body["currentRevision"], harness.state.settings_revision());
    assert_eq!(
        body["processGeneration"],
        harness.state.process_generation()
    );

    let public = start_public("browser-ws-token-auth").await;
    let public_origin = format!("http://127.0.0.1:{}", public.handle.port);
    let (status, body) = websocket_get(
        &public,
        "/browser/sessions/opaque-token/ws",
        Some(&public_origin),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    public.stop();
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_browser_websocket_origin_scheme_follows_direct_http_and_trusted_proxy() {
    let harness = start_loopback("browser-ws-scheme-direct").await;
    let http_origin = format!("http://127.0.0.1:{}", harness.handle.port);
    let https_origin = format!("https://127.0.0.1:{}", harness.handle.port);

    let (status, body) = websocket_get(
        &harness,
        "/browser/sessions/opaque-token/ws",
        Some(&https_origin),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_v3_error(&body, ERROR_FORBIDDEN);

    let (status, body) = websocket_get(
        &harness,
        "/browser/sessions/opaque-token/ws",
        Some(&https_origin),
        &[("x-forwarded-proto", "https")],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the established loopback forwarding boundary must reject x-forwarded-proto before Origin validation: {body}"
    );
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    let (status, body) = websocket_get(
        &harness,
        "/browser/sessions/opaque-token/ws",
        Some(&http_origin),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_v3_error(&body, ERROR_GONE);
    harness.stop();

    let public = start_public("browser-ws-scheme-proxy").await;
    let public_http = format!("http://127.0.0.1:{}", public.handle.port);
    let public_https = format!("https://127.0.0.1:{}", public.handle.port);
    let register = public
        .client
        .post(format!("{}/auth/register", public.v2_base))
        .json(&json!({ "username": "admin", "password": "password123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(register.status(), StatusCode::CREATED);
    let cookie = register
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let (status, body) = {
        let response = public
            .client
            .get(format!(
                "{}/browser/sessions/opaque-token/ws",
                public.v3_base
            ))
            .headers(websocket_headers(Some(&public_https)))
            .header(reqwest::header::COOKIE, &cookie)
            .header("x-forwarded-proto", "https")
            .send()
            .await
            .unwrap();
        (
            response.status(),
            response.json().await.unwrap_or(Value::Null),
        )
    };
    assert_eq!(status, StatusCode::GONE, "{body}");
    assert_v3_error(&body, ERROR_GONE);

    let (status, body) = {
        let response = public
            .client
            .get(format!(
                "{}/browser/sessions/opaque-token/ws",
                public.v3_base
            ))
            .headers(websocket_headers(Some(&public_http)))
            .header(reqwest::header::COOKIE, &cookie)
            .header("x-forwarded-proto", "https")
            .send()
            .await
            .unwrap();
        (
            response.status(),
            response.json().await.unwrap_or(Value::Null),
        )
    };
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "http origin must not match trusted-proxy https: {body}"
    );
    assert_v3_error(&body, ERROR_FORBIDDEN);

    let (status, body) = {
        let response = public
            .client
            .get(format!(
                "{}/browser/sessions/opaque-token/ws",
                public.v3_base
            ))
            .headers(websocket_headers(Some(&public_https)))
            .header(reqwest::header::COOKIE, &cookie)
            .header("x-forwarded-scheme", "https")
            .send()
            .await
            .unwrap();
        (
            response.status(),
            response.json().await.unwrap_or(Value::Null),
        )
    };
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "arbitrary scheme header must not mint https: {body}"
    );
    assert_v3_error(&body, ERROR_FORBIDDEN);

    public.stop();
}

#[tokio::test]
async fn dashboard_v3_native_open_returns_no_session_token() {
    let harness = start_loopback("browser-native-open").await;
    let launches = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    register_native(&harness, launches.clone(), stops.clone(), None);
    let account_id = create_go_account(&harness, "native", "sk-native-open").await;
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        &cas(&harness, json!({ "target": "console" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let opened = parse_open(&body);
    assert_eq!(opened.mode, BrowserMode::Native);
    assert!(opened.session_token.is_none());
    assert_eq!(body["sessionToken"], Value::Null);
    assert_eq!(opened.revision, before);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    assert_eq!(body.as_object().unwrap().len(), 4);
    assert_secret_free(
        &body,
        &["sk-native-open", "ws://", "wss://", "control-token"],
    );

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        &cas(&harness, json!({ "target": "google_signup" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(launches.load(Ordering::SeqCst), 1);

    let remote_shape = serde_json::to_value(BrowserOpen {
        mode: BrowserMode::Remote,
        session_token: Some("opaque-only".into()),
        revision: before,
        process_generation: harness.state.process_generation(),
    })
    .unwrap();
    assert_eq!(remote_shape["mode"], "remote");
    assert_eq!(remote_shape["sessionToken"], "opaque-only");
    assert_eq!(remote_shape.as_object().unwrap().len(), 4);
    assert!(remote_shape.get("workerUrl").is_none());
    assert!(remote_shape.get("vncWsUrl").is_none());
    assert!(remote_shape.get("controlToken").is_none());

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_revision_or_generation_has_no_browser_side_effect() {
    let harness = start_loopback("browser-stale-cas").await;
    let launches = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    register_native(&harness, launches.clone(), stops.clone(), None);
    let account_id = create_go_account(&harness, "cas", "sk-browser-cas").await;
    let profile = browser_profile_paths(&harness.state.data_dir(), &account_id).unwrap()[0].clone();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), b"stale must preserve this").unwrap();
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        &json!({
            "expectedRevision": before.saturating_sub(1),
            "processGeneration": generation,
            "target": "console"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(launches.load(Ordering::SeqCst), 0);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        &json!({
            "expectedRevision": before,
            "processGeneration": generation.wrapping_add(1),
            "target": "console"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(launches.load(Ordering::SeqCst), 0);

    let (status, body) = send_raw(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        r#"{"target":"console"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(launches.load(Ordering::SeqCst), 0);

    let (status, body) = send_raw(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        "not-json",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let (status, body) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}/browser-profile"),
        &json!({
            "expectedRevision": before.saturating_sub(1),
            "processGeneration": generation
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(stops.load(Ordering::SeqCst), 0);
    assert!(profile.join("Cookies").is_file());
    assert!(!profile_tombstones_exist(
        &harness.state.data_dir(),
        &account_id
    ));

    let blocked = {
        let _browser_operation = harness.state.browser.operation().await;
        tokio::time::timeout(
            Duration::from_secs(3),
            send_json(
                &harness,
                Method::DELETE,
                &format!("/accounts/{account_id}/browser-profile"),
                &json!({
                    "expectedRevision": before.saturating_sub(1),
                    "processGeneration": generation
                }),
            ),
        )
        .await
        .expect("stale reset must not wait on a live browser operation")
    };
    assert_eq!(blocked.0, StatusCode::CONFLICT, "{}", blocked.1);
    assert_eq!(stops.load(Ordering::SeqCst), 0);
    assert!(profile.join("Cookies").is_file());
    assert_eq!(harness.state.settings_revision(), before);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts/does-not-exist/browser",
        &cas(&harness, json!({ "target": "console" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_v3_error(&body, ERROR_NOT_FOUND);
    assert_eq!(launches.load(Ordering::SeqCst), 0);

    harness.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_v3_delayed_open_conflicts_and_compensates_after_revision_advance() {
    let harness = start_loopback("browser-delayed-open").await;
    let launches = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    harness
        .state
        .browser
        .register_native_hooks(
            {
                let launches = launches.clone();
                let entered = entered.clone();
                let release = release.clone();
                Arc::new(move |_, _| {
                    if launches.fetch_add(1, Ordering::SeqCst) == 0 {
                        entered.wait();
                        release.wait();
                    }
                    Ok(())
                })
            },
            {
                let stops = stops.clone();
                Arc::new(move |_| {
                    stops.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
        )
        .unwrap();
    let account_id = create_go_account(&harness, "delayed-open", "sk-delayed-open").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let open = tokio::spawn({
        let harness_client = harness.client.clone();
        let url = format!("{}/accounts/{account_id}/browser", harness.v3_base);
        let body = json!({
            "expectedRevision": before,
            "processGeneration": generation,
            "target": "console"
        });
        async move {
            let response = harness_client.post(url).json(&body).send().await.unwrap();
            let status = response.status();
            let body = response.json().await.unwrap_or(Value::Null);
            (status, body)
        }
    });

    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking({
            let entered = entered.clone();
            move || {
                entered.wait();
            }
        }),
    )
    .await
    .expect("native open should enter the delayed launcher")
    .unwrap();
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    assert_eq!(stops.load(Ordering::SeqCst), 0);
    assert_eq!(harness.state.bump_settings_revision(), before + 1);
    tokio::task::spawn_blocking(move || {
        release.wait();
    })
    .await
    .unwrap();

    let (status, body) = open.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], before + 1);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    assert_eq!(
        stops.load(Ordering::SeqCst),
        1,
        "stale open must stop/revoke the session opened during the race"
    );
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert!(body.get("sessionToken").is_none() || body["sessionToken"].is_null());

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        &cas(&harness, json!({ "target": "console" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(parse_open(&body).mode, BrowserMode::Native);
    assert_eq!(launches.load(Ordering::SeqCst), 2);
    assert_eq!(stops.load(Ordering::SeqCst), 1);

    harness.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_v3_delayed_open_rechecks_account_and_compensates_if_it_disappears() {
    let harness = start_loopback("browser-delayed-open-account").await;
    let launches = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    harness
        .state
        .browser
        .register_native_hooks(
            {
                let launches = launches.clone();
                let entered = entered.clone();
                let release = release.clone();
                Arc::new(move |_, _| {
                    launches.fetch_add(1, Ordering::SeqCst);
                    entered.wait();
                    release.wait();
                    Ok(())
                })
            },
            {
                let stops = stops.clone();
                Arc::new(move |_| {
                    stops.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
            },
        )
        .unwrap();
    let account_id = create_go_account(&harness, "delayed-account", "sk-delayed-account").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let open = tokio::spawn({
        let harness_client = harness.client.clone();
        let url = format!("{}/accounts/{account_id}/browser", harness.v3_base);
        let body = json!({
            "expectedRevision": before,
            "processGeneration": generation,
            "target": "console"
        });
        async move {
            let response = harness_client.post(url).json(&body).send().await.unwrap();
            let status = response.status();
            let body = response.json().await.unwrap_or(Value::Null);
            (status, body)
        }
    });

    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::task::spawn_blocking({
            let entered = entered.clone();
            move || {
                entered.wait();
            }
        }),
    )
    .await
    .expect("native open should enter the delayed launcher")
    .unwrap();
    {
        let mut db = harness.state.db.lock();
        db.delete_account(&account_id).unwrap();
    }
    tokio::task::spawn_blocking(move || {
        release.wait();
    })
    .await
    .unwrap();

    let (status, body) = open.await.unwrap();
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_v3_error(&body, ERROR_NOT_FOUND);
    assert_eq!(body["currentRevision"], before);
    assert_eq!(launches.load(Ordering::SeqCst), 1);
    assert_eq!(
        stops.load(Ordering::SeqCst),
        1,
        "an open whose account vanished must be stopped before returning"
    );
    assert_eq!(harness.state.settings_revision(), before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_reset_stop_failure_preserves_profile() {
    let harness = start_loopback("browser-reset-stop").await;
    let launches = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    register_native(
        &harness,
        launches,
        stops.clone(),
        Some(
            "injected browser stop failure token=/run/ocg-browser/control-token ws://browser.internal:6080/websockify",
        ),
    );
    let account_id = create_go_account(&harness, "stop-fail", "sk-stop-fail").await;
    let profile = browser_profile_paths(&harness.state.data_dir(), &account_id).unwrap()[0].clone();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), b"stop failure must preserve this").unwrap();
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();

    let (status, body) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}/browser-profile"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_v3_error(&body, ERROR_SERVICE_UNAVAILABLE);
    assert_eq!(body["currentRevision"], before);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert!(profile.join("Cookies").is_file());
    assert!(!profile_tombstones_exist(
        &harness.state.data_dir(),
        &account_id
    ));
    assert_secret_free(
        &body,
        &[
            "sk-stop-fail",
            "/run/ocg-browser/control-token",
            "browser.internal",
            "ws://",
            "6080",
        ],
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_reset_purges_profile_and_rewinds_managed_setup() {
    let harness = start_loopback("browser-reset-success").await;
    let launches = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    register_native(&harness, launches, stops.clone(), None);

    let (status, managed) = send_json(
        &harness,
        Method::POST,
        "/accounts/managed",
        &cas(&harness, json!({ "name": "draft" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{managed}");
    let account_id = mutation_account(&managed).id;
    let (status, advanced) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{account_id}/setup"),
        &cas(&harness, json!({ "setupStep": "opencode_registration" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{advanced}");
    assert_eq!(
        mutation_account(&advanced).setup_step,
        AccountSetupStep::OpencodeRegistration
    );

    let profile = browser_profile_paths(&harness.state.data_dir(), &account_id).unwrap()[0].clone();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), b"reset me").unwrap();
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}/browser-profile"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let reset = mutation_account(&body);
    assert_eq!(reset.setup_step, AccountSetupStep::GoogleAccount);
    assert!(!reset.enabled);
    assert_eq!(reset.revision, before + 1);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert!(!profile.exists());
    assert!(!profile_tombstones_exist(
        &harness.state.data_dir(),
        &account_id
    ));
    assert_secret_free(&body, &["sk-secret", "ws://", "control-token"]);
    assert_eq!(reset.id, account_id);

    let (status, missing) = send_json(
        &harness,
        Method::DELETE,
        "/accounts/does-not-exist/browser-profile",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_v3_error(&missing, ERROR_NOT_FOUND);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_reset_purge_failure_still_advances_revision_after_db_commit() {
    let harness = start_loopback("browser-reset-purge-fail").await;
    let launches = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    register_native(&harness, launches, stops.clone(), None);

    let (status, managed) = send_json(
        &harness,
        Method::POST,
        "/accounts/managed",
        &cas(&harness, json!({ "name": "purge-fail" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{managed}");
    let account_id = mutation_account(&managed).id;
    let (status, advanced) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{account_id}/setup"),
        &cas(&harness, json!({ "setupStep": "opencode_registration" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{advanced}");
    assert_eq!(
        mutation_account(&advanced).setup_step,
        AccountSetupStep::OpencodeRegistration
    );

    let profile = browser_profile_paths(&harness.state.data_dir(), &account_id).unwrap()[0].clone();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), b"must not be restored").unwrap();
    let before = harness.state.settings_revision();
    let _guard = ocg_core::dashboard_v3::install_browser_profile_purge_error_for_tests(
        harness.state.process_generation(),
        "injected browser profile purge failure token=/run/ocg-browser/control-token ws://browser.internal:6080/websockify",
    );

    let (status, body) = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}/browser-profile"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_v3_error(&body, ERROR_INTERNAL);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(stops.load(Ordering::SeqCst), 1);
    assert_secret_free(
        &body,
        &[
            "/run/ocg-browser/control-token",
            "browser.internal",
            "ws://",
            "6080",
        ],
    );

    let (status, after) = harness
        .get_json(&format!("{}/accounts/{account_id}", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{after}");
    let account: Account = serde_json::from_value(after.clone()).unwrap();
    assert_eq!(account.setup_step, AccountSetupStep::GoogleAccount);
    assert!(!account.enabled);
    assert_eq!(account.revision, before + 1);
    assert!(!profile.join("Cookies").is_file());

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_open_invite_requires_configured_url() {
    let harness = start_loopback("browser-invite").await;
    let launches = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    register_native(&harness, launches.clone(), stops, None);
    let (status, managed) = send_json(
        &harness,
        Method::POST,
        "/accounts/managed",
        &cas(&harness, json!({ "name": "invite" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{managed}");
    let account_id = mutation_account(&managed).id;
    let mut config = harness.state.config();
    config.opencode_invite_url.clear();
    harness.state.set_config(config).unwrap();
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        &cas(&harness, json!({ "target": "invite" })),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    assert_v3_error(&body, ERROR_PRECONDITION_FAILED);
    assert_eq!(launches.load(Ordering::SeqCst), 0);
    assert_eq!(harness.state.settings_revision(), before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_browser_coexists_with_v2() {
    let harness = start_loopback("browser-v2-coexist").await;
    let launches = Arc::new(AtomicUsize::new(0));
    let stops = Arc::new(AtomicUsize::new(0));
    register_native(&harness, launches.clone(), stops.clone(), None);
    let account_id = create_go_account(&harness, "coexist", "sk-v2-coexist").await;

    let v2_caps = harness
        .client
        .get(format!("{}/browser/capabilities", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_caps.status(), StatusCode::OK);
    let v2_caps: Value = v2_caps.json().await.unwrap();
    assert_eq!(v2_caps["mode"], "native");
    assert!(v2_caps.get("processGeneration").is_none());

    let v3_caps = harness
        .get_json(&format!("{}/browser/capabilities", harness.v3_base))
        .await;
    assert_eq!(v3_caps.0, StatusCode::OK);
    assert_eq!(v3_caps.1["mode"], "native");
    assert!(v3_caps.1.get("processGeneration").is_some());
    assert_eq!(v3_caps.1["reason"], Value::Null);

    let v2_open = harness
        .client
        .post(format!("{}/accounts/{account_id}/browser", harness.v2_base))
        .json(&json!({ "target": "console" }))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_open.status(), StatusCode::OK);
    let v2_open: Value = v2_open.json().await.unwrap();
    assert_eq!(v2_open["mode"], "native");
    assert!(v2_open.get("session_token").is_none() || v2_open["session_token"].is_null());
    assert_eq!(launches.load(Ordering::SeqCst), 1);

    let v3_open = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account_id}/browser"),
        &cas(&harness, json!({ "target": "console" })),
    )
    .await;
    assert_eq!(v3_open.0, StatusCode::OK, "{}", v3_open.1);
    assert_eq!(v3_open.1["sessionToken"], Value::Null);
    assert_eq!(launches.load(Ordering::SeqCst), 2);

    let profile = browser_profile_paths(&harness.state.data_dir(), &account_id).unwrap()[0].clone();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), b"v2 reset").unwrap();
    let v2_reset = harness
        .client
        .delete(format!(
            "{}/accounts/{account_id}/browser-profile",
            harness.v2_base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_reset.status(), StatusCode::OK);
    assert!(!profile.exists());
    assert_eq!(stops.load(Ordering::SeqCst), 1);

    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("Cookies"), b"v3 reset").unwrap();
    let v3_reset = send_json(
        &harness,
        Method::DELETE,
        &format!("/accounts/{account_id}/browser-profile"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(v3_reset.0, StatusCode::OK, "{}", v3_reset.1);
    assert!(!profile.exists());
    assert_eq!(stops.load(Ordering::SeqCst), 2);

    harness.stop();
}
