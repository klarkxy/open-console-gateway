//! Dashboard V3 account verify: auth, CAS, Custom probe matrix,
//! revision bump rules, secrecy, and retired V2 paths.

use axum::Router;
use axum::body::Bytes;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, Method as HttpMethod, header};
use axum::response::IntoResponse;
use axum::routing::any;
#[cfg(debug_assertions)]
use ocg_core::custom::CustomVerifyFailure;
use ocg_core::custom::MAX_CUSTOM_VERIFICATION_BODY_BYTES;
#[cfg(debug_assertions)]
use ocg_core::dashboard_v3::install_custom_verify_probe_for_tests;
use ocg_core::dashboard_v3::{
    AccountMutation, AccountVerificationStatus, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST,
    ERROR_MISSING_EXPECTED_REVISION, ERROR_NOT_FOUND, ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED,
    install_official_protocol_fetch_fallback_chat_for_tests,
};
use ocg_core::gateway::provider_adapter::install_goat_catalog_origin_for_test;
use ocg_core::models::{AccountUpdate, ProxyMode};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

const CUSTOM_KEY: &str = "v3-verify-secret-key";
const CUSTOM_MODEL: &str = "custom-local-model";
const CUSTOM_MODEL_2: &str = "custom-other-model";
#[path = "fixtures/probe_response.rs"]
mod probe_response;
const SUCCESS_BODY: &str = probe_response::CHAT;
const LEAKY_401_BODY: &str = r#"{"error":"rejected v3-verify-secret-key"}"#;
const GOAT_MODELS_BODY: &str =
    r#"{"object":"list","data":[{"id":"deepseek/deepseek-v4-flash"},{"id":"claude-sonnet-4-6"}]}"#;

#[derive(Clone, Debug)]
struct CapturedCall {
    method: String,
    path: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
    cookie: Option<String>,
    body: String,
}

struct ProbeOrigin {
    url: String,
    calls: Arc<Mutex<Vec<CapturedCall>>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl ProbeOrigin {
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

async fn start_origin(status: StatusCode, body: &str, delay: Duration) -> ProbeOrigin {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let body = body.to_string();
    let app = Router::new().fallback(any(
        move |method: HttpMethod, uri: OriginalUri, headers: HeaderMap, payload: Bytes| {
            let calls = calls_for_handler.clone();
            let body = if body == SUCCESS_BODY {
                probe_response::for_path(uri.0.path()).to_string()
            } else {
                body.clone()
            };
            async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                calls.lock().unwrap().push(CapturedCall {
                    method: method.to_string(),
                    path: uri.0.path().to_string(),
                    authorization: header_value(&headers, "authorization"),
                    x_api_key: header_value(&headers, "x-api-key"),
                    cookie: header_value(&headers, "cookie"),
                    body: String::from_utf8_lossy(&payload).into_owned(),
                });
                (
                    status,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }
        },
    ));
    serve_app(app, calls).await
}

async fn start_redirect_origin() -> (ProbeOrigin, Arc<AtomicUsize>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let second_hits = Arc::new(AtomicUsize::new(0));
    let calls_for_handler = calls.clone();
    let second_hits_for_handler = second_hits.clone();
    let app = Router::new().fallback(any(
        move |method: HttpMethod, uri: OriginalUri, headers: HeaderMap, payload: Bytes| {
            let calls = calls_for_handler.clone();
            let second_hits = second_hits_for_handler.clone();
            async move {
                let path = uri.0.path().to_string();
                calls.lock().unwrap().push(CapturedCall {
                    method: method.to_string(),
                    path: path.clone(),
                    authorization: header_value(&headers, "authorization"),
                    x_api_key: header_value(&headers, "x-api-key"),
                    cookie: header_value(&headers, "cookie"),
                    body: String::from_utf8_lossy(&payload).into_owned(),
                });
                if path.contains("second") {
                    second_hits.fetch_add(1, Ordering::SeqCst);
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json")],
                        probe_response::for_path(uri.0.path()),
                    )
                        .into_response();
                }
                (
                    StatusCode::FOUND,
                    [(header::LOCATION, "/second")],
                    "redirect",
                )
                    .into_response()
            }
        },
    ));
    let origin = serve_app(app, calls).await;
    (origin, second_hits)
}

async fn serve_app(app: Router, calls: Arc<Mutex<Vec<CapturedCall>>>) -> ProbeOrigin {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown.await;
            })
            .await
            .ok();
    });
    ProbeOrigin {
        url: format!("http://{addr}"),
        calls,
        _stop: stop,
    }
}

struct HeldJsonServer {
    base_url: String,
    hits: Arc<AtomicUsize>,
    release: tokio::sync::watch::Sender<bool>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl HeldJsonServer {
    async fn start(status: u16, body: &str) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("hold listener");
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let (release, release_rx) = tokio::sync::watch::channel(false);
        let (stop, mut shutdown) = tokio::sync::oneshot::channel();
        let body = body.to_string();
        let hits_task = hits.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown => break,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else { break };
                        let hits = hits_task.clone();
                        let body = body.clone();
                        let mut release_rx = release_rx.clone();
                        tokio::spawn(async move {
                            let mut buf = vec![0_u8; 8192];
                            let _ = stream.read(&mut buf).await;
                            hits.fetch_add(1, Ordering::SeqCst);
                            while !*release_rx.borrow() {
                                if release_rx.changed().await.is_err() {
                                    return;
                                }
                            }
                            let response = format!(
                                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        });
                    }
                }
            }
        });
        Self {
            base_url: format!("http://{addr}"),
            hits,
            release,
            stop: Some(stop),
        }
    }

    async fn wait_hits(&self, count: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while self.hits.load(Ordering::SeqCst) < count {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {count} held probes, have {}",
                self.hits.load(Ordering::SeqCst)
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn release(&self) {
        let _ = self.release.send(true);
    }
}

impl Drop for HeldJsonServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

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

async fn send_raw(harness: &V3Harness, path: &str, body: &str) -> (StatusCode, Value) {
    let response = harness
        .client
        .post(format!("{}{path}", harness.v3_base))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

fn verify_path(id: &str) -> String {
    format!("/accounts/{id}/verify")
}

fn assert_v3_error(body: &Value, code: &str) {
    assert_eq!(body["code"], code, "{body}");
    assert!(body.get("message").and_then(Value::as_str).is_some());
    assert!(body.as_object().unwrap().contains_key("currentRevision"));
    assert!(body.as_object().unwrap().contains_key("processGeneration"));
    assert!(body.get("current_revision").is_none());
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

fn assert_secret_free(body: &Value, secrets: &[&str]) {
    for name in json_field_names(body) {
        assert!(
            !matches!(
                name,
                "key"
                    | "password"
                    | "passwordCipher"
                    | "keyCipher"
                    | "gatewayKey"
                    | "gateway_key"
                    | "primaryKey"
                    | "primary_key"
                    | "referralCode"
                    | "referral_code"
                    | "cipher"
                    | "apiKey"
                    | "api_key"
                    | "token"
                    | "secret"
            ),
            "verify payload leaked field {name}: {body}"
        );
    }
    let encoded = body.to_string();
    for secret in secrets {
        assert!(
            !encoded.contains(secret),
            "verify payload leaked credential {secret}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in secrets {
            assert!(
                !value.contains(secret),
                "verify payload leaked credential {secret}: {body}"
            );
        }
    }
}

fn parse_mutation(body: &Value) -> AccountMutation {
    serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("AccountMutation: {body}"))
}

fn mutation_account(body: &Value) -> ocg_core::dashboard_v3::Account {
    parse_mutation(body)
        .account
        .expect("verify mutation should return an account")
}

fn force_direct_proxy(harness: &V3Harness) {
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    config.connect_timeout_secs = 5;
    config.non_stream_timeout_secs = 5;
    harness.state.set_config(config).unwrap();
}

async fn create_go_account(harness: &V3Harness) -> String {
    let (status, created) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(
            harness,
            json!({ "name": "Go verify", "key": "sk-go-verify" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    created["account"]["id"]
        .as_str()
        .expect("created Go account id")
        .to_string()
}

async fn create_goat_account(harness: &V3Harness) -> String {
    let (status, created) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(
            harness,
            json!({
                "name": "GOAT verify",
                "key": "sk-goat-verify",
                "providerId": COMMAND_CODE_PROVIDER_ID,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    created["account"]["id"]
        .as_str()
        .expect("created GOAT account id")
        .to_string()
}

async fn create_custom_account(
    harness: &V3Harness,
    name: &str,
    key: &str,
    base_url: &str,
    protocol: &str,
    _auth_scheme: &str,
    models: &[&str],
) -> String {
    let capabilities: Vec<Value> = models
        .iter()
        .map(|model_id| {
            json!({
                "modelId": model_id,
                "protocol": protocol
            })
        })
        .collect();
    let suffix = match protocol {
        "chat_completions" => "chat/completions",
        "responses" => "responses",
        "messages" => "messages",
        other => panic!("unsupported Custom test protocol {other}"),
    };
    let endpoint_url = format!("{}/{suffix}", base_url.trim_end_matches('/'));
    let (status, created) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(
            harness,
            json!({
                "name": name,
                "key": key,
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": {
                    "endpointUrl": endpoint_url,
                    "upstreamProtocol": protocol
                },
                "modelCapabilities": capabilities
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account = mutation_account(&created);
    assert!(account.enabled);
    assert_eq!(
        account.verification_status,
        AccountVerificationStatus::Pending
    );
    account.id
}

#[tokio::test]
async fn dashboard_v3_account_verify_requires_the_v3_session() {
    let harness = start_public("verify-auth").await;
    let origin = start_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("missing"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);
    assert_eq!(origin.call_count(), 0);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_login_cookie_authorizes_account_verify() {
    let harness = start_public("verify-cookie").await;
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
    let verified = harness
        .client
        .post(format!(
            "{}{}",
            harness.v3_base,
            verify_path(ZEN_FREE_ACCOUNT_ID)
        ))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&cas(&harness, json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(verified.status(), StatusCode::OK);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_account_verify_cas_rejects_missing_malformed_and_stale_before_network() {
    let harness = start_loopback("verify-cas").await;
    force_direct_proxy(&harness);
    let origin = start_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let custom_id = create_custom_account(
        &harness,
        "custom-cas",
        CUSTOM_KEY,
        &origin.url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;
    let before = harness.state.settings_revision();
    let path = verify_path(&custom_id);

    let (status, missing) = send_raw(
        &harness,
        &path,
        &json!({ "processGeneration": harness.state.process_generation() }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    assert_v3_error(&missing, ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, malformed) = send_raw(&harness, &path, "{not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{malformed}");
    assert_v3_error(&malformed, ERROR_INVALID_JSON);

    let (status, unknown) = send_raw(
        &harness,
        &path,
        &cas(&harness, json!({ "key": CUSTOM_KEY })).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unknown}");
    assert_v3_error(&unknown, ERROR_INVALID_JSON);

    let (status, stale) = send_json(
        &harness,
        Method::POST,
        &path,
        &json!({
            "expectedRevision": 1,
            "processGeneration": harness.state.process_generation()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_v3_error(&stale, ERROR_REVISION_CONFLICT);
    assert_eq!(stale["currentRevision"], before);
    assert_eq!(
        stale["processGeneration"],
        harness.state.process_generation()
    );
    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn go_and_zen_verify_are_not_required_no_ops_without_a_revision_bump() {
    let harness = start_loopback("verify-not-required").await;
    let origin = start_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let go_id = create_go_account(&harness).await;
    let before = harness.state.settings_revision();

    let (status, go) = send_json(
        &harness,
        Method::POST,
        &verify_path(&go_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{go}");
    let go = mutation_account(&go);
    assert_eq!(go.id, go_id);
    assert_eq!(go.revision, before);
    assert_eq!(harness.state.settings_revision(), before);
    assert_secret_free(&serde_json::to_value(&go).unwrap(), &["sk-go-verify"]);

    let (status, zen) = send_json(
        &harness,
        Method::POST,
        &verify_path(ZEN_FREE_ACCOUNT_ID),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{zen}");
    let zen = mutation_account(&zen);
    assert_eq!(zen.id, ZEN_FREE_ACCOUNT_ID);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(origin.call_count(), 0);
    harness.stop();
}

#[tokio::test]
async fn unknown_offerings_fail_closed_without_touching_goat_or_upstream() {
    let harness = start_loopback("verify-fail-closed").await;
    force_direct_proxy(&harness);
    let origin = start_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let goat_id = create_goat_account(&harness).await;
    let unknown_id = create_go_account(&harness).await;
    {
        let conn = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
        conn.execute(
            "UPDATE accounts SET provider_id = 'unknown-provider' WHERE id = ?1",
            [&unknown_id],
        )
        .unwrap();
    }
    let before = harness.state.settings_revision();

    let (status, unknown) = send_json(
        &harness,
        Method::POST,
        &verify_path(&unknown_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unknown}");
    assert_v3_error(&unknown, ERROR_INVALID_REQUEST);

    let (status, missing) = send_json(
        &harness,
        Method::POST,
        &verify_path("missing-account"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    assert_v3_error(&missing, ERROR_NOT_FOUND);

    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), before);
    let stored = harness
        .state
        .db
        .lock()
        .get_account(&goat_id)
        .unwrap()
        .unwrap();
    assert!(stored.enabled);
    harness.stop();
}

#[tokio::test]
async fn goat_verify_is_not_applicable_and_never_fetches_the_public_catalog() {
    let harness = start_loopback("verify-goat-not-required").await;
    force_direct_proxy(&harness);
    let origin = start_origin(StatusCode::UNAUTHORIZED, LEAKY_401_BODY, Duration::ZERO).await;
    let _guard = install_goat_catalog_origin_for_test(
        harness.state.process_generation(),
        origin.url.clone(),
    )
    .unwrap();
    let id = create_goat_account(&harness).await;
    let before = harness.state.settings_revision();

    let (status, response) = send_json(
        &harness,
        Method::POST,
        &verify_path(&id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let account = mutation_account(&response);
    assert!(account.enabled);
    assert_eq!(
        account.verification_status,
        AccountVerificationStatus::NotRequired
    );
    assert!(account.connection_verified_at.is_none());
    assert!(account.verification_error.is_none());
    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn provider_model_refresh_uses_go_account_and_public_command_catalog() {
    let harness = start_loopback("provider-model-refresh").await;
    force_direct_proxy(&harness);
    let go_origin = start_origin(
        StatusCode::OK,
        r#"{"object":"list","data":[{"id":"glm-5.3"},{"id":"future-go-model"}]}"#,
        Duration::ZERO,
    )
    .await;

    let mut config = harness.state.config();
    config.upstream_base_url = format!("{}/provider/v1", go_origin.url);
    harness.state.set_config(config).unwrap();

    let go_id = create_go_account(&harness).await;
    harness.enable_account(&go_id);
    let (status, go_models) = send_json(
        &harness,
        Method::POST,
        &format!("/providers/{OPENCODE_PROVIDER_ID}/models/refresh"),
        &cas(&harness, json!({ "accountId": go_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{go_models}");
    assert_eq!(go_models["providerId"], OPENCODE_PROVIDER_ID);
    assert_eq!(
        go_models["sourceUrl"],
        format!("{}/provider/v1/models", go_origin.url)
    );
    assert_eq!(go_models["models"].as_array().unwrap().len(), 2);
    assert_eq!(
        go_origin.calls.lock().unwrap()[0].path,
        "/provider/v1/models"
    );
    assert_eq!(
        go_origin.calls.lock().unwrap()[0].authorization.as_deref(),
        Some("Bearer sk-go-verify")
    );

    let gateway_base = harness.v3_base.strip_suffix("/dashboard/api/v3").unwrap();
    let listed: Value = harness
        .client
        .get(format!("{gateway_base}/v1/models"))
        .bearer_auth(harness.state.config().gateway_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let listed_ids = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect::<Vec<_>>();
    assert!(
        !listed_ids.contains(&"glm-5.3"),
        "refreshed catalog IDs stay unpublished until their protocol is enabled: {listed_ids:?}"
    );
    assert!(
        !harness
            .state
            .provider_contracts()
            .providers
            .get(OPENCODE_PROVIDER_ID)
            .unwrap()
            .catalog
            .models
            .iter()
            .any(|model| model == "grok-4.5"),
        "saved Go catalog is authoritative even when another Provider supplies the Alias"
    );
    assert!(
        !listed_ids.contains(&"future-go-model"),
        "unknown protocols stay visible in Provider catalog but fail closed for routing"
    );

    let goat_origin = start_origin(StatusCode::OK, GOAT_MODELS_BODY, Duration::ZERO).await;
    let _guard = install_goat_catalog_origin_for_test(
        harness.state.process_generation(),
        goat_origin.url.clone(),
    )
    .unwrap();
    let _goat_id = create_goat_account(&harness).await;

    let before = harness.state.settings_revision();
    let (status, goat_models) = send_json(
        &harness,
        Method::POST,
        &format!("/providers/{COMMAND_CODE_PROVIDER_ID}/models/refresh"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{goat_models}");
    assert_eq!(goat_models["providerId"], COMMAND_CODE_PROVIDER_ID);
    assert_eq!(
        goat_models["sourceUrl"],
        format!("{}/provider/v1/models", goat_origin.url)
    );
    assert_eq!(goat_models["models"].as_array().unwrap().len(), 2);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(goat_models["accountId"], Value::Null);
    assert_eq!(goat_origin.calls.lock().unwrap().len(), 1);
    assert!(goat_origin.calls.lock().unwrap().iter().all(|call| {
        call.method == "GET" && call.path == "/provider/v1/models" && call.authorization.is_none()
    }));

    let failed = start_origin(
        StatusCode::BAD_GATEWAY,
        r#"{"error":"down"}"#,
        Duration::ZERO,
    )
    .await;
    let _failed_guard = install_goat_catalog_origin_for_test(
        harness.state.process_generation(),
        failed.url.clone(),
    )
    .unwrap();
    let revision_before_failure = harness.state.settings_revision();
    let (status, failure) = send_json(
        &harness,
        Method::POST,
        &format!("/providers/{COMMAND_CODE_PROVIDER_ID}/models/refresh"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{failure}");
    assert_eq!(harness.state.settings_revision(), revision_before_failure);
    let persisted = harness
        .state
        .provider_contracts()
        .providers
        .get(COMMAND_CODE_PROVIDER_ID)
        .unwrap()
        .catalog
        .models
        .clone();
    assert_eq!(persisted.len(), 2);
    harness.stop();
}

#[tokio::test]
async fn unified_catalog_refresh_selects_an_eligible_account_and_defaults_new_models_off() {
    let harness = start_loopback("unified-provider-catalog-refresh").await;
    force_direct_proxy(&harness);
    let go_origin = start_origin(
        StatusCode::OK,
        r#"{"object":"list","data":[{"id":"glm-5.3"},{"id":"future-go-model"}]}"#,
        Duration::ZERO,
    )
    .await;
    let mut config = harness.state.config();
    config.upstream_base_url = format!("{}/provider/v1", go_origin.url);
    harness.state.set_config(config).unwrap();
    let account_id = create_go_account(&harness).await;
    harness.enable_account(&account_id);
    let _docs =
        install_official_protocol_fetch_fallback_chat_for_tests(harness.state.process_generation());

    let before = harness.state.settings_revision();
    let (status, contracts) = send_json(
        &harness,
        Method::POST,
        "/provider-contracts/provider/opencode/catalog/refresh",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contracts}");
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(go_origin.call_count(), 1);
    assert_eq!(
        go_origin.calls.lock().unwrap()[0].authorization.as_deref(),
        Some("Bearer sk-go-verify")
    );

    let go = contracts["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == OPENCODE_PROVIDER_ID)
        .expect("OpenCode Go provider contract");
    assert_eq!(go["catalog"]["source"], "opencode_get_models");
    assert_eq!(
        go["catalog"]["models"],
        json!(["glm-5.3", "future-go-model"])
    );
    assert!(
        go["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|model| model["modelId"] != "grok-4.5"),
        "the official snapshot replaces the static seed after refresh"
    );
    let future = go["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "future-go-model")
        .expect("new official model stays visible");
    assert_eq!(future["routable"], false);
    for protocol in ["chat_completions", "responses", "messages"] {
        assert_eq!(future["protocols"][protocol]["override"], "force_off");
        assert_eq!(future["protocols"][protocol]["enabled"], false);
    }

    harness.stop();
}

#[tokio::test]
async fn unified_catalog_refresh_uses_a_disabled_ready_key() {
    let harness = start_loopback("unified-catalog-refresh-disabled-key").await;
    force_direct_proxy(&harness);
    let go_origin = start_origin(
        StatusCode::OK,
        r#"{"object":"list","data":[{"id":"glm-5.3"}]}"#,
        Duration::ZERO,
    )
    .await;
    let mut config = harness.state.config();
    config.upstream_base_url = format!("{}/provider/v1", go_origin.url);
    harness.state.set_config(config).unwrap();
    let account_id = create_go_account(&harness).await;
    harness
        .state
        .db
        .lock()
        .update_account(
            &account_id,
            &AccountUpdate {
                enabled: Some(false),
                ..AccountUpdate::default()
            },
            None,
            None,
        )
        .unwrap();
    assert!(
        !harness
            .state
            .db
            .lock()
            .get_account(&account_id)
            .unwrap()
            .unwrap()
            .enabled
    );
    let _docs =
        install_official_protocol_fetch_fallback_chat_for_tests(harness.state.process_generation());

    let (status, contracts) = send_json(
        &harness,
        Method::POST,
        "/provider-contracts/provider/opencode/catalog/refresh",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contracts}");
    assert_eq!(go_origin.call_count(), 1);
    assert_eq!(
        go_origin.calls.lock().unwrap()[0].authorization.as_deref(),
        Some("Bearer sk-go-verify")
    );
    let go = contracts["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == OPENCODE_PROVIDER_ID)
        .expect("OpenCode Go provider contract");
    assert_eq!(go["catalog"]["models"], json!(["glm-5.3"]));
    harness.stop();
}

#[tokio::test]
async fn command_code_contract_refresh_defaults_new_rows_off_and_legacy_route_stays_wire_compatible()
 {
    let harness = start_loopback("command-code-contract-catalog-refresh").await;
    force_direct_proxy(&harness);
    let origin = start_origin(
        StatusCode::OK,
        r#"{"object":"list","data":[{"id":"deepseek/deepseek-v4-flash"},{"id":"future-command-model"}]}"#,
        Duration::ZERO,
    )
    .await;
    let _guard = install_goat_catalog_origin_for_test(
        harness.state.process_generation(),
        origin.url.clone(),
    )
    .unwrap();
    let _goat_id = create_goat_account(&harness).await;
    let _docs =
        install_official_protocol_fetch_fallback_chat_for_tests(harness.state.process_generation());

    let before = harness.state.settings_revision();
    let (status, contracts) = send_json(
        &harness,
        Method::POST,
        &format!("/provider-contracts/provider/{COMMAND_CODE_PROVIDER_ID}/catalog/refresh"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{contracts}");
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(origin.call_count(), 1);
    assert!(origin.calls.lock().unwrap()[0].authorization.is_none());
    assert_eq!(origin.calls.lock().unwrap()[0].path, "/provider/v1/models");

    let command = contracts["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| provider["providerId"] == COMMAND_CODE_PROVIDER_ID)
        .expect("Command Code provider contract");
    assert_eq!(command["catalog"]["source"], "command_code_get_models");
    assert_eq!(
        command["catalog"]["models"],
        json!(["deepseek/deepseek-v4-flash", "future-command-model"])
    );
    let discovered = command["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["modelId"] == "future-command-model")
        .expect("newly discovered Command Code row stays visible");
    assert_eq!(discovered["routable"], false);
    assert_eq!(
        discovered["protocols"]["chat_completions"]["override"],
        "force_off"
    );
    assert_eq!(
        discovered["protocols"]["chat_completions"]["enabled"],
        false
    );

    let (status, legacy) = send_json(
        &harness,
        Method::POST,
        &format!("/providers/{COMMAND_CODE_PROVIDER_ID}/models/refresh"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{legacy}");
    assert_eq!(legacy["providerId"], COMMAND_CODE_PROVIDER_ID);
    assert_eq!(legacy["accountId"], Value::Null);
    assert_eq!(
        legacy["models"],
        json!(["deepseek/deepseek-v4-flash", "future-command-model"])
    );
    assert_eq!(
        legacy["sourceUrl"],
        format!("{}/provider/v1/models", origin.url)
    );
    assert!(
        legacy["refreshedAt"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(legacy["revision"], harness.state.settings_revision());
    assert_eq!(
        legacy["processGeneration"],
        harness.state.process_generation()
    );
    harness.stop();
}

#[tokio::test]
async fn go_model_refresh_filters_zen_free_models_before_persisting() {
    let harness = start_loopback("go-refresh-free-filter").await;
    force_direct_proxy(&harness);
    let go_origin = start_origin(
        StatusCode::OK,
        r#"{"object":"list","data":[{"id":"glm-5.3"},{"id":"hy3-free"}]}"#,
        Duration::ZERO,
    )
    .await;
    let mut config = harness.state.config();
    config.upstream_base_url = format!("{}/provider/v1", go_origin.url);
    harness.state.set_config(config).unwrap();

    let go_id = create_go_account(&harness).await;
    harness.enable_account(&go_id);
    let (status, go_models) = send_json(
        &harness,
        Method::POST,
        &format!("/providers/{OPENCODE_PROVIDER_ID}/models/refresh"),
        &cas(&harness, json!({ "accountId": go_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{go_models}");
    assert_eq!(
        go_models["models"],
        json!(["glm-5.3"]),
        "Zen Free ids must be filtered out of the persisted Go catalog"
    );

    // The persisted catalog (and therefore the reloaded routing view) is
    // filtered, so future rebuilds cannot surface Zen Free ids under Go.
    let contracts = harness.state.provider_contracts();
    let go_scope = contracts
        .providers
        .get(OPENCODE_PROVIDER_ID)
        .expect("go scope");
    assert_eq!(go_scope.catalog.models, vec!["glm-5.3".to_string()]);
    assert!(!go_scope.models.contains_key("hy3-free"));
    harness.stop();
}

#[tokio::test]
async fn custom_verify_success_persists_verified_without_flipping_enable_and_bumps_once() {
    let harness = start_loopback("verify-custom-ok").await;
    force_direct_proxy(&harness);
    let origin = start_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let id = create_custom_account(
        &harness,
        "custom-ok",
        CUSTOM_KEY,
        &origin.url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL, CUSTOM_MODEL_2],
    )
    .await;
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path(&id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let account = mutation_account(&body);
    assert!(
        account.enabled,
        "verify must not change the card's enabled state"
    );
    assert_eq!(
        account.verification_status,
        AccountVerificationStatus::Verified
    );
    assert!(account.connection_verified_at.is_some());
    assert!(account.verification_error.is_none());
    assert_eq!(account.revision, before + 1);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_secret_free(&body, &[CUSTOM_KEY]);

    let calls = origin.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].method, "POST");
    assert_eq!(calls[0].path, "/chat/completions");
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some("Bearer v3-verify-secret-key")
    );
    assert!(calls[0].x_api_key.is_none());
    assert!(calls[0].cookie.is_none());
    assert!(calls[0].body.contains(CUSTOM_MODEL));
    assert!(!calls[0].body.contains(CUSTOM_MODEL_2));
    assert!(calls[0].body.contains("\"stream\":false"));

    let again_before = harness.state.settings_revision();
    let (status, again) = send_json(
        &harness,
        Method::POST,
        &verify_path(&id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        mutation_account(&again).verification_status,
        AccountVerificationStatus::Verified
    );
    assert_eq!(harness.state.settings_revision(), again_before);
    assert_eq!(origin.call_count(), 1);
    harness.stop();
}

#[tokio::test]
async fn custom_verify_x_api_key_does_not_forward_dashboard_auth() {
    let harness = start_loopback("verify-custom-x-api").await;
    force_direct_proxy(&harness);
    let origin = start_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let id = create_custom_account(
        &harness,
        "custom-x",
        CUSTOM_KEY,
        &origin.url,
        "messages",
        "x-api-key",
        &[CUSTOM_MODEL],
    )
    .await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path(&id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        mutation_account(&body).verification_status,
        AccountVerificationStatus::Verified
    );
    let calls = origin.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].path, "/messages");
    assert!(calls[0].authorization.is_none());
    assert_eq!(calls[0].x_api_key.as_deref(), Some(CUSTOM_KEY));
    assert!(calls[0].cookie.is_none());
    assert_secret_free(&body, &[CUSTOM_KEY]);
    harness.stop();
}

#[tokio::test]
async fn custom_verify_probes_only_the_single_declared_protocol() {
    let harness = start_loopback("verify-custom-single").await;
    force_direct_proxy(&harness);
    let origin = start_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "custom-single",
                "key": CUSTOM_KEY,
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": {
                    "endpointUrl": format!("{}/messages", origin.url),
                    "upstreamProtocol": "messages"
                },
                "modelCapabilities": [
                    { "modelId": CUSTOM_MODEL, "protocol": "messages" }
                ]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = created["account"]["id"].as_str().unwrap().to_string();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path(&id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        mutation_account(&body).verification_status,
        AccountVerificationStatus::Verified
    );
    let calls = origin.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1, "exactly one probe is allowed: {calls:?}");
    assert_eq!(calls[0].method, "POST");
    assert_eq!(calls[0].path, "/messages");
    assert!(
        calls
            .iter()
            .all(|call| call.body.contains(CUSTOM_MODEL) && call.body.contains("\"stream\":false")),
        "the selected protocol probes the first declared model: {calls:?}"
    );
    assert_secret_free(&body, &[CUSTOM_KEY]);
    harness.stop();
}

#[tokio::test]
async fn custom_verify_failure_401_429_redirect_and_oversize_persist_failed_without_flipping_enable()
 {
    let harness = start_loopback("verify-custom-fail").await;
    force_direct_proxy(&harness);

    let unauthorized = start_origin(StatusCode::UNAUTHORIZED, LEAKY_401_BODY, Duration::ZERO).await;
    let id_401 = create_custom_account(
        &harness,
        "custom-401",
        CUSTOM_KEY,
        &unauthorized.url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;
    let before = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path(&id_401),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(status, StatusCode::UNAUTHORIZED);
    let account = mutation_account(&body);
    assert!(
        account.enabled,
        "failed verify must not change the card's enabled state"
    );
    assert_eq!(
        account.verification_status,
        AccountVerificationStatus::Failed
    );
    assert!(
        account
            .verification_error
            .as_deref()
            .is_some_and(|error| error.contains("401"))
    );
    assert_secret_free(&body, &[CUSTOM_KEY, "rejected v3-verify-secret-key"]);
    assert_eq!(harness.state.settings_revision(), before + 1);

    let limited = start_origin(
        StatusCode::TOO_MANY_REQUESTS,
        "5-hour usage limit reached. Resets in 13min.",
        Duration::ZERO,
    )
    .await;
    let id_429 = create_custom_account(
        &harness,
        "custom-429",
        CUSTOM_KEY,
        &limited.url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path(&id_429),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let account = mutation_account(&body);
    assert_eq!(
        account.verification_status,
        AccountVerificationStatus::Failed
    );
    assert!(account.cooldown_generic_until.is_none());
    assert!(account.cooldown_5h_until.is_none());
    assert!(
        account
            .verification_error
            .as_deref()
            .is_some_and(|error| error.contains("429"))
    );

    let (redirect, second_hits) = start_redirect_origin().await;
    let id_redirect = create_custom_account(
        &harness,
        "custom-redirect",
        CUSTOM_KEY,
        &redirect.url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path(&id_redirect),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        mutation_account(&body).verification_status,
        AccountVerificationStatus::Failed
    );
    assert_eq!(second_hits.load(Ordering::SeqCst), 0);

    let pad = "x".repeat(MAX_CUSTOM_VERIFICATION_BODY_BYTES);
    let huge = format!(r#"{{"id":"ok","pad":"{pad}"}}"#);
    let oversize = start_origin(StatusCode::OK, &huge, Duration::ZERO).await;
    let id_oversize = create_custom_account(
        &harness,
        "custom-oversize",
        CUSTOM_KEY,
        &oversize.url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path(&id_oversize),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let account = mutation_account(&body);
    assert!(
        account.enabled,
        "failed verify must not change the card's enabled state"
    );
    assert_eq!(
        account.verification_status,
        AccountVerificationStatus::Failed
    );
    assert!(
        account
            .verification_error
            .as_deref()
            .is_some_and(|error| error.contains("exceeded"))
    );
    harness.stop();
}

#[tokio::test]
async fn stale_after_network_does_not_commit_or_bump() {
    let held = HeldJsonServer::start(200, SUCCESS_BODY).await;
    let harness = start_loopback("verify-stale-after").await;
    force_direct_proxy(&harness);
    let id = create_custom_account(
        &harness,
        "custom-stale-after",
        CUSTOM_KEY,
        &held.base_url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;
    let before = harness.state.settings_revision();
    let verify = tokio::spawn({
        let client = harness.client.clone();
        let url = format!("{}{}", harness.v3_base, verify_path(&id));
        let body = cas(&harness, json!({}));
        async move { client.post(url).json(&body).send().await.unwrap() }
    });
    held.wait_hits(1).await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(&harness, json!({ "name": "other", "key": "sk-other" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let bumped = harness.state.settings_revision();
    assert_eq!(bumped, before + 1);
    held.release();
    let response = verify.await.unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], bumped);
    assert_eq!(harness.state.settings_revision(), bumped);
    let stored = harness.state.db.lock().get_account(&id).unwrap().unwrap();
    let verification = harness
        .state
        .db
        .lock()
        .account_verification_state(&id)
        .unwrap()
        .unwrap();
    assert!(stored.enabled);
    assert_eq!(
        verification.status,
        ocg_core::provider::ConnectionVerificationStatus::Pending
    );
    harness.stop();
}

#[tokio::test]
async fn concurrent_custom_verifies_certify_once() {
    let held = HeldJsonServer::start(200, SUCCESS_BODY).await;
    let harness = start_loopback("verify-concurrent").await;
    force_direct_proxy(&harness);
    let id = create_custom_account(
        &harness,
        "custom-concurrent",
        CUSTOM_KEY,
        &held.base_url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;
    let before = harness.state.settings_revision();
    let body = cas(&harness, json!({}));
    let spawn_verify = || {
        let client = harness.client.clone();
        let url = format!("{}{}", harness.v3_base, verify_path(&id));
        let body = body.clone();
        async move { client.post(url).json(&body).send().await.unwrap() }
    };
    let first = tokio::spawn(spawn_verify());
    let second = tokio::spawn(spawn_verify());
    held.wait_hits(2).await;
    held.release();
    let first = first.await.unwrap();
    let second = second.await.unwrap();
    let first_status = first.status();
    let second_status = second.status();
    let first_body: Value = first.json().await.unwrap_or(Value::Null);
    let second_body: Value = second.json().await.unwrap_or(Value::Null);
    let (winner_status, winner_body, loser_status, loser_body) = if first_status.is_success() {
        (first_status, first_body, second_status, second_body)
    } else {
        (second_status, second_body, first_status, first_body)
    };
    assert_eq!(winner_status, StatusCode::OK, "{winner_body}");
    assert_eq!(loser_status, StatusCode::CONFLICT, "{loser_body}");
    assert_v3_error(&loser_body, ERROR_REVISION_CONFLICT);
    assert_eq!(harness.state.settings_revision(), before + 1);
    let verification = harness
        .state
        .db
        .lock()
        .account_verification_state(&id)
        .unwrap()
        .unwrap();
    assert_eq!(
        verification.status,
        ocg_core::provider::ConnectionVerificationStatus::Verified
    );
    harness.stop();
}

#[tokio::test]
async fn retired_v2_account_verify_does_not_mutate() {
    let harness = start_loopback("verify-v2-retired").await;
    force_direct_proxy(&harness);
    let origin = start_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let goat_id = create_goat_account(&harness).await;
    let custom_id = create_custom_account(
        &harness,
        "custom-v2",
        CUSTOM_KEY,
        &origin.url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;

    let v2_goat = harness
        .client
        .post(format!("{}/accounts/{goat_id}/verify", harness.v2_base))
        .json(&json!({ "expected_revision": harness.state.settings_revision() }))
        .send()
        .await
        .unwrap();
    V3Harness::assert_v2_removed(v2_goat.status(), &v2_goat.json().await.unwrap());
    let stored_goat = harness
        .state
        .db
        .lock()
        .get_account(&goat_id)
        .unwrap()
        .unwrap();
    assert!(stored_goat.enabled);

    let v2_custom = harness
        .client
        .post(format!("{}/accounts/{custom_id}/verify", harness.v2_base))
        .json(&json!({ "expected_revision": harness.state.settings_revision() }))
        .send()
        .await
        .unwrap();
    V3Harness::assert_v2_removed(v2_custom.status(), &v2_custom.json().await.unwrap());
    let (status, v3_custom) = send_json(
        &harness,
        Method::POST,
        &verify_path(&custom_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v3_custom}");
    assert_eq!(
        mutation_account(&v3_custom).verification_status,
        ocg_core::dashboard_v3::AccountVerificationStatus::Verified
    );
    assert!(mutation_account(&v3_custom).enabled);
    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn custom_verify_debug_seam_is_process_generation_keyed_and_loopback_only() {
    let first = start_loopback("verify-seam-a").await;
    let second = start_loopback("verify-seam-b").await;
    force_direct_proxy(&first);
    force_direct_proxy(&second);
    let origin = start_origin(StatusCode::OK, SUCCESS_BODY, Duration::ZERO).await;
    let first_id = create_custom_account(
        &first,
        "custom-seam-a",
        CUSTOM_KEY,
        &origin.url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;
    let second_id = create_custom_account(
        &second,
        "custom-seam-b",
        CUSTOM_KEY,
        &origin.url,
        "chat_completions",
        "bearer",
        &[CUSTOM_MODEL],
    )
    .await;
    assert_ne!(
        first.state.process_generation(),
        second.state.process_generation()
    );

    let _guard = install_custom_verify_probe_for_tests(
        first.state.process_generation(),
        |_config, _custom, _capability, _key| {
            Err(CustomVerifyFailure {
                message: "first-harness-probe".into(),
            })
        },
    );

    let (status, first_body) = send_json(
        &first,
        Method::POST,
        &verify_path(&first_id),
        &cas(&first, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{first_body}");
    let first_account = mutation_account(&first_body);
    assert_eq!(
        first_account.verification_status,
        AccountVerificationStatus::Failed
    );
    assert_eq!(
        first_account.verification_error.as_deref(),
        Some("first-harness-probe")
    );

    let (status, second_body) = send_json(
        &second,
        Method::POST,
        &verify_path(&second_id),
        &cas(&second, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second_body}");
    assert_eq!(
        mutation_account(&second_body).verification_status,
        AccountVerificationStatus::Verified
    );
    assert_eq!(origin.call_count(), 1);
    first.stop();
    second.stop();
}
