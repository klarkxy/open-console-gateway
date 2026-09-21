//! Public V4 billing and personal credit calibration wiring.

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

fn expectation(harness: &V3Harness, mut body: Value) -> Value {
    body["expectedRevision"] = json!(harness.state.settings_revision());
    body["processGeneration"] = json!(harness.state.process_generation());
    body
}

async fn request(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v4_base))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or(Value::Null))
}

async fn create(harness: &V3Harness, endpoint: &str) -> String {
    let (status, body) = request(
        harness,
        Method::POST,
        "/accounts",
        expectation(
            harness,
            json!({
                "name": "StepFun fixture",
                "key": "stepfun-inference-fixture",
                "providerId": "custom",
                "customConfig": { "endpointUrl": endpoint, "upstreamProtocol": "chat_completions" },
                "modelCapabilities": [{"modelId": "step-router-v1", "protocol": "chat_completions"}]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["account"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn billing_v4_requires_dashboard_auth() {
    let harness = start_public("billing-auth").await;
    for (method, path) in [
        (Method::GET, "/accounts/unknown/billing"),
        (Method::PUT, "/accounts/unknown/billing/credits"),
        (Method::DELETE, "/accounts/unknown/billing/credits"),
        (Method::POST, "/accounts/unknown/billing/credits/calibrate"),
        (Method::POST, "/accounts/unknown/billing/credits/grants"),
    ] {
        let (status, _) = request(&harness, method, path, json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    harness.stop();
}

#[tokio::test]
async fn personal_credit_setup_calibration_and_grant_are_local_cas_operations() {
    let harness = start_loopback("personal-credits").await;
    let id = create(
        &harness,
        "https://api.stepfun.com/step_plan/v1/chat/completions",
    )
    .await;
    let path = format!("/accounts/{id}/billing");
    let before = harness.state.db.lock().get_account(&id).unwrap().unwrap();
    let (status, initial) = request(&harness, Method::GET, &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{initial}");
    assert_eq!(initial["model"], "credits");
    assert!(initial["credits"].is_null());
    let configuration = initial["presets"][0]["configuration"].clone();
    let grant = initial["presets"][0]["initialGrant"].as_f64().unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    let configure = json!({
        "configuration": configuration,
        "initialBuckets": [{
            "id": "initial", "kind": "monthly", "label": "month",
            "granted": grant, "remaining": grant * 0.75,
            "startsAt": now, "expiresAt": configuration["monthly"]["nextResetAt"]
        }]
    });
    let old = expectation(
        &harness,
        json!({"balances":[{"bucketId":"initial","remaining":grant*0.25}]}),
    );
    let (status, _) = request(
        &harness,
        Method::PUT,
        &format!("{path}/credits"),
        configure.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, configured) = request(
        &harness,
        Method::PUT,
        &format!("{path}/credits"),
        expectation(&harness, configure),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{configured}");
    assert_eq!(
        configured["credits"]["remaining"].as_f64(),
        Some(grant * 0.75)
    );
    assert_eq!(configured["source"], "local_estimate");
    assert!(!configured.to_string().contains("stepfun-inference-fixture"));
    let (status, _) = request(
        &harness,
        Method::POST,
        &format!("{path}/credits/calibrate"),
        old,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, calibrated) = request(
        &harness,
        Method::POST,
        &format!("{path}/credits/calibrate"),
        expectation(
            &harness,
            json!({"balances":[{"bucketId":"initial","remaining":grant*0.25}]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{calibrated}");
    assert_eq!(
        calibrated["credits"]["remaining"].as_f64(),
        Some(grant * 0.25)
    );
    let (status, edited) = request(
        &harness,
        Method::PUT,
        &format!("{path}/credits"),
        expectation(
            &harness,
            json!({"configuration":configuration,"initialBuckets":null}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert_eq!(edited["credits"]["remaining"].as_f64(), Some(grant * 0.25));
    let (status, topped_up) = request(&harness, Method::POST, &format!("{path}/credits/grants"), expectation(&harness, json!({"label":"topup","amount":123.0,"expiresAt":(chrono::Utc::now()+chrono::Duration::days(30)).to_rfc3339()}))).await;
    assert_eq!(status, StatusCode::OK, "{topped_up}");
    assert_eq!(
        topped_up["credits"]["remaining"].as_f64(),
        Some(grant * 0.25 + 123.0)
    );
    let second = create(
        &harness,
        "https://api.stepfun.com/step_plan/v1/chat/completions",
    )
    .await;
    let (_, other) = request(
        &harness,
        Method::GET,
        &format!("/accounts/{second}/billing"),
        json!({}),
    )
    .await;
    assert!(
        other["credits"].is_null(),
        "another account must remain independent: {other}"
    );
    let after = harness.state.db.lock().get_account(&id).unwrap().unwrap();
    assert_eq!(before.key_cipher, after.key_cipher);
    assert_eq!(before.enabled, after.enabled);
    assert_eq!(before.cooldown_generic_until, after.cooldown_generic_until);
    let sql = rusqlite::Connection::open(harness.dir.join("data.sqlite")).unwrap();
    let stored: String = sql
        .query_row(
            "SELECT credit_meter_json FROM credentials WHERE legacy_account_id = ?1",
            [&id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!stored.contains("stepfun-inference-fixture"));
    assert!(!stored.contains("Oasis-Token"));
    drop(sql);
    let (status, disabled) = request(
        &harness,
        Method::DELETE,
        &format!("{path}/credits"),
        expectation(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert!(disabled["credits"].is_null());
    let (status, _) = request(
        &harness,
        Method::GET,
        &format!("/accounts/{id}/stepfun-usage"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    harness.stop();
}

#[tokio::test]
async fn ordinary_api_billing_does_not_auto_select_step_plan() {
    let harness = start_loopback("billing-api-separate").await;
    let id = create(&harness, "https://api.stepfun.com/v1/chat/completions").await;
    let (status, body) = request(
        &harness,
        Method::GET,
        &format!("/accounts/{id}/billing"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"], "cash");
    assert!(body["credits"].is_null());
    harness.stop();
}

#[tokio::test]
async fn stepfun_v4_plan_never_projects_an_api_balance() {
    let harness = start_loopback("stepfun-balance-projection").await;
    for (endpoint, count) in [
        ("https://api.stepfun.com/v1/chat/completions", 1),
        ("https://api.stepfun.com/step_plan/v1/chat/completions", 0),
    ] {
        let id = create(&harness, endpoint).await;
        let now = chrono::Utc::now();
        harness
            .state
            .db
            .lock()
            .upsert_credit_balance(&ocg_core::models::CreditBalance {
                account_id: id.clone(),
                balance_kind: "balance".into(),
                amount: 12.5,
                unit: "cny".into(),
                source: "stepfun-api-official".into(),
                observed_at: Some(now),
                updated_at: now,
            })
            .unwrap();
        let (status, body) = request(
            &harness,
            Method::GET,
            &format!("/accounts/{id}/provider-usage"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["creditBalances"].as_array().unwrap().len(),
            count,
            "{body}"
        );
    }
    harness.stop();
}
