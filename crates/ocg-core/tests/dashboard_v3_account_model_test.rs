//! Dashboard V3 exact-account operational model tests.

use axum::Router;
use axum::body::Bytes;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, Method as HttpMethod};
use axum::routing::any;
use chrono::Utc;
use ocg_core::dashboard_v3::ERROR_INVALID_REQUEST;
use ocg_core::models::ProxyMode;
use ocg_core::provider::CUSTOM_PROVIDER_ID;
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::sync::{Arc, Mutex};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;
#[path = "fixtures/refreshed_go_catalog.rs"]
mod refreshed_go_catalog;

use harness::{V3Harness, start_loopback};

#[path = "fixtures/probe_response.rs"]
mod probe_response;
const TARGET_KEY: &str = "sk-account-model-target-secret";
const SIBLING_KEY: &str = "sk-account-model-sibling-secret";
const CUSTOM_KEY: &str = "custom-account-model-secret";

#[derive(Clone, Debug)]
struct CapturedCall {
    path: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
    body: Value,
}

struct ProbeOrigin {
    url: String,
    calls: Arc<Mutex<Vec<CapturedCall>>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

async fn start_origin() -> ProbeOrigin {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let target_auth = format!("Bearer {TARGET_KEY}");
    let app = Router::new().fallback(any(
        move |_method: HttpMethod, uri: OriginalUri, headers: HeaderMap, body: Bytes| {
            let calls = calls_for_handler.clone();
            let target_auth = target_auth.clone();
            async move {
                let authorization = header_value(&headers, "authorization");
                let x_api_key = header_value(&headers, "x-api-key");
                calls.lock().unwrap().push(CapturedCall {
                    path: uri.0.path().to_string(),
                    authorization: authorization.clone(),
                    x_api_key,
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                });
                if authorization.as_deref() == Some(target_auth.as_str()) {
                    (
                        StatusCode::UNAUTHORIZED,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        format!(r#"{{"error":"rejected {TARGET_KEY}"}}"#),
                    )
                } else {
                    (
                        StatusCode::OK,
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        probe_response::for_path(uri.0.path()).to_string(),
                    )
                }
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
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
        url: format!("http://{address}"),
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
    body: Value,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v3_base))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn create_go_account(harness: &V3Harness, name: &str, key: &str) -> String {
    let (status, body) = send_json(
        harness,
        Method::POST,
        "/accounts",
        cas(harness, json!({"name": name, "key": key})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["account"]["id"].as_str().unwrap().to_string()
}

fn disable_inference_binding(harness: &V3Harness, account_id: &str) {
    let db = harness.state.db.lock();
    let binding = db
        .list_inference_bindings()
        .unwrap()
        .into_iter()
        .find(|row| row.account_id == account_id)
        .expect("inference binding");
    db.update_credential_binding(&binding.binding_id, None, Some(false), None, None)
        .unwrap();
}

fn point_upstream(harness: &V3Harness, origin: &ProbeOrigin) {
    let mut config = harness.state.config();
    config.upstream_base_url = origin.url.clone();
    config.proxy_mode = ProxyMode::Direct;
    config.non_stream_timeout_secs = 5;
    harness.state.set_config(config).unwrap();
}

#[tokio::test]
async fn exact_account_probe_keeps_target_and_honors_live_authorization() {
    let harness = start_loopback("account-model-test-locked").await;
    refreshed_go_catalog::persist_refreshed_go_catalog(&harness.state);
    let origin = start_origin().await;
    point_upstream(&harness, &origin);
    let target = create_go_account(&harness, "Target", TARGET_KEY).await;
    let _sibling = create_go_account(&harness, "Healthy sibling", SIBLING_KEY).await;
    harness
        .state
        .db
        .lock()
        .set_account_cooldown(
            &target,
            Some(Utc::now() + chrono::Duration::hours(1)),
            Some("pre-existing cooldown"),
        )
        .unwrap();
    let before_revision = harness.state.settings_revision();
    let before = harness
        .state
        .db
        .lock()
        .get_account(&target)
        .unwrap()
        .unwrap();
    assert!(!before.enabled);
    assert!(before.cooldown_until.is_some());

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{target}/model-tests"),
        json!({"modelId":"grok-4.5"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accountId"], target);
    assert_eq!(body["modelId"], "grok-4.5");
    assert_eq!(body["success"], false, "{body}");
    assert_eq!(body["httpStatus"], 401, "{body}");
    assert!(body["durationMs"].as_u64().is_some(), "{body}");
    assert!(!body.to_string().contains(TARGET_KEY), "{body}");
    assert_eq!(harness.state.settings_revision(), before_revision);
    let after = harness
        .state
        .db
        .lock()
        .get_account(&target)
        .unwrap()
        .unwrap();
    assert!(!after.enabled);
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.auth_error, before.auth_error);
    let target_authorization = format!("Bearer {TARGET_KEY}");
    {
        let calls = origin.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_eq!(
            calls[0].authorization.as_deref(),
            Some(target_authorization.as_str())
        );
        assert_eq!(calls[0].path, "/v1/responses");
    }

    disable_inference_binding(&harness, &target);
    let disabled_revision = harness.state.settings_revision();
    let disabled_before = harness
        .state
        .db
        .lock()
        .get_account(&target)
        .unwrap()
        .unwrap();
    assert!(!disabled_before.enabled);
    assert_eq!(disabled_before.cooldown_until, before.cooldown_until);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{target}/model-tests"),
        json!({"modelId":"grok-4.5"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accountId"], target);
    assert_eq!(body["modelId"], "grok-4.5");
    assert_eq!(
        body["success"], false,
        "binding-disabled stored-key probe must fail closed: {body}"
    );
    assert_eq!(
        body["httpStatus"],
        Value::Null,
        "binding-disabled stored-key probe must not reach upstream: {body}"
    );
    assert!(body["durationMs"].as_u64().is_some(), "{body}");
    assert!(!body.to_string().contains(TARGET_KEY), "{body}");
    assert_eq!(harness.state.settings_revision(), disabled_revision);
    let disabled_after = harness
        .state
        .db
        .lock()
        .get_account(&target)
        .unwrap()
        .unwrap();
    assert!(!disabled_after.enabled);
    assert_eq!(
        disabled_after.cooldown_until,
        disabled_before.cooldown_until
    );
    assert_eq!(disabled_after.auth_error, disabled_before.auth_error);
    {
        let calls = origin.calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "binding-disabled stored-key must add zero origin hits and must not use a sibling: {calls:?}"
        );
    }
    harness.stop();
}

#[tokio::test]
async fn model_test_rejects_unknown_models_before_outbound() {
    let harness = start_loopback("account-model-test-unknown").await;
    let origin = start_origin().await;
    point_upstream(&harness, &origin);
    let account = create_go_account(&harness, "Known", SIBLING_KEY).await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account}/model-tests"),
        json!({"modelId":"not-a-current-model"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], ERROR_INVALID_REQUEST);
    assert!(origin.calls.lock().unwrap().is_empty());
    harness.stop();
}

#[tokio::test]
async fn custom_model_test_uses_the_declared_protocol_and_route_without_secrets() {
    let harness = start_loopback("account-model-test-custom").await;
    let origin = start_origin().await;
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        cas(
            &harness,
            json!({
                "name":"Custom",
                "key": CUSTOM_KEY,
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": {
                    "endpointUrl": origin.url,
                    "upstreamProtocol": "responses"
                },
                "modelCapabilities": [{
                    "publicModel":"declared-custom-model",
                    "upstreamModel":"upstream-custom-model:latest",
                    "protocol":"responses"
                }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let account = created["account"]["id"].as_str().unwrap();
    harness.enable_account(account);
    let before_revision = harness.state.settings_revision();
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account}/model-tests"),
        json!({"modelId":"declared-custom-model"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["protocol"], "responses");
    assert_eq!(body["modelId"], "declared-custom-model");
    assert_eq!(body["success"], true);
    assert_eq!(body["httpStatus"], 200);
    assert!(!body.to_string().contains(CUSTOM_KEY), "{body}");
    assert_eq!(harness.state.settings_revision(), before_revision);
    let calls = origin.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(calls[0].path, "/v1/responses");
    assert_eq!(calls[0].body["model"], "upstream-custom-model:latest");
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some("Bearer custom-account-model-secret")
    );
    assert_eq!(calls[0].x_api_key, None);
    harness.stop();
}

fn dynamic_create_body(
    name: &str,
    endpoint: &str,
    protocol: &str,
    auth: &str,
    key: Option<&str>,
    public_model: &str,
    upstream_model: &str,
) -> Value {
    let mut body = json!({
        "name": name,
        "endpointUrl": endpoint,
        "upstreamProtocol": protocol,
        "authKind": auth,
        "models": [{
            "publicModel": public_model,
            "upstreamModel": upstream_model
        }]
    });
    if let Some(key) = key {
        body["key"] = json!(key);
    }
    body
}

async fn account_id_for_provider(harness: &V3Harness, provider_id: &str) -> String {
    harness
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == provider_id)
        .map(|account| account.id)
        .unwrap_or_else(|| panic!("missing account for {provider_id}"))
}

struct DynamicExactCase {
    label: &'static str,
    protocol: &'static str,
    auth: &'static str,
    key: Option<&'static str>,
    sibling_key: Option<&'static str>,
    expected_path: &'static str,
    expected_authorization: Option<&'static str>,
    expected_x_api_key: Option<&'static str>,
}

#[tokio::test]
async fn dynamic_exact_account_probes_preserve_auth_and_honor_disablement() {
    let cases = [
        DynamicExactCase {
            label: "account-model-test-dyn-bearer",
            protocol: "chat_completions",
            auth: "bearer",
            key: Some("sk-dyn-bearer"),
            sibling_key: Some("sk-dyn-sibling"),
            expected_path: "/v1/chat/completions",
            expected_authorization: Some("Bearer sk-dyn-bearer"),
            expected_x_api_key: None,
        },
        DynamicExactCase {
            label: "account-model-test-dyn-x-api-key",
            protocol: "messages",
            auth: "x-api-key",
            key: Some("sk-dyn-x"),
            sibling_key: None,
            expected_path: "/v1/messages",
            expected_authorization: None,
            expected_x_api_key: Some("sk-dyn-x"),
        },
        DynamicExactCase {
            label: "account-model-test-dyn-none",
            protocol: "responses",
            auth: "none",
            key: None,
            sibling_key: None,
            expected_path: "/v1/responses",
            expected_authorization: None,
            expected_x_api_key: None,
        },
    ];
    for case in cases {
        let harness = start_loopback(case.label).await;
        let origin = start_origin().await;
        let mut config = harness.state.config();
        config.proxy_mode = ProxyMode::Direct;
        config.non_stream_timeout_secs = 5;
        harness.state.set_config(config).unwrap();
        let (status, created) = send_json(
            &harness,
            Method::POST,
            "/providers",
            cas(
                &harness,
                dynamic_create_body(
                    case.label,
                    &origin.url,
                    case.protocol,
                    case.auth,
                    case.key,
                    "lab-opus",
                    "vendor/opus",
                ),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{} {created}", case.label);
        let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
        let account = account_id_for_provider(&harness, &provider_id).await;
        harness.enable_account(&account);
        if let Some(sibling_key) = case.sibling_key {
            let (status, sibling) = send_json(
                &harness,
                Method::POST,
                "/accounts",
                cas(
                    &harness,
                    json!({
                        "name": "sibling",
                        "providerId": provider_id,
                        "key": sibling_key
                    }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{} {sibling}", case.label);
        }
        harness
            .state
            .db
            .lock()
            .set_account_cooldown(
                &account,
                Some(Utc::now() + chrono::Duration::hours(1)),
                Some("pre-existing cooldown"),
            )
            .unwrap();
        let before_revision = harness.state.settings_revision();
        let before = harness
            .state
            .db
            .lock()
            .get_account(&account)
            .unwrap()
            .unwrap();
        assert!(before.enabled, "{} {before:?}", case.label);
        assert!(before.cooldown_until.is_some());

        let (status, body) = send_json(
            &harness,
            Method::POST,
            &format!("/accounts/{account}/model-tests"),
            json!({"modelId":"lab-opus"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{} {body}", case.label);
        assert_eq!(body["accountId"], account);
        assert_eq!(body["modelId"], "lab-opus");
        assert_eq!(body["protocol"], case.protocol);
        assert_eq!(body["success"], true, "{} {body}", case.label);
        assert_eq!(body["httpStatus"], 200, "{} {body}", case.label);
        if let Some(key) = case.key {
            assert!(!body.to_string().contains(key), "{} {body}", case.label);
        }
        assert_eq!(harness.state.settings_revision(), before_revision);
        let after = harness
            .state
            .db
            .lock()
            .get_account(&account)
            .unwrap()
            .unwrap();
        assert_eq!(after.enabled, before.enabled);
        assert_eq!(after.cooldown_until, before.cooldown_until);
        assert_eq!(after.auth_error, before.auth_error);
        {
            let calls = origin.calls.lock().unwrap();
            assert_eq!(calls.len(), 1, "{} {calls:?}", case.label);
            assert_eq!(calls[0].path, case.expected_path);
            assert_eq!(calls[0].body["model"], "vendor/opus");
            assert_eq!(
                calls[0].authorization.as_deref(),
                case.expected_authorization
            );
            assert_eq!(calls[0].x_api_key.as_deref(), case.expected_x_api_key);
        }

        if case.key.is_some() {
            disable_inference_binding(&harness, &account);
            let disabled_revision = harness.state.settings_revision();
            let disabled_before = harness
                .state
                .db
                .lock()
                .get_account(&account)
                .unwrap()
                .unwrap();
            assert!(disabled_before.enabled);
            assert_eq!(disabled_before.cooldown_until, before.cooldown_until);

            let (status, body) = send_json(
                &harness,
                Method::POST,
                &format!("/accounts/{account}/model-tests"),
                json!({"modelId":"lab-opus"}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{} {body}", case.label);
            assert_eq!(body["accountId"], account);
            assert_eq!(body["modelId"], "lab-opus");
            assert_eq!(body["protocol"], case.protocol);
            assert_eq!(
                body["success"], false,
                "{} binding-disabled stored-key probe must fail closed: {body}",
                case.label
            );
            assert_eq!(
                body["httpStatus"],
                Value::Null,
                "{} binding-disabled stored-key probe must not reach upstream: {body}",
                case.label
            );
            if let Some(key) = case.key {
                assert!(!body.to_string().contains(key), "{} {body}", case.label);
            }
            assert_eq!(harness.state.settings_revision(), disabled_revision);
            let disabled_after = harness
                .state
                .db
                .lock()
                .get_account(&account)
                .unwrap()
                .unwrap();
            assert!(disabled_after.enabled);
            assert_eq!(
                disabled_after.cooldown_until,
                disabled_before.cooldown_until
            );
            assert_eq!(disabled_after.auth_error, disabled_before.auth_error);
            {
                let calls = origin.calls.lock().unwrap();
                assert_eq!(
                    calls.len(),
                    1,
                    "{} binding-disabled stored-key must add zero origin hits and must not use a sibling: {calls:?}",
                    case.label
                );
            }
        }
        harness.stop();
    }
}

#[tokio::test]
async fn dynamic_unknown_model_fails_before_outbound() {
    let harness = start_loopback("account-model-test-dyn-unknown").await;
    let origin = start_origin().await;
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    harness.state.set_config(config).unwrap();
    let (status, created) = send_json(
        &harness,
        Method::POST,
        "/providers",
        cas(
            &harness,
            dynamic_create_body(
                "Unknown",
                &origin.url,
                "chat_completions",
                "bearer",
                Some("sk-dyn-unknown"),
                "lab-opus",
                "vendor/opus",
            ),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"].as_str().unwrap().to_string();
    let account = account_id_for_provider(&harness, &provider_id).await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{account}/model-tests"),
        json!({"modelId":"not-a-current-model"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], ERROR_INVALID_REQUEST);
    assert!(origin.calls.lock().unwrap().is_empty());
    harness.stop();
}
