use super::*;

#[test]
fn sub2_key_windows_and_subscription_remaining_keep_their_scope() {
    let mut snapshot = PlatformSnapshot::default();
    parse_sub2_usage(&json!({"mode":"quota_limited","rate_limits":[{"window":"5h","limit":20,"used":4,"remaining":16,"reset_at":"2026-09-08T12:00:00Z"},{"window":"1d","limit":50}]}),&mut snapshot).unwrap();
    assert_eq!(snapshot.quotas.len(), 3);
    assert_eq!(snapshot.quotas[1].remaining, Some(16.0));
    assert_eq!(snapshot.quotas[2].used, None);
    assert!(
        snapshot
            .quotas
            .iter()
            .all(|q| matches!(q.kind, PlatformQuotaKind::KeyLimit))
    );
    let mut snapshot = PlatformSnapshot::default();
    parse_sub2_usage(&json!({"mode":"unrestricted","remaining":8,"subscription":{"daily_usage_usd":2,"daily_limit_usd":10,"weekly_usage_usd":5,"weekly_limit_usd":30}}),&mut snapshot).unwrap();
    assert!(
        !snapshot
            .quotas
            .iter()
            .any(|q| matches!(q.kind, PlatformQuotaKind::Wallet))
    );
    assert_eq!(
        snapshot
            .quotas
            .iter()
            .filter(|q| matches!(q.kind, PlatformQuotaKind::Subscription))
            .count(),
        2
    );
    let mut snapshot = PlatformSnapshot::default();
    parse_sub2_usage(&json!({"mode":"unrestricted","remaining":8}), &mut snapshot).unwrap();
    assert!(
        !snapshot
            .quotas
            .iter()
            .any(|q| matches!(q.kind, PlatformQuotaKind::Wallet))
    );
}

#[test]
fn sub2_group_subscription_type_and_model_platform_are_preserved() {
    let mut snapshot = PlatformSnapshot::default();
    parse_sub2_groups(
        &json!([{"id":4,"platform":"composite","subscription_type":"subscription"}]),
        &mut snapshot,
    );
    assert_eq!(
        snapshot.groups[0].subscription_type.as_deref(),
        Some("subscription")
    );
    let mut allowed = BTreeSet::new();
    collect_models(
        &json!({"data":[{"id":"model-a","platform":"openai"}]}),
        Some("4"),
        Some("composite"),
        SRC_V1_MODELS,
        "sub2api.models",
        &mut allowed,
        &mut snapshot,
    )
    .unwrap();
    assert_eq!(snapshot.models[0].platform.as_deref(), Some("openai"));
}
use crate::platform::{PlatformGroup, PlatformKind, PlatformQuotaKind, PlatformReadRequest};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const USER: &str = "pat-user-credential-do-not-echo";
const KEY: &str = "sk-key-secret-do-not-echo";

fn now() -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-04-15T12:00:00Z")
        .unwrap()
        .timestamp()
}

struct Captured {
    path: String,
    authorization: Option<String>,
}

struct Route {
    status: u16,
    body: String,
    location: Option<String>,
}

impl Route {
    fn ok(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            body: body.into(),
            location: None,
        }
    }
}

async fn spawn_mock(
    routes: HashMap<String, Route>,
) -> (String, reqwest::Client, Arc<Mutex<Vec<Captured>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let hits = captured.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0_u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            let path = request_path(&head);
            let authorization = header_value(&head, "authorization");
            hits.lock().unwrap().push(Captured {
                path: path.clone(),
                authorization,
            });
            let route = routes.get(&path);
            let (status, reason, location, body) = match route {
                Some(route)
                    if route.status == 302 || route.status == 307 || route.status == 301 =>
                {
                    (route.status, "Found", route.location.clone(), String::new())
                }
                Some(route) => (route.status, "OK", None, route.body.clone()),
                None => (404, "Not Found", None, r#"{"success":false}"#.to_string()),
            };
            let mut response =
                format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n");
            if let Some(location) = location {
                response.push_str(&format!("Location: {location}\r\n"));
            }
            response.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ));
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    (format!("http://{addr}"), client, captured)
}

fn request_path(head: &str) -> String {
    let line = head.lines().next().unwrap_or_default();
    let path = line.split_whitespace().nth(1).unwrap_or("/");
    path.split('?').next().unwrap_or(path).to_string()
}

fn header_value(head: &str, name: &str) -> Option<String> {
    let prefix = format!("{name}:");
    head.lines().find_map(|line| {
        if line.len() >= prefix.len() && line[..prefix.len()].eq_ignore_ascii_case(&prefix) {
            Some(line[prefix.len()..].trim().to_string())
        } else {
            None
        }
    })
}

fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

fn group_with(id: Option<&str>, auto: &[&str]) -> PlatformGroup {
    PlatformGroup {
        subscription_type: None,
        id: id.map(str::to_string),
        platform: None,
        auto_groups: auto.iter().map(|value| value.to_string()).collect(),
        verified: false,
    }
}

#[tokio::test]
async fn invalid_base_url_is_sanitized_and_stale() {
    let group = group_with(None, &[]);
    let client = no_redirect_client();
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: "https://user:pass@evil.example/v1",
            user_credential: Some(USER),
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;
    assert!(snapshot.stale);
    assert_eq!(snapshot.errors, vec![ERR_BASE_URL_INVALID]);
    assert!(snapshot.quotas.is_empty());
    let joined = snapshot.errors.join(" ");
    assert!(!joined.contains(USER));
    assert!(!joined.contains(KEY));
    assert!(!joined.contains("pass"));
}

#[tokio::test]
async fn missing_auth_is_fixed_code() {
    let group = group_with(None, &[]);
    let snapshot = read(
        &no_redirect_client(),
        &PlatformReadRequest {
            kind: PlatformKind::Sub2api,
            base_url: "https://panel.example",
            user_credential: Some("  "),
            key: None,
            group: &group,
            now: now(),
        },
    )
    .await;
    assert_eq!(snapshot.errors, vec![ERR_AUTH_MISSING]);
    assert!(snapshot.stale);
}

#[tokio::test]
async fn new_api_key_only_reads_proven_models_and_key_quota() {
    // Fixtures follow New API 71c1fd7 GetStatus, ListModels, GetTokenUsage, GetPricing.
    let mut routes = HashMap::new();
    routes.insert(
        "/api/status".to_string(),
        Route::ok(r#"{"success":true,"data":{"quota_per_unit":500000}}"#),
    );
    routes.insert(
        "/v1/models".to_string(),
        Route::ok(r#"{"object":"list","data":[{"id":"gpt-4","object":"model"},{"id":"claude-sonnet","object":"model"}]}"#),
    );
    routes.insert(
        "/api/usage/token/".to_string(),
        Route::ok(
            r#"{"code":true,"message":"ok","data":{"object":"token_usage","name":"cli","group":"Codex稳定","total_granted":800000,"total_used":300000,"total_available":500000,"unlimited_quota":false,"model_limits_enabled":false,"expires_at":1776384000}}"#,
        ),
    );
    routes.insert(
        "/api/pricing".to_string(),
        Route::ok(
            json!({
                "success": true,
                "data": [
                    {"model_name":"gpt-4","quota_type":0,"model_ratio":2.5,"completion_ratio":4.0,"cache_ratio":0.5,"create_cache_ratio":1.25},
                    {"model_name":"claude-sonnet","quota_type":0,"model_ratio":3.0,"completion_ratio":1.0,"billing_mode":"tiered_expr","billing_expr":"tokens*tier"},
                    {"model_name":"storefront-only","quota_type":0,"model_ratio":9.0,"completion_ratio":1.0}
                ],
                "group_ratio": {"default": 2.0},
                "auto_groups": ["default"]
            })
            .to_string(),
        ),
    );
    let (base, client, captured) = spawn_mock(routes).await;
    let group = group_with(Some("default"), &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: &base,
            user_credential: None,
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;

    assert!(!snapshot.stale, "{:?}", snapshot.errors);
    assert!(
        snapshot
            .quotas
            .iter()
            .all(|q| !matches!(q.kind, PlatformQuotaKind::Wallet))
    );
    let key_quota = snapshot
        .quotas
        .iter()
        .find(|q| matches!(q.kind, PlatformQuotaKind::KeyLimit))
        .expect("key quota");
    assert_eq!(key_quota.used, Some(0.6));
    assert_eq!(key_quota.remaining, Some(1.0));
    assert_eq!(key_quota.limit, Some(1.6));
    assert_eq!(key_quota.unit, "usd");
    assert_eq!(key_quota.scope_id, "cli");
    assert!(
        snapshot
            .groups
            .iter()
            .any(|group| group.id.as_deref() == Some("Codex稳定")),
        "{:?}",
        snapshot.groups
    );

    let ids: Vec<_> = snapshot.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["gpt-4", "claude-sonnet"]);
    assert!(snapshot.models.iter().all(|m| m.source == SRC_V1_MODELS));

    let gpt = snapshot
        .prices
        .iter()
        .find(|p| p.model == "gpt-4")
        .expect("gpt price");
    assert_eq!(gpt.input, Some(2.5 * 2.0 / 500_000.0));
    assert_eq!(gpt.output, Some(2.5 * 2.0 / 500_000.0 * 4.0));
    assert_eq!(gpt.cache_read, Some(2.5 * 2.0 / 500_000.0 * 0.5));
    assert_eq!(gpt.cache_write, Some(2.5 * 2.0 / 500_000.0 * 1.25));
    assert_eq!(gpt.valid_until, now() + SNAPSHOT_TTL_SECS);

    let claude = snapshot
        .prices
        .iter()
        .find(|p| p.model == "claude-sonnet")
        .expect("claude price");
    assert_eq!(claude.unavailable_reason.as_deref(), Some(UNAVAIL_TIERED));
    assert!(claude.input.is_none());
    assert!(snapshot.prices.iter().all(|p| p.model != "storefront-only"));

    let hits = captured.lock().unwrap();
    assert!(hits.iter().all(|hit| {
        hit.authorization
            .as_deref()
            .is_none_or(|value| value == format!("Bearer {KEY}") || hit.path == "/api/status")
    }));
    assert!(
        hits.iter()
            .any(|hit| hit.path == "/api/status" && hit.authorization.is_none())
    );
}

#[tokio::test]
async fn o03_wallet_subscription_and_key_limits_are_not_summed() {
    let mut routes = HashMap::new();
    routes.insert(
        "/api/status".to_string(),
        Route::ok(r#"{"success":true,"data":{"quota_per_unit":500000}}"#),
    );
    routes.insert(
        "/api/user/self".to_string(),
        Route::ok(r#"{"success":true,"data":{"id":2,"username":"demo","group":"default","quota":1500000}}"#),
    );
    routes.insert(
        "/api/user/self/groups".to_string(),
        Route::ok(r#"{"success":true,"data":{"default":{"ratio":1,"desc":"default"},"auto":{"ratio":"auto","desc":"auto"}}}"#),
    );
    routes.insert(
        "/api/subscription/self".to_string(),
        Route::ok(
            r#"{"success":true,"data":{"billing_preference":"subscription","subscriptions":[{"subscription":{"id":9,"amount_total":2000000,"amount_used":250000,"end_time":1776384000,"next_reset_time":1773705600,"allow_wallet_overflow":false,"status":"active"}}]}}"#,
        ),
    );
    routes.insert(
        "/api/token/auto-groups".to_string(),
        Route::ok(r#"{"success":true,"data":{"groups":["default","vip"],"max_count":5}}"#),
    );
    routes.insert(
        "/v1/models".to_string(),
        Route::ok(r#"{"success":true,"data":[{"id":"gpt-4"}]}"#),
    );
    routes.insert(
        "/api/usage/token/".to_string(),
        Route::ok(r#"{"code":true,"data":{"name":"cli","total_used":1,"total_available":2,"total_granted":3,"unlimited_quota":false}}"#),
    );
    routes.insert(
        "/api/pricing".to_string(),
        Route::ok(
            r#"{"success":true,"data":[],"group_ratio":{"default":1},"auto_groups":["default"]}"#,
        ),
    );
    let (base, client, captured) = spawn_mock(routes).await;
    let group = group_with(Some("default"), &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: &base,
            user_credential: Some(USER),
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;

    let wallet = snapshot
        .quotas
        .iter()
        .find(|q| matches!(q.kind, PlatformQuotaKind::Wallet))
        .expect("wallet");
    assert_eq!(wallet.remaining, Some(3.0));
    assert_eq!(wallet.unit, "usd");
    assert!(
        wallet.used.is_none(),
        "missing used_quota must not be synthesized as zero"
    );
    assert!(wallet.limit.is_none());

    let sub = snapshot
        .quotas
        .iter()
        .find(|q| matches!(q.kind, PlatformQuotaKind::Subscription))
        .expect("subscription");
    assert_eq!(sub.used, Some(0.5));
    assert_eq!(sub.remaining, Some(3.5));
    assert_eq!(sub.unit, "usd");
    assert_eq!(snapshot.billing_preference.as_deref(), Some("subscription"));
    assert_eq!(snapshot.wallet_overflow, Some(false));

    let key_limit = snapshot
        .quotas
        .iter()
        .find(|q| matches!(q.kind, PlatformQuotaKind::KeyLimit))
        .expect("key limit");
    assert_eq!(key_limit.used, Some(1.0 / 500_000.0));
    assert_eq!(key_limit.remaining, Some(2.0 / 500_000.0));
    assert_eq!(key_limit.limit, Some(3.0 / 500_000.0));
    assert_eq!(key_limit.unit, "usd");
    let invented_total =
        wallet.remaining.unwrap() + sub.remaining.unwrap() + key_limit.remaining.unwrap();
    assert!(
        snapshot
            .quotas
            .iter()
            .all(|q| q.remaining != Some(invented_total)),
        "wallet, subscription, and Key limits must not be added into one available total"
    );

    let auto = snapshot
        .groups
        .iter()
        .find(|g| g.id.as_deref() == Some("auto"))
        .expect("auto group");
    assert_eq!(auto.auto_groups, vec!["default", "vip"]);
    assert!(auto.verified);

    let hits = captured.lock().unwrap();
    let user_auth = format!("Bearer {USER}");
    let key_auth = format!("Bearer {KEY}");
    for hit in hits.iter() {
        if hit.path.starts_with("/api/user")
            || hit.path.starts_with("/api/subscription")
            || hit.path.starts_with("/api/token")
            || hit.path == "/api/pricing"
        {
            assert_eq!(
                hit.authorization.as_deref(),
                Some(user_auth.as_str()),
                "{}",
                hit.path
            );
        }
        if hit.path == "/v1/models" || hit.path == "/api/usage/token/" {
            assert_eq!(
                hit.authorization.as_deref(),
                Some(key_auth.as_str()),
                "{}",
                hit.path
            );
        }
        if hit.path == "/api/status" {
            assert!(hit.authorization.is_none());
        }
    }
}

#[tokio::test]
async fn new_api_auto_group_and_per_request_prices_are_unavailable() {
    let mut routes = HashMap::new();
    routes.insert(
        "/api/status".to_string(),
        Route::ok(r#"{"success":true,"data":{"quota_per_unit":500000}}"#),
    );
    routes.insert(
        "/v1/models".to_string(),
        Route::ok(r#"{"data":[{"id":"gpt-4"},{"id":"image-1"}]}"#),
    );
    routes.insert(
        "/api/usage/token/".to_string(),
        Route::ok(r#"{"code":true,"data":{"unlimited_quota":true,"total_used":10}}"#),
    );
    routes.insert(
        "/api/pricing".to_string(),
        Route::ok(
            json!({
                "success": true,
                "data": [
                    {"model_name":"gpt-4","quota_type":0,"model_ratio":1.0,"completion_ratio":1.0},
                    {"model_name":"image-1","quota_type":1,"model_price":0.04}
                ],
                "group_ratio": {"default": 1.0}
            })
            .to_string(),
        ),
    );
    let (base, client, _) = spawn_mock(routes).await;
    let group = group_with(Some("auto"), &["default"]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: &base,
            user_credential: None,
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;
    let gpt = snapshot.prices.iter().find(|p| p.model == "gpt-4").unwrap();
    assert_eq!(gpt.unavailable_reason.as_deref(), Some(UNAVAIL_AUTO));
    assert!(gpt.input.is_none());
    let image = snapshot
        .prices
        .iter()
        .find(|p| p.model == "image-1")
        .unwrap();
    assert_eq!(
        image.unavailable_reason.as_deref(),
        Some(UNAVAIL_PER_REQUEST)
    );
    let key_quota = snapshot
        .quotas
        .iter()
        .find(|q| matches!(q.kind, PlatformQuotaKind::KeyLimit))
        .unwrap();
    assert!(key_quota.unlimited);
    assert!(key_quota.remaining.is_none());
    assert!(key_quota.limit.is_none());
    assert_eq!(key_quota.used, Some(10.0 / 500_000.0));
    assert_eq!(key_quota.unit, "usd");
}

#[tokio::test]
async fn redirect_is_rejected_without_echoing_credentials() {
    let mut routes = HashMap::new();
    routes.insert(
        "/api/status".to_string(),
        Route::ok(r#"{"success":true,"data":{"quota_per_unit":500000}}"#),
    );
    routes.insert(
        "/v1/models".to_string(),
        Route {
            status: 302,
            body: String::new(),
            location: Some("https://evil.example/stolen".to_string()),
        },
    );
    let (base, client, _) = spawn_mock(routes).await;
    let group = group_with(None, &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: &base,
            user_credential: None,
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;
    assert!(
        snapshot
            .errors
            .iter()
            .any(|error| error == "new_api.models.redirect_rejected")
    );
    let joined = snapshot.errors.join(" ");
    assert!(!joined.contains(KEY));
    assert!(!joined.contains("evil.example"));
    assert!(snapshot.models.is_empty());
}

#[tokio::test]
async fn sub2_key_only_uses_billing_multiplier_and_ignores_plaza_catalog() {
    // Fixtures follow Sub2API 772a038 Usage, KeyBillingInfo, /v1/models, plaza DTO.
    let mut routes = HashMap::new();
    routes.insert(
        "/v1/models".to_string(),
        Route::ok(r#"{"object":"list","data":[{"id":"claude-opus"}]}"#),
    );
    routes.insert(
        "/v1/usage".to_string(),
        Route::ok(
            r#"{"mode":"quota_limited","quota":{"limit":40,"used":12.5,"remaining":27.5,"unit":"USD"},"usage":{"total":{"cost":12.5}}}"#,
        ),
    );
    routes.insert(
        "/v1/sub2api/billing".to_string(),
        Route::ok(
            r#"{"object":"sub2api.key_billing","schema_version":1,"billing_scope":"token","group_rate_multiplier":1.5,"resolved_rate_multiplier":1.5,"peak_rate_enabled":true,"peak_start":"09:00","peak_end":"18:00","timezone":"Asia/Shanghai","effective_rate_multiplier":1.5,"observed_at":"2026-04-15T12:00:00Z"}"#,
        ),
    );
    routes.insert(
        "/api/v1/model-plaza".to_string(),
        Route::ok(
            json!({
                "code": 0,
                "message": "success",
                "data": {
                    "description": "plaza",
                    "groups": [{
                        "id": 3,
                        "name": "claude",
                        "platform": "claude",
                        "rate_multiplier": 9.0,
                        "peak_rate_enabled": true,
                        "peak_start": "09:00",
                        "peak_end": "18:00",
                        "models": [
                            {
                                "name":"claude-opus",
                                "pricing":{"billing_mode":"token","input_price":0.000015,"output_price":0.000075,"cache_read_price":0.0000015,"intervals":[]},
                                "official_pricing":{"input_price":0.000015,"output_price":0.000075,"cache_read_price":0.0000015}
                            },
                            {"name":"plaza-only-model","official_pricing":{"input_price":0.001}}
                        ]
                    }]
                }
            })
            .to_string(),
        ),
    );
    let (base, client, captured) = spawn_mock(routes).await;
    let group = group_with(Some("3"), &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::Sub2api,
            base_url: &base,
            user_credential: None,
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;

    assert_eq!(
        snapshot
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["claude-opus"]
    );
    assert!(
        snapshot
            .quotas
            .iter()
            .all(|q| !matches!(q.kind, PlatformQuotaKind::Wallet))
    );
    let key_quota = snapshot
        .quotas
        .iter()
        .find(|q| matches!(q.kind, PlatformQuotaKind::KeyLimit))
        .expect("key usage quota");
    assert_eq!(key_quota.used, Some(12.5));
    assert_eq!(key_quota.remaining, Some(27.5));
    assert_eq!(key_quota.limit, Some(40.0));

    let billed = snapshot
        .prices
        .iter()
        .find(|p| p.model == "claude-opus" && !p.official_reference)
        .unwrap();
    assert_eq!(billed.unavailable_reason.as_deref(), Some(UNAVAIL_TIME));
    assert!(billed.input.is_none());
    let official = snapshot
        .prices
        .iter()
        .find(|p| p.model == "claude-opus" && p.official_reference)
        .unwrap();
    assert_eq!(official.input, Some(0.000015));
    assert_eq!(official.output, Some(0.000075));
    assert_eq!(official.valid_until, now() + SNAPSHOT_TTL_SECS);
    assert!(
        snapshot
            .prices
            .iter()
            .all(|p| p.model != "plaza-only-model")
    );

    let hits = captured.lock().unwrap();
    assert!(hits.iter().any(|h| h.path == "/v1/usage"));
    assert!(hits.iter().any(|h| h.path == "/v1/sub2api/billing"));
    assert!(
        hits.iter()
            .any(|h| h.path == "/api/v1/model-plaza" && h.authorization.is_none())
    );
}

#[tokio::test]
async fn sub2_user_subscriptions_do_not_zero_missing_windows() {
    let mut routes = HashMap::new();
    routes.insert(
        "/api/v1/user/profile".to_string(),
        Route::ok(r#"{"code":0,"data":{"id":8,"balance":15.5,"frozen_balance":1.0}}"#),
    );
    routes.insert(
        "/api/v1/subscriptions/summary".to_string(),
        Route::ok(
            r#"{"code":0,"message":"success","data":{"active_count":1,"subscriptions":[{"id":4,"group_id":3,"group_name":"claude","status":"active","daily_used_usd":1.25,"daily_limit_usd":20,"expires_at":"2026-05-01T00:00:00Z"}]}}"#,
        ),
    );
    routes.insert(
        "/api/v1/groups/available".to_string(),
        Route::ok(r#"{"code":0,"data":[{"id":3,"name":"claude","platform":"claude","rate_multiplier":1.2}]}"#),
    );
    routes.insert(
        "/v1/models".to_string(),
        Route::ok(r#"{"data":[{"id":"claude-opus"}]}"#),
    );
    routes.insert(
        "/v1/usage".to_string(),
        Route::ok(r#"{"mode":"unrestricted","remaining":15.5,"balance":15.5,"unit":"USD"}"#),
    );
    routes.insert(
        "/v1/sub2api/billing".to_string(),
        Route::ok(r#"{"object":"sub2api.key_billing","schema_version":1,"billing_scope":"token","effective_rate_multiplier":1.2,"peak_rate_enabled":false}"#),
    );
    routes.insert(
        "/api/v1/model-plaza".to_string(),
        Route::ok(
            json!({
                "code": 0,
                "data": {
                    "groups": [{
                        "id": 3,
                        "name": "claude",
                        "rate_multiplier": 1.2,
                        "peak_rate_enabled": false,
                        "models": [{
                            "name": "claude-opus",
                            "pricing": {"billing_mode":"token","input_price":0.00001,"output_price":0.00003,"intervals":[]},
                            "official_pricing": {"input_price":0.000015,"output_price":0.000075}
                        }]
                    }]
                }
            })
            .to_string(),
        ),
    );
    let (base, client, captured) = spawn_mock(routes).await;
    let group = group_with(None, &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::Sub2api,
            base_url: &base,
            user_credential: Some(USER),
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;

    let wallet = snapshot
        .quotas
        .iter()
        .find(|q| matches!(q.kind, PlatformQuotaKind::Wallet))
        .expect("profile wallet");
    assert_eq!(wallet.remaining, Some(15.5));
    assert!(wallet.used.is_none());
    assert_eq!(
        snapshot
            .quotas
            .iter()
            .filter(|q| matches!(q.kind, PlatformQuotaKind::Wallet))
            .count(),
        1,
        "unrestricted /v1/usage must not duplicate the profile wallet"
    );
    let billed = snapshot
        .prices
        .iter()
        .find(|p| p.model == "claude-opus" && !p.official_reference)
        .unwrap();
    assert_eq!(billed.input, Some(0.00001 * 1.2));
    assert_eq!(billed.output, Some(0.00003 * 1.2));
    let official = snapshot
        .prices
        .iter()
        .find(|p| p.model == "claude-opus" && p.official_reference)
        .unwrap();
    assert_eq!(official.input, Some(0.000015));
    let daily = snapshot
        .quotas
        .iter()
        .find(|q| matches!(q.kind, PlatformQuotaKind::Subscription))
        .expect("subscription");
    assert_eq!(daily.used, Some(1.25));
    assert_eq!(daily.limit, Some(20.0));
    assert_eq!(daily.unit, "usd");
    assert!(
        snapshot.quotas.iter().all(
            |q| q.period.as_deref() != Some("weekly") && q.period.as_deref() != Some("monthly")
        ),
        "omitted windows must not appear as zeroed quotas"
    );
    assert!(
        snapshot
            .groups
            .iter()
            .any(|g| g.id.as_deref() == Some("3") && g.platform.as_deref() == Some("claude"))
    );

    let user_auth = format!("Bearer {USER}");
    let key_auth = format!("Bearer {KEY}");
    let hits = captured.lock().unwrap();
    for hit in hits.iter() {
        if hit.path.starts_with("/api/v1/") && hit.path != "/api/v1/model-plaza" {
            assert_eq!(
                hit.authorization.as_deref(),
                Some(user_auth.as_str()),
                "{}",
                hit.path
            );
        }
        if hit.path.starts_with("/v1/") {
            assert_eq!(
                hit.authorization.as_deref(),
                Some(key_auth.as_str()),
                "{}",
                hit.path
            );
        }
    }
}

#[test]
fn snapshot_expiry_is_capped_at_24h() {
    assert_eq!(snapshot_expiry(now()), now() + SNAPSHOT_TTL_SECS);
}

#[test]
fn json_helpers_do_not_invent_zero_for_missing_values() {
    let value = json!({"used": 0, "limit": null});
    assert_eq!(json_f64(value.get("used")), Some(0.0));
    assert_eq!(json_f64(value.get("limit")), None);
    assert_eq!(json_f64(value.get("remaining")), None);
    assert_eq!(json_f64(Some(&json!("NaN"))), None);
    assert_eq!(json_i64(Some(&json!(3.5))), None);
}

#[tokio::test]
async fn o01_storefront_listing_is_not_model_permission() {
    let mut routes = HashMap::new();
    routes.insert(
        "/api/status".to_string(),
        Route::ok(r#"{"success":true,"data":{"quota_per_unit":500000}}"#),
    );
    routes.insert(
        "/api/user/self".to_string(),
        Route::ok(r#"{"success":true,"data":{"group":"default","quota":10,"used_quota":2}}"#),
    );
    routes.insert(
        "/api/user/self/groups".to_string(),
        Route::ok(r#"{"success":true,"data":{"default":{"ratio":1}}}"#),
    );
    routes.insert(
        "/api/subscription/self".to_string(),
        Route::ok(r#"{"success":true,"data":{"billing_preference":"balance","subscriptions":[]}}"#),
    );
    routes.insert(
        "/api/token/auto-groups".to_string(),
        Route::ok(r#"{"success":true,"data":{"groups":[]}}"#),
    );
    routes.insert(
        "/api/pricing".to_string(),
        Route::ok(
            r#"{"success":true,"data":[{"model_name":"gpt-4","quota_type":0,"model_ratio":1,"completion_ratio":1}],"group_ratio":{"default":1}}"#,
        ),
    );
    let (base, client, captured) = spawn_mock(routes).await;
    let group = group_with(Some("default"), &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: &base,
            user_credential: Some(USER),
            key: None,
            group: &group,
            now: now(),
        },
    )
    .await;
    assert!(snapshot.models.is_empty());
    assert!(snapshot.prices.is_empty());
    assert!(
        snapshot
            .quotas
            .iter()
            .any(|q| matches!(q.kind, PlatformQuotaKind::Wallet))
    );
    assert!(
        captured
            .lock()
            .unwrap()
            .iter()
            .all(|hit| hit.path != "/v1/models" && hit.path != "/api/usage/token/")
    );
}

#[tokio::test]
async fn new_api_missing_completion_ratio_is_unavailable() {
    let mut routes = HashMap::new();
    routes.insert(
        "/api/status".to_string(),
        Route::ok(r#"{"success":true,"data":{"quota_per_unit":500000}}"#),
    );
    routes.insert(
        "/v1/models".to_string(),
        Route::ok(r#"{"success":true,"data":[{"id":"gpt-4"}]}"#),
    );
    routes.insert(
        "/api/usage/token/".to_string(),
        Route::ok(r#"{"code":true,"data":{"unlimited_quota":true}}"#),
    );
    routes.insert(
        "/api/pricing".to_string(),
        Route::ok(
            r#"{"success":true,"data":[{"model_name":"gpt-4","quota_type":0,"model_ratio":2.5}],"group_ratio":{"default":2}}"#,
        ),
    );
    let (base, client, _) = spawn_mock(routes).await;
    let group = group_with(Some("default"), &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: &base,
            user_credential: None,
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;
    let price = snapshot.prices.iter().find(|p| p.model == "gpt-4").unwrap();
    assert_eq!(
        price.unavailable_reason.as_deref(),
        Some(UNAVAIL_MISSING_RATE)
    );
    assert!(price.input.is_none());
    assert!(price.output.is_none());
}

#[tokio::test]
async fn trailing_v1_base_is_stripped_once() {
    let mut routes = HashMap::new();
    routes.insert(
        "/api/status".to_string(),
        Route::ok(r#"{"success":true,"data":{"quota_per_unit":500000}}"#),
    );
    routes.insert(
        "/v1/models".to_string(),
        Route::ok(r#"{"success":true,"data":[{"id":"gpt-4"}]}"#),
    );
    routes.insert(
        "/api/usage/token/".to_string(),
        Route::ok(r#"{"code":true,"data":{"unlimited_quota":true}}"#),
    );
    routes.insert(
        "/api/pricing".to_string(),
        Route::ok(r#"{"success":true,"data":[],"group_ratio":{"default":1}}"#),
    );
    let (origin, client, captured) = spawn_mock(routes).await;
    let base = format!("{origin}/v1");
    let group = group_with(None, &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: &base,
            user_credential: None,
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;
    assert_eq!(
        snapshot
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gpt-4"]
    );
    let paths: Vec<_> = captured
        .lock()
        .unwrap()
        .iter()
        .map(|h| h.path.clone())
        .collect();
    assert!(paths.contains(&"/api/status".to_string()));
    assert!(paths.contains(&"/v1/models".to_string()));
    assert!(
        !paths
            .iter()
            .any(|path| path.contains("/v1/api/") || path.contains("/v1/v1/"))
    );
}

#[tokio::test]
async fn reflected_secret_is_stripped_from_snapshot() {
    let mut routes = HashMap::new();
    routes.insert(
        "/api/status".to_string(),
        Route::ok(r#"{"success":true,"data":{"quota_per_unit":500000}}"#),
    );
    routes.insert(
        "/v1/models".to_string(),
        Route::ok(format!(
            r#"{{"success":true,"data":[{{"id":"{KEY}"}},{{"id":"gpt-4"}}]}}"#
        )),
    );
    routes.insert(
        "/api/usage/token/".to_string(),
        Route::ok(format!(
            r#"{{"code":true,"data":{{"name":"{KEY}","total_used":1,"total_available":2,"total_granted":3,"unlimited_quota":false}}}}"#
        )),
    );
    routes.insert(
        "/api/pricing".to_string(),
        Route::ok(r#"{"success":true,"data":[],"group_ratio":{"default":1}}"#),
    );
    let (base, client, _) = spawn_mock(routes).await;
    let group = group_with(None, &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: &base,
            user_credential: None,
            key: Some(KEY),
            group: &group,
            now: now(),
        },
    )
    .await;
    let blob = serde_json::to_string(&snapshot).unwrap();
    assert!(!blob.contains(KEY));
    assert!(snapshot.models.iter().all(|m| m.id != KEY));
    assert!(
        snapshot
            .errors
            .iter()
            .any(|e| e == "snapshot.secret_reflected")
    );
}

#[tokio::test]
async fn incompatible_envelope_is_parse_error_not_empty_success() {
    let mut routes = HashMap::new();
    routes.insert(
        "/api/status".to_string(),
        Route::ok(r#"{"ok":true,"quota_per_unit":500000}"#),
    );
    routes.insert(
        "/api/user/self".to_string(),
        Route::ok(r#"{"ok":true,"quota":9}"#),
    );
    routes.insert(
        "/api/user/self/groups".to_string(),
        Route::ok(r#"{"success":true,"data":{"default":{"ratio":1}}}"#),
    );
    routes.insert(
        "/api/subscription/self".to_string(),
        Route::ok(r#"{"success":true,"data":{"subscriptions":[]}}"#),
    );
    routes.insert(
        "/api/token/auto-groups".to_string(),
        Route::ok(r#"{"success":true,"data":{"groups":[]}}"#),
    );
    routes.insert(
        "/api/pricing".to_string(),
        Route::ok(r#"{"success":true,"data":[],"group_ratio":{}}"#),
    );
    let (base, client, _) = spawn_mock(routes).await;
    let group = group_with(None, &[]);
    let snapshot = read(
        &client,
        &PlatformReadRequest {
            kind: PlatformKind::NewApi,
            base_url: &base,
            user_credential: Some(USER),
            key: None,
            group: &group,
            now: now(),
        },
    )
    .await;
    assert!(
        snapshot
            .errors
            .iter()
            .any(|e| e == "new_api.user_self.parse")
    );
    assert!(
        snapshot
            .quotas
            .iter()
            .all(|q| !matches!(q.kind, PlatformQuotaKind::Wallet)),
        "incompatible self envelope must not become a wallet observation"
    );
}
