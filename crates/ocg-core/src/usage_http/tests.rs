use super::*;
use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn fixed_now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 18, 12, 0, 0).unwrap()
}

fn test_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
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

async fn serve_once(status: u16, reason: &str, headers: &str, body: &[u8]) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let addr = listener.local_addr().unwrap();
    let reason = reason.to_string();
    let headers = headers.to_string();
    let body = body.to_vec();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_http_head(&mut stream).await;
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(&body).await;
    });
    endpoint_url(addr)
}

async fn serve_chunked(chunks: Vec<Vec<u8>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test listener should bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let _ = read_http_head(&mut stream).await;
        let _ = stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await;
        for chunk in chunks {
            let header = format!("{:x}\r\n", chunk.len());
            let _ = stream.write_all(header.as_bytes()).await;
            let _ = stream.write_all(&chunk).await;
            let _ = stream.write_all(b"\r\n").await;
        }
        let _ = stream.write_all(b"0\r\n\r\n").await;
    });
    endpoint_url(addr)
}

fn endpoint_url(addr: SocketAddr) -> String {
    format!("http://{addr}/usage")
}

async fn get(url: &str) -> reqwest::Response {
    test_client().get(url).send().await.unwrap()
}

#[test]
fn ceil_minutes_until_expired_exact_and_sub_minute() {
    let now = fixed_now();
    assert_eq!(ceil_minutes_until(now, now), 0);
    assert_eq!(ceil_minutes_until(now - ChronoDuration::hours(1), now), 0);
    assert_eq!(
        ceil_minutes_until(now + ChronoDuration::milliseconds(1), now),
        1
    );
    assert_eq!(
        ceil_minutes_until(now + ChronoDuration::milliseconds(59_999), now),
        1
    );
    assert_eq!(
        ceil_minutes_until(now + ChronoDuration::milliseconds(60_000), now),
        1
    );
    assert_eq!(
        ceil_minutes_until(now + ChronoDuration::milliseconds(60_001), now),
        2
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

#[test]
fn bounded_resets_in_minutes_clamps_max_plus_one_only() {
    let now = fixed_now();
    let max = 300;
    assert_eq!(
        bounded_resets_in_minutes(now - ChronoDuration::minutes(5), now, max),
        Ok(0)
    );
    assert_eq!(
        bounded_resets_in_minutes(now + ChronoDuration::minutes(max), now, max),
        Ok(max)
    );
    assert_eq!(
        bounded_resets_in_minutes(
            now + ChronoDuration::minutes(max) + ChronoDuration::milliseconds(1),
            now,
            max
        ),
        Ok(max)
    );
    assert_eq!(
        bounded_resets_in_minutes(
            now + ChronoDuration::minutes(max) + ChronoDuration::seconds(1),
            now,
            max
        ),
        Ok(max)
    );
    assert_eq!(
        bounded_resets_in_minutes(now + ChronoDuration::minutes(max + 1), now, max),
        Ok(max)
    );
    assert_eq!(
        bounded_resets_in_minutes(now + ChronoDuration::minutes(max + 2), now, max),
        Err(WindowOutOfRange)
    );
}

#[test]
fn classify_http_status_maps_401_403_429_and_leaves_other_codes() {
    assert_eq!(
        classify_http_status(StatusCode::UNAUTHORIZED),
        UsageHttpError::Unauthorized
    );
    assert_eq!(
        classify_http_status(StatusCode::FORBIDDEN),
        UsageHttpError::Forbidden
    );
    assert_eq!(
        classify_http_status(StatusCode::TOO_MANY_REQUESTS),
        UsageHttpError::RateLimited
    );
    assert_eq!(
        classify_http_status(StatusCode::OK),
        UsageHttpError::Http(200)
    );
    assert_eq!(
        classify_http_status(StatusCode::CREATED),
        UsageHttpError::Http(201)
    );
    assert_eq!(
        classify_http_status(StatusCode::FOUND),
        UsageHttpError::Http(302)
    );
    assert_eq!(
        classify_http_status(StatusCode::INTERNAL_SERVER_ERROR),
        UsageHttpError::Http(500)
    );
}

#[tokio::test]
async fn classify_transport_timeout_vs_refused() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hanging = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    let timeout_client = reqwest::Client::builder()
        .timeout(Duration::from_millis(150))
        .build()
        .unwrap();
    let timeout = timeout_client
        .get(format!("http://{hanging}/"))
        .send()
        .await
        .expect_err("hanging accept must time out");
    assert_eq!(classify_transport(&timeout), UsageHttpError::Timeout);

    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_addr = closed.local_addr().unwrap();
    drop(closed);
    let refused = test_client()
        .get(format!("http://{closed_addr}/"))
        .send()
        .await
        .expect_err("closed port must fail");
    assert_eq!(classify_transport(&refused), UsageHttpError::Network);
}

#[tokio::test]
async fn read_body_limited_accepts_exact_cap_and_rejects_one_byte_over() {
    const CAP: usize = 32;
    let exact = serve_once(200, "OK", "", &[b'x'; CAP]).await;
    let body = read_body_limited(get(&exact).await, CAP).await.unwrap();
    assert_eq!(body.len(), CAP);

    let over = serve_once(200, "OK", "", &[b'x'; CAP + 1]).await;
    assert!(matches!(
        read_body_limited(get(&over).await, CAP).await,
        Err(BoundedBodyError::Oversize)
    ));
}

#[tokio::test]
async fn read_body_limited_rejects_chunked_oversize_without_content_length() {
    const CAP: usize = 16;
    let url = serve_chunked(vec![vec![b'a'; 8], vec![b'b'; 8], vec![b'c'; 8]]).await;
    assert!(matches!(
        read_body_limited(get(&url).await, CAP).await,
        Err(BoundedBodyError::Oversize)
    ));
}

#[tokio::test]
async fn read_ok_body_accepts_only_http_200() {
    const CAP: usize = 64;
    let ok = serve_once(200, "OK", "", b"{\"ok\":true}").await;
    let body = read_ok_body(get(&ok).await, CAP).await.unwrap();
    assert_eq!(body, b"{\"ok\":true}");

    let created = serve_once(201, "Created", "", b"{\"ok\":true}").await;
    assert_eq!(
        read_ok_body(get(&created).await, CAP).await,
        Err(UsageHttpError::Http(201))
    );

    let unauthorized = serve_once(401, "Unauthorized", "", b"no").await;
    assert_eq!(
        read_ok_body(get(&unauthorized).await, CAP).await,
        Err(UsageHttpError::Unauthorized)
    );
    let forbidden = serve_once(403, "Forbidden", "", b"no").await;
    assert_eq!(
        read_ok_body(get(&forbidden).await, CAP).await,
        Err(UsageHttpError::Forbidden)
    );
    let limited = serve_once(429, "Too Many Requests", "", b"no").await;
    assert_eq!(
        read_ok_body(get(&limited).await, CAP).await,
        Err(UsageHttpError::RateLimited)
    );
    let found = serve_once(302, "Found", "Location: /elsewhere\r\n", b"").await;
    assert_eq!(
        read_ok_body(get(&found).await, CAP).await,
        Err(UsageHttpError::Http(302))
    );
}

#[tokio::test]
async fn read_ok_body_maps_oversize_without_leaking_a_key() {
    const CAP: usize = 8;
    let url = serve_once(200, "OK", "", &[b'x'; 16]).await;
    let error = read_ok_body(get(&url).await, CAP)
        .await
        .expect_err("oversize 200 must fail");
    assert_eq!(error, UsageHttpError::Oversize);
    let display = format!("{error:?}");
    assert!(!display.contains("Bearer"));
    assert!(!display.contains("sk-"));
}
