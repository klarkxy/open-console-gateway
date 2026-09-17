use super::*;
use chrono::Duration as ChronoDuration;
use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TEST_KEY: &str = "user-test-command-code-key-do-not-echo";

fn fixed_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-08T07:52:43Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn usage_body(now: DateTime<Utc>) -> String {
    serde_json::json!({
        "credits": {
            "monthlyCredits": 69.99995534,
            "purchasedCredits": 0,
            "freeCredits": 0
        },
        "windowLimits": {
            "limited": true,
            "fiveHour": {
                "used": 0.00004466,
                "cap": 14,
                "resetAt": (now + ChronoDuration::hours(5)).timestamp_millis()
            },
            "weekly": {
                "used": 0.00004466,
                "cap": 35,
                "resetAt": (now + ChronoDuration::days(7)).timestamp_millis()
            }
        }
    })
    .to_string()
}

#[test]
fn parses_official_goat_windows_and_monthly_remaining_credit() {
    let now = fixed_now();
    let snapshot = parse_command_code_usage_body(usage_body(now).as_bytes(), now).unwrap();
    assert!((snapshot.rolling_percent - 0.000319).abs() < 0.000001);
    assert!((snapshot.weekly_percent - 0.0001276).abs() < 0.000001);
    assert!((snapshot.monthly_percent - 0.0000638).abs() < 0.000001);
    assert_eq!(snapshot.rolling_resets_in_minutes, 300);
    assert_eq!(snapshot.weekly_resets_in_minutes, 10_080);
    assert_eq!(snapshot.earliest_resets_in_minutes, 300);
}

#[test]
fn rejects_non_goat_or_malformed_usage_shapes() {
    let now = fixed_now();
    let provider = serde_json::json!({
        "credits": {"monthlyCredits": 15},
        "windowLimits": {"limited": false}
    });
    assert_eq!(
        parse_command_code_usage_body(provider.to_string().as_bytes(), now),
        Err(CommandCodeUsageError::Plan)
    );

    let mut wrong_cap: Value = serde_json::from_str(&usage_body(now)).unwrap();
    wrong_cap["windowLimits"]["weekly"]["cap"] = Value::from(40);
    assert_eq!(
        parse_command_code_usage_body(wrong_cap.to_string().as_bytes(), now),
        Err(CommandCodeUsageError::Plan)
    );

    let mut far_reset: Value = serde_json::from_str(&usage_body(now)).unwrap();
    far_reset["windowLimits"]["fiveHour"]["resetAt"] =
        Value::from((now + ChronoDuration::hours(6)).timestamp_millis());
    assert_eq!(
        parse_command_code_usage_body(far_reset.to_string().as_bytes(), now),
        Err(CommandCodeUsageError::Window)
    );
}

#[derive(Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    authorization: Option<String>,
    accept: Option<String>,
}

async fn serve_once(
    status: u16,
    reason: &str,
    body: String,
) -> (String, tokio::task::JoinHandle<CapturedRequest>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let reason = reason.to_string();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let head = read_http_head(&mut stream).await;
        let text = String::from_utf8_lossy(&head);
        let first = text.lines().next().unwrap_or_default();
        let mut parts = first.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let path = parts.next().unwrap_or_default().to_string();
        let authorization = header_value(&text, "authorization");
        let accept = header_value(&text, "accept");
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        CapturedRequest {
            method,
            path,
            authorization,
            accept,
        }
    });
    (endpoint_url(addr), task)
}

fn endpoint_url(addr: SocketAddr) -> String {
    format!("http://{addr}/alpha/billing/credits")
}

async fn read_http_head(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    buf
}

fn header_value(head: &str, expected: &str) -> Option<String> {
    head.lines().find_map(|line| {
        let (name, value) = line.trim_end_matches('\r').split_once(':')?;
        name.eq_ignore_ascii_case(expected)
            .then(|| value.trim().to_string())
    })
}

#[tokio::test]
async fn http_fetch_uses_fixed_shape_bearer_auth_and_no_key_in_errors() {
    let now = Utc::now();
    let (url, request) = serve_once(200, "OK", usage_body(now)).await;
    let snapshot = fetch_command_code_usage_from(&AppConfig::default(), TEST_KEY, &url, Utc::now)
        .await
        .unwrap();
    assert!(snapshot.weekly_percent >= 0.0);
    let request = request.await.unwrap();
    assert_eq!(request.method, "GET");
    assert_eq!(request.path, "/alpha/billing/credits");
    assert_eq!(
        request.authorization.as_deref(),
        Some("Bearer user-test-command-code-key-do-not-echo")
    );
    assert_eq!(request.accept.as_deref(), Some("application/json"));

    let (url, _) = serve_once(401, "Unauthorized", "{}".to_string()).await;
    let error = fetch_command_code_usage_from(&AppConfig::default(), TEST_KEY, &url, Utc::now)
        .await
        .unwrap_err();
    assert_eq!(error, CommandCodeUsageError::Unauthorized);
    assert!(!error.to_string().contains(TEST_KEY));
    assert!(!format!("{error:?}").contains(TEST_KEY));
}

#[test]
fn production_endpoint_is_fixed_to_command_code_account_usage() {
    assert_eq!(
        COMMAND_CODE_GOAT_USAGE_URL,
        "https://api.commandcode.ai/alpha/billing/credits"
    );
}
