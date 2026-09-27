use super::*;
use serde_json::json;

#[test]
fn token_list_reads_items_and_skips_disabled() {
    let data = json!({
        "items": [
            {"id": 12, "name": "Codex", "status": 1},
            {"id": "13", "name": "Grok", "status": 2},
            {"id": 14, "status": 1}
        ],
        "total": 3
    });
    let tokens = parse_token_list(&data);
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0].name, "Codex");
    assert!(tokens[0].enabled);
    assert!(!tokens[1].enabled);
    assert_eq!(tokens[2].name, "Key 14");
}

#[test]
fn token_list_accepts_a_bare_array() {
    let data = json!([{"id": 1, "name": "a"}]);
    assert_eq!(parse_token_list(&data)[0].id, "1");
}

#[test]
fn full_key_reads_object_or_string() {
    assert_eq!(
        parse_full_key(&json!({"key": " sk-live "})).as_deref(),
        Some("sk-live")
    );
    assert_eq!(
        parse_full_key(&json!("sk-plain")).as_deref(),
        Some("sk-plain")
    );
    assert_eq!(parse_full_key(&json!({"key": ""})), None);
}

#[tokio::test]
async fn fetch_full_key_uses_post_and_ignores_get() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback listener");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0_u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            let line = head.lines().next().unwrap_or_default();
            let allowed = line.starts_with("POST /api/token/7/key");
            let body = if allowed {
                r#"{"success":true,"data":{"key":"sk-live"}}"#
            } else {
                r#"{"success":false}"#
            };
            let status = if allowed {
                "200 OK"
            } else {
                "405 Method Not Allowed"
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let origin = reqwest::Url::parse(&format!("http://{addr}")).unwrap();
    let key = fetch_full_key(&client, &origin, "9:pat-secret", "7")
        .await
        .expect("POST /api/token/{{id}}/key must succeed");
    assert_eq!(key, "sk-live");
    let get_url = origin.join("api/token/7/key").unwrap();
    let get_status = client.get(get_url).send().await.unwrap().status();
    assert_eq!(get_status, reqwest::StatusCode::METHOD_NOT_ALLOWED);
}

#[test]
fn pagination_advances_even_when_the_first_fifty_tokens_are_disabled() {
    let rows: Vec<Value> = (1..=50).map(|id| json!({"id": id, "status": 2})).collect();
    let data = json!({"items": rows, "total": 100});
    assert!(parse_token_list(&data).iter().all(|token| !token.enabled));
    assert_eq!(
        next_page(1, token_rows(&data).len(), Some(100)).unwrap(),
        Some(2)
    );
    assert_eq!(next_page(2, 50, Some(100)).unwrap(), None);
}

#[test]
fn pagination_uses_raw_rows_not_the_number_of_valid_or_enabled_tokens() {
    let rows: Vec<Value> = (1..=50).map(|_| json!({"name": "malformed"})).collect();
    let data = json!({"items": rows, "total": 51});
    assert!(parse_token_list(&data).is_empty());
    assert_eq!(
        next_page(1, token_rows(&data).len(), Some(51)).unwrap(),
        Some(2)
    );
}

#[test]
fn pagination_without_a_total_requires_another_page_only_when_full() {
    assert_eq!(next_page(1, 50, None).unwrap(), Some(2));
    assert_eq!(next_page(2, 0, None).unwrap(), None);
    assert_eq!(next_page(2, 7, None).unwrap(), None);
}

#[test]
fn inconsistent_or_unbounded_pages_never_claim_completion() {
    assert!(next_page(0, 50, Some(100)).is_err());
    assert!(next_page(1, 51, Some(100)).is_err());
    assert!(next_page(1, 0, Some(100)).is_err());
    assert!(next_page(MAX_PAGE, 50, None).is_err());
    assert!(next_page(1, 1, Some(-1)).is_err());
}

#[test]
fn remote_token_ids_cannot_change_the_full_key_request_path() {
    assert!(token_id(&json!({"id": "../user/self"})).is_none());
    assert!(token_id(&json!({"id": -1})).is_none());
}
