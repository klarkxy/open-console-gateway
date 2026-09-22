use super::*;

#[test]
fn parses_minimax_remaining_counts_as_remaining_not_used() {
    let value = serde_json::json!({"model_remains":[{
        "model_name":"MiniMax-M3","current_interval_total_count":100,
        "current_interval_usage_count":96,"current_interval_status":1,"remains_time":60000,
        "current_weekly_total_count":200,"current_weekly_usage_count":150,
        "current_weekly_status":1,"weekly_remains_time":120000
    }]});
    let rows = parse_minimax("a", &value, Utc::now()).unwrap();
    assert_eq!(rows[0].used, 4.0);
    assert_eq!(rows[1].used, 50.0);
}

#[test]
fn preserves_minimax_window_duration_and_boosted_weekly_percent() {
    let base = 1_800_000_000_000_i64;
    let models = [(2_i64, 1500_i64), (6, 2000), (12, 3000)]
        .into_iter()
        .map(|(hours, boost)| {
            serde_json::json!({
                "model_name": format!("model-{hours}h"),
                "start_time": base,
                "end_time": base + hours * 60 * 60 * 1000,
                "current_interval_total_count": 100,
                "current_interval_usage_count": 100,
                "current_interval_status": 1,
                "current_weekly_total_count": 0,
                "current_weekly_usage_count": 0,
                "current_weekly_remaining_percent": 100,
                "weekly_boost_permille": boost,
                "current_weekly_status": 1
            })
        })
        .collect::<Vec<_>>();
    let rows = parse_minimax(
        "a",
        &serde_json::json!({"model_remains": models}),
        Utc::now(),
    )
    .unwrap();
    for (index, hours) in [2_i64, 6, 12].into_iter().enumerate() {
        let current = &rows[index * 2];
        assert_eq!(
            current.resets_at.unwrap() - current.started_at.unwrap(),
            ChronoDuration::hours(hours)
        );
    }
    assert_eq!(rows[1].limit_value, Some(150.0));
    assert_eq!(rows[1].used, 0.0);
    assert_eq!(rows[3].limit_value, Some(200.0));
    assert_eq!(rows[3].used, 0.0);
    assert_eq!(rows[5].limit_value, Some(200.0));
    assert_eq!(rows[5].used, 0.0);
}

#[test]
fn parses_kimi_summary_and_limits() {
    let value = serde_json::json!({
        "usage":{"limit":100,"used":4},
        "limits":[{
            "window":{"duration":300,"timeUnit":"MINUTE"},
            "detail":{"limit":10,"remaining":7}
        }]
    });
    let rows = parse_kimi("a", &value, Utc::now()).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].used, 4.0);
    assert_eq!(rows[1].used, 3.0);
    assert_eq!(rows[1].window_kind, "kimi_5h");
}

#[test]
fn kimi_minute_windows_use_the_generic_hourly_kind() {
    let value = serde_json::json!({
        "limits":[
            {"window":{"duration":300,"timeUnit":"MINUTE"},"detail":{"limit":10,"remaining":7}},
            {"window":{"duration":60,"timeUnit":"MINUTE"},"detail":{"limit":10,"remaining":9}},
            {"window":{"duration":90,"timeUnit":"MINUTE"},"detail":{"limit":10,"remaining":8}}
        ]
    });
    let rows = parse_kimi("a", &value, Utc::now()).unwrap();
    assert_eq!(rows[0].window_kind, "kimi_5h");
    assert_eq!(rows[1].window_kind, "kimi_1h");
    assert_eq!(rows[2].window_kind, "kimi_limit_3");
}

#[test]
fn official_usage_urls_are_the_fixed_production_constants() {
    assert_eq!(
        official_usage_target(ProviderAdapterKind::MiniMaxCn).unwrap(),
        (MINIMAX_CN_USAGE_URL, "MiniMax CN")
    );
    assert_eq!(
        official_usage_target(ProviderAdapterKind::KimiCn).unwrap(),
        (KIMI_CN_USAGE_URL, "Kimi Code CN")
    );
    assert!(official_usage_target(ProviderAdapterKind::OpenCodeGo).is_err());
}

#[derive(Clone)]
struct CapturedUsageCall {
    method: String,
    path: String,
    authorization: Option<String>,
    accept: Option<String>,
}

async fn start_usage_origin(
    status: axum::http::StatusCode,
    body: &'static str,
    path: &str,
) -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<CapturedUsageCall>>>,
) {
    use axum::Router;
    use axum::extract::OriginalUri;
    use axum::http::{HeaderMap, Method};
    use axum::routing::any;
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls_for_handler = calls.clone();
    let app = Router::new().fallback(any(
        move |method: Method, uri: OriginalUri, headers: HeaderMap| {
            let calls = calls_for_handler.clone();
            async move {
                calls.lock().unwrap().push(CapturedUsageCall {
                    method: method.to_string(),
                    path: uri.path().to_string(),
                    authorization: headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string),
                    accept: headers
                        .get(axum::http::header::ACCEPT)
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
    (format!("http://{addr}{path}"), calls)
}

#[tokio::test]
async fn minimax_usage_http_sends_bearer_get_and_parses() {
    let (url, calls) = start_usage_origin(
        axum::http::StatusCode::OK,
        r#"{"model_remains":[{"model_name":"MiniMax-M3","current_interval_total_count":100,"current_interval_usage_count":96,"current_interval_status":1,"current_weekly_total_count":200,"current_weekly_usage_count":150,"current_weekly_status":1}]}"#,
        "/v1/token_plan/remains",
    )
    .await;
    let rows = fetch_from_url(
        &AppConfig::default(),
        ProviderAdapterKind::MiniMaxCn,
        "acct-1",
        "sk-minimax",
        &url,
    )
    .await
    .unwrap();
    assert_eq!(rows[0].used, 4.0);
    assert_eq!(rows[0].source, MINIMAX_USAGE_SOURCE);
    let captured = calls.lock().unwrap();
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].method, "GET");
    assert_eq!(captured[0].path, "/v1/token_plan/remains");
    assert_eq!(
        captured[0].authorization.as_deref(),
        Some("Bearer sk-minimax")
    );
    assert_eq!(captured[0].accept.as_deref(), Some("application/json"));
}

#[tokio::test]
async fn kimi_usage_http_sends_bearer_get_and_parses() {
    let (url, calls) = start_usage_origin(
        axum::http::StatusCode::OK,
        r#"{"usage":{"limit":100,"used":4},"limits":[]}"#,
        "/coding/v1/usages",
    )
    .await;
    let rows = fetch_from_url(
        &AppConfig::default(),
        ProviderAdapterKind::KimiCn,
        "acct-1",
        "sk-kimi",
        &url,
    )
    .await
    .unwrap();
    assert_eq!(rows[0].used, 4.0);
    assert_eq!(rows[0].source, KIMI_USAGE_SOURCE);
    let captured = calls.lock().unwrap();
    assert_eq!(captured[0].method, "GET");
    assert_eq!(captured[0].path, "/coding/v1/usages");
    assert_eq!(captured[0].authorization.as_deref(), Some("Bearer sk-kimi"));
    assert_eq!(captured[0].accept.as_deref(), Some("application/json"));
}

#[tokio::test]
async fn usage_http_non_2xx_fails_without_parsing() {
    let (url, _) = start_usage_origin(
        axum::http::StatusCode::BAD_GATEWAY,
        r#"{"error":"down"}"#,
        "/v1/token_plan/remains",
    )
    .await;
    let error = fetch_from_url(
        &AppConfig::default(),
        ProviderAdapterKind::MiniMaxCn,
        "acct-1",
        "sk-minimax",
        &url,
    )
    .await
    .unwrap_err();
    assert!(error.contains("502"), "{error}");
}

#[tokio::test]
async fn usage_refresh_requires_a_stored_key() {
    let error = fetch_from_url(
        &AppConfig::default(),
        ProviderAdapterKind::MiniMaxCn,
        "acct-1",
        "  ",
        "http://127.0.0.1:1/v1/token_plan/remains",
    )
    .await
    .unwrap_err();
    assert!(error.contains("requires a stored Key"), "{error}");
}

#[tokio::test]
async fn usage_http_oversize_body_fails_without_parsing() {
    use axum::Router;
    use axum::routing::any;
    let body = vec![b'x'; MAX_BODY_BYTES + 1];
    let app = Router::new().fallback(any(move || {
        let body = body.clone();
        async move { (axum::http::StatusCode::OK, body) }
    }));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let url = format!("http://{addr}/v1/token_plan/remains");
    let error = fetch_from_url(
        &AppConfig::default(),
        ProviderAdapterKind::MiniMaxCn,
        "acct-1",
        "sk-minimax",
        &url,
    )
    .await
    .unwrap_err();
    assert!(error.contains(&MAX_BODY_BYTES.to_string()), "{error}");
    assert!(error.contains("exceeded"), "{error}");
    assert!(!error.contains("sk-minimax"), "{error}");
}

#[tokio::test]
async fn usage_http_does_not_follow_redirects() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let success = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let success_addr = success.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = success.accept().await {
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf).await;
            let body = r#"{"model_remains":[{"model_name":"MiniMax-M3","current_interval_total_count":100,"current_interval_usage_count":96,"current_interval_status":1,"current_weekly_total_count":200,"current_weekly_usage_count":150,"current_weekly_status":1}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = [0_u8; 1024];
            let _ = stream.read(&mut buf).await;
            let location = format!("http://{success_addr}/v1/token_plan/remains");
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let error = fetch_from_url(
        &AppConfig::default(),
        ProviderAdapterKind::MiniMaxCn,
        "acct-1",
        "sk-minimax",
        &format!("http://{addr}/v1/token_plan/remains"),
    )
    .await
    .unwrap_err();
    assert!(error.contains("302"), "{error}");
}
