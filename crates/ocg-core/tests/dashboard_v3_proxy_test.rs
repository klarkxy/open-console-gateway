//! Dashboard V3 POST `/settings/test-proxy`: session, diagnostic overlay,
//! default-leg parity, secrecy, and V2 coexistence.

use ocg_core::dashboard_v3::{
    ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_UNAUTHORIZED, PROXY_TEST_TARGET,
};
#[cfg(debug_assertions)]
use ocg_core::dashboard_v3::{ERROR_OUTBOUND_FAILED, ProxyTestResponse};
use ocg_core::db::CURRENT_SCHEMA_VERSION;
#[cfg(debug_assertions)]
use ocg_core::models::ProxyListDirection;
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

#[cfg(debug_assertions)]
use axum::Router;
#[cfg(debug_assertions)]
use axum::extract::OriginalUri;
#[cfg(debug_assertions)]
use axum::http::HeaderMap;
#[cfg(debug_assertions)]
use axum::response::IntoResponse;
#[cfg(debug_assertions)]
use axum::routing::get;
#[cfg(debug_assertions)]
use ocg_core::dashboard_v3::install_proxy_test_target_for_tests;

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
    "proxyAuthorization",
    "proxy_authorization",
];

#[cfg(debug_assertions)]
const BODY_SECRET: &str = "sk-secret-upstream-body";
#[cfg(debug_assertions)]
const LOCATION_SECRET: &str = "http://evil.example/steal?token=abc";

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
            "proxy-test JSON leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in extra {
            assert!(
                !value.contains(secret),
                "proxy-test JSON leaked secret sample {secret}: {body}"
            );
        }
    }
}

async fn post_json(harness: &V3Harness, body: &Value) -> (StatusCode, Value) {
    let response = harness
        .client
        .post(format!("{}/settings/test-proxy", harness.v3_base))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn post_raw(harness: &V3Harness, body: &str) -> (StatusCode, Value) {
    let response = harness
        .client
        .post(format!("{}/settings/test-proxy", harness.v3_base))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

fn snapshot_config(harness: &V3Harness) -> (u64, u64, ocg_core::models::AppConfig) {
    (
        harness.state.settings_revision(),
        harness.state.process_generation(),
        harness.state.config(),
    )
}

fn assert_unmutated(harness: &V3Harness, before: &(u64, u64, ocg_core::models::AppConfig)) {
    assert_eq!(harness.state.settings_revision(), before.0);
    assert_eq!(harness.state.process_generation(), before.1);
    let after = harness.state.config();
    assert_eq!(after.proxy_mode, before.2.proxy_mode);
    assert_eq!(after.proxy_url, before.2.proxy_url);
    assert_eq!(after.proxy_list_direction, before.2.proxy_list_direction);
    assert_eq!(after.upstream_base_url, before.2.upstream_base_url);
    assert_eq!(after.gateway_key, before.2.gateway_key);
}

#[cfg(debug_assertions)]
#[derive(Clone, Debug)]
struct CapturedCall {
    path: String,
    authorization: Option<String>,
    x_api_key: Option<String>,
    x_goog_api_key: Option<String>,
    cookie: Option<String>,
    proxy_authorization: Option<String>,
}

#[cfg(debug_assertions)]
struct DiagnosticOrigin {
    url: String,
    calls: Arc<Mutex<Vec<CapturedCall>>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

#[cfg(debug_assertions)]
impl DiagnosticOrigin {
    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[cfg(debug_assertions)]
async fn start_diagnostic_origin(status: StatusCode, body: &'static str) -> DiagnosticOrigin {
    start_diagnostic_origin_with(status, body, None).await
}

#[cfg(debug_assertions)]
async fn start_redirect_origin(location: String) -> DiagnosticOrigin {
    start_diagnostic_origin_with(StatusCode::FOUND, BODY_SECRET, Some(location)).await
}

#[cfg(debug_assertions)]
async fn start_diagnostic_origin_with(
    status: StatusCode,
    body: &'static str,
    location: Option<String>,
) -> DiagnosticOrigin {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let app = Router::new().fallback(get(move |uri: OriginalUri, headers: HeaderMap| {
        let calls = calls_for_handler.clone();
        let location = location.clone();
        async move {
            calls.lock().unwrap().push(CapturedCall {
                path: uri.0.path().to_string(),
                authorization: header_value(&headers, "authorization"),
                x_api_key: header_value(&headers, "x-api-key"),
                x_goog_api_key: header_value(&headers, "x-goog-api-key"),
                cookie: header_value(&headers, "cookie"),
                proxy_authorization: header_value(&headers, "proxy-authorization"),
            });
            if let Some(location) = location {
                return (
                    StatusCode::FOUND,
                    [(axum::http::header::LOCATION, location)],
                    body.to_string(),
                )
                    .into_response();
            }
            (status, body.to_string()).into_response()
        }
    }));
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
    DiagnosticOrigin {
        url: format!("http://{addr}/"),
        calls,
        _stop: stop,
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
async fn closed_proxy_addr() -> String {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    format!("http://{address}")
}

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
}

#[test]
fn dashboard_v3_proxy_test_source_keeps_the_fixed_production_target() {
    let source = include_str!("../src/dashboard_v3/proxy_test.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("production source precedes tests");
    assert!(production.contains(PROXY_TEST_TARGET));
    assert!(production.contains("configured_builder(&config)"));
    assert!(production.contains("#[cfg(debug_assertions)]"));
    assert!(production.contains("#[cfg(not(debug_assertions))]"));
    let release_idx = production
        .find("#[cfg(not(debug_assertions))]")
        .expect("release path");
    let release = &production[release_idx..];
    assert!(release.contains("PROXY_TEST_TARGET"));
    assert!(!release.contains("install_proxy_test_target_for_tests"));
    assert!(!release.contains("PROXY_TEST_TARGET_OVERRIDES"));
}

#[tokio::test]
async fn dashboard_v3_proxy_test_requires_the_v3_session() {
    let harness = start_public("proxy-test-auth").await;

    let (status, body) = post_json(&harness, &json!({ "proxyMode": "direct" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);

    let v2 = harness
        .client
        .post(format!("{}/settings/test-proxy", harness.v2_base))
        .json(&json!({
            "proxy_mode": "direct",
            "proxy_url": "",
            "upstream_base_url": "https://opencode.ai/zen/go"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::UNAUTHORIZED);
    let v2_body = v2.text().await.unwrap();
    assert!(
        v2_body.is_empty(),
        "V2 must stay an empty 401, got {v2_body}"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_login_cookie_authorizes_proxy_test() {
    let harness = start_public("proxy-test-cookie").await;
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
        .post(format!("{}/settings/test-proxy", harness.v3_base))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let authorized = harness
        .client
        .post(format!("{}/settings/test-proxy", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::BAD_REQUEST);
    let body: Value = authorized.json().await.unwrap();
    assert_v3_error(&body, ERROR_INVALID_JSON);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_proxy_test_rejects_unknown_fields_and_malformed_modes() {
    let harness = start_loopback("proxy-test-validate").await;
    let before = snapshot_config(&harness);

    let (status, body) = post_raw(&harness, "not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(&harness, &json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "direct",
            "upstreamBaseUrl": "http://127.0.0.1"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "direct",
            "expectedRevision": before.0
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(&harness, &json!({ "proxyMode": "bogus" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("proxyMode must be auto, manual, direct, or list"),
        "{body}"
    );
    assert_eq!(body["currentRevision"], before.0);
    assert_eq!(body["processGeneration"], before.1);
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "list",
            "proxyUrl": "http://127.0.0.1:7890",
            "proxyListDirection": "both"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "manual",
            "proxyUrl": ""
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(body["message"], "manual proxy mode requires a proxy URL");
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "list",
            "proxyUrl": "",
            "proxyListDirection": "whitelist"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(body["message"], "list proxy mode requires a proxy URL");
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "manual",
            "proxyUrl": "http://user:pass@127.0.0.1:7890"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_eq!(body["message"], "proxy URL must not include credentials");
    assert_secret_free(&body, &["user:pass", "user:pass@"]);
    assert_unmutated(&harness, &before);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_proxy_test_direct_manual_auto_and_list_default_legs() {
    let harness = start_loopback("proxy-test-legs").await;
    let origin = start_diagnostic_origin(StatusCode::UNAUTHORIZED, BODY_SECRET).await;
    let _guard =
        install_proxy_test_target_for_tests(harness.state.process_generation(), origin.url.clone());
    let before = snapshot_config(&harness);
    let primary = before.2.gateway_key.clone();

    let (status, body) = post_json(&harness, &json!({ "proxyMode": "direct" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: ProxyTestResponse = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.proxy_mode, ocg_core::dashboard_v3::ProxyMode::Direct);
    assert_eq!(parsed.status, StatusCode::UNAUTHORIZED.as_u16());
    assert_eq!(parsed.revision, before.0);
    assert_eq!(parsed.process_generation, before.1);
    assert!(body.get("latency_ms").is_none());
    assert!(body.get("proxyUrl").is_none());
    assert!(body.get("upstreamBaseUrl").is_none());
    assert_eq!(origin.call_count(), 1);
    let captured = origin.calls.lock().unwrap()[0].clone();
    assert_eq!(captured.path, "/");
    assert!(captured.authorization.is_none());
    assert!(captured.x_api_key.is_none());
    assert!(captured.x_goog_api_key.is_none());
    assert!(captured.cookie.is_none());
    assert!(captured.proxy_authorization.is_none());
    assert_secret_free(&body, &[BODY_SECRET, &primary, LOCATION_SECRET]);
    assert_unmutated(&harness, &before);

    let dead_proxy = closed_proxy_addr().await;
    let after_direct = origin.call_count();
    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "manual",
            "proxyUrl": dead_proxy
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert_eq!(body["currentRevision"], before.0);
    assert_eq!(body["processGeneration"], before.1);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("outbound connection test"),
        "{body}"
    );
    assert_eq!(
        origin.call_count(),
        after_direct,
        "manual must not fall back"
    );
    assert_secret_free(&body, &[BODY_SECRET, &primary, "user:pass"]);
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "auto",
            "proxyUrl": dead_proxy
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: ProxyTestResponse = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.proxy_mode, ocg_core::dashboard_v3::ProxyMode::Auto);
    assert_eq!(parsed.status, StatusCode::UNAUTHORIZED.as_u16());
    assert_eq!(origin.call_count(), after_direct + 1);
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "list",
            "proxyUrl": dead_proxy,
            "proxyListDirection": "whitelist"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: ProxyTestResponse = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.proxy_mode, ocg_core::dashboard_v3::ProxyMode::List);
    assert_eq!(parsed.status, StatusCode::UNAUTHORIZED.as_u16());
    assert_eq!(origin.call_count(), after_direct + 2);
    assert_unmutated(&harness, &before);

    let after_whitelist = origin.call_count();
    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "list",
            "proxyUrl": dead_proxy,
            "proxyListDirection": "blacklist"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert_eq!(origin.call_count(), after_whitelist);
    assert_unmutated(&harness, &before);

    let mut persisted = harness.state.config();
    persisted.proxy_list_direction = ProxyListDirection::Blacklist;
    persisted.proxy_url = dead_proxy.clone();
    harness.state.set_config(persisted).unwrap();
    let after_persist = snapshot_config(&harness);
    let (status, body) = post_json(
        &harness,
        &json!({
            "proxyMode": "list",
            "proxyUrl": dead_proxy
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert_eq!(origin.call_count(), after_whitelist);
    assert_eq!(harness.state.settings_revision(), after_persist.0);
    assert_eq!(
        harness.state.config().proxy_list_direction,
        ProxyListDirection::Blacklist
    );

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_proxy_test_does_not_follow_redirects_or_send_credentials() {
    let harness = start_loopback("proxy-test-redirect").await;
    let hop = start_diagnostic_origin(StatusCode::OK, BODY_SECRET).await;
    let origin = start_redirect_origin(hop.url.clone()).await;
    let _guard =
        install_proxy_test_target_for_tests(harness.state.process_generation(), origin.url.clone());
    let before = snapshot_config(&harness);
    let primary = before.2.gateway_key.clone();

    let (status, body) = post_json(&harness, &json!({ "proxyMode": "direct" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: ProxyTestResponse = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.status, StatusCode::FOUND.as_u16());
    assert_eq!(origin.call_count(), 1);
    assert_eq!(hop.call_count(), 0, "redirects must not be followed");
    let captured = origin.calls.lock().unwrap()[0].clone();
    assert_eq!(captured.path, "/");
    assert!(captured.authorization.is_none());
    assert!(captured.x_api_key.is_none());
    assert!(captured.x_goog_api_key.is_none());
    assert!(captured.cookie.is_none());
    assert_secret_free(
        &body,
        &[BODY_SECRET, LOCATION_SECRET, &primary, &hop.url, "steal"],
    );
    assert_unmutated(&harness, &before);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_proxy_test_overrides_are_generation_isolated() {
    let origin_a = start_diagnostic_origin(StatusCode::OK, "iso-a").await;
    let origin_b = start_diagnostic_origin(StatusCode::NO_CONTENT, "iso-b").await;
    let harness_a = start_loopback("proxy-test-iso-a").await;
    let harness_b = start_loopback("proxy-test-iso-b").await;
    assert_ne!(
        harness_a.state.process_generation(),
        harness_b.state.process_generation()
    );
    let _guard_a = install_proxy_test_target_for_tests(
        harness_a.state.process_generation(),
        origin_a.url.clone(),
    );
    let _guard_b = install_proxy_test_target_for_tests(
        harness_b.state.process_generation(),
        origin_b.url.clone(),
    );

    let body_a = json!({ "proxyMode": "direct" });
    let body_b = json!({ "proxyMode": "direct" });
    let (result_a, result_b) = tokio::join!(
        post_json(&harness_a, &body_a),
        post_json(&harness_b, &body_b),
    );
    assert_eq!(result_a.0, StatusCode::OK, "{}", result_a.1);
    assert_eq!(result_b.0, StatusCode::OK, "{}", result_b.1);
    assert_eq!(result_a.1["status"], StatusCode::OK.as_u16());
    assert_eq!(result_b.1["status"], StatusCode::NO_CONTENT.as_u16());
    assert_eq!(origin_a.call_count(), 1);
    assert_eq!(origin_b.call_count(), 1);

    harness_a.stop();
    harness_b.stop();
}

#[tokio::test]
async fn dashboard_v3_proxy_test_coexists_with_v2() {
    let harness = start_loopback("proxy-test-v2").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let v2_hits = Arc::new(Mutex::new(0_u32));
    let hits = v2_hits.clone();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        *hits.lock().unwrap() += 1;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut request = vec![0_u8; 8192];
        let _ = stream.read(&mut request).await;
        let body = "v2-origin";
        let response = format!(
            "HTTP/1.1 418 I'm a teapot\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });

    let before = snapshot_config(&harness);
    let v2 = harness
        .client
        .post(format!("{}/settings/test-proxy", harness.v2_base))
        .json(&json!({
            "proxy_mode": "direct",
            "proxy_url": "",
            "upstream_base_url": format!("http://{address}")
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::OK);
    let v2_body: Value = v2.json().await.unwrap();
    assert_eq!(v2_body["status"], 418);
    assert_eq!(v2_body["proxy_mode"], "direct");
    assert!(v2_body.get("processGeneration").is_none());
    assert_eq!(*v2_hits.lock().unwrap(), 1);
    assert_unmutated(&harness, &before);

    #[cfg(debug_assertions)]
    {
        let origin = start_diagnostic_origin(StatusCode::NO_CONTENT, BODY_SECRET).await;
        let _guard = install_proxy_test_target_for_tests(
            harness.state.process_generation(),
            origin.url.clone(),
        );
        let (status, body) = post_json(
            &harness,
            &json!({
                "proxyMode": "direct",
                "upstreamBaseUrl": format!("http://{address}")
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_v3_error(&body, ERROR_INVALID_JSON);
        assert_eq!(origin.call_count(), 0);
        assert_eq!(*v2_hits.lock().unwrap(), 1);

        let (status, body) = post_json(&harness, &json!({ "proxyMode": "direct" })).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], StatusCode::NO_CONTENT.as_u16());
        assert_eq!(body["revision"], before.0);
        assert_eq!(body["processGeneration"], before.1);
        assert_eq!(origin.call_count(), 1);
        assert_eq!(*v2_hits.lock().unwrap(), 1);
        assert_unmutated(&harness, &before);
    }

    harness.stop();
}
