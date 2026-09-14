//! Dashboard V3 account usage slice: auth, CAS, projections, sealed-provider
//! manual refresh, and retired V2 paths.

use chrono::{Duration, Local, Utc};
use ocg_core::dashboard_v3::{
    ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION, ERROR_NOT_FOUND,
    ERROR_REVISION_CONFLICT, ERROR_UNAUTHORIZED, ProviderUsage, UsageWindow,
    install_official_pricing_fetch_error_for_tests,
};
use ocg_core::db::AccountUsageCalibrationSnapshot;
use ocg_core::models::UsageWindowKind;
use ocg_core::models::{CreditBalance, ForwardLog};
use ocg_core::provider::{COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback, start_public};

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
    "snapshotJson",
    "snapshot_json",
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

async fn send_raw(
    harness: &V3Harness,
    method: Method,
    path: &str,
    body: &str,
) -> (StatusCode, Value) {
    let response = harness
        .client
        .request(method, format!("{}{path}", harness.v3_base))
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

fn assert_secret_free(body: &Value) {
    for name in json_field_names(body) {
        assert!(
            !SECRET_FIELD_NAMES.contains(&name),
            "usage JSON leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in ["sk-secret", "ocg-secret", "pw-secret", "user:pass@"] {
            assert!(
                !value.contains(secret),
                "usage JSON leaked secret sample {secret}: {body}"
            );
        }
    }
}

fn assert_usage_shape(body: &Value, harness: &V3Harness) {
    let object = body.as_object().expect("UsageWindow object");
    for field in [
        "accountId",
        "window5h",
        "windowWeek",
        "windowMonth",
        "resetsIn5h",
        "resetsInWeek",
        "resetsInMonth",
        "revision",
        "processGeneration",
        "pricingRevision",
    ] {
        assert!(object.contains_key(field), "missing {field}: {body}");
    }
    assert!(object.get("window_5h").is_none());
    assert!(object.get("resets_in_5h").is_none());
    assert_eq!(body["revision"], harness.state.settings_revision());
    assert_eq!(
        body["processGeneration"],
        harness.state.process_generation()
    );
    assert_secret_free(body);
}

fn parse_usage(body: &Value) -> UsageWindow {
    serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("UsageWindow JSON: {body}"))
}

fn parse_provider_usage(body: &Value) -> ProviderUsage {
    serde_json::from_value(body.clone()).unwrap_or_else(|_| panic!("ProviderUsage JSON: {body}"))
}

async fn create_go(harness: &V3Harness) -> String {
    let (status, body) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(harness, json!({ "name": "Go", "key": "sk-go" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["account"]["id"].as_str().unwrap().to_string()
}

async fn create_goat(harness: &V3Harness) -> String {
    let purchase_date = Local::now().date_naive().format("%Y-%m-%d").to_string();
    let (status, body) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(
            harness,
            json!({
                "name": "GOAT",
                "key": "goat-key",
                "providerId": COMMAND_CODE_PROVIDER_ID,
                "purchaseDate": purchase_date
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["account"]["id"].as_str().unwrap().to_string()
}

fn seed_priced_goat_request(harness: &V3Harness, account_id: &str, cost: f64) {
    harness
        .state
        .db
        .lock()
        .log_forward(&ForwardLog {
            id: 0,
            timestamp: Utc::now(),
            model: "deepseek/deepseek-v4-flash".into(),
            account_id: account_id.into(),
            account_name: "GOAT".into(),
            route_account_id: Some(account_id.into()),
            provider_id: Some(COMMAND_CODE_PROVIDER_ID.into()),

            credential_account_id: Some(account_id.into()),
            client_key_id: None,
            client_key_name: None,
            status: "success".into(),
            http_status: Some(200),
            route: "proxy".into(),
            prompt_tokens: 10,
            completion_tokens: 20,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: Some(cost),
            raw_cost_usd: Some(cost),
            quota_debit: Some(cost),
            effective_paid_cost_usd: Some(cost),
            pricing_revision_id: Some("goat-test-pricing".into()),
            quota_multiplier: Some(1.0),
            local_adjustment_multiplier: Some(1.0),
            service_tier: None,
            cost_state: "priced".into(),
            error_message: None,
            request_id: Some("goat-usage-estimate".into()),
            attempt: Some(1),
            error_source: None,
            error_stage: None,
            duration_ms: Some(5),
            diagnostic: None,
        })
        .unwrap();
}

fn seed_credit_balance(harness: &V3Harness, account_id: &str) {
    let now = Utc::now();
    harness
        .state
        .db
        .lock()
        .upsert_credit_balance(&CreditBalance {
            account_id: account_id.to_string(),
            balance_kind: "purchased".to_string(),
            amount: 42.0,
            unit: "credits".to_string(),
            source: "test-fixture".to_string(),
            observed_at: Some(now),
            updated_at: now,
        })
        .unwrap();
    assert_eq!(
        harness
            .state
            .db
            .lock()
            .list_credit_balances(account_id)
            .unwrap()
            .len(),
        1,
        "{account_id} should keep the seeded credit row"
    );
}

#[tokio::test]
async fn dashboard_v3_usage_routes_require_the_v3_session() {
    let harness = start_public("usage-auth").await;

    for path in [
        "/accounts/missing/usage",
        "/accounts/missing/provider-usage",
    ] {
        let (status, body) = harness
            .get_json(&format!("{}{path}", harness.v3_base))
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_v3_error(&body, ERROR_UNAUTHORIZED);
        assert_eq!(body["currentRevision"], Value::Null);
        assert_eq!(body["processGeneration"], Value::Null);
    }

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        "/accounts/missing/usage",
        &cas(&harness, json!({ "window": "window_5h", "percent": 50.0 })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_v3_error(&body, ERROR_UNAUTHORIZED);

    let v2 = harness
        .client
        .get(format!("{}/accounts/missing/usage", harness.v2_base))
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
async fn dashboard_v3_usage_missing_account_is_json_404() {
    let harness = start_loopback("usage-404").await;

    for path in [
        "/accounts/does-not-exist/usage",
        "/accounts/does-not-exist/provider-usage",
    ] {
        let (status, body) = harness
            .get_json(&format!("{}{path}", harness.v3_base))
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path} {body}");
        assert_v3_error(&body, ERROR_NOT_FOUND);
        assert_eq!(body["currentRevision"], harness.state.settings_revision());
    }

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        "/accounts/does-not-exist/usage",
        &cas(&harness, json!({ "window": "window_5h", "percent": 50.0 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_v3_error(&body, ERROR_NOT_FOUND);

    harness.stop();
}

#[tokio::test]
async fn provider_usage_refresh_rejects_non_cn_plans_before_outbound_io() {
    let harness = start_loopback("usage-refresh-wrong-plan").await;
    let go_id = create_go(&harness).await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &format!("/accounts/{go_id}/provider-usage"),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_secret_free(&body);
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_go_usage_uses_live_pricing_limits_for_seeded_windows() {
    let harness = start_loopback("usage-go-seed").await;
    let go_id = create_go(&harness).await;
    let limits = harness.state.pricing_snapshot().limits.clone();
    let pricing_revision = harness.state.pricing_snapshot().revision.clone();
    harness
        .state
        .db
        .lock()
        .calibrate_account_usage_snapshot(
            &go_id,
            &AccountUsageCalibrationSnapshot {
                rolling_percent: 50.0,
                weekly_percent: 20.0,
                monthly_percent: 10.0,
                rolling_resets_in_minutes: 180,
                weekly_resets_in_minutes: 1_440,
            },
            &limits,
        )
        .unwrap();

    let _guard = install_official_pricing_fetch_error_for_tests(
        harness.state.process_generation(),
        "usage GET must not fetch official pricing",
    );

    let (status, body) = harness
        .get_json(&format!("{}/accounts/{go_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_usage_shape(&body, &harness);
    let parsed = parse_usage(&body);
    assert!((parsed.window_5h - limits.window_5h * 0.5).abs() < 1e-9);
    assert!((parsed.window_week - limits.window_week * 0.2).abs() < 1e-9);
    assert!((parsed.window_month - limits.window_month * 0.1).abs() < 1e-9);
    assert_eq!(
        parsed.pricing_revision.as_deref(),
        Some(pricing_revision.as_str())
    );
    assert!(parsed.resets_in_5h.is_some());
    assert!(parsed.resets_in_week.is_some());

    let (status, provider) = harness
        .get_json(&format!(
            "{}/accounts/{go_id}/provider-usage",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{provider}");
    assert_secret_free(&provider);
    let provider = parse_provider_usage(&provider);
    assert_eq!(
        provider.availability,
        ocg_core::dashboard_v3::UsageAvailability::Available
    );
    assert!(!provider.experimental);
    assert_eq!(provider.quota_windows.len(), 3);
    assert_eq!(
        provider.pricing_revision.as_deref(),
        Some(pricing_revision.as_str())
    );
    let rolling = provider
        .quota_windows
        .iter()
        .find(|window| window.window_kind == "five_hours")
        .unwrap();
    assert_eq!(rolling.source, "opencode-go-live");
    assert!((rolling.used - limits.window_5h * 0.5).abs() < 1e-9);
    assert_eq!(rolling.limit_value, Some(limits.window_5h));

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_goat_estimates_priced_logs_and_allows_local_calibration() {
    let harness = start_loopback("usage-goat-local-estimate").await;
    let goat_id = create_goat(&harness).await;
    seed_priced_goat_request(&harness, &goat_id, 2.8);
    let before = harness.state.settings_revision();

    let (status, estimated) = harness
        .get_json(&format!("{}/accounts/{goat_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{estimated}");
    assert_eq!(estimated["window5h"], 2.8);
    assert_eq!(estimated["windowWeek"], 2.8);
    assert_eq!(estimated["windowMonth"], 2.8);
    assert_eq!(estimated["pricingRevision"], Value::Null);

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{goat_id}/usage"),
        &cas(
            &harness,
            json!({
                "window": "window_5h",
                "percent": 50.04,
                "resetsInMinutes": 180
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["usage"]["window5h"], 7.0);
    assert_eq!(body["usage"]["windowWeek"], 2.8);
    assert_eq!(body["usage"]["windowMonth"], 2.8);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, body) = harness
        .get_json(&format!("{}/accounts/{goat_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["window5h"], 7.0);
    assert_eq!(body["windowWeek"], 2.8);
    assert_eq!(body["windowMonth"], 2.8);

    let (status, provider) = harness
        .get_json(&format!(
            "{}/accounts/{goat_id}/provider-usage",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{provider}");
    assert_eq!(provider["availability"], "local_state");
    assert_eq!(provider["pricingRevision"], Value::Null);
    let provider = parse_provider_usage(&provider);
    assert_eq!(provider.quota_windows.len(), 3);
    for (kind, limit, used) in [
        ("five_hours", 14.0, 7.0),
        ("week", 35.0, 2.8),
        ("month", 70.0, 2.8),
    ] {
        let window = provider
            .quota_windows
            .iter()
            .find(|window| window.window_kind == kind)
            .unwrap();
        assert_eq!(window.limit_value, Some(limit));
        assert!((window.used - used).abs() < 1e-9);
        assert_eq!(window.source, "command-code-goat-local");
    }
    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_zen_provider_usage_is_the_synthetic_free_window() {
    let harness = start_loopback("usage-zen-free").await;
    let free_until = Utc::now() + Duration::minutes(5);
    harness
        .state
        .db
        .lock()
        .set_account_rate_limit(
            ZEN_FREE_ACCOUNT_ID,
            free_until,
            "test free cooldown",
            Some(UsageWindowKind::Free),
        )
        .unwrap();

    let (status, body) = harness
        .get_json(&format!(
            "{}/accounts/{ZEN_FREE_ACCOUNT_ID}/provider-usage",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_secret_free(&body);
    assert_eq!(body["availability"], "local_state");
    assert_eq!(body["experimental"], false);
    assert!(body["freeCooldownUntil"].is_string());
    assert_eq!(body["quotaWindows"][0]["windowKind"], "free");
    assert_eq!(body["quotaWindows"][0]["used"], 1.0);
    assert_eq!(body["quotaWindows"][0]["source"], "egress-cooldown-live");
    assert_eq!(body["pricingRevision"], Value::Null);
    assert!(body.get("free_cooldown_until").is_none());
    assert!(body.get("quota_windows").is_none());

    let (status, usage) = harness
        .get_json(&format!(
            "{}/accounts/{ZEN_FREE_ACCOUNT_ID}/usage",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{usage}");
    assert_v3_error(&usage, ERROR_INVALID_REQUEST);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_non_zen_provider_usage_does_not_project_free_cooldown_window() {
    let harness = start_loopback("usage-go-no-free-window").await;
    let go_id = create_go(&harness).await;
    let free_until = Utc::now() + Duration::minutes(5);
    harness
        .state
        .db
        .lock()
        .set_account_rate_limit(
            ZEN_FREE_ACCOUNT_ID,
            free_until,
            "test free cooldown",
            Some(UsageWindowKind::Free),
        )
        .unwrap();

    let (status, body) = harness
        .get_json(&format!(
            "{}/accounts/{go_id}/provider-usage",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["freeCooldownUntil"], Value::Null, "{body}");
    let windows = body["quotaWindows"].as_array().expect("quotaWindows array");
    assert!(
        windows.iter().all(|window| {
            window["windowKind"] != "free" && window["source"] != "egress-cooldown-live"
        }),
        "{body}"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_unsupported_provider_usage_is_unavailable_and_empty() {
    let harness = start_loopback("usage-unavailable").await;

    let (status, custom) = send_json(
        &harness,
        Method::POST,
        "/accounts",
        &cas(
            &harness,
            json!({
                "name": "Custom",
                "key": "custom-key",
                "providerId": CUSTOM_PROVIDER_ID,
                "customConfig": {
                    "endpointUrl": "https://api.example.com/v1/messages",
                    "upstreamProtocol": "messages"
                },
                "modelCapabilities": [{ "modelId": "org/model", "protocol": "messages" }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{custom}");
    let custom_id = custom["account"]["id"].as_str().unwrap().to_string();

    seed_credit_balance(&harness, &custom_id);

    for (id, experimental) in [(&custom_id, false)] {
        let (status, body) = harness
            .get_json(&format!("{}/accounts/{id}/provider-usage", harness.v3_base))
            .await;
        assert_eq!(status, StatusCode::OK, "{id} {body}");
        assert_eq!(body["availability"], "unavailable", "{id}");
        assert_eq!(body["experimental"], experimental, "{id}");
        assert_eq!(body["quotaWindows"], json!([]), "{id}");
        assert_eq!(body["creditBalances"], json!([]), "{id}");
        assert_eq!(body["freeCooldownUntil"], Value::Null, "{id}");
        assert_eq!(body["pricingRevision"], Value::Null, "{id}");
        assert_secret_free(&body);
        assert_eq!(
            harness
                .state
                .db
                .lock()
                .list_credit_balances(id)
                .unwrap()
                .len(),
            1,
            "{id} stored credit row must stay suppressed, not deleted"
        );

        harness
            .assert_v2_path_removed(Method::GET, &format!("/accounts/{id}/provider-usage"), None)
            .await;
    }

    let (status, custom_usage) = harness
        .get_json(&format!("{}/accounts/{custom_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{custom_usage}");
    assert_v3_error(&custom_usage, ERROR_INVALID_REQUEST);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_usage_patch_clamps_percent_and_parses_resets() {
    let harness = start_loopback("usage-validate").await;
    let go_id = create_go(&harness).await;
    let before = harness.state.settings_revision();

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}/usage"),
        &cas(&harness, json!({ "window": "invalid", "percent": 50.0 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}/usage"),
        &cas(&harness, json!({ "window": "window_5h", "percent": -0.1 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}/usage"),
        &cas(&harness, json!({ "window": "window_5h", "percent": 100.1 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);

    for (window, minutes) in [
        ("window_5h", 301),
        ("window_week", 10_081),
        ("window_5h", i64::MAX),
    ] {
        let (status, body) = send_json(
            &harness,
            Method::PATCH,
            &format!("/accounts/{go_id}/usage"),
            &cas(
                &harness,
                json!({
                    "window": window,
                    "percent": 50.0,
                    "resetsInMinutes": minutes
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{window} {minutes} {body}");
        assert_v3_error(&body, ERROR_INVALID_REQUEST);
    }

    let (status, body) = harness
        .get_json(&format!("{}/accounts/{go_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["window5h"], 0.0);
    assert_eq!(harness.state.settings_revision(), before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_revision_or_generation_does_not_write_usage() {
    let harness = start_loopback("usage-stale-cas").await;
    let go_id = create_go(&harness).await;
    let revision = harness.state.settings_revision();
    let generation = harness.state.process_generation();

    let mut stale_revision = cas(
        &harness,
        json!({ "window": "window_5h", "percent": 50.0, "resetsInMinutes": 180 }),
    );
    stale_revision["expectedRevision"] = json!(revision.saturating_sub(1));
    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}/usage"),
        &stale_revision,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);

    let mut stale_generation = cas(
        &harness,
        json!({ "window": "window_5h", "percent": 50.0, "resetsInMinutes": 180 }),
    );
    stale_generation["processGeneration"] = json!(generation.wrapping_add(1));
    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}/usage"),
        &stale_generation,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);

    let (status, body) = harness
        .get_json(&format!("{}/accounts/{go_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["window5h"], 0.0);
    assert_eq!(harness.state.settings_revision(), revision);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_usage_mutations_reject_unknown_and_missing_fields() {
    let harness = start_loopback("usage-json").await;
    let go_id = create_go(&harness).await;
    let path = format!("/accounts/{go_id}/usage");
    let before = harness.state.settings_revision();

    let (status, body) = send_raw(
        &harness,
        Method::PATCH,
        &path,
        &json!({
            "processGeneration": harness.state.process_generation(),
            "window": "window_5h",
            "percent": 50.0
        })
        .to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);

    let (status, body) = send_raw(&harness, Method::PATCH, &path, "not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &path,
        &cas(&harness, json!({ "window": "window_5h" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &path,
        &cas(
            &harness,
            json!({
                "window": "window_5h",
                "percent": 50.0,
                "key": "sk-secret"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &path,
        &cas(
            &harness,
            json!({
                "window": "window_5h",
                "percent": 50.0,
                "resets_in_minutes": 180
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_eq!(harness.state.settings_revision(), before);

    harness.stop();
}

#[tokio::test]
async fn retired_v2_usage_stays_gone_and_v3_omits_legacy_aliases() {
    let harness = start_loopback("usage-v2-retired").await;
    let goat_id = create_goat(&harness).await;
    let go_id = create_go(&harness).await;

    harness
        .assert_v2_path_removed(
            Method::PATCH,
            &format!("/accounts/{goat_id}/usage"),
            Some(json!({
                "window": "window_5h",
                "percent": 50.0,
                "resets_in_minutes": 180
            })),
        )
        .await;

    let (status, patched) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}/usage"),
        &cas(
            &harness,
            json!({
                "window": "window_5h",
                "percent": 50.0,
                "resetsInMinutes": 180
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert!(patched["usage"]["window5h"].as_f64().unwrap() > 0.0);

    let (status, v3) = harness
        .get_json(&format!("{}/accounts/{go_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{v3}");
    assert!(v3["window5h"].as_f64().unwrap() > 0.0);
    assert!(v3.get("window_5h").is_none());

    harness
        .assert_v2_path_removed(
            Method::GET,
            &format!("/accounts/{go_id}/provider-usage"),
            None,
        )
        .await;
    let (status, v3_provider) = harness
        .get_json(&format!(
            "{}/accounts/{go_id}/provider-usage",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{v3_provider}");
    assert_eq!(v3_provider["availability"], "available");
    assert!(v3_provider.get("quotaWindows").is_some());
    assert!(v3_provider.get("quota_windows").is_none());

    harness
        .assert_v2_path_removed(
            Method::GET,
            &format!("/providers/accounts/{go_id}/usage"),
            None,
        )
        .await;

    let v3_alias = harness
        .client
        .get(format!(
            "{}/providers/accounts/{go_id}/usage",
            harness.v3_base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(v3_alias.status(), StatusCode::NOT_FOUND);

    let v3_refresh = harness
        .client
        .post(format!(
            "{}/accounts/{go_id}/usage/refresh",
            harness.v3_base
        ))
        .json(&json!({ "processGeneration": harness.state.process_generation() }))
        .send()
        .await
        .unwrap();
    assert_eq!(v3_refresh.status(), StatusCode::BAD_REQUEST);
    let refresh_body: Value = v3_refresh.json().await.unwrap();
    assert_eq!(refresh_body["code"], ERROR_MISSING_EXPECTED_REVISION);

    let v3_provider_model = harness
        .client
        .get(format!(
            "{}/accounts/{go_id}/provider-models",
            harness.v3_base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(v3_provider_model.status(), StatusCode::NOT_FOUND);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_usage_reads_do_not_call_official_pricing_fetch() {
    let harness = start_loopback("usage-no-outbound").await;
    let go_id = create_go(&harness).await;
    let _guard = install_official_pricing_fetch_error_for_tests(
        harness.state.process_generation(),
        "usage slice must not fetch official pricing",
    );

    let (status, _) = harness
        .get_json(&format!("{}/accounts/{go_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = harness
        .get_json(&format!(
            "{}/accounts/{go_id}/provider-usage",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}/usage"),
        &cas(&harness, json!({ "window": "window_5h", "percent": 10.0 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    harness.stop();
}
