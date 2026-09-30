//! Dashboard V3 contract kernel: schema drift, public auth, and process generation.

use ocg_core::dashboard_v3::{ControlRevision, contract_schema_pretty};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{start_loopback, start_public, state};

fn checked_in_schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/dashboard-api-v3.schema.json")
}

fn normalize_schema_text(text: &str) -> String {
    text.replace("\r\n", "\n")
}

#[test]
fn checked_in_schema_matches_rust_dtos() {
    let generated = contract_schema_pretty();
    let checked_in = fs::read_to_string(checked_in_schema_path())
        .expect("schema/dashboard-api-v3.schema.json must be checked in");
    assert_eq!(
        normalize_schema_text(&generated),
        normalize_schema_text(&checked_in),
        "Dashboard V3 schema drifted; run `pnpm run contract:v3:generate`"
    );
}

#[test]
fn process_generation_is_stable_per_core_state_and_differs_across_fresh_states() {
    let first = state("generation-a");
    let generation = first.process_generation();
    assert_eq!(generation, first.process_generation());
    assert_eq!(generation & !0x0000_FFFF_FFFF_FFFF, 0);

    first.bump_settings_revision();
    assert_eq!(
        first.process_generation(),
        generation,
        "mutations must not change processGeneration"
    );

    let second = state("generation-b");
    assert_ne!(
        first.process_generation(),
        second.process_generation(),
        "fresh CoreState must assign a new processGeneration"
    );

    let _ = fs::remove_dir_all(first.data_dir());
    let _ = fs::remove_dir_all(second.data_dir());
}

#[tokio::test]
async fn public_auth_status_and_v3_contract_are_reachable() {
    let harness = start_loopback("public-auth-contract").await;
    let auth = harness
        .client
        .get(format!("{}/auth/status", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(auth.status(), StatusCode::OK);
    let auth_body: Value = auth.json().await.unwrap();
    assert_eq!(auth_body["local"], true);
    assert_eq!(auth_body["authenticated"], true);

    let (status, contract) = harness
        .get_json(&format!("{}/contract", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{contract}");
    let parsed: ControlRevision = serde_json::from_value(contract.clone()).unwrap();
    assert_eq!(parsed.revision, harness.state.settings_revision());
    assert_eq!(
        parsed.process_generation,
        harness.state.process_generation()
    );
    assert_eq!(
        parsed.pricing_revision,
        harness.state.pricing_snapshot().revision
    );
    let second = harness
        .get_json(&format!("{}/contract", harness.v3_base))
        .await
        .1;
    assert_eq!(contract["processGeneration"], second["processGeneration"]);

    harness.stop();
}

#[tokio::test]
async fn v3_auth_failure_is_json_401_while_v2_stays_empty_401() {
    let harness = start_public("json-401").await;

    let v2 = harness
        .client
        .get(format!("{}/settings", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::UNAUTHORIZED);
    let v2_body = v2.text().await.unwrap();
    assert!(
        v2_body.is_empty(),
        "V2 session middleware must stay an empty 401, got {v2_body}"
    );

    let v3 = harness
        .client
        .get(format!("{}/contract", harness.v3_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v3.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        v3.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("application/json")),
        Some(true)
    );
    let body: Value = v3.json().await.unwrap();
    assert_eq!(body["code"], "unauthorized");
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);
    assert!(body.get("current_revision").is_none());

    harness.stop();
}

#[tokio::test]
async fn v3_session_cookie_from_v2_login_authorizes_contract_routes() {
    let harness = start_public("cookie").await;
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
        .get(format!("{}/contract", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    let body: Value = authorized.json().await.unwrap();
    assert_eq!(
        body["processGeneration"],
        harness.state.process_generation()
    );

    harness.stop();
}
