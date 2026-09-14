use super::*;
use chrono::Duration as ChronoDuration;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TEST_BEARER: &str = "sk-test-bearer-do-not-echo-12345";

fn fixed_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-08-18T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn rfc3339(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339()
}

fn official_body(
    rolling: f64,
    weekly: f64,
    monthly: f64,
    rolling_status: &str,
    rolling_reset: &str,
    weekly_reset: &str,
    monthly_reset: &str,
) -> String {
    format!(
        r#"{{"usage":{{"rolling":{{"status":"{rolling_status}","percent":{rolling},"resetsAt":"{rolling_reset}","extra":true}},"weekly":{{"status":"ok","percent":{weekly},"resetsAt":"{weekly_reset}"}},"monthly":{{"status":"ok","percent":{monthly},"resetsAt":"{monthly_reset}"}},"newWindow":{{}}}},"ignored":true}}"#
    )
}

fn success_body_at(now: DateTime<Utc>) -> String {
    official_body(
        0.0,
        37.0,
        100.0,
        "ok",
        &rfc3339(now + ChronoDuration::minutes(180)),
        &rfc3339(now + ChronoDuration::minutes(1_440)),
        &rfc3339(now + ChronoDuration::days(20)),
    )
}

struct ServedRequest {
    authorization: Option<String>,
}

async fn serve_once(
    status: u16,
    reason: &str,
    body: &[u8],
) -> (String, tokio::task::JoinHandle<ServedRequest>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let addr = listener.local_addr().unwrap();
    let reason = reason.to_string();
    let body = body.to_vec();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let head = read_http_head(&mut stream).await;
        let authorization = authorization_header(&head);
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(&body).await.unwrap();
        ServedRequest { authorization }
    });
    (endpoint_url(addr), task)
}

async fn serve_chunked_oversize() -> (String, tokio::task::JoinHandle<ServedRequest>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let head = read_http_head(&mut stream).await;
        let authorization = authorization_header(&head);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let chunk = vec![b'x'; 16 * 1024];
        let header = format!("{:x}\r\n", chunk.len());
        for _ in 0..5 {
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(&chunk).await.unwrap();
            stream.write_all(b"\r\n").await.unwrap();
        }
        stream.write_all(b"0\r\n\r\n").await.unwrap();
        ServedRequest { authorization }
    });
    (endpoint_url(addr), task)
}

async fn serve_redirect_to_success() -> String {
    let success = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("success listener should bind");
    let success_addr = success.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = success.accept().await.unwrap();
        let _ = read_http_head(&mut stream).await;
        let body = success_body_at(Utc::now());
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("redirect listener should bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_http_head(&mut stream).await;
        let location = endpoint_url(success_addr);
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes()).await;
    });
    endpoint_url(addr)
}

fn endpoint_url(addr: SocketAddr) -> String {
    format!("http://{addr}/zen/go/v1/usage")
}

async fn read_http_head(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") || buf.len() > 16 * 1024 {
            break;
        }
    }
    buf
}

fn authorization_header(head: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(head);
    text.lines().find_map(|line| {
        let line = line.trim_end_matches('\r');
        line.split_once(':').and_then(|(name, value)| {
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_string())
        })
    })
}

fn assert_error_hides_bearer(error: &GoUsageError) {
    let display = error.to_string();
    let debug = format!("{error:?}");
    assert!(
        !display.contains(TEST_BEARER),
        "Display leaked test bearer: {display}"
    );
    assert!(
        !debug.contains(TEST_BEARER),
        "Debug leaked test bearer: {debug}"
    );
    assert!(
        !display.contains("Bearer"),
        "Display should not mention Bearer: {display}"
    );
}

#[test]
fn official_success_fixture_accepts_0_37_100_and_unknown_fields() {
    let now = fixed_now();
    let snapshot = parse_go_usage_body(success_body_at(now).as_bytes(), now).unwrap();
    assert_eq!(snapshot.rolling_status, GoUsageWindowStatus::Ok);
    assert_eq!(snapshot.weekly_status, GoUsageWindowStatus::Ok);
    assert_eq!(snapshot.monthly_status, GoUsageWindowStatus::Ok);
    assert_eq!(snapshot.rolling_percent, 0.0);
    assert_eq!(snapshot.weekly_percent, 37.0);
    assert_eq!(snapshot.monthly_percent, 100.0);
    assert_eq!(snapshot.rolling_resets_in_minutes, 180);
    assert_eq!(snapshot.weekly_resets_in_minutes, 1_440);
    assert_eq!(snapshot.earliest_resets_in_minutes, 180);
}

#[test]
fn rate_limited_window_status_is_success() {
    let now = fixed_now();
    let body = official_body(
        100.0,
        37.0,
        0.0,
        "rate-limited",
        &rfc3339(now + ChronoDuration::minutes(12)),
        &rfc3339(now + ChronoDuration::minutes(60)),
        &rfc3339(now + ChronoDuration::days(10)),
    );
    let snapshot = parse_go_usage_body(body.as_bytes(), now).unwrap();
    assert_eq!(snapshot.rolling_status, GoUsageWindowStatus::RateLimited);
    assert_eq!(snapshot.rolling_percent, 100.0);
    assert_eq!(snapshot.rolling_resets_in_minutes, 12);
}

#[test]
fn missing_window_is_schema_error() {
    let now = fixed_now();
    let body = format!(
        r#"{{"usage":{{"rolling":{{"status":"ok","percent":0,"resetsAt":"{}"}},"monthly":{{"status":"ok","percent":0,"resetsAt":"{}"}}}}}}"#,
        rfc3339(now + ChronoDuration::minutes(10)),
        rfc3339(now + ChronoDuration::days(1)),
    );
    assert_eq!(
        parse_go_usage_body(body.as_bytes(), now),
        Err(GoUsageError::Schema)
    );
}

#[test]
fn old_proposal_schema_is_rejected() {
    let now = fixed_now();
    let body = r#"{"rollingUsage":{"status":"ok","usagePercent":12,"resetInSec":3600},"weeklyUsage":{"status":"ok","usagePercent":34,"resetInSec":7200},"monthlyUsage":{"status":"ok","usagePercent":56,"resetInSec":10800}}"#;
    assert_eq!(
        parse_go_usage_body(body.as_bytes(), now),
        Err(GoUsageError::Schema)
    );

    let nested = r#"{"usage":{"rolling":{"status":"ok","usagePercent":0,"resetInSec":3600},"weekly":{"status":"ok","usagePercent":1,"resetInSec":7200},"monthly":{"status":"ok","usagePercent":2,"resetInSec":10800}}}"#;
    assert_eq!(
        parse_go_usage_body(nested.as_bytes(), now),
        Err(GoUsageError::Schema)
    );
}

#[test]
fn illegal_status_is_schema_error() {
    let now = fixed_now();
    let body = official_body(
        0.0,
        0.0,
        0.0,
        "OK",
        &rfc3339(now + ChronoDuration::minutes(10)),
        &rfc3339(now + ChronoDuration::minutes(10)),
        &rfc3339(now + ChronoDuration::days(1)),
    );
    assert_eq!(
        parse_go_usage_body(body.as_bytes(), now),
        Err(GoUsageError::Schema)
    );
}

#[test]
fn percent_nan_and_out_of_range_are_schema_errors() {
    let now = fixed_now();
    let reset = rfc3339(now + ChronoDuration::minutes(10));
    let nan = format!(
        r#"{{"usage":{{"rolling":{{"status":"ok","percent":NaN,"resetsAt":"{reset}"}},"weekly":{{"status":"ok","percent":0,"resetsAt":"{reset}"}},"monthly":{{"status":"ok","percent":0,"resetsAt":"{reset}"}}}}}}"#
    );
    assert_eq!(
        parse_go_usage_body(nan.as_bytes(), now),
        Err(GoUsageError::Schema)
    );

    let nan_string = format!(
        r#"{{"usage":{{"rolling":{{"status":"ok","percent":"NaN","resetsAt":"{reset}"}},"weekly":{{"status":"ok","percent":0,"resetsAt":"{reset}"}},"monthly":{{"status":"ok","percent":0,"resetsAt":"{reset}"}}}}}}"#
    );
    assert_eq!(
        parse_go_usage_body(nan_string.as_bytes(), now),
        Err(GoUsageError::Schema)
    );

    assert_eq!(
        parse_percent(&Value::from(f64::NAN)),
        Err(GoUsageError::Schema)
    );
    assert_eq!(parse_percent(&Value::from(-0.1)), Err(GoUsageError::Schema));
    assert_eq!(
        parse_percent(&Value::from(100.1)),
        Err(GoUsageError::Schema)
    );
    assert_eq!(parse_percent(&Value::from(0.0)), Ok(0.0));
    assert_eq!(parse_percent(&Value::from(100.0)), Ok(100.0));
}

#[test]
fn illegal_and_far_out_of_range_resets_at_are_rejected() {
    let now = fixed_now();
    let weekly = rfc3339(now + ChronoDuration::minutes(10));
    let monthly = rfc3339(now + ChronoDuration::days(1));
    let illegal = format!(
        r#"{{"usage":{{"rolling":{{"status":"ok","percent":0,"resetsAt":"tomorrow"}},"weekly":{{"status":"ok","percent":0,"resetsAt":"{weekly}"}},"monthly":{{"status":"ok","percent":0,"resetsAt":"{monthly}"}}}}}}"#
    );
    assert_eq!(
        parse_go_usage_body(illegal.as_bytes(), now),
        Err(GoUsageError::Schema)
    );

    let rolling_over = official_body(
        0.0,
        0.0,
        0.0,
        "ok",
        &rfc3339(now + ChronoDuration::minutes(302)),
        &weekly,
        &monthly,
    );
    assert_eq!(
        parse_go_usage_body(rolling_over.as_bytes(), now),
        Err(GoUsageError::Window)
    );

    let weekly_over = official_body(
        0.0,
        0.0,
        0.0,
        "ok",
        &rfc3339(now + ChronoDuration::minutes(300)),
        &rfc3339(now + ChronoDuration::minutes(10_082)),
        &monthly,
    );
    assert_eq!(
        parse_go_usage_body(weekly_over.as_bytes(), now),
        Err(GoUsageError::Window)
    );

    let past = official_body(
        0.0,
        0.0,
        0.0,
        "ok",
        &rfc3339(now - ChronoDuration::hours(1)),
        &weekly,
        &monthly,
    );
    let snapshot = parse_go_usage_body(past.as_bytes(), now).unwrap();
    assert_eq!(snapshot.rolling_resets_in_minutes, 0);
}

#[test]
fn expired_window_ceil_slack_clamps_to_max() {
    let now = fixed_now();
    let weekly = rfc3339(now + ChronoDuration::minutes(10));
    let monthly = rfc3339(now + ChronoDuration::days(1));

    let exact = official_body(
        0.0,
        0.0,
        0.0,
        "ok",
        &rfc3339(now + ChronoDuration::minutes(300)),
        &weekly,
        &monthly,
    );
    assert_eq!(
        parse_go_usage_body(exact.as_bytes(), now)
            .unwrap()
            .rolling_resets_in_minutes,
        300
    );

    let one_ms_over = official_body(
        0.0,
        0.0,
        0.0,
        "ok",
        &rfc3339(now + ChronoDuration::minutes(300) + ChronoDuration::milliseconds(1)),
        &weekly,
        &monthly,
    );
    assert_eq!(
        parse_go_usage_body(one_ms_over.as_bytes(), now)
            .unwrap()
            .rolling_resets_in_minutes,
        300
    );

    let one_second_over = official_body(
        0.0,
        0.0,
        0.0,
        "ok",
        &rfc3339(now + ChronoDuration::minutes(300) + ChronoDuration::seconds(1)),
        &weekly,
        &monthly,
    );
    assert_eq!(
        parse_go_usage_body(one_second_over.as_bytes(), now)
            .unwrap()
            .rolling_resets_in_minutes,
        300
    );

    let one_minute_over = official_body(
        0.0,
        0.0,
        0.0,
        "ok",
        &rfc3339(now + ChronoDuration::minutes(301)),
        &weekly,
        &monthly,
    );
    assert_eq!(
        parse_go_usage_body(one_minute_over.as_bytes(), now)
            .unwrap()
            .rolling_resets_in_minutes,
        300
    );

    let weekly_slack = official_body(
        0.0,
        0.0,
        0.0,
        "ok",
        &rfc3339(now + ChronoDuration::minutes(300)),
        &rfc3339(now + ChronoDuration::minutes(10_081)),
        &monthly,
    );
    assert_eq!(
        parse_go_usage_body(weekly_slack.as_bytes(), now)
            .unwrap()
            .weekly_resets_in_minutes,
        10_080
    );
}

#[test]
fn remaining_minutes_round_up() {
    let now = fixed_now();
    assert_eq!(ceil_minutes_until(now, now), 0);
    assert_eq!(
        ceil_minutes_until(now + ChronoDuration::milliseconds(1), now),
        1
    );
    assert_eq!(
        ceil_minutes_until(now + ChronoDuration::seconds(90), now),
        2
    );
    assert_eq!(
        ceil_minutes_until(now + ChronoDuration::minutes(300), now),
        300
    );
}

#[tokio::test]
async fn fetch_official_success_fixture_over_http() {
    let now = Utc::now();
    let body = success_body_at(now);
    let (url, server) = serve_once(200, "OK", body.as_bytes()).await;
    let snapshot = fetch_go_usage_from(&AppConfig::default(), TEST_BEARER, &url)
        .await
        .unwrap();
    let served = server.await.unwrap();
    assert_eq!(
        served.authorization.as_deref(),
        Some(format!("Bearer {TEST_BEARER}").as_str())
    );
    assert_eq!(snapshot.rolling_percent, 0.0);
    assert_eq!(snapshot.weekly_percent, 37.0);
    assert_eq!(snapshot.monthly_percent, 100.0);
    assert!((179..=180).contains(&snapshot.rolling_resets_in_minutes));
    assert!((1_439..=1_440).contains(&snapshot.weekly_resets_in_minutes));
}

#[tokio::test]
async fn fetch_rate_limited_window_over_http() {
    let now = Utc::now();
    let body = official_body(
        100.0,
        37.0,
        0.0,
        "rate-limited",
        &rfc3339(now + ChronoDuration::minutes(5)),
        &rfc3339(now + ChronoDuration::minutes(60)),
        &rfc3339(now + ChronoDuration::days(10)),
    );
    let (url, server) = serve_once(200, "OK", body.as_bytes()).await;
    let snapshot = fetch_go_usage_from(&AppConfig::default(), TEST_BEARER, &url)
        .await
        .unwrap();
    let _ = server.await;
    assert_eq!(snapshot.rolling_status, GoUsageWindowStatus::RateLimited);
    assert_eq!(snapshot.rolling_percent, 100.0);
}

async fn assert_http_status(status: u16, reason: &str, expected: GoUsageError) {
    let (url, server) = serve_once(status, reason, br#"{"error":"no"}"#).await;
    let error = fetch_go_usage_from(&AppConfig::default(), TEST_BEARER, &url)
        .await
        .expect_err("HTTP error should fail");
    let served = server.await.unwrap();
    assert_eq!(
        served.authorization.as_deref(),
        Some(format!("Bearer {TEST_BEARER}").as_str())
    );
    assert_eq!(error, expected);
    assert_error_hides_bearer(&error);
}

#[tokio::test]
async fn fetch_maps_401_403_429_and_5xx() {
    assert_http_status(401, "Unauthorized", GoUsageError::Unauthorized).await;
    assert_http_status(403, "Forbidden", GoUsageError::Forbidden).await;
    assert_http_status(429, "Too Many Requests", GoUsageError::RateLimited).await;
    assert_http_status(500, "Internal Server Error", GoUsageError::Http(500)).await;
    assert_http_status(503, "Service Unavailable", GoUsageError::Http(503)).await;
}

#[tokio::test]
async fn fetch_does_not_follow_redirects() {
    let url = serve_redirect_to_success().await;
    let error = fetch_go_usage_from(&AppConfig::default(), TEST_BEARER, &url)
        .await
        .expect_err("redirect must not be followed");
    assert_eq!(error, GoUsageError::Http(302));
    assert_error_hides_bearer(&error);
}

#[tokio::test]
async fn fetch_rejects_chunked_oversize_without_trusting_content_length() {
    let (url, server) = serve_chunked_oversize().await;
    let error = fetch_go_usage_from(&AppConfig::default(), TEST_BEARER, &url)
        .await
        .expect_err("oversize chunked body must fail");
    let served = server.await.unwrap();
    assert_eq!(
        served.authorization.as_deref(),
        Some(format!("Bearer {TEST_BEARER}").as_str())
    );
    assert_eq!(error, GoUsageError::Oversize);
    assert_error_hides_bearer(&error);
}

#[test]
fn error_display_never_contains_a_test_bearer() {
    for error in [
        GoUsageError::Unauthorized,
        GoUsageError::Forbidden,
        GoUsageError::RateLimited,
        GoUsageError::Http(502),
        GoUsageError::Timeout,
        GoUsageError::Network,
        GoUsageError::Oversize,
        GoUsageError::Schema,
        GoUsageError::Window,
    ] {
        assert_error_hides_bearer(&error);
    }
}
