//! Dashboard V3 Custom model discovery: session, operational (non-CAS) probe,
//! trusted-admin HTTP boundary, V2 status distinctions, and V2 coexistence.

use axum::Router;
use axum::body::Bytes;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, HeaderValue, Method as HttpMethod, header};
use axum::response::Response;
use axum::routing::any;
use ocg_core::dashboard_v3::{
    CustomModelDiscoveryResponse, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_NOT_FOUND,
    ERROR_UNAUTHORIZED,
};
use ocg_core::db::CURRENT_SCHEMA_VERSION;
use ocg_core::models::{ProxyListDirection, ProxyMode};
use ocg_core::provider::{CUSTOM_API_OFFERING_ID, CUSTOM_PROVIDER_ID};
use ocg_core::provider_contracts::ContractScope;
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

const CUSTOM_KEY: &str = "custom-discovery-secret-key-42";
const CUSTOM_BEARER: &str = "Bearer custom-discovery-secret-key-42";
const SUCCESS_BODY: &str = r#"{"data":[{"id":"org/model-a"},{"id":"org/model-b"}]}"#;
const LEAKY_401_BODY: &str = r#"{"error":"leaked custom-discovery-secret-key-42","endpoint":"https://user:pass@evil.example/v1"}"#;

#[derive(Clone, Debug)]
struct CapturedCall {
    method: String,
    path: String,
    query: Option<String>,
    authorization: Option<String>,
    x_api_key: Option<String>,
    cookie: Option<String>,
    anthropic_version: Option<String>,
    body: String,
}

struct DiscoveryOrigin {
    url: String,
    calls: Arc<Mutex<Vec<CapturedCall>>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

impl DiscoveryOrigin {
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn last(&self) -> CapturedCall {
        self.calls
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("origin should have been called")
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[derive(Clone)]
enum OriginScript {
    Fixed { status: StatusCode, body: String },
    Redirect { location: String, body: String },
    Sequence(Arc<Mutex<Vec<(StatusCode, String)>>>),
}

async fn start_discovery_origin(script: OriginScript) -> DiscoveryOrigin {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let app = Router::new().fallback(any(
        move |method: HttpMethod, uri: OriginalUri, headers: HeaderMap, payload: Bytes| {
            let calls = calls_for_handler.clone();
            let script = script.clone();
            async move {
                calls.lock().unwrap().push(CapturedCall {
                    method: method.to_string(),
                    path: uri.0.path().to_string(),
                    query: uri.0.query().map(str::to_string),
                    authorization: header_value(&headers, "authorization"),
                    x_api_key: header_value(&headers, "x-api-key"),
                    cookie: header_value(&headers, "cookie"),
                    anthropic_version: header_value(&headers, "anthropic-version"),
                    body: String::from_utf8_lossy(&payload).into_owned(),
                });
                match script {
                    OriginScript::Fixed { status, body } => Response::builder()
                        .status(status)
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(axum::body::Body::from(body))
                        .unwrap(),
                    OriginScript::Redirect { location, body } => Response::builder()
                        .status(StatusCode::FOUND)
                        .header(header::CONTENT_TYPE, "text/plain")
                        .header(header::LOCATION, HeaderValue::from_str(&location).unwrap())
                        .body(axum::body::Body::from(body))
                        .unwrap(),
                    OriginScript::Sequence(pages) => {
                        let (status, body) = pages.lock().unwrap().pop().unwrap_or((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            r#"{"error":"no more pages"}"#.to_string(),
                        ));
                        Response::builder()
                            .status(status)
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(axum::body::Body::from(body))
                            .unwrap()
                    }
                }
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
    DiscoveryOrigin {
        url: format!("http://{addr}"),
        calls,
        _stop: stop,
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

fn discover_body(base_url: &str, protocol: &str, auth: &str, api_key: Option<&str>) -> Value {
    let mut body = json!({
        "baseUrl": base_url,
        "upstreamProtocol": protocol,
        "authScheme": auth,
    });
    if let Some(api_key) = api_key {
        body["apiKey"] = json!(api_key);
    }
    body
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

fn hostile_discovery_body(secret: &str) -> String {
    json!({
        "data": [
            {"id": "org/model-a"},
            {"id": secret},
            {"id": format!("org/{secret}-echo")},
            {"id": "org/model-b"}
        ]
    })
    .to_string()
}

fn assert_no_plaintext_in_log_facing_values(harness: &V3Harness, secret: &str) {
    let db = harness.state.db.lock();
    for log in db.list_gateway_logs(200).unwrap() {
        let encoded = serde_json::to_string(&log).expect("gateway log json");
        assert!(
            !encoded.contains(secret),
            "gateway log leaked credential: {encoded}"
        );
    }
    for log in db.list_forward_logs(200).unwrap() {
        let encoded = serde_json::to_string(&log).expect("forward log json");
        assert!(
            !encoded.contains(secret),
            "forward log leaked credential: {encoded}"
        );
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
            "discovery payload leaked field {name}: {body}"
        );
    }
    let encoded = body.to_string();
    for secret in secrets {
        assert!(
            !encoded.contains(secret),
            "discovery payload leaked credential {secret}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in secrets {
            assert!(
                !value.contains(secret),
                "discovery payload leaked credential {secret}: {body}"
            );
        }
    }
}

fn parse_discovery(body: &Value) -> CustomModelDiscoveryResponse {
    serde_json::from_value(body.clone())
        .unwrap_or_else(|_| panic!("CustomModelDiscoveryResponse: {body}"))
}

fn point_direct(harness: &V3Harness) {
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    config.non_stream_timeout_secs = 5;
    config.connect_timeout_secs = 5;
    harness.state.set_config(config).unwrap();
}

async fn create_custom_account(harness: &V3Harness, base_url: &str, auth: &str) -> String {
    let (status, created) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(
            harness,
            json!({
                "name": "Custom discovery",
                "key": CUSTOM_KEY,
                "providerId": CUSTOM_PROVIDER_ID,
                "offeringId": CUSTOM_API_OFFERING_ID,
                "customConfig": {
                    "baseUrl": base_url,
                    "upstreamProtocol": "chat_completions",
                    "authScheme": auth
                },
                "modelCapabilities": [{
                    "modelId": "org/model",
                    "protocol": "chat_completions"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    created["account"]["id"]
        .as_str()
        .expect("created Custom account id")
        .to_string()
}

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
}

#[tokio::test]
async fn discovery_requires_the_v3_session() {
    let harness = start_public("discover-auth").await;
    let origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::OK,
        body: SUCCESS_BODY.into(),
    })
    .await;
    point_direct(&harness);
    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &discover_body(&origin.url, "chat_completions", "bearer", Some(CUSTOM_KEY)),
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
async fn discovery_does_not_require_expected_revision_and_rejects_unknown_fields() {
    let harness = start_loopback("discover-json").await;
    let origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::OK,
        body: SUCCESS_BODY.into(),
    })
    .await;
    point_direct(&harness);
    let before = harness.state.settings_revision();

    let (status, missing) = send_raw(&harness, "/custom/models/discover", "{").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing}");
    assert_v3_error(&missing, ERROR_INVALID_JSON);
    assert_eq!(missing["currentRevision"], Value::Null);

    let (status, array) = send_raw(&harness, "/custom/models/discover", "[]").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{array}");
    assert_v3_error(&array, ERROR_INVALID_JSON);

    let (status, unknown) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &json!({
            "baseUrl": origin.url,
            "upstreamProtocol": "chat_completions",
            "authScheme": "bearer",
            "apiKey": CUSTOM_KEY,
            "expectedRevision": before
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{unknown}");
    assert_v3_error(&unknown, ERROR_INVALID_JSON);

    let (status, snake) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &json!({
            "base_url": origin.url,
            "upstream_protocol": "chat_completions",
            "auth_scheme": "bearer",
            "api_key": CUSTOM_KEY
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{snake}");
    assert_v3_error(&snake, ERROR_INVALID_JSON);

    let (status, protocol) = send_raw(
        &harness,
        "/custom/models/discover",
        &json!({
            "baseUrl": origin.url,
            "upstreamProtocol": "gemini",
            "authScheme": "bearer",
            "apiKey": CUSTOM_KEY
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{protocol}");
    assert_v3_error(&protocol, ERROR_INVALID_JSON);

    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn successful_bearer_discovery_is_secret_free_and_does_not_mutate() {
    let harness = start_loopback("discover-success").await;
    let origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::OK,
        body: SUCCESS_BODY.into(),
    })
    .await;
    point_direct(&harness);
    let account_id = create_custom_account(&harness, &origin.url, "bearer").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let pricing = harness.state.pricing_snapshot().revision.clone();
    let before_account = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    let before_contracts = harness.state.provider_contracts();

    let response = harness
        .client
        .post(format!("{}/custom/models/discover", harness.v3_base))
        .header(
            reqwest::header::COOKIE,
            "ocg_dashboard_session=should-not-leak",
        )
        .header(reqwest::header::AUTHORIZATION, "Bearer dashboard-token")
        .json(&discover_body(
            &origin.url,
            "chat_completions",
            "bearer",
            Some(CUSTOM_KEY),
        ))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_discovery(&body);
    assert_eq!(parsed.models, vec!["org/model-a", "org/model-b"]);
    assert!(!parsed.truncated);
    assert_eq!(parsed.revision, before);
    assert_eq!(parsed.process_generation, generation);
    assert_eq!(parsed.pricing_revision, pricing);
    assert_eq!(body["processGeneration"], generation);
    assert!(body.get("apiKey").is_none());
    assert!(body.get("truncated").and_then(Value::as_bool).is_some());
    assert_secret_free(&body, &[CUSTOM_KEY, "dashboard-token", "should-not-leak"]);

    let call = origin.last();
    assert_eq!(call.method, "GET");
    assert_eq!(call.path, "/models");
    assert_eq!(call.authorization.as_deref(), Some(CUSTOM_BEARER));
    assert!(call.x_api_key.is_none(), "{call:?}");
    assert!(call.cookie.is_none(), "{call:?}");
    assert_ne!(
        call.authorization.as_deref(),
        Some("Bearer dashboard-token")
    );
    assert!(call.body.is_empty(), "{call:?}");

    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(harness.state.process_generation(), generation);
    let after_account = harness
        .state
        .db
        .lock()
        .get_account(&account_id)
        .unwrap()
        .unwrap();
    assert_eq!(after_account.updated_at, before_account.updated_at);
    assert_eq!(
        before_contracts.as_ref(),
        harness.state.provider_contracts().as_ref()
    );
    let discovered_persisted = harness
        .state
        .provider_contracts()
        .scope(&ContractScope::custom_endpoint(&account_id))
        .is_some_and(|scope| scope.catalog.models.iter().any(|id| id == "org/model-a"));
    assert!(
        !discovered_persisted,
        "discovery must not persist discovered Custom catalog models"
    );
    harness.stop();
}

#[tokio::test]
async fn x_api_key_discovery_sends_isolated_auth_and_messages_sets_anthropic_version() {
    let harness = start_loopback("discover-auth-scheme").await;
    let origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::OK,
        body: SUCCESS_BODY.into(),
    })
    .await;
    point_direct(&harness);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &discover_body(
            &origin.url,
            "chat_completions",
            "x-api-key",
            Some(CUSTOM_KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let call = origin.last();
    assert_eq!(call.x_api_key.as_deref(), Some(CUSTOM_KEY));
    assert!(call.authorization.is_none(), "{call:?}");
    assert!(call.anthropic_version.is_none(), "{call:?}");

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &discover_body(&origin.url, "messages", "bearer", Some(CUSTOM_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let call = origin.last();
    assert_eq!(call.anthropic_version.as_deref(), Some("2023-06-01"));
    assert_eq!(call.authorization.as_deref(), Some(CUSTOM_BEARER));
    harness.stop();
}

#[tokio::test]
async fn discovery_does_not_follow_redirects() {
    let harness = start_loopback("discover-redirect").await;
    let origin = start_discovery_origin(OriginScript::Redirect {
        location: "/stolen".into(),
        body: format!("redirected-with-{CUSTOM_KEY}"),
    })
    .await;
    point_direct(&harness);
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &discover_body(&origin.url, "chat_completions", "bearer", Some(CUSTOM_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    let message = body["message"].as_str().unwrap();
    assert!(message.contains("302"), "{body}");
    assert!(!message.contains(CUSTOM_KEY), "{body}");
    assert!(!message.contains("stolen"), "{body}");
    assert!(!message.contains("redirected-with"), "{body}");
    assert_secret_free(&body, &[CUSTOM_KEY]);
    assert_eq!(origin.call_count(), 1);
    assert_eq!(origin.last().path, "/models");
    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn discovery_preserves_v2_status_distinctions_without_leaking_bodies() {
    let harness = start_loopback("discover-status").await;
    point_direct(&harness);
    let before = harness.state.settings_revision();

    for (status, body, expect_http, expect_code, needle) in [
        (
            StatusCode::UNAUTHORIZED,
            LEAKY_401_BODY,
            StatusCode::BAD_REQUEST,
            ERROR_INVALID_REQUEST,
            "authentication failed",
        ),
        (
            StatusCode::FORBIDDEN,
            LEAKY_401_BODY,
            StatusCode::BAD_REQUEST,
            ERROR_INVALID_REQUEST,
            "authentication failed",
        ),
        (
            StatusCode::NOT_FOUND,
            r#"{"error":"no such catalog"}"#,
            StatusCode::BAD_REQUEST,
            ERROR_INVALID_REQUEST,
            "unsupported",
        ),
        (
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":"slow down"}"#,
            StatusCode::BAD_REQUEST,
            ERROR_INVALID_REQUEST,
            "rate limited",
        ),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!(r#"{{"error":"{CUSTOM_KEY}"}}"#),
            StatusCode::BAD_GATEWAY,
            "outboundFailed",
            "upstream server error",
        ),
    ] {
        let origin = start_discovery_origin(OriginScript::Fixed {
            status,
            body: body.to_string(),
        })
        .await;
        let (got, payload) = send_json(
            &harness,
            Method::POST,
            "/custom/models/discover",
            &discover_body(&origin.url, "chat_completions", "bearer", Some(CUSTOM_KEY)),
        )
        .await;
        assert_eq!(got, expect_http, "{status} {payload}");
        assert_v3_error(&payload, expect_code);
        let message = payload["message"].as_str().unwrap();
        assert!(message.contains(needle), "{payload}");
        assert!(!message.contains(CUSTOM_KEY), "{payload}");
        assert!(!message.contains("leaked"), "{payload}");
        assert!(!message.contains("user:pass@"), "{payload}");
        assert!(!message.contains("no such catalog"), "{payload}");
        assert!(!message.contains("slow down"), "{payload}");
        assert_secret_free(&payload, &[CUSTOM_KEY, "user:pass@"]);
        assert_eq!(payload["currentRevision"], before);
        assert_eq!(
            payload["processGeneration"],
            harness.state.process_generation()
        );
    }

    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn discovery_rejects_malformed_inputs_before_upstream() {
    let harness = start_loopback("discover-validate").await;
    let origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::OK,
        body: SUCCESS_BODY.into(),
    })
    .await;
    point_direct(&harness);
    let before = harness.state.settings_revision();

    let (status, missing_key) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &discover_body(&origin.url, "chat_completions", "bearer", None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{missing_key}");
    assert_v3_error(&missing_key, ERROR_INVALID_REQUEST);
    assert!(
        missing_key["message"].as_str().unwrap().contains("API key"),
        "{missing_key}"
    );

    let (status, embedded) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &discover_body(
            "https://user:pass@api.example.com/v1",
            "chat_completions",
            "bearer",
            Some(CUSTOM_KEY),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{embedded}");
    assert_v3_error(&embedded, ERROR_INVALID_REQUEST);
    assert_secret_free(&embedded, &[CUSTOM_KEY, "user:pass@"]);
    assert_eq!(harness.state.settings_revision(), before);

    let go = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({ "name": "Go", "key": "sk-go-not-for-discovery" }),
        ),
    )
    .await;
    assert_eq!(go.0, StatusCode::OK, "{}", go.1);
    let go_id = go.1["account"]["id"].as_str().unwrap().to_string();
    let after_create = harness.state.settings_revision();
    assert_ne!(after_create, before);
    let (status, wrong_plan) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &json!({
            "baseUrl": origin.url,
            "upstreamProtocol": "chat_completions",
            "authScheme": "bearer",
            "accountId": go_id
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{wrong_plan}");
    assert_v3_error(&wrong_plan, ERROR_INVALID_REQUEST);
    assert!(
        wrong_plan["message"]
            .as_str()
            .unwrap()
            .contains("Custom API"),
        "{wrong_plan}"
    );

    let (status, missing_account) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &json!({
            "baseUrl": origin.url,
            "upstreamProtocol": "chat_completions",
            "authScheme": "bearer",
            "accountId": "missing-account"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing_account}");
    assert_v3_error(&missing_account, ERROR_NOT_FOUND);

    assert_eq!(origin.call_count(), 0);
    assert_eq!(harness.state.settings_revision(), after_create);
    harness.stop();
}

#[tokio::test]
async fn stored_key_discovery_does_not_use_a_stale_expected_revision() {
    let harness = start_loopback("discover-stored-key").await;
    let pages = Arc::new(Mutex::new(vec![
        (
            StatusCode::OK,
            r#"{"data":[{"id":"model-b"}],"has_more":false}"#.to_string(),
        ),
        (
            StatusCode::OK,
            r#"{"data":[{"id":"model-a"}],"has_more":true,"last_id":"model-a"}"#.to_string(),
        ),
    ]));
    let origin = start_discovery_origin(OriginScript::Sequence(pages)).await;
    point_direct(&harness);
    let account_id = create_custom_account(&harness, &origin.url, "bearer").await;
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &json!({
            "baseUrl": origin.url,
            "upstreamProtocol": "chat_completions",
            "authScheme": "bearer",
            "accountId": account_id
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_discovery(&body);
    assert_eq!(parsed.models, vec!["model-a", "model-b"]);
    assert!(!parsed.truncated);
    assert_eq!(parsed.revision, before);
    assert_eq!(origin.call_count(), 2);
    let first = origin.calls.lock().unwrap()[0].clone();
    let second = origin.calls.lock().unwrap()[1].clone();
    assert_eq!(first.path, "/models");
    assert!(first.query.is_none(), "{first:?}");
    assert_eq!(second.query.as_deref(), Some("after_id=model-a"));
    assert_eq!(first.authorization.as_deref(), Some(CUSTOM_BEARER));
    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn discovery_uses_the_default_proxy_leg_not_a_model_exception() {
    let harness = start_loopback("discover-proxy-leg").await;
    let origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::OK,
        body: SUCCESS_BODY.into(),
    })
    .await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::List;
    config.proxy_list_direction = ProxyListDirection::Whitelist;
    config.proxy_list_models = vec!["org/model-a".into()];
    config.proxy_url = "http://127.0.0.1:1".into();
    config.non_stream_timeout_secs = 5;
    config.connect_timeout_secs = 5;
    harness.state.set_config(config).unwrap();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &discover_body(&origin.url, "chat_completions", "bearer", Some(CUSTOM_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_discovery(&body);
    assert_eq!(parsed.models[0], "org/model-a");
    assert_eq!(origin.call_count(), 1);
    harness.stop();
}

#[tokio::test]
async fn v2_discovery_coexists_and_keeps_snake_case() {
    let harness = start_loopback("discover-v2-coexist").await;
    let origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::OK,
        body: SUCCESS_BODY.into(),
    })
    .await;
    point_direct(&harness);
    let before = harness.state.settings_revision();

    let v2 = harness
        .client
        .post(format!("{}/custom/models/discover", harness.v2_base))
        .json(&json!({
            "base_url": origin.url,
            "upstream_protocol": "chat_completions",
            "auth_scheme": "bearer",
            "api_key": CUSTOM_KEY
        }))
        .send()
        .await
        .unwrap();
    let v2_status = v2.status();
    let v2_body: Value = v2.json().await.unwrap();
    assert_eq!(v2_status, StatusCode::OK, "{v2_body}");
    assert_eq!(v2_body["models"][0], "org/model-a");
    assert_eq!(v2_body["truncated"], false);
    assert!(v2_body.get("revision").is_none(), "{v2_body}");
    assert!(v2_body.get("processGeneration").is_none(), "{v2_body}");
    assert!(v2_body.get("api_key").is_none(), "{v2_body}");

    let auth_origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::UNAUTHORIZED,
        body: LEAKY_401_BODY.into(),
    })
    .await;
    let v2_auth = harness
        .client
        .post(format!("{}/custom/models/discover", harness.v2_base))
        .json(&json!({
            "base_url": auth_origin.url,
            "upstream_protocol": "chat_completions",
            "auth_scheme": "bearer",
            "api_key": CUSTOM_KEY
        }))
        .send()
        .await
        .unwrap();
    let v2_auth_status = v2_auth.status();
    let v2_auth_body: Value = v2_auth.json().await.unwrap_or(Value::Null);
    assert_eq!(v2_auth_status, StatusCode::BAD_REQUEST, "{v2_auth_body}");
    let v2_text = v2_auth_body.to_string();
    assert!(
        v2_text.contains("authentication failed") || v2_text.contains("401"),
        "{v2_auth_body}"
    );
    assert!(!v2_text.contains(CUSTOM_KEY), "{v2_auth_body}");

    let (status, v3) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &discover_body(&origin.url, "chat_completions", "bearer", Some(CUSTOM_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v3}");
    assert_eq!(v3["models"][0], "org/model-a");
    assert_eq!(v3["revision"], before);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
    harness.stop();
}

#[tokio::test]
async fn supplied_key_hostile_upstream_ids_are_dropped_without_echoing_plaintext() {
    let harness = start_loopback("discover-supplied-reflection").await;
    let origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::OK,
        body: hostile_discovery_body(CUSTOM_KEY),
    })
    .await;
    point_direct(&harness);
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &discover_body(&origin.url, "chat_completions", "bearer", Some(CUSTOM_KEY)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_discovery(&body);
    assert_eq!(parsed.models, vec!["org/model-a", "org/model-b"]);
    assert!(!parsed.truncated);
    assert_eq!(parsed.revision, before);
    assert_eq!(parsed.process_generation, generation);
    assert!(body.get("error").is_none(), "{body}");
    assert_secret_free(&body, &[CUSTOM_KEY]);
    assert_no_plaintext_in_log_facing_values(&harness, CUSTOM_KEY);
    assert_eq!(origin.call_count(), 1);
    assert_eq!(harness.state.settings_revision(), before);
    harness.stop();
}

#[tokio::test]
async fn stored_key_hostile_upstream_ids_are_dropped_without_echoing_plaintext() {
    let harness = start_loopback("discover-stored-reflection").await;
    let origin = start_discovery_origin(OriginScript::Fixed {
        status: StatusCode::OK,
        body: hostile_discovery_body(CUSTOM_KEY),
    })
    .await;
    point_direct(&harness);
    let account_id = create_custom_account(&harness, &origin.url, "bearer").await;
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let before_contracts = harness.state.provider_contracts();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/custom/models/discover",
        &json!({
            "baseUrl": origin.url,
            "upstreamProtocol": "chat_completions",
            "authScheme": "bearer",
            "accountId": account_id
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed = parse_discovery(&body);
    assert_eq!(parsed.models, vec!["org/model-a", "org/model-b"]);
    assert!(!parsed.truncated);
    assert_eq!(parsed.revision, before);
    assert_eq!(parsed.process_generation, generation);
    assert!(body.get("error").is_none(), "{body}");
    assert_secret_free(&body, &[CUSTOM_KEY]);
    assert_no_plaintext_in_log_facing_values(&harness, CUSTOM_KEY);
    assert_eq!(origin.call_count(), 1);
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(
        before_contracts.as_ref(),
        harness.state.provider_contracts().as_ref()
    );
    harness.stop();
}
