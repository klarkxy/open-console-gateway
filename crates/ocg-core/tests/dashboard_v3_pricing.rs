//! Retired provider pricing routes are unmatched on the V4 prefix.
//! Anonymous and signed callers both get the router's 404. There is no 410
//! tombstone and no session challenge for these paths.

use reqwest::{Method, StatusCode};
use serde_json::json;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{start_loopback, start_public};

const RETIRED_PATHS: &[(Method, &str)] = &[
    (Method::GET, "/providers/opencode/pricing"),
    (Method::GET, "/providers/command-code/pricing"),
    (Method::POST, "/providers/opencode/pricing/refresh"),
    (Method::PUT, "/providers/opencode/pricing/multipliers"),
    (Method::GET, "/providers/opencode/official-api/pricing"),
    (Method::POST, "/providers/opencode/official-api/pricing"),
];

async fn assert_unmatched(harness: &harness::V3Harness, method: Method, path: &str) {
    let response = harness
        .client
        .request(method.clone(), format!("{}{path}", harness.v3_base))
        .json(&json!({
            "expectedRevision": harness.state.settings_revision(),
            "processGeneration": harness.state.process_generation(),
            "expectedPricingRevision": "stale"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "{method} {path} must be an unmatched route"
    );
    assert_ne!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    assert_ne!(response.status(), StatusCode::GONE, "{path}");
}

#[tokio::test]
async fn retired_pricing_routes_are_unmatched_for_anonymous_callers() {
    let harness = start_public("pricing-retired-anonymous").await;
    for (method, path) in RETIRED_PATHS {
        assert_unmatched(&harness, method.clone(), path).await;
    }
    harness.stop();
}

#[tokio::test]
async fn retired_pricing_routes_are_unmatched_for_local_and_cookie_sessions() {
    let local = start_loopback("pricing-retired-local").await;
    for (method, path) in RETIRED_PATHS {
        assert_unmatched(&local, method.clone(), path).await;
    }
    local.stop();

    let harness = start_public("pricing-retired-cookie").await;
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
    for (method, path) in RETIRED_PATHS {
        let response = harness
            .client
            .request(method.clone(), format!("{}{path}", harness.v3_base))
            .header(reqwest::header::COOKIE, &cookie)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
    harness.stop();
}

#[tokio::test]
async fn legacy_unprefixed_pricing_routes_stay_unmatched() {
    let harness = start_loopback("pricing-legacy-routes-removed").await;
    for (method, path) in [
        (Method::GET, "/pricing"),
        (Method::POST, "/pricing/refresh"),
        (Method::PUT, "/pricing/multipliers"),
    ] {
        let response = harness
            .client
            .request(method.clone(), format!("{}{path}", harness.v3_base))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
    }
    harness.stop();
}
