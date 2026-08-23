//! Dashboard V3 POST `/accounts/{id}/setup/verify-key`: session, CAS, V2
//! onboarding semantics, secrecy, and V2 coexistence.

use chrono::Utc;
#[cfg(debug_assertions)]
use ocg_core::dashboard_v3::{
    AccountMutation, AccountSetupStep, AccountVerificationStatus, ERROR_OUTBOUND_FAILED,
};
use ocg_core::dashboard_v3::{
    ERROR_CONFLICT, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION,
    ERROR_NOT_FOUND, ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED,
};
use ocg_core::db::CURRENT_SCHEMA_VERSION;
#[cfg(debug_assertions)]
use ocg_core::models::DEFAULT_ACCOUNT_TEST_MODEL;
use ocg_core::models::{
    Account as ModelAccount, AccountSetupStep as ModelSetupStep, AccountType as ModelAccountType,
};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, ConnectionVerificationStatus, GO_OFFERING_ID, GOAT_OFFERING_ID,
    OPENCODE_PROVIDER_ID, SCNET_PROVIDER_ID, SCNET_TOKEN_PLAN_BASIC_OFFERING_ID,
};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
#[cfg(debug_assertions)]
use std::sync::{Arc, Mutex};
#[cfg(debug_assertions)]
use std::time::Duration;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

#[cfg(debug_assertions)]
use axum::Router;
#[cfg(debug_assertions)]
use axum::body::Bytes;
#[cfg(debug_assertions)]
use axum::extract::OriginalUri;
#[cfg(debug_assertions)]
use axum::http::{HeaderMap, Method as HttpMethod};
#[cfg(debug_assertions)]
use axum::response::IntoResponse;
#[cfg(debug_assertions)]
use axum::routing::any;
#[cfg(debug_assertions)]
use ocg_core::dashboard_v3::install_managed_key_verify_target_for_tests;
#[cfg(debug_assertions)]
use ocg_core::models::{ProxyListDirection, ProxyMode};

const OPAQUE_KEY: &str = "opaque/account+key=42";
#[cfg(debug_assertions)]
const BODY_SECRET: &str = "sk-secret-upstream-body";

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

fn verify_path(id: &str) -> String {
    format!("/accounts/{id}/setup/verify-key")
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

fn assert_secret_free(body: &Value, extra: &[&str]) {
    for name in json_field_names(body) {
        assert!(
            !SECRET_FIELD_NAMES.contains(&name),
            "verify-key JSON leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in extra {
            assert!(
                !value.contains(secret),
                "verify-key JSON leaked secret sample {secret}: {body}"
            );
        }
    }
    let encoded = body.to_string();
    for secret in extra {
        assert!(
            !encoded.contains(secret),
            "verify-key JSON leaked secret sample {secret} in encoded JSON: {body}"
        );
    }
}

fn managed_waiting(id: &str) -> ModelAccount {
    let now = Utc::now();
    ModelAccount {
        id: id.into(),
        provider_id: OPENCODE_PROVIDER_ID.into(),
        offering_id: GO_OFFERING_ID.into(),
        credential_kind: ocg_core::provider::default_credential_kind(),
        quota_scope: ocg_core::provider::default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: String::new(),
        enabled: false,
        account_type: ModelAccountType::Managed,
        setup_step: ModelSetupStep::KeyVerification,
        referral_code: None,
        purchase_date: "2026-06-15".into(),
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
        created_at: now,
        updated_at: now,
    }
}

fn insert_managed_waiting(harness: &V3Harness, id: &str) {
    harness
        .state
        .db
        .lock()
        .create_account(&managed_waiting(id))
        .unwrap();
}

fn stored_account(harness: &V3Harness, id: &str) -> ModelAccount {
    harness
        .state
        .db
        .lock()
        .get_account(id)
        .unwrap()
        .expect("account should exist")
}

fn assert_still_pending(harness: &V3Harness, id: &str, expected_revision: u64) {
    let stored = stored_account(harness, id);
    assert_eq!(stored.setup_step, ModelSetupStep::KeyVerification);
    assert!(!stored.enabled);
    assert_eq!(harness.state.settings_revision(), expected_revision);
    assert_ne!(stored.key_cipher, OPAQUE_KEY);
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
}

fn assert_retained_key(harness: &V3Harness, id: &str, expected_plaintext: &str) {
    let stored = stored_account(harness, id);
    assert!(!stored.key_cipher.is_empty());
    assert_eq!(
        harness.state.decrypt_key(&stored.key_cipher).unwrap(),
        expected_plaintext
    );
}

#[cfg(debug_assertions)]
#[derive(Clone, Debug)]
struct CapturedCall {
    method: String,
    path: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
    x_goog_api_key: Option<String>,
    cookie: Option<String>,
    body: String,
}

#[cfg(debug_assertions)]
struct VerifyOrigin {
    url: String,
    calls: Arc<Mutex<Vec<CapturedCall>>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

#[cfg(debug_assertions)]
impl VerifyOrigin {
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[cfg(debug_assertions)]
fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[cfg(debug_assertions)]
struct OriginSpec {
    status: StatusCode,
    body: String,
    location: Option<String>,
    hold: Option<Arc<Hold>>,
    delay: Duration,
}

#[cfg(debug_assertions)]
struct Hold {
    received: tokio::sync::watch::Sender<usize>,
    release: tokio::sync::Notify,
}

#[cfg(debug_assertions)]
impl Hold {
    fn new() -> Arc<Self> {
        let (received, _) = tokio::sync::watch::channel(0);
        Arc::new(Self {
            received,
            release: tokio::sync::Notify::new(),
        })
    }

    async fn wait_until_received(&self) {
        self.wait_until_received_n(1).await;
    }

    async fn wait_until_received_n(&self, n: usize) {
        let mut rx = self.received.subscribe();
        while *rx.borrow_and_update() < n {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    fn mark_received(&self) {
        self.received.send_modify(|count| *count += 1);
    }

    fn release(&self) {
        self.release.notify_waiters();
    }
}

#[cfg(debug_assertions)]
async fn start_origin(status: StatusCode, body: impl Into<String>) -> VerifyOrigin {
    start_origin_with(OriginSpec {
        status,
        body: body.into(),
        location: None,
        hold: None,
        delay: Duration::ZERO,
    })
    .await
}

#[cfg(debug_assertions)]
async fn start_origin_with(spec: OriginSpec) -> VerifyOrigin {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let app = Router::new().fallback(any(
        move |method: HttpMethod, uri: OriginalUri, headers: HeaderMap, payload: Bytes| {
            let calls = calls_for_handler.clone();
            let spec_status = spec.status;
            let spec_body = spec.body.clone();
            let spec_location = spec.location.clone();
            let hold = spec.hold.clone();
            let delay = spec.delay;
            async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                if let Some(hold) = hold.as_ref() {
                    // Subscribe before signalling so notify_waiters cannot be lost.
                    let notified = hold.release.notified();
                    hold.mark_received();
                    notified.await;
                }
                calls.lock().unwrap().push(CapturedCall {
                    method: method.to_string(),
                    path: uri.0.path().to_string(),
                    authorization: header_value(&headers, "authorization"),
                    x_api_key: header_value(&headers, "x-api-key"),
                    x_goog_api_key: header_value(&headers, "x-goog-api-key"),
                    cookie: header_value(&headers, "cookie"),
                    body: String::from_utf8_lossy(&payload).into_owned(),
                });
                if let Some(location) = spec_location {
                    return (
                        StatusCode::FOUND,
                        [(axum::http::header::LOCATION, location)],
                        spec_body,
                    )
                        .into_response();
                }
                (spec_status, spec_body).into_response()
            }
        },
    ));
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
    VerifyOrigin {
        url: format!("http://{addr}/"),
        calls,
        _stop: stop,
    }
}

#[cfg(debug_assertions)]
async fn closed_proxy_addr() -> String {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    format!("http://{address}")
}

#[cfg(debug_assertions)]
fn force_direct_proxy(harness: &V3Harness) {
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
}

#[cfg(debug_assertions)]
fn assert_no_secret_logs(harness: &V3Harness, extra: &[&str]) {
    let logs = harness.state.db.lock().list_gateway_logs(50).unwrap();
    for log in logs {
        for secret in extra {
            assert!(
                !log.message.contains(secret),
                "gateway log leaked secret sample {secret}: {}",
                log.message
            );
        }
    }
}

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
}

#[test]
fn dashboard_v3_managed_key_verify_source_gates_the_debug_seam() {
    let source = include_str!("../src/dashboard_v3/managed_key_verify.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("production source precedes tests");
    assert!(production.contains("configured_builder(&prepared.config)"));
    assert!(production.contains("no_redirect_policy"));
    assert!(!production.contains("upstream_context"));
    assert!(production.contains("#[cfg(debug_assertions)]"));
    assert!(production.contains("#[cfg(not(debug_assertions))]"));
    let release_idx = production
        .find("#[cfg(not(debug_assertions))]")
        .expect("release path");
    let release = &production[release_idx..];
    assert!(!release.contains("install_managed_key_verify_target_for_tests"));
    assert!(!release.contains("MANAGED_KEY_VERIFY_TARGET_OVERRIDES"));
}

#[tokio::test]
async fn dashboard_v3_managed_key_verify_requires_the_v3_session() {
    let harness = start_public("verify-key-auth").await;
    insert_managed_waiting(&harness, "managed-1");

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("managed-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);
    assert_secret_free(&body, &[OPAQUE_KEY]);
    assert_eq!(
        stored_account(&harness, "managed-1").setup_step,
        ModelSetupStep::KeyVerification
    );

    let v2 = harness
        .client
        .post(format!(
            "{}/accounts/managed-1/setup/verify-key",
            harness.v2_base
        ))
        .json(&json!({ "key": OPAQUE_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::UNAUTHORIZED);
    let v2_body = v2.text().await.unwrap();
    assert!(
        v2_body.is_empty(),
        "V2 session middleware must stay an empty 401, got {v2_body}"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_loopback_trust_is_fail_closed_when_forwarded_headers_are_present() {
    let harness = start_loopback("verify-key-forwarded").await;
    insert_managed_waiting(&harness, "managed-1");
    let before = harness.state.settings_revision();

    let response = harness
        .client
        .post(format!("{}{}", harness.v3_base, verify_path("managed-1")))
        .header("x-forwarded-for", "203.0.113.10")
        .json(&cas(&harness, json!({ "key": OPAQUE_KEY })))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.unwrap_or(Value::Null);
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    assert_secret_free(&body, &[OPAQUE_KEY]);
    assert_still_pending(&harness, "managed-1", before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_login_cookie_authorizes_managed_key_verify() {
    let harness = start_public("verify-key-cookie").await;
    insert_managed_waiting(&harness, "managed-1");
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

    let unauthorized = harness
        .client
        .post(format!("{}{}", harness.v3_base, verify_path("managed-1")))
        .json(&cas(&harness, json!({ "key": OPAQUE_KEY })))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let authorized = harness
        .client
        .post(format!("{}{}", harness.v3_base, verify_path("managed-1")))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({
            "expectedRevision": harness.state.settings_revision(),
            "processGeneration": harness.state.process_generation()
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::BAD_REQUEST);
    let body: Value = authorized.json().await.unwrap();
    assert_v3_error(&body, ERROR_INVALID_JSON);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_managed_key_verify_rejects_unknown_missing_and_empty_fields() {
    let harness = start_loopback("verify-key-validate").await;
    insert_managed_waiting(&harness, "managed-1");
    let before = harness.state.settings_revision();
    let path = verify_path("managed-1");

    let (status, body) = send_raw(&harness, &path, "not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_still_pending(&harness, "managed-1", before);

    let (status, body) = send_json(&harness, Method::POST, &path, &json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &path,
        &json!({
            "expectedRevision": before,
            "key": OPAQUE_KEY
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_secret_free(&body, &[OPAQUE_KEY]);

    let (status, body) = send_json(&harness, Method::POST, &path, &cas(&harness, json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &path,
        &cas(
            &harness,
            json!({
                "key": OPAQUE_KEY,
                "setupStep": "key_verification"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_secret_free(&body, &[OPAQUE_KEY]);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &path,
        &cas(&harness, json!({ "key": "   " })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(body["message"], "key is required");

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &path,
        &cas(&harness, json!({ "key": "k".repeat(4097) })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(body["message"], "key is too long");
    assert_still_pending(&harness, "managed-1", before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_managed_key_verify_rejects_unknown_wrong_step_and_unroutable_without_network()
{
    let harness = start_loopback("verify-key-eligibility").await;
    insert_managed_waiting(&harness, "managed-1");
    let mut draft = managed_waiting("draft-1");
    draft.setup_step = ModelSetupStep::Payment;
    harness.state.db.lock().create_account(&draft).unwrap();
    let mut key_acct = managed_waiting("key-1");
    key_acct.account_type = ModelAccountType::Key;
    key_acct.setup_step = ModelSetupStep::Ready;
    key_acct.key_cipher = "cipher-key-1".into();
    key_acct.enabled = true;
    harness.state.db.lock().create_account(&key_acct).unwrap();
    let mut goat = managed_waiting("goat-1");
    goat.provider_id = COMMAND_CODE_PROVIDER_ID.into();
    goat.offering_id = GOAT_OFFERING_ID.into();
    harness.state.db.lock().create_account(&goat).unwrap();
    let mut scnet = managed_waiting("scnet-1");
    scnet.provider_id = SCNET_PROVIDER_ID.into();
    scnet.offering_id = SCNET_TOKEN_PLAN_BASIC_OFFERING_ID.into();
    harness.state.db.lock().create_account(&scnet).unwrap();
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("missing"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_v3_error(&body, ERROR_NOT_FOUND);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("draft-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_CONFLICT);
    assert_eq!(
        body["message"],
        "managed account is not waiting for key verification"
    );
    assert_eq!(
        stored_account(&harness, "draft-1").setup_step,
        ModelSetupStep::Payment
    );

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("key-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_CONFLICT);
    assert!(stored_account(&harness, "key-1").enabled);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("goat-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_CONFLICT);
    let goat_stored = stored_account(&harness, "goat-1");
    assert!(!goat_stored.enabled);
    assert_eq!(goat_stored.setup_step, ModelSetupStep::KeyVerification);
    assert!(goat_stored.key_cipher.is_empty());

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("scnet-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_CONFLICT);
    let scnet_stored = stored_account(&harness, "scnet-1");
    assert!(!scnet_stored.enabled);
    assert_eq!(scnet_stored.setup_step, ModelSetupStep::KeyVerification);
    assert!(scnet_stored.key_cipher.is_empty());
    assert_eq!(harness.state.settings_revision(), before);
    assert_secret_free(&body, &[OPAQUE_KEY]);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_revision_or_generation_rejects_before_network() {
    let harness = start_loopback("verify-key-stale-before").await;
    insert_managed_waiting(&harness, "managed-1");
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("managed-1"),
        &json!({
            "expectedRevision": before.saturating_sub(1),
            "processGeneration": generation,
            "key": OPAQUE_KEY
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], before);
    assert_still_pending(&harness, "managed-1", before);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("managed-1"),
        &json!({
            "expectedRevision": before,
            "processGeneration": generation.wrapping_add(1),
            "key": OPAQUE_KEY
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_still_pending(&harness, "managed-1", before);
    assert_secret_free(&body, &[OPAQUE_KEY]);

    harness.stop();
}

#[cfg(debug_assertions)]
#[derive(Clone, Copy)]
enum OutcomeKind {
    Ready,
    ReadyCooldown,
    AuthFail,
    ClientFail,
    Outbound,
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_managed_key_verify_success_401_429_5xx_network_and_oversize() {
    let harness = start_loopback("verify-key-outcomes").await;
    force_direct_proxy(&harness);

    for (label, status, body, kind) in [
        (
            "ok",
            StatusCode::OK,
            r#"{"choices":[]}"#.to_string(),
            OutcomeKind::Ready,
        ),
        (
            "rate-limited",
            StatusCode::TOO_MANY_REQUESTS,
            format!(r#"{{"error":{{"message":"weekly usage limit reached for {OPAQUE_KEY}"}}}}"#),
            OutcomeKind::ReadyCooldown,
        ),
        (
            "unauthorized",
            StatusCode::UNAUTHORIZED,
            format!(r#"{{"error":{{"message":"invalid key {OPAQUE_KEY}"}}}}"#),
            OutcomeKind::AuthFail,
        ),
        (
            "forbidden",
            StatusCode::FORBIDDEN,
            format!(r#"{{"error":{{"message":"forbidden key {OPAQUE_KEY}"}}}}"#),
            OutcomeKind::AuthFail,
        ),
        (
            "client-error",
            StatusCode::BAD_REQUEST,
            format!(r#"{{"error":{{"message":"bad ping {OPAQUE_KEY}"}}}}"#),
            OutcomeKind::ClientFail,
        ),
        (
            "server-error",
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(r#"{{"error":{{"message":"temporary failure for {OPAQUE_KEY}"}}}}"#),
            OutcomeKind::Outbound,
        ),
    ] {
        let origin = start_origin(status, body).await;
        let _target = install_managed_key_verify_target_for_tests(
            harness.state.process_generation(),
            origin.url.clone(),
        );
        insert_managed_waiting(&harness, label);
        if label == "ok" {
            let db = harness.state.db.lock();
            db.set_account_rate_limit(
                label,
                Utc::now() + chrono::Duration::hours(2),
                "old cooldown",
                None,
            )
            .unwrap();
            db.set_account_auth_error(label, Some("old auth error"))
                .unwrap();
            assert!(
                db.set_account_verification(
                    label,
                    ConnectionVerificationStatus::Pending,
                    None,
                    Some("old verification error"),
                )
                .unwrap()
            );
        }
        let before = harness.state.settings_revision();
        let (http_status, response) = send_json(
            &harness,
            Method::POST,
            &verify_path(label),
            &cas(&harness, json!({ "key": OPAQUE_KEY })),
        )
        .await;
        let stored = stored_account(&harness, label);
        assert_ne!(stored.key_cipher, OPAQUE_KEY);
        assert!(!stored.key_cipher.is_empty());
        assert_eq!(origin.call_count(), 1);
        let captured = origin.calls.lock().unwrap()[0].clone();
        assert_eq!(captured.method, "POST");
        assert_eq!(captured.path, "/v1/chat/completions");
        assert_eq!(
            captured.authorization.as_deref(),
            Some(format!("Bearer {OPAQUE_KEY}").as_str())
        );
        assert!(captured.x_api_key.is_none());
        assert!(captured.x_goog_api_key.is_none());
        assert!(captured.cookie.is_none());
        let ping: Value = serde_json::from_str(&captured.body).unwrap();
        assert_eq!(ping["model"], DEFAULT_ACCOUNT_TEST_MODEL);
        assert_eq!(ping["stream"], false);
        assert_eq!(ping["max_tokens"], 1);
        assert_secret_free(&response, &[OPAQUE_KEY, BODY_SECRET]);
        match kind {
            OutcomeKind::Ready | OutcomeKind::ReadyCooldown => {
                assert_eq!(http_status, StatusCode::OK, "{response}");
                let parsed: AccountMutation = serde_json::from_value(response.clone()).unwrap();
                let account = parsed.account.expect("verified account");
                assert_eq!(account.setup_step, AccountSetupStep::Ready);
                assert!(account.enabled);
                assert_eq!(parsed.revision, before + 1);
                assert_eq!(
                    parsed.process_generation,
                    harness.state.process_generation()
                );
                assert_eq!(harness.state.settings_revision(), before + 1);
                assert_eq!(stored.setup_step, ModelSetupStep::Ready);
                assert!(stored.enabled);
                if matches!(kind, OutcomeKind::ReadyCooldown) {
                    assert!(stored.cooldown_until.is_some());
                    assert!(stored.cooldown_week_until.is_some());
                } else {
                    assert!(stored.cooldown_until.is_none());
                    assert!(stored.cooldown_generic_until.is_none());
                    assert!(stored.cooldown_5h_until.is_none());
                    assert!(stored.cooldown_week_until.is_none());
                    assert!(stored.cooldown_month_until.is_none());
                    assert!(stored.cooldown_free_until.is_none());
                    assert!(stored.auth_error.is_none());
                    assert_eq!(
                        account.verification_status,
                        AccountVerificationStatus::Verified
                    );
                    assert!(account.connection_verified_at.is_some());
                    assert!(account.verification_error.is_none());
                }
            }
            OutcomeKind::AuthFail => {
                assert_eq!(http_status, StatusCode::BAD_REQUEST, "{response}");
                assert_v3_error(&response, ERROR_INVALID_REQUEST);
                assert!(stored.auth_error.is_some());
                assert_eq!(response["currentRevision"], before + 1);
                assert_still_pending(&harness, label, before + 1);
                assert_retained_key(&harness, label, OPAQUE_KEY);
            }
            OutcomeKind::ClientFail => {
                assert_eq!(http_status, StatusCode::BAD_REQUEST, "{response}");
                assert_v3_error(&response, ERROR_INVALID_REQUEST);
                assert_eq!(response["currentRevision"], before + 1);
                assert_still_pending(&harness, label, before + 1);
                assert_retained_key(&harness, label, OPAQUE_KEY);
            }
            OutcomeKind::Outbound => {
                assert_eq!(http_status, StatusCode::BAD_GATEWAY, "{response}");
                assert_v3_error(&response, ERROR_OUTBOUND_FAILED);
                assert_eq!(response["currentRevision"], before + 1);
                assert_still_pending(&harness, label, before + 1);
                assert_retained_key(&harness, label, OPAQUE_KEY);
            }
        }
        assert_no_secret_logs(&harness, &[OPAQUE_KEY, BODY_SECRET]);
    }

    insert_managed_waiting(&harness, "network");
    let before = harness.state.settings_revision();
    let dead = closed_proxy_addr().await;
    let _dead_target = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        format!("{dead}/"),
    );
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("network"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("the account remains pending"),
        "{body}"
    );
    assert_secret_free(&body, &[OPAQUE_KEY]);
    assert_eq!(body["currentRevision"], before + 1);
    assert_still_pending(&harness, "network", before + 1);
    assert_retained_key(&harness, "network", OPAQUE_KEY);

    let oversized = "x".repeat(64 * 1024 + 1024);
    let origin = start_origin(StatusCode::OK, oversized).await;
    let _target = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        origin.url.clone(),
    );
    insert_managed_waiting(&harness, "oversize");
    let before = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("oversize"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: AccountMutation = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        parsed.account.as_ref().unwrap().setup_step,
        AccountSetupStep::Ready
    );
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_secret_free(&body, &[OPAQUE_KEY]);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_managed_key_verify_does_not_follow_redirects_or_echo_upstream_bodies() {
    let harness = start_loopback("verify-key-redirect").await;
    force_direct_proxy(&harness);
    let hop = start_origin(StatusCode::OK, r#"{"choices":[]}"#).await;
    let origin = start_origin_with(OriginSpec {
        status: StatusCode::FOUND,
        body: BODY_SECRET.to_string(),
        location: Some(hop.url.clone()),
        hold: None,
        delay: Duration::ZERO,
    })
    .await;
    let _guard = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        origin.url.clone(),
    );
    insert_managed_waiting(&harness, "managed-1");
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("managed-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(origin.call_count(), 1);
    assert_eq!(hop.call_count(), 0, "redirects must not be followed");
    assert_secret_free(&body, &[OPAQUE_KEY, BODY_SECRET, &hop.url, "steal"]);
    assert!(!body["message"].as_str().unwrap().contains(BODY_SECRET));
    assert_eq!(body["currentRevision"], before + 1);
    assert_still_pending(&harness, "managed-1", before + 1);
    assert_retained_key(&harness, "managed-1", OPAQUE_KEY);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_stale_during_network_has_no_side_effect() {
    let harness = start_loopback("verify-key-stale-during").await;
    force_direct_proxy(&harness);
    let hold = Hold::new();
    let origin = start_origin_with(OriginSpec {
        status: StatusCode::OK,
        body: r#"{"choices":[]}"#.into(),
        location: None,
        hold: Some(hold.clone()),
        delay: Duration::ZERO,
    })
    .await;
    let _guard = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        origin.url.clone(),
    );
    insert_managed_waiting(&harness, "managed-1");
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let body = cas(&harness, json!({ "key": OPAQUE_KEY }));
    let path = verify_path("managed-1");
    let ((status, response), bumped) =
        tokio::join!(send_json(&harness, Method::POST, &path, &body), async {
            tokio::time::timeout(Duration::from_secs(10), hold.wait_until_received())
                .await
                .expect("verification request should reach upstream");
            assert_eq!(harness.state.settings_revision(), before);
            assert!(stored_account(&harness, "managed-1").key_cipher.is_empty());
            let bumped = harness.state.bump_settings_revision();
            hold.release();
            bumped
        });
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_v3_error(&response, ERROR_REVISION_CONFLICT);
    assert_eq!(response["currentRevision"], bumped);
    assert_eq!(response["processGeneration"], generation);
    assert_eq!(origin.call_count(), 1);
    let stored = stored_account(&harness, "managed-1");
    assert_eq!(stored.setup_step, ModelSetupStep::KeyVerification);
    assert!(!stored.enabled);
    assert!(stored.key_cipher.is_empty());
    assert_eq!(harness.state.settings_revision(), bumped);
    assert_secret_free(&response, &[OPAQUE_KEY]);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_delayed_verify_loses_to_revisionless_v2_key_replacement() {
    const V2_REPLACEMENT_KEY: &str = "opaque/v2-replacement+key=7";

    let harness = start_loopback("verify-key-v2-race").await;
    let v2_origin = start_origin(
        StatusCode::BAD_REQUEST,
        r#"{"error":{"message":"candidate rejected"}}"#,
    )
    .await;
    let v3_hold = Hold::new();
    let v3_origin = start_origin_with(OriginSpec {
        status: StatusCode::OK,
        body: r#"{"choices":[]}"#.into(),
        location: None,
        hold: Some(v3_hold.clone()),
        delay: Duration::ZERO,
    })
    .await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    config.upstream_base_url = v2_origin.url.trim_end_matches('/').to_string();
    harness.state.set_config(config).unwrap();
    let _v3_target = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        v3_origin.url.clone(),
    );
    insert_managed_waiting(&harness, "managed-1");
    let before = harness.state.settings_revision();
    let v3_body = cas(&harness, json!({ "key": OPAQUE_KEY }));
    let v3_path = verify_path("managed-1");

    let (v3_result, v2_status) = tokio::join!(
        send_json(&harness, Method::POST, &v3_path, &v3_body),
        async {
            tokio::time::timeout(Duration::from_secs(10), v3_hold.wait_until_received())
                .await
                .expect("V3 verification should reach its delayed upstream");
            let response = harness
                .client
                .post(format!(
                    "{}/accounts/managed-1/setup/verify-key",
                    harness.v2_base
                ))
                .json(&json!({ "key": V2_REPLACEMENT_KEY }))
                .send()
                .await
                .unwrap();
            let status = response.status();
            let body = response.text().await.unwrap();
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(!body.contains(V2_REPLACEMENT_KEY));
            assert_eq!(harness.state.settings_revision(), before);
            assert_retained_key(&harness, "managed-1", V2_REPLACEMENT_KEY);
            v3_hold.release();
            status
        }
    );

    assert_eq!(v2_status, StatusCode::BAD_REQUEST);
    assert_eq!(v3_result.0, StatusCode::CONFLICT, "{}", v3_result.1);
    assert_v3_error(&v3_result.1, ERROR_CONFLICT);
    assert_eq!(v3_result.1["currentRevision"], before);
    assert_eq!(harness.state.settings_revision(), before);
    assert_still_pending(&harness, "managed-1", before);
    assert_retained_key(&harness, "managed-1", V2_REPLACEMENT_KEY);
    assert_eq!(v2_origin.call_count(), 1);
    assert_eq!(v3_origin.call_count(), 1);
    assert_secret_free(&v3_result.1, &[OPAQUE_KEY, V2_REPLACEMENT_KEY]);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_concurrent_verify_key_bumps_revision_once() {
    let harness = start_loopback("verify-key-concurrent").await;
    force_direct_proxy(&harness);
    let hold = Hold::new();
    let origin = start_origin_with(OriginSpec {
        status: StatusCode::OK,
        body: r#"{"choices":[]}"#.into(),
        location: None,
        hold: Some(hold.clone()),
        delay: Duration::ZERO,
    })
    .await;
    let _guard = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        origin.url.clone(),
    );
    insert_managed_waiting(&harness, "managed-1");
    let before = harness.state.settings_revision();
    let body = cas(&harness, json!({ "key": OPAQUE_KEY }));
    let path = verify_path("managed-1");
    let (first, second, _) = tokio::join!(
        send_json(&harness, Method::POST, &path, &body),
        send_json(&harness, Method::POST, &path, &body),
        async {
            tokio::time::timeout(Duration::from_secs(10), hold.wait_until_received_n(2))
                .await
                .expect("both verification requests should reach upstream");
            hold.release();
        }
    );
    let statuses = [first.0, second.0];
    let ok = statuses
        .iter()
        .filter(|status| **status == StatusCode::OK)
        .count();
    let conflict = statuses
        .iter()
        .filter(|status| **status == StatusCode::CONFLICT)
        .count();
    assert_eq!(ok, 1, "first={first:?} second={second:?}");
    assert_eq!(conflict, 1, "first={first:?} second={second:?}");
    for response in [&first.1, &second.1] {
        assert_secret_free(response, &[OPAQUE_KEY]);
    }
    let stored = stored_account(&harness, "managed-1");
    assert_eq!(stored.setup_step, ModelSetupStep::Ready);
    assert!(stored.enabled);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_eq!(origin.call_count(), 2);
    assert_no_secret_logs(&harness, &[OPAQUE_KEY]);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_managed_key_verify_list_blacklist_uses_default_proxy_leg() {
    let harness = start_loopback("verify-key-proxy").await;
    let origin = start_origin(StatusCode::OK, r#"{"choices":[]}"#).await;
    let _guard = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        origin.url.clone(),
    );
    insert_managed_waiting(&harness, "managed-1");
    let dead_proxy = closed_proxy_addr().await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::List;
    config.proxy_list_direction = ProxyListDirection::Blacklist;
    config.proxy_url = dead_proxy;
    harness.state.set_config(config).unwrap();
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("managed-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert_eq!(
        origin.call_count(),
        0,
        "blacklist default must not skip the proxy"
    );
    assert_eq!(body["currentRevision"], before + 1);
    assert_still_pending(&harness, "managed-1", before + 1);
    assert_retained_key(&harness, "managed-1", OPAQUE_KEY);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_v2_verify_key_still_completes_beside_v3() {
    let harness = start_loopback("verify-key-v2-coexist").await;
    let origin = start_origin(StatusCode::OK, r#"{"choices":[]}"#).await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    config.upstream_base_url = origin.url.trim_end_matches('/').to_string();
    harness.state.set_config(config).unwrap();
    insert_managed_waiting(&harness, "v2-managed");
    insert_managed_waiting(&harness, "v3-managed");
    let v3_guard = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        origin.url.clone(),
    );

    let v2 = harness
        .client
        .post(format!(
            "{}/accounts/v2-managed/setup/verify-key",
            harness.v2_base
        ))
        .json(&json!({ "key": OPAQUE_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        v2.status(),
        StatusCode::OK,
        "{}",
        v2.text().await.unwrap_or_default()
    );
    let v2_account = stored_account(&harness, "v2-managed");
    assert_eq!(v2_account.setup_step, ModelSetupStep::Ready);
    assert!(v2_account.enabled);

    let before = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("v3-managed"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: AccountMutation = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        parsed.account.as_ref().unwrap().setup_step,
        AccountSetupStep::Ready
    );
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_secret_free(&body, &[OPAQUE_KEY]);
    drop(v3_guard);
    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_overrides_are_generation_isolated() {
    let origin_a = start_origin(StatusCode::OK, r#"{"choices":[]}"#).await;
    let origin_b = start_origin(StatusCode::UNAUTHORIZED, r#"{"error":"b"}"#).await;
    let harness_a = start_loopback("verify-key-iso-a").await;
    let harness_b = start_loopback("verify-key-iso-b").await;
    force_direct_proxy(&harness_a);
    force_direct_proxy(&harness_b);
    assert_ne!(
        harness_a.state.process_generation(),
        harness_b.state.process_generation()
    );
    insert_managed_waiting(&harness_a, "managed-a");
    insert_managed_waiting(&harness_b, "managed-b");
    let _guard_a = install_managed_key_verify_target_for_tests(
        harness_a.state.process_generation(),
        origin_a.url.clone(),
    );
    let _guard_b = install_managed_key_verify_target_for_tests(
        harness_b.state.process_generation(),
        origin_b.url.clone(),
    );

    let path_a = verify_path("managed-a");
    let path_b = verify_path("managed-b");
    let body_a = cas(&harness_a, json!({ "key": OPAQUE_KEY }));
    let body_b = cas(&harness_b, json!({ "key": OPAQUE_KEY }));
    let (result_a, result_b) = tokio::join!(
        send_json(&harness_a, Method::POST, &path_a, &body_a),
        send_json(&harness_b, Method::POST, &path_b, &body_b),
    );
    assert_eq!(result_a.0, StatusCode::OK, "{}", result_a.1);
    assert_eq!(result_b.0, StatusCode::BAD_REQUEST, "{}", result_b.1);
    assert_eq!(origin_a.call_count(), 1);
    assert_eq!(origin_b.call_count(), 1);
    assert_eq!(
        stored_account(&harness_a, "managed-a").setup_step,
        ModelSetupStep::Ready
    );
    assert_eq!(
        stored_account(&harness_b, "managed-b").setup_step,
        ModelSetupStep::KeyVerification
    );

    harness_a.stop();
    harness_b.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_invalid_stale_and_wrong_step_never_call_upstream() {
    let harness = start_loopback("verify-key-zero-network").await;
    force_direct_proxy(&harness);
    let origin = start_origin(StatusCode::OK, r#"{"choices":[]}"#).await;
    let _guard = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        origin.url.clone(),
    );
    insert_managed_waiting(&harness, "managed-1");
    let mut draft = managed_waiting("draft-1");
    draft.setup_step = ModelSetupStep::Payment;
    harness.state.db.lock().create_account(&draft).unwrap();
    let mut goat = managed_waiting("goat-1");
    goat.provider_id = COMMAND_CODE_PROVIDER_ID.into();
    goat.offering_id = GOAT_OFFERING_ID.into();
    harness.state.db.lock().create_account(&goat).unwrap();
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();

    let stale = send_json(
        &harness,
        Method::POST,
        &verify_path("managed-1"),
        &json!({
            "expectedRevision": before.saturating_sub(1),
            "processGeneration": generation,
            "key": OPAQUE_KEY
        }),
    )
    .await;
    assert_eq!(stale.0, StatusCode::CONFLICT, "{}", stale.1);

    let wrong_step = send_json(
        &harness,
        Method::POST,
        &verify_path("draft-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(wrong_step.0, StatusCode::CONFLICT, "{}", wrong_step.1);

    let unroutable = send_json(
        &harness,
        Method::POST,
        &verify_path("goat-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(unroutable.0, StatusCode::CONFLICT, "{}", unroutable.1);
    assert_eq!(origin.call_count(), 0);
    assert_still_pending(&harness, "managed-1", before);
    assert!(stored_account(&harness, "goat-1").key_cipher.is_empty());

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_managed_key_verify_times_out_without_completing_setup() {
    let harness = start_loopback("verify-key-timeout").await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    config.connect_timeout_secs = 1;
    config.non_stream_timeout_secs = 1;
    harness.state.set_config(config).unwrap();
    let origin = start_origin_with(OriginSpec {
        status: StatusCode::OK,
        body: r#"{"choices":[]}"#.into(),
        location: None,
        hold: None,
        delay: Duration::from_secs(5),
    })
    .await;
    let _guard = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        origin.url.clone(),
    );
    insert_managed_waiting(&harness, "managed-1");
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("managed-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert!(
        body["message"].as_str().unwrap().contains("timed out"),
        "{body}"
    );
    assert_secret_free(&body, &[OPAQUE_KEY]);
    assert_eq!(body["currentRevision"], before + 1);
    assert_still_pending(&harness, "managed-1", before + 1);
    assert_retained_key(&harness, "managed-1", OPAQUE_KEY);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_managed_key_verify_list_whitelist_uses_direct_default_leg() {
    let harness = start_loopback("verify-key-whitelist").await;
    let origin = start_origin(StatusCode::OK, r#"{"choices":[]}"#).await;
    let _guard = install_managed_key_verify_target_for_tests(
        harness.state.process_generation(),
        origin.url.clone(),
    );
    insert_managed_waiting(&harness, "managed-1");
    let dead_proxy = closed_proxy_addr().await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::List;
    config.proxy_list_direction = ProxyListDirection::Whitelist;
    config.proxy_url = dead_proxy;
    harness.state.set_config(config).unwrap();
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &verify_path("managed-1"),
        &cas(&harness, json!({ "key": OPAQUE_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: AccountMutation = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        parsed.account.as_ref().unwrap().setup_step,
        AccountSetupStep::Ready
    );
    assert_eq!(origin.call_count(), 1);
    assert_eq!(harness.state.settings_revision(), before + 1);
    assert_secret_free(&body, &[OPAQUE_KEY]);

    harness.stop();
}
