//! Dashboard V3 session-protected desktop update check/status/install:
//! auth, exact DTOs, CAS, lifecycle, outbound policy, redaction, V2 coexistence.

use ocg_core::dashboard_v3::{
    DesktopUpdate, ERROR_CONFLICT, ERROR_INTERNAL, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST,
    ERROR_MISSING_EXPECTED_REVISION, ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED,
    GITHUB_LATEST_RELEASE_API, GITHUB_LATEST_RELEASE_URL, UpdateCheck,
};
use ocg_core::db::CURRENT_SCHEMA_VERSION;
use reqwest::StatusCode;
use serde_json::{Map, Value, json};
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
use ocg_core::dashboard_v3::{ERROR_OUTBOUND_FAILED, install_update_check_url_for_tests};
#[cfg(debug_assertions)]
use ocg_core::models::{ProxyListDirection, ProxyMode};
#[cfg(debug_assertions)]
use tokio::sync::Notify;

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
const BODY_SECRET: &str = "sk-secret-github-body";
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
            "updater JSON leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in extra {
            assert!(
                !value.contains(secret),
                "updater JSON leaked secret sample {secret}: {body}"
            );
        }
    }
}

fn snapshot_identity(harness: &V3Harness) -> (u64, u64, ocg_core::models::AppConfig) {
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
    assert_eq!(after.gateway_key, before.2.gateway_key);
    assert_eq!(after.upstream_base_url, before.2.upstream_base_url);
}

fn cas_patch(harness: &V3Harness, patch: Value) -> Value {
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

fn newer_version() -> String {
    let current = env!("CARGO_PKG_VERSION");
    let major: u64 = current
        .split('.')
        .next()
        .unwrap()
        .parse()
        .expect("package major");
    format!("{}.0.0", major + 1)
}

async fn get_path(harness: &V3Harness, path: &str) -> (StatusCode, Value) {
    let response = harness
        .client
        .get(format!("{}{path}", harness.v3_base))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

async fn post_json(harness: &V3Harness, body: &Value) -> (StatusCode, Value) {
    let response = harness
        .client
        .post(format!("{}/settings/install-update", harness.v3_base))
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
        .post(format!("{}/settings/install-update", harness.v3_base))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

#[cfg(debug_assertions)]
#[derive(Clone, Debug)]
struct CapturedCall {
    path: String,
    accept: Option<String>,
    user_agent: Option<String>,
    authorization: Option<String>,
    x_api_key: Option<String>,
    cookie: Option<String>,
    proxy_authorization: Option<String>,
}

#[cfg(debug_assertions)]
struct GithubOrigin {
    url: String,
    calls: Arc<Mutex<Vec<CapturedCall>>>,
    _stop: tokio::sync::oneshot::Sender<()>,
}

#[cfg(debug_assertions)]
impl GithubOrigin {
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

#[cfg(debug_assertions)]
fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

#[cfg(debug_assertions)]
#[derive(Clone)]
struct OriginScript {
    status: StatusCode,
    body: String,
    location: Option<String>,
    hold: Option<(Arc<Notify>, Arc<Notify>)>,
}

#[cfg(debug_assertions)]
async fn start_github_origin(status: StatusCode, body: &str) -> GithubOrigin {
    start_github_origin_with(OriginScript {
        status,
        body: body.to_string(),
        location: None,
        hold: None,
    })
    .await
}

#[cfg(debug_assertions)]
async fn start_github_origin_with(script: OriginScript) -> GithubOrigin {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let app = Router::new().fallback(get(move |uri: OriginalUri, headers: HeaderMap| {
        let calls = calls_for_handler.clone();
        let script = script.clone();
        async move {
            calls.lock().unwrap().push(CapturedCall {
                path: uri.0.path().to_string(),
                accept: header_value(&headers, "accept"),
                user_agent: header_value(&headers, "user-agent"),
                authorization: header_value(&headers, "authorization"),
                x_api_key: header_value(&headers, "x-api-key"),
                cookie: header_value(&headers, "cookie"),
                proxy_authorization: header_value(&headers, "proxy-authorization"),
            });
            if let Some((received, release)) = &script.hold {
                received.notify_one();
                release.notified().await;
            }
            let mut response = axum::http::Response::builder().status(script.status);
            if let Some(location) = &script.location {
                response = response.header(axum::http::header::LOCATION, location.as_str());
            }
            response
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(script.body.clone()))
                .unwrap()
                .into_response()
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
    GithubOrigin {
        url: format!("http://{addr}/"),
        calls,
        _stop: stop,
    }
}

#[cfg(debug_assertions)]
fn point_direct(harness: &V3Harness) {
    let mut config = harness.state.config();
    config.proxy_mode = ProxyMode::Direct;
    config.proxy_url.clear();
    harness
        .state
        .set_config(config)
        .expect("direct proxy mode should persist");
}

#[cfg(debug_assertions)]
async fn closed_proxy_addr() -> String {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = closed.local_addr().unwrap();
    drop(closed);
    format!("http://{address}")
}

#[cfg(debug_assertions)]
fn github_tag_body(tag: &str) -> String {
    json!({ "tag_name": tag, "html_url": "https://example.invalid/should-not-leak" }).to_string()
}

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
}

#[test]
fn dashboard_v3_updater_source_keeps_the_fixed_github_api() {
    let source = include_str!("../src/dashboard_v3/updater.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("production source precedes tests");
    assert!(production.contains(GITHUB_LATEST_RELEASE_API));
    assert!(production.contains(GITHUB_LATEST_RELEASE_URL));
    assert!(production.contains("configured_builder(config)"));
    assert!(production.contains("#[cfg(debug_assertions)]"));
    assert!(production.contains("#[cfg(not(debug_assertions))]"));
    let release_idx = production
        .find("#[cfg(not(debug_assertions))]")
        .expect("release path");
    let release = &production[release_idx..];
    assert!(release.contains("GITHUB_LATEST_RELEASE_API"));
    assert!(!release.contains("install_update_check_url_for_tests"));
    assert!(!release.contains("UPDATE_CHECK_URL_OVERRIDES"));
}

#[test]
fn dashboard_v3_mod_reexports_update_check_installer_only_under_debug_assertions() {
    let source = include_str!("../src/dashboard_v3/mod.rs");
    let idx = source
        .find("install_update_check_url_for_tests")
        .expect("installer re-export");
    let before = &source[..idx];
    let cfg_idx = before
        .rfind("#[cfg(debug_assertions)]")
        .expect("installer re-export must be debug-only");
    assert!(
        !before[cfg_idx..].contains("#[cfg(not(debug_assertions))]"),
        "installer re-export compiled into release"
    );
}

#[tokio::test]
async fn dashboard_v3_updater_routes_require_the_v3_session() {
    let harness = start_public("updater-auth").await;

    for path in [
        "/settings/check-update",
        "/settings/update-status",
        "/settings/install-update",
    ] {
        let response = if path.ends_with("install-update") {
            harness
                .client
                .post(format!("{}{path}", harness.v3_base))
                .json(&json!({
                    "expectedRevision": 0,
                    "processGeneration": 0,
                    "expectedVersion": newer_version()
                }))
                .send()
                .await
                .unwrap()
        } else {
            harness
                .client
                .get(format!("{}{path}", harness.v3_base))
                .send()
                .await
                .unwrap()
        };
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        let body: Value = response.json().await.unwrap();
        assert_v3_error(&body, ERROR_UNAUTHORIZED);
        assert_eq!(body["currentRevision"], Value::Null);
        assert_eq!(body["processGeneration"], Value::Null);
        assert_secret_free(&body, &[GITHUB_LATEST_RELEASE_API]);
    }

    for path in [
        "/settings/check-update",
        "/settings/update-status",
        "/settings/install-update",
    ] {
        let v2 = if path.ends_with("install-update") {
            harness
                .client
                .post(format!("{}{path}", harness.v2_base))
                .json(&json!({ "expected_version": newer_version() }))
                .send()
                .await
                .unwrap()
        } else {
            harness
                .client
                .get(format!("{}{path}", harness.v2_base))
                .send()
                .await
                .unwrap()
        };
        assert_eq!(v2.status(), StatusCode::UNAUTHORIZED, "v2 {path}");
        let v2_body = v2.text().await.unwrap();
        assert!(
            v2_body.is_empty(),
            "V2 must stay an empty 401, got {v2_body}"
        );
    }

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_login_cookie_authorizes_updater_routes() {
    let harness = start_public("updater-cookie").await;
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
        .get(format!("{}/settings/update-status", harness.v3_base))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let authorized = harness
        .client
        .get(format!("{}/settings/update-status", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    let body: Value = authorized.json().await.unwrap();
    let parsed: DesktopUpdate = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        parsed.phase,
        ocg_core::dashboard_v3::DesktopUpdatePhase::Idle
    );
    assert_eq!(parsed.current_version, env!("CARGO_PKG_VERSION"));
    assert!(!parsed.install_supported);
    assert_eq!(body["total"], Value::Null);
    assert_eq!(body["error"], Value::Null);
    assert!(body.get("current_version").is_none());

    let install = harness
        .client
        .post(format!("{}/settings/install-update", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&cas_patch(
            &harness,
            json!({ "expectedVersion": newer_version() }),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(install.status(), StatusCode::BAD_REQUEST);
    let install_body: Value = install.json().await.unwrap();
    assert_v3_error(&install_body, ERROR_INVALID_REQUEST);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_update_status_is_idle_camelcase_nulls_and_does_not_bump() {
    let harness = start_loopback("updater-status-idle").await;
    let before = snapshot_identity(&harness);

    let (status, body) = get_path(&harness, "/settings/update-status").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: DesktopUpdate = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        body,
        json!({
            "phase": "idle",
            "downloaded": 0,
            "total": null,
            "error": null,
            "currentVersion": env!("CARGO_PKG_VERSION"),
            "installSupported": false,
            "revision": before.0,
            "processGeneration": before.1,
        })
    );
    assert_eq!(parsed.revision, before.0);
    assert_eq!(parsed.process_generation, before.1);
    assert_secret_free(&body, &[GITHUB_LATEST_RELEASE_API, &before.2.gateway_key]);
    assert_unmutated(&harness, &before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_install_requires_cas_tokens_and_rejects_unknown_fields() {
    let harness = start_loopback("updater-install-json").await;
    let before = snapshot_identity(&harness);
    let started = Arc::new(Mutex::new(Vec::new()));
    let captured = started.clone();
    harness
        .state
        .set_desktop_update_starter(Arc::new(move |version| {
            captured.lock().unwrap().push(version);
            Ok(())
        }));

    let (status, body) = post_raw(&harness, "not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_eq!(body["currentRevision"], Value::Null);

    let (status, body) = post_json(
        &harness,
        &json!({
            "processGeneration": before.1,
            "expectedVersion": newer_version()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);

    let (status, body) = post_json(
        &harness,
        &json!({
            "expectedRevision": before.0,
            "expectedVersion": newer_version()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let (status, body) = post_json(
        &harness,
        &json!({
            "expectedRevision": before.0,
            "processGeneration": before.1,
            "expected_version": newer_version()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let (status, body) = post_json(
        &harness,
        &cas_patch(
            &harness,
            json!({
                "expectedVersion": newer_version(),
                "key": "sk-secret"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_secret_free(&body, &["sk-secret"]);

    assert!(started.lock().unwrap().is_empty());
    assert_unmutated(&harness, &before);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_revision_or_generation_rejects_install() {
    let harness = start_loopback("updater-install-cas").await;
    let started = Arc::new(Mutex::new(Vec::new()));
    let captured = started.clone();
    harness
        .state
        .set_desktop_update_starter(Arc::new(move |version| {
            captured.lock().unwrap().push(version);
            Ok(())
        }));
    let before = snapshot_identity(&harness);

    let (status, body) = post_json(
        &harness,
        &json!({
            "expectedRevision": before.0 + 1,
            "processGeneration": before.1,
            "expectedVersion": newer_version()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], before.0);
    assert_eq!(body["processGeneration"], before.1);

    let (status, body) = post_json(
        &harness,
        &json!({
            "expectedRevision": before.0,
            "processGeneration": before.1.wrapping_add(1),
            "expectedVersion": newer_version()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);

    assert!(started.lock().unwrap().is_empty());
    assert_unmutated(&harness, &before);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_install_is_strictly_newer_atomic_and_retriable() {
    let harness = start_loopback("updater-install-lifecycle").await;
    let started = Arc::new(Mutex::new(Vec::new()));
    let captured = started.clone();
    harness
        .state
        .set_desktop_update_starter(Arc::new(move |version| {
            captured.lock().unwrap().push(version);
            Ok(())
        }));
    let before = snapshot_identity(&harness);
    let current = env!("CARGO_PKG_VERSION").to_string();
    let newer = newer_version();

    for rejected in [
        current.clone(),
        "0.0.1".to_string(),
        "not-a-version".to_string(),
    ] {
        let (status, body) = post_json(
            &harness,
            &cas_patch(&harness, json!({ "expectedVersion": rejected })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected} {body}");
        assert_v3_error(&body, ERROR_INVALID_REQUEST);
        assert_eq!(body["currentRevision"], before.0);
        assert_eq!(body["processGeneration"], before.1);
    }
    assert!(started.lock().unwrap().is_empty());

    let (status, body) = post_json(
        &harness,
        &cas_patch(
            &harness,
            json!({ "expectedVersion": format!("v{newer}-beta.1") }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let parsed: DesktopUpdate = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(
        parsed.phase,
        ocg_core::dashboard_v3::DesktopUpdatePhase::Checking
    );
    assert_eq!(parsed.downloaded, 0);
    assert_eq!(parsed.total, None);
    assert_eq!(parsed.error, None);
    assert!(parsed.install_supported);
    assert_eq!(parsed.revision, before.0);
    assert_eq!(parsed.process_generation, before.1);
    assert_eq!(body["total"], Value::Null);
    assert_eq!(body["error"], Value::Null);
    assert_eq!(
        started.lock().unwrap().as_slice(),
        [format!("{newer}-beta.1")]
    );
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &cas_patch(&harness, json!({ "expectedVersion": newer })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_CONFLICT);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("already in progress"),
        "{body}"
    );
    assert_eq!(started.lock().unwrap().len(), 1);

    assert!(harness.state.set_desktop_update_progress(64, Some(128)));
    let (status, body) = get_path(&harness, "/settings/update-status").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["phase"], "downloading");
    assert_eq!(body["downloaded"], 64);
    assert_eq!(body["total"], 128);
    assert_eq!(body["error"], Value::Null);
    assert_eq!(body["revision"], before.0);

    assert!(harness.state.set_desktop_update_installing());
    harness
        .state
        .set_desktop_update_failed("signature verification failed");
    let (status, body) = get_path(&harness, "/settings/update-status").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["phase"], "failed");
    assert_eq!(body["error"], "signature verification failed");
    assert_eq!(body["revision"], before.0);
    assert_unmutated(&harness, &before);

    let (status, body) = post_json(
        &harness,
        &cas_patch(&harness, json!({ "expectedVersion": newer })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["phase"], "checking");
    assert_eq!(body["downloaded"], 0);
    assert_eq!(body["total"], Value::Null);
    assert_eq!(body["error"], Value::Null);
    assert_eq!(started.lock().unwrap().len(), 2);
    assert_unmutated(&harness, &before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_install_unsupported_busy_failure_and_concurrency() {
    let unsupported = start_loopback("updater-install-unsupported").await;
    let before = snapshot_identity(&unsupported);
    let (status, body) = post_json(
        &unsupported,
        &cas_patch(&unsupported, json!({ "expectedVersion": newer_version() })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("unavailable in this runtime"),
        "{body}"
    );
    assert_unmutated(&unsupported, &before);
    unsupported.stop();

    let harness = start_loopback("updater-install-fail").await;
    let leak = harness.state.config().gateway_key.clone();
    harness
        .state
        .set_desktop_update_starter(Arc::new(move |_| {
            anyhow::bail!("starter failed for {leak} at https://api.github.com/repos/klarkxy/opencode-go-mgr/releases/latest")
        }));
    let before = snapshot_identity(&harness);
    let (status, body) = post_json(
        &harness,
        &cas_patch(&harness, json!({ "expectedVersion": newer_version() })),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_v3_error(&body, ERROR_INTERNAL);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_secret_free(
        &body,
        &[
            &before.2.gateway_key,
            GITHUB_LATEST_RELEASE_API,
            "api.github.com",
        ],
    );
    let failed = harness.state.desktop_update_status();
    assert_eq!(failed.phase, ocg_core::desktop::DesktopUpdatePhase::Failed);
    let (status, status_body) = get_path(&harness, "/settings/update-status").await;
    assert_eq!(status, StatusCode::OK, "{status_body}");
    assert_eq!(status_body["phase"], "failed");
    assert_secret_free(
        &status_body,
        &[
            &before.2.gateway_key,
            GITHUB_LATEST_RELEASE_API,
            "api.github.com",
        ],
    );
    assert_unmutated(&harness, &before);

    let started = Arc::new(Mutex::new(Vec::new()));
    let captured = started.clone();
    let retry = start_loopback("updater-install-concurrent").await;
    retry
        .state
        .set_desktop_update_starter(Arc::new(move |version| {
            captured.lock().unwrap().push(version);
            Ok(())
        }));
    let payload = cas_patch(&retry, json!({ "expectedVersion": newer_version() }));
    let (first, second) = tokio::join!(post_json(&retry, &payload), post_json(&retry, &payload));
    let statuses = [first.0, second.0];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::ACCEPTED)
            .count(),
        1,
        "{first:?} {second:?}"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1,
        "{first:?} {second:?}"
    );
    assert_eq!(started.lock().unwrap().len(), 1);
    retry.stop();
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_updater_coexists_with_v2_status_and_install() {
    let harness = start_loopback("updater-v2").await;
    let started = Arc::new(Mutex::new(Vec::new()));
    let captured = started.clone();
    harness
        .state
        .set_desktop_update_starter(Arc::new(move |version| {
            captured.lock().unwrap().push(version);
            Ok(())
        }));
    let before = snapshot_identity(&harness);
    let newer = newer_version();

    let v2_status = harness
        .client
        .get(format!("{}/settings/update-status", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_status.status(), StatusCode::OK);
    let v2_body: Value = v2_status.json().await.unwrap();
    assert_eq!(v2_body["phase"], "idle");
    assert_eq!(v2_body["current_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v2_body["install_supported"], true);
    assert!(v2_body.get("revision").is_none());
    assert!(v2_body.get("currentVersion").is_none());

    let v2_install = harness
        .client
        .post(format!("{}/settings/install-update", harness.v2_base))
        .json(&json!({ "expected_version": newer }))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_install.status(), StatusCode::ACCEPTED);
    let v2_accepted: Value = v2_install.json().await.unwrap();
    assert_eq!(v2_accepted["phase"], "checking");
    assert_eq!(
        started.lock().unwrap().as_slice(),
        std::slice::from_ref(&newer)
    );

    let (status, body) = get_path(&harness, "/settings/update-status").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["phase"], "checking");
    assert_eq!(body["revision"], before.0);
    assert_eq!(body["processGeneration"], before.1);
    assert!(body.get("current_version").is_none());

    let (status, body) = post_json(
        &harness,
        &cas_patch(&harness, json!({ "expectedVersion": newer })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_CONFLICT);
    assert_eq!(started.lock().unwrap().len(), 1);
    assert_unmutated(&harness, &before);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_check_update_is_captured_secret_free_and_does_not_bump() {
    let harness = start_loopback("updater-check-success").await;
    point_direct(&harness);
    let newer = newer_version();
    let origin = start_github_origin(StatusCode::OK, &github_tag_body(&format!("v{newer}"))).await;
    let _guard =
        install_update_check_url_for_tests(harness.state.process_generation(), origin.url.clone());
    let before = snapshot_identity(&harness);
    let primary = before.2.gateway_key.clone();

    let (status, body) = get_path(&harness, "/settings/check-update").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: UpdateCheck = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.current_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(parsed.latest_version, newer);
    assert!(parsed.update_available);
    assert_eq!(parsed.release_url, GITHUB_LATEST_RELEASE_URL);
    assert!(!parsed.install_supported);
    assert_eq!(parsed.revision, before.0);
    assert_eq!(parsed.process_generation, before.1);
    assert_eq!(body["releaseUrl"], GITHUB_LATEST_RELEASE_URL);
    assert!(body.get("release_url").is_none());
    assert_eq!(origin.call_count(), 1);
    let captured = origin.last();
    assert_eq!(captured.path, "/");
    assert_eq!(
        captured.accept.as_deref(),
        Some("application/vnd.github+json")
    );
    assert_eq!(
        captured.user_agent.as_deref(),
        Some(concat!("ocg-manager/", env!("CARGO_PKG_VERSION")))
    );
    assert!(captured.authorization.is_none());
    assert!(captured.x_api_key.is_none());
    assert!(captured.cookie.is_none());
    assert!(captured.proxy_authorization.is_none());
    assert_secret_free(
        &body,
        &[
            &primary,
            BODY_SECRET,
            LOCATION_SECRET,
            GITHUB_LATEST_RELEASE_API,
            &origin.url,
            "example.invalid",
        ],
    );
    assert_unmutated(&harness, &before);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_check_update_captures_revision_before_the_github_await() {
    let harness = start_loopback("updater-check-capture").await;
    point_direct(&harness);
    let received = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let newer = newer_version();
    let origin = start_github_origin_with(OriginScript {
        status: StatusCode::OK,
        body: github_tag_body(&format!("v{newer}")),
        location: None,
        hold: Some((received.clone(), release.clone())),
    })
    .await;
    let _guard =
        install_update_check_url_for_tests(harness.state.process_generation(), origin.url.clone());
    let before = snapshot_identity(&harness);

    let request = harness
        .client
        .get(format!("{}/settings/check-update", harness.v3_base));
    let pending = tokio::spawn(async move {
        let response = request.send().await.unwrap();
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    });
    received.notified().await;
    harness.state.bump_settings_revision();
    assert_eq!(harness.state.settings_revision(), before.0 + 1);
    release.notify_one();
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], before.0);
    assert_eq!(body["processGeneration"], before.1);
    assert_eq!(harness.state.settings_revision(), before.0 + 1);
    assert_eq!(harness.state.process_generation(), before.1);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_check_update_outbound_failures_are_redacted_and_redirects_remain_compatible()
{
    let harness = start_loopback("updater-check-errors").await;
    point_direct(&harness);
    let before_direct = snapshot_identity(&harness);
    let primary = before_direct.2.gateway_key.clone();

    let invalid = start_github_origin(StatusCode::OK, r#"{"tag_name":"not-a-version"}"#).await;
    let _guard =
        install_update_check_url_for_tests(harness.state.process_generation(), invalid.url.clone());
    let before = snapshot_identity(&harness);
    let (status, body) = get_path(&harness, "/settings/check-update").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert_eq!(body["currentRevision"], before.0);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("invalid SemVer tag"),
        "{body}"
    );
    assert_secret_free(&body, &[GITHUB_LATEST_RELEASE_API, &invalid.url, &primary]);
    assert_unmutated(&harness, &before);
    drop(_guard);

    let forbidden = start_github_origin(StatusCode::FORBIDDEN, BODY_SECRET).await;
    let _guard = install_update_check_url_for_tests(
        harness.state.process_generation(),
        forbidden.url.clone(),
    );
    let (status, body) = get_path(&harness, "/settings/check-update").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("GitHub returned HTTP 403"),
        "{body}"
    );
    assert_secret_free(
        &body,
        &[
            BODY_SECRET,
            GITHUB_LATEST_RELEASE_API,
            &forbidden.url,
            &primary,
        ],
    );
    drop(_guard);

    let decode = start_github_origin(StatusCode::OK, "not-json").await;
    let _guard =
        install_update_check_url_for_tests(harness.state.process_generation(), decode.url.clone());
    let (status, body) = get_path(&harness, "/settings/check-update").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("invalid response"),
        "{body}"
    );
    assert_secret_free(&body, &[GITHUB_LATEST_RELEASE_API, &decode.url]);
    drop(_guard);

    let newer = newer_version();
    let hop = start_github_origin(StatusCode::OK, &github_tag_body(&format!("v{newer}"))).await;
    let origin = start_github_origin_with(OriginScript {
        status: StatusCode::FOUND,
        body: String::new(),
        location: Some(hop.url.clone()),
        hold: None,
    })
    .await;
    let _guard =
        install_update_check_url_for_tests(harness.state.process_generation(), origin.url.clone());
    let (status, body) = get_path(&harness, "/settings/check-update").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: UpdateCheck = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.latest_version, newer);
    assert_eq!(origin.call_count(), 1);
    assert_eq!(hop.call_count(), 1, "legitimate redirects must be followed");
    for captured in [origin.last(), hop.last()] {
        assert_eq!(
            captured.accept.as_deref(),
            Some("application/vnd.github+json")
        );
        assert_eq!(
            captured.user_agent.as_deref(),
            Some(concat!("ocg-manager/", env!("CARGO_PKG_VERSION")))
        );
        assert!(captured.authorization.is_none());
        assert!(captured.x_api_key.is_none());
        assert!(captured.cookie.is_none());
        assert!(captured.proxy_authorization.is_none());
    }
    assert_secret_free(
        &body,
        &[
            BODY_SECRET,
            LOCATION_SECRET,
            GITHUB_LATEST_RELEASE_API,
            &origin.url,
            &hop.url,
            &primary,
        ],
    );

    let closed = closed_proxy_addr().await;
    let _closed_guard = install_update_check_url_for_tests(
        harness.state.process_generation(),
        format!("{closed}/"),
    );
    let (status, body) = get_path(&harness, "/settings/check-update").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert_secret_free(&body, &[&closed, GITHUB_LATEST_RELEASE_API, &primary]);
    assert_unmutated(&harness, &before);

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_check_update_uses_list_default_legs() {
    let harness = start_loopback("updater-check-legs").await;
    let newer = newer_version();
    let origin = start_github_origin(StatusCode::OK, &github_tag_body(&format!("v{newer}"))).await;
    let _guard =
        install_update_check_url_for_tests(harness.state.process_generation(), origin.url.clone());
    let dead_proxy = closed_proxy_addr().await;

    let mut whitelist = harness.state.config();
    whitelist.proxy_mode = ProxyMode::List;
    whitelist.proxy_list_direction = ProxyListDirection::Whitelist;
    whitelist.proxy_url = dead_proxy.clone();
    harness.state.set_config(whitelist).unwrap();
    let before = snapshot_identity(&harness);
    let (status, body) = get_path(&harness, "/settings/check-update").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["latestVersion"], newer);
    assert_eq!(origin.call_count(), 1);
    assert_eq!(body["revision"], before.0);

    let mut blacklist = harness.state.config();
    blacklist.proxy_list_direction = ProxyListDirection::Blacklist;
    harness.state.set_config(blacklist).unwrap();
    let after = snapshot_identity(&harness);
    let (status, body) = get_path(&harness, "/settings/check-update").await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert_eq!(
        origin.call_count(),
        1,
        "blacklist default must not fall back"
    );
    assert_eq!(body["currentRevision"], after.0);
    assert_secret_free(
        &body,
        &[&dead_proxy, GITHUB_LATEST_RELEASE_API, &origin.url],
    );

    harness.stop();
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn dashboard_v3_check_update_overrides_are_generation_isolated() {
    let first = start_loopback("updater-check-iso-a").await;
    let second = start_loopback("updater-check-iso-b").await;
    point_direct(&first);
    point_direct(&second);
    assert_ne!(
        first.state.process_generation(),
        second.state.process_generation()
    );
    let newer = newer_version();
    let origin_a =
        start_github_origin(StatusCode::OK, &github_tag_body(&format!("v{newer}"))).await;
    let origin_b = start_github_origin(StatusCode::FORBIDDEN, BODY_SECRET).await;
    let _guard_a =
        install_update_check_url_for_tests(first.state.process_generation(), origin_a.url.clone());
    let _guard_b =
        install_update_check_url_for_tests(second.state.process_generation(), origin_b.url.clone());

    let (result_a, result_b) = tokio::join!(
        get_path(&first, "/settings/check-update"),
        get_path(&second, "/settings/check-update"),
    );
    assert_eq!(result_a.0, StatusCode::OK, "{}", result_a.1);
    assert_eq!(result_b.0, StatusCode::BAD_GATEWAY, "{}", result_b.1);
    assert_eq!(result_a.1["latestVersion"], newer);
    assert_v3_error(&result_b.1, ERROR_OUTBOUND_FAILED);
    assert_eq!(origin_a.call_count(), 1);
    assert_eq!(origin_b.call_count(), 1);
    assert_secret_free(&result_b.1, &[BODY_SECRET, &origin_b.url]);

    first.stop();
    second.stop();
}
