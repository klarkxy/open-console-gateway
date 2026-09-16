use super::*;
use crate::models::{AppConfig, ProxyListDirection, ProxyMode};
use axum::Router;
use axum::extract::OriginalUri;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::any;
use serde_json::json;
use std::sync::{Arc, Mutex};

fn config() -> AppConfig {
    AppConfig {
        proxy_mode: ProxyMode::Direct,
        proxy_list_direction: ProxyListDirection::Whitelist,
        connect_timeout_secs: 5,
        non_stream_timeout_secs: 5,
        ..AppConfig::default()
    }
}

#[test]
fn only_exact_official_hosts_are_balance_capable() {
    assert!(probe_from_endpoint("https://api.deepseek.com/chat/completions").is_some());
    assert!(probe_from_endpoint("https://api.deepseek.com/v1/chat/completions").is_some());
    assert!(probe_from_endpoint("https://api.moonshot.cn/v1/chat/completions").is_some());
    assert!(probe_from_endpoint("https://api.moonshot.ai/v1/chat/completions").is_some());
    assert!(probe_from_endpoint("https://evil.api.deepseek.com/chat/completions").is_none());
    assert!(
        probe_from_endpoint("https://api.deepseek.com.evil.example/chat/completions").is_none()
    );
    assert!(probe_from_endpoint("https://api.openai.com/v1/chat/completions").is_none());
    assert!(probe_from_endpoint("https://127.0.0.1/chat/completions").is_none());
    assert!(probe_from_endpoint("not a url").is_none());
}

#[test]
fn official_balance_sources_are_the_known_pair() {
    assert!(is_official_balance_source(DEEPSEEK_BALANCE_SOURCE));
    assert!(is_official_balance_source(MOONSHOT_BALANCE_SOURCE));
    assert!(!is_official_balance_source("test-fixture"));
    assert!(!is_official_balance_source("minimax-cn-official"));
}

#[test]
fn deepseek_total_balance_is_the_current_amount() {
    let now = Utc::now();
    let rows = parse_deepseek(
        "acc",
        DEEPSEEK_BALANCE_SOURCE,
        &json!({
            "is_available": true,
            "balance_infos": [
                {
                    "currency": "CNY",
                    "total_balance": "9.50",
                    "granted_balance": "0.00",
                    "topped_up_balance": "9.50"
                }
            ]
        }),
        now,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].amount, 9.5);
    assert_eq!(rows[0].unit, "cny");
    assert_eq!(rows[0].balance_kind, "available:CNY");
    assert_eq!(rows[0].source, DEEPSEEK_BALANCE_SOURCE);
}

#[test]
fn moonshot_available_balance_is_the_current_amount() {
    let now = Utc::now();
    let rows = parse_moonshot(
        "acc",
        MOONSHOT_BALANCE_SOURCE,
        "usd",
        &json!({
            "code": 0,
            "data": {
                "available_balance": 49.58894,
                "voucher_balance": 46.58893,
                "cash_balance": 3.00001
            },
            "status": true
        }),
        now,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].amount, 49.58894);
    assert_eq!(rows[0].unit, "usd");
    assert_eq!(rows[0].balance_kind, "available");
}

#[test]
fn moonshot_non_zero_code_is_not_a_balance() {
    let error = parse_moonshot(
        "acc",
        MOONSHOT_BALANCE_SOURCE,
        "cny",
        &json!({"code": 401, "data": {"available_balance": 1.0}}),
        Utc::now(),
    )
    .unwrap_err();
    assert!(error.contains("not successful"));
}

#[derive(Clone)]
struct CapturedCall {
    method: String,
    path: String,
    authorization: Option<String>,
}

async fn start_origin(
    status: StatusCode,
    body: &'static str,
) -> (String, Arc<Mutex<Vec<CapturedCall>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let app = Router::new().fallback(any(
        move |method: Method, uri: OriginalUri, headers: HeaderMap| {
            let calls = calls_for_handler.clone();
            async move {
                calls.lock().unwrap().push(CapturedCall {
                    method: method.to_string(),
                    path: uri.path().to_string(),
                    authorization: headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string),
                });
                (status, body)
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), calls)
}

#[tokio::test]
async fn fetch_probe_sends_bearer_get_and_parses_deepseek() {
    let body =
        r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"12.00"}]}"#;
    let (origin, calls) = start_origin(StatusCode::OK, body).await;
    let url = reqwest::Url::parse(&format!("{origin}/user/balance")).unwrap();
    let rows = fetch_probe(
        &config(),
        "acc",
        "sk-test",
        BalanceProbe {
            url,
            source: DEEPSEEK_BALANCE_SOURCE,
            kind: BalanceKind::DeepSeek,
            unit_hint: "cny",
        },
    )
    .await
    .unwrap();
    assert_eq!(rows[0].amount, 12.0);
    let hits = calls.lock().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].method, "GET");
    assert_eq!(hits[0].path, "/user/balance");
    assert_eq!(hits[0].authorization.as_deref(), Some("Bearer sk-test"));
}
