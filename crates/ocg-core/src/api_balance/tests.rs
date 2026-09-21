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
    assert!(probe_from_endpoint("https://api.stepfun.com/v1/chat/completions").is_some());
    assert!(probe_from_endpoint("https://api.stepfun.com/v1/accounts").is_some());
    assert!(probe_from_endpoint("https://api.stepfun.com:443/v1/chat/completions").is_some());
    assert!(probe_from_endpoint("https://evil.api.deepseek.com/chat/completions").is_none());
    assert!(
        probe_from_endpoint("https://api.deepseek.com.evil.example/chat/completions").is_none()
    );
    assert!(probe_from_endpoint("https://api.openai.com/v1/chat/completions").is_none());
    assert!(probe_from_endpoint("https://api.stepfun.ai/v1/chat/completions").is_none());
    assert!(probe_from_endpoint("https://evil.api.stepfun.com/v1/chat/completions").is_none());
    assert!(
        probe_from_endpoint("https://api.stepfun.com.evil.example/v1/chat/completions").is_none()
    );
    assert!(probe_from_endpoint("http://api.stepfun.com/v1/chat/completions").is_none());
    assert!(probe_from_endpoint("https://api.stepfun.com:444/v1/chat/completions").is_none());
    assert!(probe_from_endpoint("https://127.0.0.1/chat/completions").is_none());
    assert!(probe_from_endpoint("not a url").is_none());
}

#[test]
fn step_plan_paths_are_excluded_only_on_stepfun_host() {
    assert!(probe_from_endpoint("https://api.stepfun.com/step_plan").is_none());
    assert!(probe_from_endpoint("https://api.stepfun.com/step_plan/").is_none());
    assert!(probe_from_endpoint("https://api.stepfun.com/step_plan/v1/chat/completions").is_none());
    assert!(probe_from_endpoint("https://api.stepfun.com/step_plan/v1/accounts").is_none());
    assert!(probe_from_endpoint("https://api.stepfun.com/step_planning").is_some());
    assert!(
        probe_from_endpoint("https://api.deepseek.com/step_plan/v1/chat/completions").is_some()
    );
    assert!(probe_from_endpoint("https://api.moonshot.cn/step_plan/v1/chat/completions").is_some());
    assert!(probe_from_endpoint("https://api.deepseek.com/chat/completions").is_some());
    assert!(probe_from_endpoint("https://api.moonshot.cn/v1/chat/completions").is_some());
}

#[test]
fn official_balance_sources_are_the_known_pair() {
    assert!(is_official_balance_source(DEEPSEEK_BALANCE_SOURCE));
    assert!(is_official_balance_source(MOONSHOT_BALANCE_SOURCE));
    assert!(is_official_balance_source(STEPFUN_BALANCE_SOURCE));
    assert!(!is_official_balance_source("test-fixture"));
    assert!(!is_official_balance_source("minimax-cn-official"));
    assert!(!is_official_balance_source("stepfun-plan-official"));
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
fn stepfun_balance_is_the_current_amount_without_summing_cash_or_voucher() {
    let now = Utc::now();
    let rows = parse_stepfun(
        "acc",
        STEPFUN_BALANCE_SOURCE,
        "cny",
        &json!({
            "object": "account",
            "type": "prepaid",
            "balance": 12.5,
            "total_cash_balance": 10.0,
            "total_voucher_balance": 26.0
        }),
        now,
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].amount, 12.5);
    assert_eq!(rows[0].unit, "cny");
    assert_eq!(rows[0].balance_kind, "balance");
    assert_eq!(rows[0].source, STEPFUN_BALANCE_SOURCE);
}

#[test]
fn stepfun_zero_and_negative_balance_are_valid() {
    let now = Utc::now();
    let zero = parse_stepfun(
        "acc",
        STEPFUN_BALANCE_SOURCE,
        "cny",
        &json!({"balance": 0.0}),
        now,
    )
    .unwrap();
    assert_eq!(zero[0].amount, 0.0);
    let negative = parse_stepfun(
        "acc",
        STEPFUN_BALANCE_SOURCE,
        "cny",
        &json!({"balance": "-1.25"}),
        now,
    )
    .unwrap();
    assert_eq!(negative[0].amount, -1.25);
}

#[test]
fn stepfun_missing_or_malformed_balance_fails() {
    let now = Utc::now();
    assert!(
        parse_stepfun(
            "acc",
            STEPFUN_BALANCE_SOURCE,
            "cny",
            &json!({
                "total_cash_balance": 4.0,
                "total_voucher_balance": 6.0
            }),
            now,
        )
        .is_err()
    );
    assert!(
        parse_stepfun(
            "acc",
            STEPFUN_BALANCE_SOURCE,
            "cny",
            &json!({"balance": "not-a-number"}),
            now,
        )
        .is_err()
    );
    assert!(
        parse_stepfun(
            "acc",
            STEPFUN_BALANCE_SOURCE,
            "cny",
            &json!({"balance": null}),
            now,
        )
        .is_err()
    );
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

#[tokio::test]
async fn fetch_probe_sends_bearer_get_and_parses_stepfun() {
    let body = r#"{"object":"account","type":"prepaid","balance":0.00,"total_cash_balance":1.00,"total_voucher_balance":26.00}"#;
    let (origin, calls) = start_origin(StatusCode::OK, body).await;
    let url = reqwest::Url::parse(&format!("{origin}/v1/accounts")).unwrap();
    let rows = fetch_probe(
        &config(),
        "acc",
        "sk-test",
        BalanceProbe {
            url,
            source: STEPFUN_BALANCE_SOURCE,
            kind: BalanceKind::StepFun,
            unit_hint: "cny",
        },
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].amount, 0.0);
    assert_eq!(rows[0].balance_kind, "balance");
    assert_eq!(rows[0].unit, "cny");
    let hits = calls.lock().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].method, "GET");
    assert_eq!(hits[0].path, "/v1/accounts");
    assert_eq!(hits[0].authorization.as_deref(), Some("Bearer sk-test"));
}
