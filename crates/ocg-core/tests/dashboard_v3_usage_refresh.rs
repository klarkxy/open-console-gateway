//! Dashboard V3 official Go usage refresh: CAS, shared coordinator, and V2 coexistence.

use chrono::{DateTime, Duration, Utc};
use ocg_core::dashboard_v3::{
    ERROR_CONFLICT, ERROR_INVALID_JSON, ERROR_INVALID_REQUEST, ERROR_MISSING_EXPECTED_REVISION,
    ERROR_NOT_FOUND, ERROR_OUTBOUND_FAILED, ERROR_REVISION_CONFLICT, ERROR_THROTTLED,
    ERROR_UNAUTHORIZED, UsageRefresh, UsageRefreshThrottleError,
};
use ocg_core::db::{AccountUsageCalibrationSnapshot, CURRENT_SCHEMA_VERSION};
use ocg_core::go_usage::{GoUsageError, GoUsageSnapshot, GoUsageWindowStatus};
use ocg_core::provider::{COMMAND_CODE_PROVIDER_ID, GOAT_OFFERING_ID, ZEN_FREE_ACCOUNT_ID};
use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;
use tokio::sync::Notify;

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
const ACCOUNT_KEY: &str = "sk-go-refresh-secret";

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
            "usage refresh JSON leaked field {name}: {body}"
        );
    }
    for value in json_string_values(body) {
        for secret in [
            ACCOUNT_KEY,
            "sk-secret",
            "ocg-secret",
            "pw-secret",
            "user:pass@",
        ] {
            assert!(
                !value.contains(secret),
                "usage refresh JSON leaked secret sample {secret}: {body}"
            );
        }
    }
}

fn sample_snapshot() -> GoUsageSnapshot {
    GoUsageSnapshot {
        rolling_status: GoUsageWindowStatus::Ok,
        weekly_status: GoUsageWindowStatus::Ok,
        monthly_status: GoUsageWindowStatus::Ok,
        rolling_percent: 50.0,
        weekly_percent: 20.0,
        monthly_percent: 10.0,
        rolling_resets_in_minutes: 180,
        weekly_resets_in_minutes: 1_440,
        earliest_resets_in_minutes: 180,
    }
}

fn install_clock(harness: &V3Harness, now: DateTime<Utc>) {
    harness.state.usage_sync.set_clock_for_test(move || now);
    harness.state.usage_sync.set_jitter_for_test(|| 0.0);
}

fn install_panic_fetch(harness: &V3Harness) {
    harness.state.usage_sync.set_fetch_for_test(|_cfg, _key| {
        panic!("must not fetch official Go usage");
    });
}

fn install_snapshot_fetch(
    harness: &V3Harness,
    snapshot: GoUsageSnapshot,
) -> (Arc<AtomicUsize>, Arc<Mutex<Option<String>>>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen_key = Arc::new(Mutex::new(None));
    let calls_fetch = calls.clone();
    let seen_key_fetch = seen_key.clone();
    harness
        .state
        .usage_sync
        .set_fetch_for_test(move |_cfg, key| {
            calls_fetch.fetch_add(1, Ordering::SeqCst);
            *seen_key_fetch.lock().unwrap() = Some(key);
            let snapshot = snapshot.clone();
            Box::pin(async move { Ok(snapshot) })
        });
    (calls, seen_key)
}

async fn create_go(harness: &V3Harness) -> String {
    let (status, body) = send_json(
        harness,
        Method::POST,
        "/accounts",
        &cas(harness, json!({ "name": "Go", "key": ACCOUNT_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["account"]["id"].as_str().unwrap().to_string()
}

async fn create_goat(harness: &V3Harness) -> String {
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
                "offeringId": GOAT_OFFERING_ID,
                "purchaseDate": "2026-01-31"
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["account"]["id"].as_str().unwrap().to_string()
}

fn refresh_path(id: &str) -> String {
    format!("/accounts/{id}/usage/refresh")
}

fn seed_local_calibration(harness: &V3Harness, account_id: &str) {
    let limits = harness.state.pricing_snapshot().limits.clone();
    harness
        .state
        .db
        .lock()
        .calibrate_account_usage_snapshot(
            account_id,
            &AccountUsageCalibrationSnapshot {
                rolling_percent: 15.0,
                weekly_percent: 25.0,
                monthly_percent: 35.0,
                rolling_resets_in_minutes: 100,
                weekly_resets_in_minutes: 200,
            },
            &limits,
        )
        .unwrap();
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
    assert!(account.cooldown_free_until.is_none(), "{account:?}");
}

#[test]
fn dashboard_v3_schema_version_stays_at_v26() {
    assert_eq!(CURRENT_SCHEMA_VERSION, 26);
}

#[tokio::test]
async fn dashboard_v3_usage_refresh_requires_the_v3_session() {
    let harness = start_public("usage-refresh-auth").await;
    install_panic_fetch(&harness);

    let response = harness
        .client
        .post(format!(
            "{}/accounts/missing/usage/refresh",
            harness.v3_base
        ))
        .json(&json!({
            "expectedRevision": 0,
            "processGeneration": 0
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body: Value = response.json().await.unwrap();
    assert_v3_error(&body, ERROR_UNAUTHORIZED);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);

    let v2 = harness
        .client
        .post(format!(
            "{}/accounts/missing/usage/refresh",
            harness.v2_base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(v2.status(), StatusCode::UNAUTHORIZED);
    let v2_body = v2.text().await.unwrap();
    assert!(
        v2_body.is_empty(),
        "V2 session middleware must stay an empty 401, got {v2_body}"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_v2_login_cookie_authorizes_usage_refresh() {
    let harness = start_public("usage-refresh-cookie").await;
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

    install_snapshot_fetch(&harness, sample_snapshot());
    install_clock(&harness, Utc::now());
    let create = harness
        .client
        .post(format!("{}/accounts", harness.v3_base))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&cas(&harness, json!({ "name": "Go", "key": ACCOUNT_KEY })))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::OK);
    let created: Value = create.json().await.unwrap();
    let go_id = created["account"]["id"].as_str().unwrap().to_string();
    let response = harness
        .client
        .post(format!("{}{}", harness.v3_base, refresh_path(&go_id)))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&cas(&harness, json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["source"], "official_go_usage");
    assert_secret_free(&body);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_usage_refresh_rejects_missing_cas_without_outbound() {
    let harness = start_loopback("usage-refresh-missing-cas").await;
    let go_id = create_go(&harness).await;
    let before = harness.state.settings_revision();
    install_panic_fetch(&harness);
    let path = refresh_path(&go_id);

    let (status, body) = send_raw(
        &harness,
        Method::POST,
        &path,
        &json!({ "processGeneration": harness.state.process_generation() }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_MISSING_EXPECTED_REVISION);
    assert_eq!(body["currentRevision"], Value::Null);
    assert_eq!(body["processGeneration"], Value::Null);
    assert_eq!(harness.state.settings_revision(), before);

    let (status, body) = send_raw(&harness, Method::POST, &path, "not-json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &path,
        &cas(&harness, json!({ "key": ACCOUNT_KEY })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_JSON);
    assert_eq!(harness.state.settings_revision(), before);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_revision_or_generation_is_409_before_outbound() {
    let harness = start_loopback("usage-refresh-stale-before").await;
    let go_id = create_go(&harness).await;
    let revision = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    install_panic_fetch(&harness);
    let path = refresh_path(&go_id);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &path,
        &json!({
            "expectedRevision": revision.saturating_sub(1),
            "processGeneration": generation
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], revision);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(harness.state.settings_revision(), revision);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &path,
        &json!({
            "expectedRevision": revision,
            "processGeneration": generation ^ 1
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], revision);
    assert_eq!(body["processGeneration"], generation);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_cas_during_success_is_409_with_zero_usage_or_sync_mutation() {
    let harness = start_loopback("usage-refresh-stale-during").await;
    let go_id = create_go(&harness).await;
    seed_local_calibration(&harness, &go_id);
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let limits = harness.state.pricing_snapshot().limits.clone();
    let before_usage = harness
        .state
        .db
        .lock()
        .account_usage_with_limits(&go_id, &limits)
        .unwrap();
    let before_sync = harness
        .state
        .db
        .lock()
        .account_usage_sync_state(&go_id)
        .unwrap()
        .unwrap();
    install_clock(&harness, Utc::now());

    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let entered = Arc::new(Notify::new());
    let calls_fetch = calls.clone();
    let release_fetch = release.clone();
    let entered_fetch = entered.clone();
    let snapshot = sample_snapshot();
    harness
        .state
        .usage_sync
        .set_fetch_for_test(move |_cfg, _key| {
            calls_fetch.fetch_add(1, Ordering::SeqCst);
            let release_fetch = release_fetch.clone();
            let entered_fetch = entered_fetch.clone();
            let snapshot = snapshot.clone();
            Box::pin(async move {
                entered_fetch.notify_one();
                release_fetch.notified().await;
                Ok(snapshot)
            })
        });

    let payload = cas(&harness, json!({}));
    let client = harness.client.clone();
    let url = format!("{}{}", harness.v3_base, refresh_path(&go_id));
    let pending = tokio::spawn(async move {
        let response = client.post(url).json(&payload).send().await.unwrap();
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    });
    entered.notified().await;
    let mid = {
        let _settings_update = harness.state.settings_update.lock();
        harness.state.bump_settings_revision()
    };
    assert_eq!(mid, before + 1);
    release.notify_waiters();
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], mid);
    assert_eq!(body["processGeneration"], generation);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(harness.state.settings_revision(), mid);
    assert_eq!(harness.state.process_generation(), generation);

    let after_usage = harness
        .state
        .db
        .lock()
        .account_usage_with_limits(&go_id, &limits)
        .unwrap();
    let after_sync = harness
        .state
        .db
        .lock()
        .account_usage_sync_state(&go_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(after_usage).unwrap(),
        serde_json::to_value(before_usage).unwrap(),
        "stale success calibrated usage"
    );
    assert_eq!(
        after_sync, before_sync,
        "stale success changed usage-sync metadata"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_stale_cas_during_failure_is_409_with_zero_backoff_mutation() {
    let harness = start_loopback("usage-refresh-stale-failure").await;
    let go_id = create_go(&harness).await;
    seed_local_calibration(&harness, &go_id);
    let before_revision = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let limits = harness.state.pricing_snapshot().limits.clone();
    let before_usage = harness
        .state
        .db
        .lock()
        .account_usage_with_limits(&go_id, &limits)
        .unwrap();
    let before_sync = harness
        .state
        .db
        .lock()
        .account_usage_sync_state(&go_id)
        .unwrap()
        .unwrap();
    install_clock(&harness, Utc::now());

    let release = Arc::new(Notify::new());
    let entered = Arc::new(Notify::new());
    let release_fetch = release.clone();
    let entered_fetch = entered.clone();
    harness
        .state
        .usage_sync
        .set_fetch_for_test(move |_cfg, _key| {
            let release_fetch = release_fetch.clone();
            let entered_fetch = entered_fetch.clone();
            Box::pin(async move {
                entered_fetch.notify_one();
                release_fetch.notified().await;
                Err(GoUsageError::Timeout)
            })
        });

    let payload = cas(&harness, json!({}));
    let client = harness.client.clone();
    let url = format!("{}{}", harness.v3_base, refresh_path(&go_id));
    let pending = tokio::spawn(async move {
        let response = client.post(url).json(&payload).send().await.unwrap();
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    });
    entered.notified().await;
    let current_revision = {
        let _settings_update = harness.state.settings_update.lock();
        harness.state.bump_settings_revision()
    };
    assert_eq!(current_revision, before_revision + 1);
    release.notify_one();

    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_REVISION_CONFLICT);
    assert_eq!(body["currentRevision"], current_revision);
    assert_eq!(body["processGeneration"], generation);

    let after_usage = harness
        .state
        .db
        .lock()
        .account_usage_with_limits(&go_id, &limits)
        .unwrap();
    let after_sync = harness
        .state
        .db
        .lock()
        .account_usage_sync_state(&go_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(after_usage).unwrap(),
        serde_json::to_value(before_usage).unwrap(),
        "stale failure changed usage"
    );
    assert_eq!(
        after_sync, before_sync,
        "stale failure changed last_attempt/failure_streak/backoff"
    );

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_revision_change_after_guarded_commit_does_not_report_false_409() {
    let harness = start_loopback("usage-refresh-post-commit-revision").await;
    let go_id = create_go(&harness).await;
    seed_local_calibration(&harness, &go_id);
    let before_revision = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let limits = harness.state.pricing_snapshot().limits.clone();
    install_clock(&harness, Utc::now());
    install_snapshot_fetch(&harness, sample_snapshot());

    let state_after_commit = harness.state.clone();
    harness
        .state
        .usage_sync
        .set_before_inflight_cleanup_for_test(move || {
            let state = state_after_commit.clone();
            Box::pin(async move {
                let _settings_update = state.settings_update.lock();
                state.bump_settings_revision();
            })
        });

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&go_id),
        &json!({
            "expectedRevision": before_revision,
            "processGeneration": generation
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["revision"], before_revision + 1);
    assert_eq!(body["processGeneration"], generation);
    let usage = harness
        .state
        .db
        .lock()
        .account_usage_with_limits(&go_id, &limits)
        .unwrap();
    assert!((usage.window_5h - limits.window_5h * 0.5).abs() < 1e-9);
    assert!((usage.window_week - limits.window_week * 0.2).abs() < 1e-9);
    assert!((usage.window_month - limits.window_month * 0.1).abs() < 1e-9);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_successful_refresh_reuses_coordinator_without_bumping_revision() {
    let harness = start_loopback("usage-refresh-success").await;
    let go_id = create_go(&harness).await;
    seed_local_calibration(&harness, &go_id);
    let before = harness.state.settings_revision();
    let generation = harness.state.process_generation();
    let pricing_revision = harness.state.pricing_snapshot().revision.clone();
    let limits = harness.state.pricing_snapshot().limits.clone();
    let now = Utc::now();
    install_clock(&harness, now);
    let (calls, seen_key) = install_snapshot_fetch(&harness, sample_snapshot());

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&go_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let parsed: UsageRefresh = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.source, "official_go_usage");
    assert_eq!(parsed.revision, before);
    assert_eq!(parsed.process_generation, generation);
    assert_eq!(parsed.usage.account_id, go_id);
    assert!((parsed.usage.window_5h - limits.window_5h * 0.5).abs() < 1e-9);
    assert!((parsed.usage.window_week - limits.window_week * 0.2).abs() < 1e-9);
    assert!((parsed.usage.window_month - limits.window_month * 0.1).abs() < 1e-9);
    assert_eq!(
        parsed.usage.pricing_revision.as_deref(),
        Some(pricing_revision.as_str())
    );
    assert_eq!(parsed.last_success_at, now.to_rfc3339());
    assert_eq!(
        parsed.next_allowed_at,
        (now + Duration::seconds(15)).to_rfc3339()
    );
    assert_eq!(body["usage"]["window5h"], parsed.usage.window_5h);
    assert!(body.get("last_success_at").is_none());
    assert!(body.get("fetched_at").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(seen_key.lock().unwrap().as_deref(), Some(ACCOUNT_KEY));
    assert_eq!(harness.state.settings_revision(), before);
    assert_eq!(harness.state.process_generation(), generation);
    assert_secret_free(&body);
    assert_no_inference_cooldown(&harness, &go_id);

    let (status, usage) = harness
        .get_json(&format!("{}/accounts/{go_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{usage}");
    assert_eq!(usage["window5h"], parsed.usage.window_5h);
    assert_eq!(usage["pricingRevision"], pricing_revision);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_manual_throttle_returns_retry_after_and_next_allowed_at() {
    let harness = start_loopback("usage-refresh-throttle").await;
    let go_id = create_go(&harness).await;
    let now = Utc::now();
    install_clock(&harness, now);
    let (calls, _) = install_snapshot_fetch(&harness, sample_snapshot());
    let path = refresh_path(&go_id);

    let (status, body) = send_json(&harness, Method::POST, &path, &cas(&harness, json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let response = harness
        .client
        .post(format!("{}{path}", harness.v3_base))
        .json(&cas(&harness, json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body: Value = response.json().await.unwrap();
    assert_v3_error(&body, ERROR_THROTTLED);
    let parsed: UsageRefreshThrottleError = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.code, ERROR_THROTTLED);
    assert_eq!(body.as_object().unwrap().len(), 5, "{body}");
    assert_eq!(retry_after.as_deref(), Some("15"));
    assert_eq!(
        body["nextAllowedAt"],
        (now + Duration::seconds(15)).to_rfc3339()
    );
    assert_eq!(body["currentRevision"], harness.state.settings_revision());
    assert_eq!(
        body["processGeneration"],
        harness.state.process_generation()
    );
    assert!(body.get("next_allowed_at").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_secret_free(&body);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_concurrent_refresh_dedupes_one_official_fetch() {
    let harness = start_loopback("usage-refresh-dedupe").await;
    let go_id = create_go(&harness).await;
    install_clock(&harness, Utc::now());

    let calls = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Notify::new());
    let entered = Arc::new(Notify::new());
    let calls_fetch = calls.clone();
    let release_fetch = release.clone();
    let entered_fetch = entered.clone();
    let snapshot = sample_snapshot();
    harness
        .state
        .usage_sync
        .set_fetch_for_test(move |_cfg, _key| {
            calls_fetch.fetch_add(1, Ordering::SeqCst);
            let release_fetch = release_fetch.clone();
            let entered_fetch = entered_fetch.clone();
            let snapshot = snapshot.clone();
            Box::pin(async move {
                entered_fetch.notify_one();
                release_fetch.notified().await;
                Ok(snapshot)
            })
        });

    let payload_a = cas(&harness, json!({}));
    let payload_b = payload_a.clone();
    let client_a = harness.client.clone();
    let client_b = harness.client.clone();
    let url_a = format!("{}{}", harness.v3_base, refresh_path(&go_id));
    let url_b = url_a.clone();
    let first = tokio::spawn(async move {
        let response = client_a.post(url_a).json(&payload_a).send().await.unwrap();
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    });
    entered.notified().await;
    let second = tokio::spawn(async move {
        let response = client_b.post(url_b).json(&payload_b).send().await.unwrap();
        let status = response.status();
        let body = response.json().await.unwrap_or(Value::Null);
        (status, body)
    });
    tokio::time::sleep(StdDuration::from_millis(20)).await;
    release.notify_waiters();
    let (status_a, body_a) = first.await.unwrap();
    let (status_b, body_b) = second.await.unwrap();
    assert_eq!(status_a, StatusCode::OK, "{body_a}");
    assert_eq!(status_b, StatusCode::OK, "{body_b}");
    assert_eq!(body_a["lastSuccessAt"], body_b["lastSuccessAt"]);
    assert_eq!(body_a["nextAllowedAt"], body_b["nextAllowedAt"]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_secret_free(&body_a);
    assert_secret_free(&body_b);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_refresh_failure_preserves_last_known_good() {
    let harness = start_loopback("usage-refresh-lkg").await;
    let go_id = create_go(&harness).await;
    seed_local_calibration(&harness, &go_id);
    let now = Utc::now();
    install_clock(&harness, now);
    install_snapshot_fetch(&harness, sample_snapshot());
    let path = refresh_path(&go_id);

    let (status, body) = send_json(&harness, Method::POST, &path, &cas(&harness, json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let success: UsageRefresh = serde_json::from_value(body).unwrap();
    let last_success = harness
        .state
        .db
        .lock()
        .account_usage_sync_state(&go_id)
        .unwrap()
        .unwrap()
        .last_success_at;

    let later = now + Duration::minutes(2);
    install_clock(&harness, later);
    harness
        .state
        .usage_sync
        .set_fetch_for_test(|_cfg, _key| Box::pin(async { Err(GoUsageError::Timeout) }));

    let (status, body) = send_json(&harness, Method::POST, &path, &cas(&harness, json!({}))).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert!(
        body["message"].as_str().unwrap().contains("timed out"),
        "{body}"
    );
    assert_secret_free(&body);

    let (status, usage) = harness
        .get_json(&format!("{}/accounts/{go_id}/usage", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{usage}");
    assert_eq!(usage["window5h"], success.usage.window_5h);
    assert_eq!(usage["windowWeek"], success.usage.window_week);
    assert_eq!(usage["windowMonth"], success.usage.window_month);
    let sync = harness
        .state
        .db
        .lock()
        .account_usage_sync_state(&go_id)
        .unwrap()
        .unwrap();
    assert_eq!(sync.last_success_at, last_success);
    assert!(sync.failure_streak >= 1);
    assert_no_inference_cooldown(&harness, &go_id);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_official_rate_limit_is_outbound_failed_without_inference_cooldown() {
    let harness = start_loopback("usage-refresh-official-429").await;
    let go_id = create_go(&harness).await;
    seed_local_calibration(&harness, &go_id);
    let limits = harness.state.pricing_snapshot().limits.clone();
    let before = harness
        .state
        .db
        .lock()
        .account_usage_with_limits(&go_id, &limits)
        .unwrap();
    install_clock(&harness, Utc::now());
    harness
        .state
        .usage_sync
        .set_fetch_for_test(|_cfg, _key| Box::pin(async { Err(GoUsageError::RateLimited) }));

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&go_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_v3_error(&body, ERROR_OUTBOUND_FAILED);
    assert_ne!(body["code"], ERROR_UNAUTHORIZED);
    assert_secret_free(&body);

    let after = harness
        .state
        .db
        .lock()
        .account_usage_with_limits(&go_id, &limits)
        .unwrap();
    assert_eq!(after.window_5h, before.window_5h);
    assert_eq!(after.window_week, before.window_week);
    assert_eq!(after.window_month, before.window_month);
    assert_no_inference_cooldown(&harness, &go_id);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_refresh_rejects_wrong_provider_and_state() {
    let harness = start_loopback("usage-refresh-ineligible").await;
    install_panic_fetch(&harness);
    let goat_id = create_goat(&harness).await;
    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts/managed",
        &cas(&harness, json!({ "name": "Managed draft" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let managed_id = body["account"]["id"].as_str().unwrap().to_string();

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&goat_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("unavailable for this provider offering"),
        "{body}"
    );

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(ZEN_FREE_ACCOUNT_ID),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&managed_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("only ready accounts"),
        "{body}"
    );

    let (status, body) = send_json(
        &harness,
        Method::POST,
        "/accounts/does-not-exist/usage/refresh",
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_v3_error(&body, ERROR_NOT_FOUND);
    assert_eq!(body["currentRevision"], harness.state.settings_revision());

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_refresh_maps_rejected_key_to_invalid_request_not_unauthorized() {
    let harness = start_loopback("usage-refresh-rejected-key").await;
    let go_id = create_go(&harness).await;
    install_clock(&harness, Utc::now());
    harness
        .state
        .usage_sync
        .set_fetch_for_test(|_cfg, _key| Box::pin(async { Err(GoUsageError::Unauthorized) }));

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&go_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_v3_error(&body, ERROR_INVALID_REQUEST);
    assert_ne!(body["code"], ERROR_UNAUTHORIZED);
    assert_eq!(
        body["message"],
        "official Go usage rejected this account key"
    );
    assert!(!body["message"].as_str().unwrap().contains("401"));
    assert_secret_free(&body);
    assert_no_inference_cooldown(&harness, &go_id);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_refresh_key_cas_conflict_preserves_windows() {
    let harness = start_loopback("usage-refresh-key-cas").await;
    let go_id = create_go(&harness).await;
    seed_local_calibration(&harness, &go_id);
    let limits = harness.state.pricing_snapshot().limits.clone();
    let before = harness
        .state
        .db
        .lock()
        .account_usage_with_limits(&go_id, &limits)
        .unwrap();
    install_clock(&harness, Utc::now());

    let state_for_fetch = harness.state.clone();
    let account_id = go_id.clone();
    harness
        .state
        .usage_sync
        .set_fetch_for_test(move |_cfg, _key| {
            let rotated = state_for_fetch.encrypt_key("sk-rotated").unwrap();
            state_for_fetch
                .db
                .lock()
                .update_account(
                    &account_id,
                    &ocg_core::models::AccountUpdate {
                        name: None,
                        username: None,
                        password: None,
                        key: Some("sk-rotated".into()),
                        enabled: None,
                        referral_code: None,
                        purchase_date: None,
                        notes: None,
                    },
                    Some(&rotated),
                    None,
                )
                .unwrap();
            let snapshot = sample_snapshot();
            Box::pin(async move { Ok(snapshot) })
        });

    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&go_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_v3_error(&body, ERROR_CONFLICT);
    assert!(!body["message"].as_str().unwrap().contains(ACCOUNT_KEY));
    assert!(!body["message"].as_str().unwrap().contains("sk-rotated"));
    let after = harness
        .state
        .db
        .lock()
        .account_usage_with_limits(&go_id, &limits)
        .unwrap();
    assert_eq!(after.window_5h, before.window_5h);
    assert_eq!(after.window_week, before.window_week);
    assert_eq!(after.window_month, before.window_month);

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_refresh_coexists_with_v2_and_shares_throttle() {
    let harness = start_loopback("usage-refresh-v2").await;
    let go_id = create_go(&harness).await;
    let now = Utc::now();
    install_clock(&harness, now);
    install_snapshot_fetch(&harness, sample_snapshot());
    let limits = harness.state.pricing_snapshot().limits.clone();

    let (status, v3) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&go_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v3}");
    assert_eq!(v3["source"], "official_go_usage");
    assert!(v3.get("processGeneration").is_some());
    assert!(v3.get("last_success_at").is_none());

    let v2_get = harness
        .client
        .get(format!("{}/accounts/{go_id}/usage", harness.v2_base))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_get.status(), StatusCode::OK);
    let v2_usage: Value = v2_get.json().await.unwrap();
    assert!((v2_usage["window_5h"].as_f64().unwrap() - limits.window_5h * 0.5).abs() < 1e-9);
    assert!(v2_usage.get("window5h").is_none());
    assert!(v2_usage.get("processGeneration").is_none());

    let v2_refresh = harness
        .client
        .post(format!(
            "{}/accounts/{go_id}/usage/refresh",
            harness.v2_base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(v2_refresh.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = v2_refresh
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let v2_body: Value = v2_refresh.json().await.unwrap();
    assert_eq!(retry_after.as_deref(), Some("15"));
    assert_eq!(
        v2_body["next_allowed_at"],
        (now + Duration::seconds(15)).to_rfc3339()
    );
    assert!(v2_body.get("nextAllowedAt").is_none());
    assert!(v2_body.get("processGeneration").is_none());
    assert!(v2_body.get("code").is_none());

    harness.stop();
}

#[tokio::test]
async fn dashboard_v3_disabled_go_account_can_still_manual_refresh() {
    let harness = start_loopback("usage-refresh-disabled").await;
    let go_id = create_go(&harness).await;
    let (status, body) = send_json(
        &harness,
        Method::PATCH,
        &format!("/accounts/{go_id}"),
        &cas(&harness, json!({ "enabled": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["account"]["enabled"], false);

    install_clock(&harness, Utc::now());
    install_snapshot_fetch(&harness, sample_snapshot());
    let (status, body) = send_json(
        &harness,
        Method::POST,
        &refresh_path(&go_id),
        &cas(&harness, json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["source"], "official_go_usage");
    assert_secret_free(&body);

    harness.stop();
}
