//! Dashboard V3 authentication/session: public status, CAS-gated register/login/logout,
//! cookie policy, V2 coexistence, and catalog append.

use ocg_core::dashboard_v3::{
    CATALOG_TYPE_NAMES, ERROR_CONFLICT, ERROR_INVALID_JSON, ERROR_MISSING_EXPECTED_REVISION,
    ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED, contract_schema,
};
use ocg_core::db::CURRENT_SCHEMA_VERSION;
use reqwest::header::{HeaderMap, SET_COOKIE};
use reqwest::{StatusCode, header};
use serde_json::{Map, Value, json};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

const AUTH_CATALOG_TYPES: &[&str] = &["AuthStatus", "AuthRegister", "AuthLogin", "AuthLogout"];

fn cas(harness: &V3Harness, extra: Value) -> Value {
    let mut body = match extra {
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

fn credentials(harness: &V3Harness, username: &str, password: &str) -> Value {
    cas(
        harness,
        json!({
            "username": username,
            "password": password
        }),
    )
}

async fn post_json(harness: &V3Harness, url: &str, body: &Value) -> (StatusCode, HeaderMap, Value) {
    send_json(harness, url, body, None).await
}

async fn post_json_with_cookie(
    harness: &V3Harness,
    url: &str,
    body: &Value,
    cookie: &str,
) -> (StatusCode, HeaderMap, Value) {
    send_json(harness, url, body, Some(cookie)).await
}

async fn send_json(
    harness: &V3Harness,
    url: &str,
    body: &Value,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = harness.client.post(url).json(body);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, headers, body)
}

fn set_cookie(headers: &HeaderMap) -> &str {
    headers
        .get(SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("Set-Cookie")
}

fn cookie_pair(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .trim()
        .to_string()
}

fn assert_cookie_attrs(set_cookie: &str, clear: bool, secure: bool) {
    let parts: Vec<&str> = set_cookie.split(';').map(str::trim).collect();
    assert!(
        parts[0].starts_with("ocg_dashboard_session="),
        "{set_cookie}"
    );
    if clear {
        assert_eq!(parts[0], "ocg_dashboard_session=");
        assert!(parts.contains(&"Max-Age=0"), "{set_cookie}");
    } else {
        assert!(
            parts[0].len() > "ocg_dashboard_session=".len(),
            "{set_cookie}"
        );
        assert!(!parts.contains(&"Max-Age=0"), "{set_cookie}");
    }
    assert!(parts.contains(&"HttpOnly"), "{set_cookie}");
    assert!(parts.contains(&"SameSite=Strict"), "{set_cookie}");
    assert!(parts.contains(&"Path=/dashboard"), "{set_cookie}");
    assert_eq!(parts.contains(&"Secure"), secure, "{set_cookie}");
}

fn assert_v3_error(body: &Value, code: &str) {
    assert_eq!(body["code"], code, "{body}");
    assert!(body.get("message").and_then(Value::as_str).is_some());
    assert!(body.as_object().unwrap().contains_key("currentRevision"));
    assert!(body.as_object().unwrap().contains_key("processGeneration"));
    assert!(body.get("current_revision").is_none());
}

fn assert_auth_status(body: &Value, harness: &V3Harness) {
    let object = body.as_object().expect("auth status object");
    for required in [
        "local",
        "initialized",
        "authenticated",
        "revision",
        "processGeneration",
    ] {
        assert!(object.contains_key(required), "missing {required}: {body}");
    }
    assert_eq!(body["revision"], harness.state.settings_revision());
    assert_eq!(
        body["processGeneration"],
        harness.state.process_generation()
    );
    for forbidden in [
        "password",
        "key",
        "token",
        "cipher",
        "secret",
        "cookie",
        "sessionToken",
        "gatewayKey",
        "ok",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "auth response leaked {forbidden}: {body}"
        );
    }
    assert!(body.get("process_generation").is_none());
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
    for value in json_string_values(body) {
        for secret in secrets {
            assert_ne!(value, *secret, "leaked {secret}: {body}");
        }
    }
}

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
}

#[test]
fn auth_catalog_types_append_after_pricing_without_rewriting_the_prefix() {
    assert_eq!(CATALOG_TYPE_NAMES[0], "ControlRevision");
    assert_eq!(
        &CATALOG_TYPE_NAMES[CATALOG_TYPE_NAMES.len() - AUTH_CATALOG_TYPES.len()..],
        AUTH_CATALOG_TYPES
    );

    let schema = contract_schema();
    let defs = schema["$defs"].as_object().expect("$defs");
    let any_of = schema["anyOf"].as_array().expect("anyOf");
    for (index, name) in CATALOG_TYPE_NAMES.iter().enumerate() {
        assert!(defs.contains_key(*name), "schema missing {name}");
        assert_eq!(
            any_of[index]["$ref"],
            format!("#/$defs/{name}"),
            "anyOf drifted at {index}"
        );
    }
    for name in AUTH_CATALOG_TYPES {
        assert_eq!(defs[*name]["additionalProperties"], false);
    }
    let status_required = defs["AuthStatus"]["required"].as_array().unwrap();
    for field in [
        "local",
        "initialized",
        "authenticated",
        "revision",
        "processGeneration",
    ] {
        assert!(
            status_required.iter().any(|value| value == field),
            "{field}"
        );
    }
    for name in ["AuthRegister", "AuthLogin"] {
        let required = defs[name]["required"].as_array().unwrap();
        for field in [
            "expectedRevision",
            "processGeneration",
            "username",
            "password",
        ] {
            assert!(
                required.iter().any(|value| value == field),
                "{name}.{field}"
            );
        }
    }
    let logout_required = defs["AuthLogout"]["required"].as_array().unwrap();
    assert!(
        logout_required
            .iter()
            .any(|value| value == "expectedRevision")
    );
    assert!(
        logout_required
            .iter()
            .any(|value| value == "processGeneration")
    );
}

#[test]
fn v2_and_v3_auth_handlers_share_one_session_implementation() {
    let v3 = include_str!("../src/dashboard_v3/auth.rs");
    let v2 = include_str!("../src/dashboard.rs");
    let shared = include_str!("../src/dashboard_session.rs");
    for needle in [
        "dashboard_session::credentials_match",
        "dashboard_session::cookie_header",
    ] {
        assert!(v3.contains(needle), "V3 missing {needle}");
        assert!(v2.contains(needle), "V2 missing {needle}");
    }
    assert!(v2.contains("register_admin(&state.db,"));
    assert!(v3.contains("dashboard_session::prepare_admin"));
    assert!(v3.contains("dashboard_session::save_prepared_admin_if_absent"));
    assert!(v2.contains("credentials_match(&state.db,"));
    assert!(v3.contains("credentials_match(&state.db,"));
    assert!(v2.contains("issue_session(&state.browser, &state.dashboard_session_token)"));
    assert!(v3.contains("dashboard_session::rotate_session_under_operation"));
    assert!(v3.contains("dashboard_session::rotate_session_if_authorized_under_operation"));
    assert!(v2.contains("dashboard_session::logout("));
    assert!(!v2.contains("register_admin(&state,"));
    assert!(!v3.contains("register_admin(&state,"));
    assert!(shared.contains("invalidate_remote_sessions"));
    assert!(shared.contains("does **not** bump"));
    let initialized = shared
        .find("let initialized = is_initialized(db)?")
        .unwrap();
    let live_token = shared
        .find("let current_token = session_token.lock()")
        .unwrap();
    assert!(
        initialized < live_token,
        "status must read initialization before the live token"
    );
    let v2_status =
        &v2[v2.find("async fn auth_status").unwrap()..v2.find("async fn register_admin").unwrap()];
    assert!(!v2_status.contains("dashboard_session_token.lock().clone()"));
    assert!(!v2.contains("const SESSION_COOKIE"));
    assert!(!include_str!("../src/dashboard_v3/mod.rs").contains("const SESSION_COOKIE"));
    for needle in [
        "CoreState",
        "crate::state",
        "crate::gateway",
        "crate::dashboard",
        "crate::dashboard_v3",
    ] {
        assert!(
            !shared.contains(needle),
            "dashboard_session must stay state-neutral, found {needle}"
        );
    }
}

#[tokio::test]
async fn public_auth_status_does_not_require_a_session() {
    let harness = start_public("auth-status-public").await;
    let (status, body) = harness
        .get_json(&format!("{}/auth/status", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_auth_status(&body, &harness);
    assert_eq!(body["local"], false);
    assert_eq!(body["initialized"], false);
    assert_eq!(body["authenticated"], false);

    let contract = harness
        .client
        .get(format!("{}/contract", harness.v3_base))
        .send()
        .await
        .unwrap();
    assert_eq!(contract.status(), StatusCode::UNAUTHORIZED);
    let contract_body: Value = contract.json().await.unwrap();
    assert_v3_error(&contract_body, ERROR_UNAUTHORIZED);
    assert_eq!(contract_body["currentRevision"], Value::Null);

    harness.stop();
}

#[tokio::test]
async fn first_register_issues_cookie_and_authorizes_protected_v3() {
    let harness = start_public("auth-register").await;
    let revision = harness.state.settings_revision();
    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &credentials(&harness, "admin", "password123"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_auth_status(&body, &harness);
    assert_eq!(body["initialized"], true);
    assert_eq!(body["authenticated"], true);
    assert_eq!(body["local"], false);
    assert_eq!(harness.state.settings_revision(), revision);
    assert_secret_free(&body, &["admin", "password123"]);
    let set_cookie = set_cookie(&headers);
    assert_cookie_attrs(set_cookie, false, false);
    let cookie = cookie_pair(set_cookie);

    let authorized = harness
        .client
        .get(format!("{}/contract", harness.v3_base))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    let v2 = harness
        .client
        .get(format!("{}/settings", harness.v2_base))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::OK);

    harness.stop();
}

#[tokio::test]
async fn duplicate_register_is_409_without_rotating_the_session() {
    let harness = start_public("auth-duplicate").await;
    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &credentials(&harness, "admin", "password123"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let cookie = cookie_pair(set_cookie(&headers));
    let token = harness.state.dashboard_session_token.lock().clone();

    let (status, dup_headers, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &credentials(&harness, "other", "password456"),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_CONFLICT);
    assert_eq!(body["currentRevision"], harness.state.settings_revision());
    assert!(dup_headers.get(SET_COOKIE).is_none());
    assert_eq!(harness.state.dashboard_session_token.lock().as_str(), token);
    assert_eq!(
        harness
            .client
            .get(format!("{}/contract", harness.v3_base))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    harness.stop();
}

#[tokio::test]
async fn bad_login_is_401_and_does_not_rotate_the_cookie() {
    let harness = start_public("auth-bad-login").await;
    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &credentials(&harness, "admin", "password123"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let cookie = cookie_pair(set_cookie(&headers));
    let token = harness.state.dashboard_session_token.lock().clone();
    let revision = harness.state.settings_revision();

    let (status, login_headers, body) = post_json(
        &harness,
        &format!("{}/auth/login", harness.v3_base),
        &credentials(&harness, "admin", "wrong-password"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_secret_free(&body, &["password123", "wrong-password"]);
    assert!(login_headers.get(SET_COOKIE).is_none());
    assert_eq!(harness.state.settings_revision(), revision);
    assert_eq!(harness.state.dashboard_session_token.lock().as_str(), token);
    assert_eq!(
        harness
            .client
            .get(format!("{}/contract", harness.v3_base))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    harness.stop();
}

#[tokio::test]
async fn login_rotates_the_cookie_and_invalidates_the_previous_one() {
    let harness = start_public("auth-rotate").await;
    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &credentials(&harness, "admin", "password123"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let first = cookie_pair(set_cookie(&headers));
    let first_token = harness.state.dashboard_session_token.lock().clone();
    assert!(!harness.state.browser.remote_session_active("stale-view"));

    let revision = harness.state.settings_revision();
    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/login", harness.v3_base),
        &credentials(&harness, "admin", "password123"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_auth_status(&body, &harness);
    assert_eq!(body["authenticated"], true);
    assert_eq!(harness.state.settings_revision(), revision);
    let second = cookie_pair(set_cookie(&headers));
    assert_cookie_attrs(set_cookie(&headers), false, false);
    assert_ne!(second, first);
    assert_ne!(
        harness.state.dashboard_session_token.lock().as_str(),
        first_token
    );
    assert!(!harness.state.browser.remote_session_active("stale-view"));
    assert_eq!(
        harness
            .client
            .get(format!("{}/contract", harness.v3_base))
            .header(header::COOKIE, &first)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        harness
            .client
            .get(format!("{}/contract", harness.v3_base))
            .header(header::COOKIE, &second)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    harness.stop();
}

#[tokio::test]
async fn logout_clears_the_cookie_and_rejects_the_stale_session() {
    let harness = start_public("auth-logout").await;
    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &credentials(&harness, "admin", "password123"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let cookie = cookie_pair(set_cookie(&headers));
    let token = harness.state.dashboard_session_token.lock().clone();
    let revision = harness.state.settings_revision();

    let (status, headers, body) = post_json_with_cookie(
        &harness,
        &format!("{}/auth/logout", harness.v3_base),
        &cas(&harness, json!({})),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_auth_status(&body, &harness);
    assert_eq!(body["authenticated"], false);
    assert_eq!(body["initialized"], true);
    assert_eq!(harness.state.settings_revision(), revision);
    assert_cookie_attrs(set_cookie(&headers), true, false);
    assert_ne!(harness.state.dashboard_session_token.lock().as_str(), token);
    assert!(!harness.state.browser.remote_session_active("stale-view"));
    assert_eq!(
        harness
            .client
            .get(format!("{}/contract", harness.v3_base))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let (status, _, body) = post_json(
        &harness,
        &format!("{}/auth/logout", harness.v3_base),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    harness.stop();
}

#[tokio::test]
async fn https_forwarded_proto_sets_secure_on_issued_and_cleared_cookies() {
    let harness = start_public("auth-secure").await;
    let response = harness
        .client
        .post(format!("{}/auth/register", harness.v3_base))
        .header("x-forwarded-proto", "https")
        .json(&credentials(&harness, "admin", "password123"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let set_cookie = response
        .headers()
        .get(SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_string();
    assert_cookie_attrs(&set_cookie, false, true);
    let cookie = cookie_pair(&set_cookie);

    let response = harness
        .client
        .post(format!("{}/auth/logout", harness.v3_base))
        .header("x-forwarded-proto", "HTTPS")
        .header(header::COOKIE, &cookie)
        .json(&cas(&harness, json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_cookie_attrs(
        response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .unwrap(),
        true,
        true,
    );

    harness.stop();
}

#[tokio::test]
async fn loopback_trust_is_fail_closed_when_forwarded_headers_are_present() {
    let harness = start_loopback("auth-forwarded").await;
    let (status, body) = harness
        .get_json(&format!("{}/auth/status", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["local"], true);
    assert_eq!(body["authenticated"], true);
    assert_eq!(
        harness
            .client
            .get(format!("{}/contract", harness.v3_base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    for (name, value) in [
        ("x-forwarded-for", "203.0.113.10"),
        ("x-forwarded-proto", "https"),
        ("x-real-ip", "203.0.113.10"),
        ("forwarded", "for=203.0.113.10"),
    ] {
        let status_body: Value = harness
            .client
            .get(format!("{}/auth/status", harness.v3_base))
            .header(name, value)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status_body["local"], false, "{name}");
        assert_eq!(status_body["authenticated"], false, "{name}");

        let v3 = harness
            .client
            .get(format!("{}/contract", harness.v3_base))
            .header(name, value)
            .send()
            .await
            .unwrap();
        assert_eq!(v3.status(), StatusCode::UNAUTHORIZED, "{name}");
        let v3_body: Value = v3.json().await.unwrap();
        assert_v3_error(&v3_body, ERROR_UNAUTHORIZED);

        let v2 = harness
            .client
            .get(format!("{}/settings", harness.v2_base))
            .header(name, value)
            .send()
            .await
            .unwrap();
        assert_eq!(v2.status(), StatusCode::UNAUTHORIZED, "{name}");
        let v2_body = v2.text().await.unwrap();
        assert!(
            v2_body.is_empty(),
            "V2 must stay an empty 401, got {v2_body}"
        );
    }

    harness.stop();
}

#[tokio::test]
async fn stale_revision_or_generation_has_no_auth_side_effects() {
    let harness = start_public("auth-stale-cas").await;
    let generation = harness.state.process_generation();
    let revision = harness.state.settings_revision();
    let token = harness.state.dashboard_session_token.lock().clone();

    let stale_revision = json!({
        "expectedRevision": revision.wrapping_add(1),
        "processGeneration": generation,
        "username": "admin",
        "password": "password123"
    });
    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &stale_revision,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], revision);
    assert!(headers.get(SET_COOKIE).is_none());
    assert!(
        !dashboard_session_initialized(&harness),
        "stale register must not persist an administrator"
    );
    assert_eq!(harness.state.dashboard_session_token.lock().as_str(), token);

    let stale_short_password = json!({
        "expectedRevision": revision.wrapping_add(1),
        "processGeneration": generation,
        "username": "admin",
        "password": "short"
    });
    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &stale_short_password,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert!(headers.get(SET_COOKIE).is_none());
    assert!(!dashboard_session_initialized(&harness));
    assert_eq!(harness.state.dashboard_session_token.lock().as_str(), token);

    let stale_generation = json!({
        "expectedRevision": revision,
        "processGeneration": generation.wrapping_add(1),
        "username": "admin",
        "password": "password123"
    });
    let (status, _, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &stale_generation,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert!(!dashboard_session_initialized(&harness));

    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &credentials(&harness, "admin", "password123"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let cookie = cookie_pair(set_cookie(&headers));
    let live_token = harness.state.dashboard_session_token.lock().clone();
    harness.state.bump_settings_revision();
    let bumped = harness.state.settings_revision();

    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/login", harness.v3_base),
        &json!({
            "expectedRevision": revision,
            "processGeneration": generation,
            "username": "admin",
            "password": "password123"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], bumped);
    assert!(headers.get(SET_COOKIE).is_none());
    assert_eq!(
        harness.state.dashboard_session_token.lock().as_str(),
        live_token
    );

    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/login", harness.v3_base),
        &json!({
            "expectedRevision": revision,
            "processGeneration": generation,
            "username": "admin",
            "password": "wrong-password"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_ne!(body["code"], ERROR_UNAUTHORIZED);
    assert!(headers.get(SET_COOKIE).is_none());
    assert_eq!(
        harness.state.dashboard_session_token.lock().as_str(),
        live_token
    );

    let (status, headers, body) = post_json_with_cookie(
        &harness,
        &format!("{}/auth/logout", harness.v3_base),
        &json!({
            "expectedRevision": revision,
            "processGeneration": generation
        }),
        &cookie,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert!(headers.get(SET_COOKIE).is_none());
    assert_eq!(
        harness
            .client
            .get(format!("{}/contract", harness.v3_base))
            .header(header::COOKIE, &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "stale logout must leave the current cookie valid"
    );

    harness.stop();
}

fn dashboard_session_initialized(harness: &V3Harness) -> bool {
    harness
        .state
        .db
        .lock()
        .get_setting("dashboard_admin")
        .unwrap()
        .is_some()
}

#[tokio::test]
async fn missing_or_unknown_fields_are_rejected_before_auth_side_effects() {
    let harness = start_public("auth-json").await;
    let token = harness.state.dashboard_session_token.lock().clone();

    let (status, _, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &json!({
            "processGeneration": harness.state.process_generation(),
            "username": "admin",
            "password": "password123"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);
    assert!(!dashboard_session_initialized(&harness));

    let (status, _, body) = post_json(
        &harness,
        &format!("{}/auth/login", harness.v3_base),
        &json!({
            "expectedRevision": harness.state.settings_revision(),
            "username": "admin",
            "password": "password123"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let mut extra = credentials(&harness, "admin", "password123");
    extra["token"] = json!("must-not-be-accepted");
    let (status, _, body) = post_json(
        &harness,
        &format!("{}/auth/register", harness.v3_base),
        &extra,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert!(!dashboard_session_initialized(&harness));
    assert_eq!(harness.state.dashboard_session_token.lock().as_str(), token);

    harness.stop();
}

#[tokio::test]
async fn v2_auth_shapes_and_empty_401_remain_behavior_compatible() {
    let harness = start_public("auth-v2-coexist").await;

    let v2_status = harness
        .client
        .get(format!("{}/auth/status", harness.v2_base))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(v2_status["local"], false);
    assert_eq!(v2_status["initialized"], false);
    assert_eq!(v2_status["authenticated"], false);
    assert!(v2_status.get("processGeneration").is_none());
    assert!(v2_status.get("revision").is_none());

    let register = harness
        .client
        .post(format!("{}/auth/register", harness.v2_base))
        .json(&json!({ "username": "admin", "password": "password123" }))
        .send()
        .await
        .unwrap();
    assert_eq!(register.status(), StatusCode::CREATED);
    let register_headers = register.headers().clone();
    let v2_body: Value = register.json().await.unwrap();
    assert_eq!(v2_body, json!({ "ok": true }));
    let cookie = cookie_pair(set_cookie(&register_headers));
    assert_cookie_attrs(set_cookie(&register_headers), false, false);

    let v3 = harness
        .client
        .get(format!("{}/contract", harness.v3_base))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(v3.status(), StatusCode::OK);

    let v2_unauth = harness
        .client
        .get(format!("{}/settings", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_unauth.status(), StatusCode::UNAUTHORIZED);
    assert!(v2_unauth.text().await.unwrap().is_empty());

    let v3_unauth = harness
        .client
        .get(format!("{}/contract", harness.v3_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v3_unauth.status(), StatusCode::UNAUTHORIZED);
    let v3_body: Value = v3_unauth.json().await.unwrap();
    assert_v3_error(&v3_body, ERROR_UNAUTHORIZED);

    let logout = harness
        .client
        .post(format!("{}/auth/logout", harness.v2_base))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    assert_cookie_attrs(set_cookie(logout.headers()), true, false);
    let logout_body = logout.text().await.unwrap();
    assert!(logout_body.is_empty(), "{logout_body}");

    harness.stop();
}

#[tokio::test]
async fn loopback_logout_does_not_require_a_cookie() {
    let harness = start_loopback("auth-local-logout").await;
    let revision = harness.state.settings_revision();
    let (status, headers, body) = post_json(
        &harness,
        &format!("{}/auth/logout", harness.v3_base),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["local"], true);
    assert_eq!(body["authenticated"], true);
    assert_eq!(harness.state.settings_revision(), revision);
    assert_cookie_attrs(set_cookie(&headers), true, false);
    assert_eq!(
        harness
            .client
            .get(format!("{}/contract", harness.v3_base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    harness.stop();
}
