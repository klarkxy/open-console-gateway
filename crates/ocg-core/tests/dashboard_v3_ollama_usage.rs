//! Dashboard V3 Ollama Cloud billing, monthly soft-credit usage, and removal
//! of the unreleased Cookie scrape path.

use chrono::Utc;
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::dashboard_v3::ERROR_INVALID_REQUEST;
use ocg_core::models::{Account, AccountType, ForwardLog};
use ocg_core::provider::{OLLAMA_PROVIDER_ID, OllamaBillingTier, ZEN_FREE_ACCOUNT_ID};
use reqwest::Method;
use serde_json::{Value, json};
use std::fs;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback};

async fn send_json(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: Value,
) -> (axum::http::StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{}", harness.v3_base, path))
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, parsed)
}

fn cas(harness: &V3Harness, patch: Value) -> Value {
    let mut body = match patch {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
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

fn base_ollama_account(id: &str) -> Account {
    let now = Utc::now();
    let cipher = StaticKeyCipher::new("v3-contract");
    Account {
        id: id.into(),
        provider_id: OLLAMA_PROVIDER_ID.into(),
        credential_kind: ocg_core::provider::default_credential_kind(),
        quota_scope: ocg_core::provider::default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: cipher.encrypt("ollama-key").unwrap(),
        enabled: true,
        account_type: AccountType::Key,
        setup_step: ocg_core::models::AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
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

#[tokio::test]
async fn ollama_cookie_routes_are_gone_and_unconfigured_accounts_stay_routeable() {
    let harness = start_loopback("ollama-cookie-gone").await;
    let account = base_ollama_account("ollama-unconfigured-1");
    harness.state.db.lock().create_account(&account).unwrap();

    let (status, _) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-unconfigured-1/ollama-usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::NOT_FOUND);

    let (status, body) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-unconfigured-1",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(body["ollamaBillingTier"].is_null(), "{body}");
    assert_eq!(body["enabled"], true);

    let (status, usage) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-unconfigured-1/provider-usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(usage["quotaWindows"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn ollama_paid_tier_requires_purchase_date_and_publishes_month_credits() {
    let harness = start_loopback("ollama-paid-month").await;
    let account = base_ollama_account("ollama-pro-1");
    harness.state.db.lock().create_account(&account).unwrap();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        cas(
            &harness,
            json!({
                "providerId": "ollama",
                "name": "ollama-missing-tier",
                "key": "ollama-key-missing-tier",
                "purchaseDate": "2026-09-01"
            }),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], ERROR_INVALID_REQUEST);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        cas(
            &harness,
            json!({
                "providerId": "ollama",
                "name": "ollama-pro-create",
                "key": "ollama-key-create",
                "ollamaBillingTier": "pro"
            }),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], ERROR_INVALID_REQUEST);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        cas(
            &harness,
            json!({
                "providerId": "ollama",
                "name": "ollama-pro-create",
                "key": "ollama-key-create",
                "ollamaBillingTier": "pro",
                "purchaseDate": "2026-09-01"
            }),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(body["account"]["ollamaBillingTier"], "pro");

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        cas(
            &harness,
            json!({
                "providerId": "ollama",
                "name": "ollama-free-rejected",
                "key": "ollama-key-free",
                "ollamaBillingTier": "free",
                "purchaseDate": "2026-09-01"
            }),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        "/accounts/ollama-pro-1",
        cas(
            &harness,
            json!({
                "ollamaBillingTier": "pro",
                "purchaseDate": "2026-09-01"
            }),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["account"]["ollamaBillingTier"], "pro");
    assert_eq!(
        harness
            .state
            .db
            .lock()
            .ollama_cloud_billing_tier("ollama-pro-1")
            .unwrap(),
        Some(OllamaBillingTier::Pro)
    );

    harness
        .state
        .db
        .lock()
        .log_forward(&ForwardLog {
            id: 0,
            timestamp: Utc::now(),
            model: "glm-5.3-flash".into(),
            account_id: "ollama-pro-1".into(),
            account_name: "ollama-pro-1".into(),
            route_account_id: Some("ollama-pro-1".into()),
            provider_id: Some(OLLAMA_PROVIDER_ID.into()),
            credential_account_id: Some("ollama-pro-1".into()),
            client_key_id: None,
            client_key_name: None,
            status: "success".into(),
            http_status: Some(200),
            route: "proxy".into(),
            prompt_tokens: 10,
            completion_tokens: 20,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: Some(12.5),
            raw_cost_usd: Some(12.5),
            quota_debit: Some(12.5),
            effective_paid_cost_usd: None,
            pricing_revision_id: Some("ollama-test".into()),
            quota_multiplier: Some(1.0),
            local_adjustment_multiplier: Some(1.0),
            service_tier: None,
            cost_state: "priced".into(),
            error_message: None,
            request_id: Some("req-1".into()),
            attempt: Some(1),
            error_source: None,
            error_stage: None,
            duration_ms: Some(5),
            diagnostic: None,
        })
        .unwrap();

    let (status, usage) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-pro-1/provider-usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let windows = usage["quotaWindows"].as_array().unwrap();
    assert!(windows.is_empty(), "{usage}");

    harness
        .state
        .db
        .lock()
        .log_forward(&ForwardLog {
            id: 0,
            timestamp: Utc::now(),
            model: "glm-5.3-flash".into(),
            account_id: "ollama-pro-1".into(),
            account_name: "ollama-pro-1".into(),
            route_account_id: Some("ollama-pro-1".into()),
            provider_id: Some(OLLAMA_PROVIDER_ID.into()),
            credential_account_id: Some("ollama-pro-1".into()),
            client_key_id: None,
            client_key_name: None,
            status: "success".into(),
            http_status: Some(200),
            route: "proxy".into(),
            prompt_tokens: 10,
            completion_tokens: 20,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: Some(70.0),
            raw_cost_usd: Some(70.0),
            quota_debit: Some(70.0),
            effective_paid_cost_usd: None,
            pricing_revision_id: Some("ollama-test".into()),
            quota_multiplier: Some(1.0),
            local_adjustment_multiplier: Some(1.0),
            service_tier: None,
            cost_state: "priced".into(),
            error_message: None,
            request_id: Some("req-over".into()),
            attempt: Some(1),
            error_source: None,
            error_stage: None,
            duration_ms: Some(5),
            diagnostic: None,
        })
        .unwrap();
    let (status, usage) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-pro-1/provider-usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(
        usage["quotaWindows"].as_array().unwrap().is_empty(),
        "{usage}"
    );
    let (status, account_usage) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-pro-1/usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(account_usage["windowMonth"].is_null(), "{account_usage}");

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        "/accounts/ollama-pro-1",
        cas(&harness, json!({ "name": "ollama-pro-renamed" })),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(body["account"]["ollamaBillingTier"], "pro");
}

#[tokio::test]
async fn ollama_export_payload_includes_a_bundle() {
    let harness = start_loopback("ollama-export-billing").await;
    let mut account = base_ollama_account("ollama-export-1");
    account.purchase_date = "2026-08-01".into();
    harness.state.db.lock().create_account(&account).unwrap();
    harness
        .state
        .db
        .lock()
        .set_ollama_cloud_billing_tier("ollama-export-1", Some(OllamaBillingTier::Max))
        .unwrap();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts/transfer/export",
        json!({ "bundlePassword": "correct horse battery" }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert!(body["bundle"].as_str().unwrap().len() > 20);
}

#[tokio::test]
async fn ollama_omit_update_preserves_null_billing() {
    let harness = start_loopback("ollama-omit-billing").await;
    let account = base_ollama_account("ollama-omit-1");
    harness.state.db.lock().create_account(&account).unwrap();
    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        "/accounts/ollama-omit-1",
        cas(&harness, json!({ "name": "still-unconfigured" })),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert!(body["account"]["ollamaBillingTier"].is_null(), "{body}");
    let (status, usage) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-omit-1/provider-usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(usage["quotaWindows"].as_array().unwrap().len(), 0);
    let _ = fs::remove_dir_all(&harness.dir);
}

#[tokio::test]
async fn ollama_first_month_percent_round_trips_without_a_tier() {
    let harness = start_loopback("ollama-manual-month-no-tier").await;
    let mut account = base_ollama_account("ollama-manual-42");
    account.purchase_date = "2026-08-01".into();
    let cooldown = "2026-10-20T00:00:00Z".parse().unwrap();
    account.cooldown_until = Some(cooldown);
    account.cooldown_month_until = Some(cooldown);
    harness.state.db.lock().create_account(&account).unwrap();
    assert!(
        harness
            .state
            .db
            .lock()
            .ollama_cloud_billing_tier("ollama-manual-42")
            .unwrap()
            .is_none()
    );

    let (status, usage) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-manual-42/usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{usage}");
    assert!(usage["windowMonth"].is_null(), "{usage}");
    assert!(usage["window5h"].is_null(), "{usage}");
    assert!(usage["windowWeek"].is_null(), "{usage}");

    let (status, provider) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-manual-42/provider-usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{provider}");
    assert_eq!(provider["quotaWindows"].as_array().unwrap().len(), 0);

    let (status, billing) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-manual-42/billing",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{billing}");
    assert_eq!(billing["manualCalibration"], true, "{billing}");
    assert_eq!(
        billing["usage"]["quotaWindows"].as_array().unwrap().len(),
        0,
        "{billing}"
    );

    let (status, rejected) = send_json(
        &harness,
        Method::PATCH,
        "/accounts/ollama-manual-42/usage",
        cas(
            &harness,
            json!({ "window": "window_week", "percent": 42.5 }),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(rejected["code"], ERROR_INVALID_REQUEST, "{rejected}");
    assert!(
        rejected["message"].as_str().unwrap().contains("monthly"),
        "{rejected}"
    );

    let (status, rejected) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{ZEN_FREE_ACCOUNT_ID}/usage"),
        cas(
            &harness,
            json!({ "window": "window_month", "percent": 42.5 }),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(rejected["code"], ERROR_INVALID_REQUEST, "{rejected}");
    assert!(
        rejected["message"]
            .as_str()
            .unwrap()
            .contains("unavailable"),
        "{rejected}"
    );

    let (status, usage) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-manual-42/usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{usage}");
    assert!(usage["windowMonth"].is_null(), "{usage}");

    let (status, saved) = send_json(
        &harness,
        Method::PATCH,
        "/accounts/ollama-manual-42/usage",
        cas(
            &harness,
            json!({ "window": "window_month", "percent": 42.5 }),
        ),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{saved}");
    assert_eq!(saved["usage"]["windowMonth"], 42.5, "{saved}");
    assert!(saved["usage"]["window5h"].is_null(), "{saved}");
    assert!(saved["usage"]["windowWeek"].is_null(), "{saved}");

    let (status, usage) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-manual-42/usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{usage}");
    assert_eq!(usage["windowMonth"], 42.5, "{usage}");
    assert!(usage["window5h"].is_null(), "{usage}");
    assert!(usage["windowWeek"].is_null(), "{usage}");

    let (status, provider) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-manual-42/provider-usage",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{provider}");
    let windows = provider["quotaWindows"].as_array().unwrap();
    assert_eq!(windows.len(), 1, "{provider}");
    assert_eq!(windows[0]["windowKind"], "month");
    assert_eq!(windows[0]["used"], 42.5);
    assert_eq!(windows[0]["limitValue"], 100.0);
    assert_eq!(windows[0]["unit"], "percent");
    assert_eq!(windows[0]["source"], "ollama-manual-percent");
    assert!(windows[0]["observedAt"].as_str().is_some());
    assert!(provider["creditBalances"].as_array().unwrap().is_empty());

    let (status, billing) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-manual-42/billing",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{billing}");
    assert_eq!(billing["manualCalibration"], true, "{billing}");
    assert_eq!(
        billing["usage"]["quotaWindows"][0]["used"], 42.5,
        "{billing}"
    );
    assert_eq!(billing["usage"]["quotaWindows"][0]["limitValue"], 100.0);
    assert_eq!(billing["usage"]["quotaWindows"][0]["unit"], "percent");
    assert_eq!(billing["usage"]["quotaWindows"][0]["windowKind"], "month");

    let (status, account_body) = send_json(
        &harness,
        Method::GET,
        "/accounts/ollama-manual-42",
        json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{account_body}");
    assert!(
        account_body["ollamaBillingTier"].is_null(),
        "{account_body}"
    );
    assert_eq!(account_body["purchaseDate"], "2026-08-01", "{account_body}");
    let stored = harness
        .state
        .db
        .lock()
        .get_account("ollama-manual-42")
        .unwrap()
        .unwrap();
    assert_eq!(stored.purchase_date, "2026-08-01");
    assert_eq!(stored.cooldown_until, Some(cooldown));
    assert_eq!(stored.cooldown_month_until, Some(cooldown));
    assert!(
        harness
            .state
            .db
            .lock()
            .ollama_cloud_billing_tier("ollama-manual-42")
            .unwrap()
            .is_none()
    );
    harness.stop();
}
