//! Dashboard V3 Command Code GOAT manual usage refresh.

use chrono::{DateTime, Duration, Utc};
use ocg_core::dashboard_v3::{
    ERROR_INVALID_REQUEST, ERROR_THROTTLED, UsageRefresh,
    install_command_code_usage_target_for_tests,
};
use ocg_core::provider::COMMAND_CODE_PROVIDER_ID;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::{V3Harness, start_loopback};

const GOAT_KEY: &str = "user-goat-refresh-secret";

fn cas(harness: &V3Harness) -> Value {
    json!({
        "expectedRevision": harness.state.settings_revision(),
        "processGeneration": harness.state.process_generation()
    })
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

async fn create_goat(harness: &V3Harness) -> String {
    let (status, body) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &json!({
            "name": "GOAT",
            "key": GOAT_KEY,
            "providerId": COMMAND_CODE_PROVIDER_ID,
            "purchaseDate": "2026-01-31",
            "expectedRevision": harness.state.settings_revision(),
            "processGeneration": harness.state.process_generation()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["account"]["id"].as_str().unwrap().to_string()
}

fn refresh_path(id: &str) -> String {
    format!("/accounts/{id}/usage/refresh")
}

async fn serve_usage_controlled(
    status: u16,
    body: Value,
    barrier: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    )>,
) -> (String, tokio::task::JoinHandle<Option<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let body = body.to_string();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await.unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let request = String::from_utf8_lossy(&request);
        let authorization = request.lines().find_map(|line| {
            let (name, value) = line.trim_end_matches('\r').split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_string())
        });
        if let Some((entered, release)) = barrier {
            entered.notify_one();
            release.notified().await;
        }
        let response = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        authorization
    });
    (format!("http://{address}/alpha/billing/credits"), task)
}

fn usage_body(now: DateTime<Utc>) -> Value {
    json!({
        "credits": { "monthlyCredits": 63.0 },
        "windowLimits": {
            "limited": true,
            "fiveHour": {
                "used": 7.0,
                "cap": 14,
                "resetAt": (now + Duration::hours(3)).timestamp_millis()
            },
            "weekly": {
                "used": 7.0,
                "cap": 35,
                "resetAt": (now + Duration::days(4)).timestamp_millis()
            }
        }
    })
}

fn assert_secret_free(body: &Value) {
    let encoded = body.to_string();
    assert!(!encoded.contains(GOAT_KEY), "{body}");
    for field in ["key", "keyCipher", "apiKey", "token", "secret"] {
        assert!(body.get(field).is_none(), "{body}");
    }
}

fn assert_no_inference_cooldown(harness: &V3Harness, account_id: &str) {
    let account = harness
        .state
        .db
        .lock()
        .get_account(account_id)
        .unwrap()
        .unwrap();
    assert!(account.cooldown_until.is_none(), "{account:?}");
    assert!(account.cooldown_generic_until.is_none(), "{account:?}");
    assert!(account.cooldown_5h_until.is_none(), "{account:?}");
    assert!(account.cooldown_week_until.is_none(), "{account:?}");
    assert!(account.cooldown_month_until.is_none(), "{account:?}");
}

#[tokio::test]
async fn refresh_calibrates_all_goat_windows_and_throttles_repeats() {
    let harness = start_loopback("command-code-usage-refresh").await;
    let account_id = create_goat(&harness).await;
    let revision = harness.state.settings_revision();
    let now = Utc::now();
    harness.state.usage_sync.set_clock_for_test(move || now);
    let (url, authorization) = serve_usage_once(200, usage_body(now)).await;
    let _target =
        install_command_code_usage_target_for_tests(harness.state.process_generation(), url);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&account_id),
        &cas(&harness),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let refresh: UsageRefresh = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(refresh.source, "official_command_code_usage");
    assert!((refresh.usage.window_5h - 7.0).abs() < 0.000001);
    assert!((refresh.usage.window_week - 7.0).abs() < 0.000001);
    assert!((refresh.usage.window_month - 7.0).abs() < 0.000001);
    assert_eq!(refresh.usage.pricing_revision, None);
    assert_eq!(refresh.revision, revision);
    assert_eq!(harness.state.settings_revision(), revision);
    assert_eq!(
        authorization.await.unwrap().as_deref(),
        Some("Bearer user-goat-refresh-secret")
    );
    assert_secret_free(&body);
    assert_no_inference_cooldown(&harness, &account_id);
    let sync = harness
        .state
        .db
        .lock()
        .account_usage_sync_state(&account_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        sync.last_success_at.map(|value| value.to_rfc3339()),
        Some(refresh.last_success_at.clone())
    );
    assert_eq!(
        sync.next_eligible_at.map(|value| value.to_rfc3339()),
        Some(refresh.next_allowed_at.clone())
    );

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&account_id),
        &cas(&harness),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], ERROR_THROTTLED);
    assert_secret_free(&body);
    harness.stop();
}

#[tokio::test]
async fn rejected_goat_key_is_safe_and_preserves_inference_state() {
    let harness = start_loopback("command-code-usage-rejected-key").await;
    let account_id = create_goat(&harness).await;
    let now = Utc::now();
    harness.state.usage_sync.set_clock_for_test(move || now);
    let (url, authorization) = serve_usage_once(401, json!({})).await;
    let _target =
        install_command_code_usage_target_for_tests(harness.state.process_generation(), url);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&account_id),
        &cas(&harness),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], ERROR_INVALID_REQUEST);
    assert_eq!(
        body["message"],
        "official Command Code usage rejected this account Key"
    );
    assert_eq!(
        authorization.await.unwrap().as_deref(),
        Some("Bearer user-goat-refresh-secret")
    );
    assert_secret_free(&body);
    assert_no_inference_cooldown(&harness, &account_id);

    let sync = harness
        .state
        .db
        .lock()
        .account_usage_sync_state(&account_id)
        .unwrap()
        .unwrap();
    assert_eq!(sync.last_attempt_at, Some(now));
    assert_eq!(sync.last_success_at, None);
    harness.stop();
}

async fn serve_usage_once(
    status: u16,
    body: Value,
) -> (String, tokio::task::JoinHandle<Option<String>>) {
    serve_usage_controlled(status, body, None).await
}

#[tokio::test]
async fn provider_usage_refresh_uses_response_time_and_shares_legacy_throttle() {
    use std::sync::{Arc, Mutex};
    use tokio::sync::Notify;
    let harness = start_loopback("goat-provider-refresh-clock").await;
    let account_id = create_goat(&harness).await;
    let started_at = Utc::now();
    let observed_at = started_at + Duration::minutes(2);
    let clock = Arc::new(Mutex::new(started_at));
    let captured_clock = clock.clone();
    harness
        .state
        .usage_sync
        .set_clock_for_test(move || *captured_clock.lock().unwrap());
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let (url, authorization) = serve_usage_controlled(
        200,
        usage_body(started_at),
        Some((entered.clone(), release.clone())),
    )
    .await;
    let _target =
        install_command_code_usage_target_for_tests(harness.state.process_generation(), url);
    let client = harness.client.clone();
    let endpoint = format!("{}/accounts/{}/provider-usage", harness.v3_base, account_id);
    let expectation = cas(&harness);
    let pending = tokio::spawn(async move {
        client
            .post(endpoint)
            .json(&expectation)
            .send()
            .await
            .unwrap()
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    *clock.lock().unwrap() = observed_at;
    release.notify_one();
    let response = pending.await.unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_secret_free(&body);
    assert_eq!(body["availability"], "available");
    let windows = body["quotaWindows"].as_array().unwrap();
    assert_eq!(windows.len(), 3, "{body}");
    let rolling = windows
        .iter()
        .find(|window| window["windowKind"] == "five_hours")
        .unwrap();
    let reset = DateTime::parse_from_rfc3339(rolling["resetsAt"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(reset, started_at + Duration::hours(3));
    assert_eq!(rolling["observedAt"], observed_at.to_rfc3339());
    assert_eq!(body["syncState"]["lastSuccessAt"], observed_at.to_rfc3339());
    assert!((rolling["used"].as_f64().unwrap() - 7.0).abs() < 0.000001);
    assert_no_inference_cooldown(&harness, &account_id);
    assert_eq!(
        authorization.await.unwrap().as_deref(),
        Some("Bearer user-goat-refresh-secret")
    );
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&account_id),
        &cas(&harness),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], ERROR_THROTTLED);
    harness.stop();
}
