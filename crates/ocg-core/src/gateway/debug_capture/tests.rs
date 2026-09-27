use super::*;
use axum::http::HeaderValue;

#[test]
fn complete_content_preserves_large_messages_tools_images_and_removes_credentials() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer unusual-credential"),
    );
    headers.insert("cookie", HeaderValue::from_static("session=private"));
    headers.insert("x-custom-token", HeaderValue::from_static("other-secret"));
    headers.insert("x-repeat", HeaderValue::from_static("first"));
    headers.append("x-repeat", HeaderValue::from_static("second"));
    let text = "中".repeat(3000);
    let body = json!({"messages":[{"content":text}], "image":"data:image/png;base64,AAAA", "tools":[{"parameters":{"properties":{"token":{"type":"string"}}}}], "metadata":{"api_key":"hidden", "echo":"unusual-credential"}});
    let record = capture_record(
        "ocg-test",
        "upstream",
        2,
        "https://user:pass@example.com/v1/messages?key=hidden&test=ok",
        &headers,
        &serde_json::to_vec(&body).unwrap(),
        &["upstream-secret".into()],
    );
    assert_eq!(record["body"]["messages"], body["messages"]);
    assert_eq!(record["body"]["image"], body["image"]);
    assert_eq!(record["body"]["tools"], body["tools"]);
    assert_eq!(record["attempt"], 2);
    assert_eq!(record["capture_status"], "complete_redacted");
    let encoded = record.to_string();
    for secret in [
        "unusual-credential",
        "other-secret",
        "session=private",
        "hidden",
        "user:pass",
    ] {
        assert!(!encoded.contains(secret), "{secret}");
    }
    assert_eq!(
        record["headers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["name"] == "x-repeat")
            .count(),
        2
    );
}

#[test]
fn malformed_input_is_explicitly_omitted() {
    let record = capture_record(
        "ocg-test",
        "client",
        0,
        "/v1/messages",
        &HeaderMap::new(),
        b"{secret",
        &[],
    );
    assert_eq!(record["capture_status"], "invalid_json_omitted");
    assert_eq!(record["body"]["bytes"], 7);
    assert!(!record.to_string().contains("{secret"));
}

#[tokio::test]
async fn capture_is_atomic_disabled_is_noop_and_write_failure_is_reported() {
    let root = std::env::temp_dir().join(format!("ocg-capture-test-{}", uuid::Uuid::new_v4()));
    let capture = DebugCapture {
        directory: Some(root.clone()),
        writer: Arc::new(parking_lot::Mutex::new(())),
    };
    let trace = RequestTrace::new();
    capture
        .save(
            &trace,
            "client",
            0,
            "/v1/messages",
            &HeaderMap::new(),
            Bytes::from_static(b"{}"),
            &[],
        )
        .await
        .unwrap();
    let files: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(files.len(), 1);
    assert!(owned_filename(&files[0].file_name().to_string_lossy()));
    let record: Value = serde_json::from_slice(&std::fs::read(files[0].path()).unwrap()).unwrap();
    assert_eq!(record["request_id"], trace.request_id);
    let blocked = DebugCapture {
        directory: Some(files[0].path()),
        writer: capture.writer.clone(),
    };
    assert!(
        blocked
            .save(
                &trace,
                "client",
                0,
                "/",
                &HeaderMap::new(),
                Bytes::new(),
                &[]
            )
            .await
            .is_err()
    );
    let disabled = DebugCapture {
        directory: None,
        writer: capture.writer.clone(),
    };
    disabled
        .save(
            &trace,
            "client",
            0,
            "/",
            &HeaderMap::new(),
            Bytes::new(),
            &[],
        )
        .await
        .unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn retention_recognizes_only_owned_capture_names() {
    assert!(owned_filename(&format!(
        "ocg-{}-100-client-0.json",
        uuid::Uuid::new_v4()
    )));
    assert!(!owned_filename("ocg-user-data-client-0.json"));
    assert!(!owned_filename("notes.json"));
}

#[tokio::test]
async fn http_capture_covers_chat_and_gemini_without_capturing_unauthorized_bodies() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    let root = std::env::temp_dir().join(format!("ocg-capture-http-{}", uuid::Uuid::new_v4()));
    let db = crate::db::Database::open(root.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let mut inner = crate::state::CoreStateInner::new(db, root.clone(), cipher).unwrap();
    let captures = root.join("captures");
    inner.debug_capture = DebugCapture {
        directory: Some(captures.clone()),
        writer: Arc::new(parking_lot::Mutex::new(())),
    };
    let state = Arc::new(inner);
    let mut config = state.config();
    config.gateway_key = "capture-test-key".into();
    state.set_config(config).unwrap();
    let router = super::super::inference_router_with_body_limit(state.clone(), 1024)
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&json!({"messages": "private"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert!(!captures.exists());
    for path in [
        "/v1/chat/completions",
        "/v1beta/models/missing:generateContent",
    ] {
        let response = client
            .post(format!("http://{addr}{path}"))
            .bearer_auth("capture-test-key")
            .json(&json!({"private_text": "保留正文"}))
            .send()
            .await
            .unwrap();
        let id = response
            .headers()
            .get("x-ocg-request-id")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(response.status(), 400);
        let file = std::fs::read_dir(&captures)
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.file_name().to_string_lossy().starts_with(id))
            .unwrap();
        let record: Value = serde_json::from_slice(&std::fs::read(file.path()).unwrap()).unwrap();
        assert_eq!(record["body"]["private_text"], "保留正文");
        assert_eq!(record["uri"], path);
        assert!(!record.to_string().contains("capture-test-key"));
        assert!(
            !state
                .db
                .lock()
                .query_gateway_logs(50, Some(id))
                .unwrap()
                .is_empty()
        );
    }
    let response = client
        .post(format!("http://{addr}/v1/messages"))
        .bearer_auth("capture-test-key")
        .body(vec![b' '; 1025])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    assert_eq!(std::fs::read_dir(&captures).unwrap().count(), 2);
    stop.send(()).unwrap();
    server.await.unwrap();
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn credential_containers_and_custom_headers_are_redacted() {
    let mut headers = HeaderMap::new();
    headers.insert("x-private-key", HeaderValue::from_static("header-secret"));
    let body = json!({"private_key": "private", "password": ["array-secret"], "authorization": {"value":"object-secret"}, "metadata":{"secretKey":"nested-secret"}, "tools":[{"input_schema":{"properties":{"token":{"type":"string"}}}}]});
    let record = capture_record(
        "ocg-test",
        "client",
        0,
        "/v1/messages",
        &headers,
        &serde_json::to_vec(&body).unwrap(),
        &[],
    );
    for secret in [
        "header-secret",
        "array-secret",
        "object-secret",
        "nested-secret",
    ] {
        assert!(!record.to_string().contains(secret));
    }
    assert_eq!(record["body"]["private_key"], "<redacted>");
    assert_eq!(record["body"]["tools"], body["tools"]);
}

#[test]
fn padded_auth_values_are_redacted_from_body_echoes() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("Bearer   opaque-credential  "),
    );
    headers.insert(
        "x-api-key",
        HeaderValue::from_static(" another-credential "),
    );
    let record = capture_record(
        "ocg-test",
        "client",
        0,
        "/",
        &headers,
        br#"{"message":"opaque-credential another-credential"}"#,
        &[],
    );
    assert!(!record.to_string().contains("opaque-credential"));
    assert!(!record.to_string().contains("another-credential"));
}

#[test]
fn retention_bounds_completed_files_and_preserves_lookalikes() {
    let root = std::env::temp_dir().join(format!("ocg-retention-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let id = uuid::Uuid::new_v4();
    let lookalike = format!("ocg-{id}-notes-client-backup.json");
    assert!(!owned_filename(&lookalike));
    std::fs::write(root.join(&lookalike), "keep").unwrap();
    for i in 0..MAX_FILES {
        std::fs::write(root.join(format!("ocg-{id}-{i}-client-0.json")), "{}").unwrap();
    }
    write_capture(&root, &format!("ocg-{id}-1001-upstream-1.json"), b"{}").unwrap();
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), MAX_FILES + 1);
    assert_eq!(
        std::fs::read_to_string(root.join(lookalike)).unwrap(),
        "keep"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn upstream_capture_removes_both_client_and_upstream_credentials() {
    let mut client = HeaderMap::new();
    client.insert(
        "authorization",
        HeaderValue::from_static("Bearer gateway-secret"),
    );
    let mut upstream = HeaderMap::new();
    upstream.insert(
        "authorization",
        HeaderValue::from_static("Bearer provider-secret"),
    );
    let mut secrets = authentication_secrets(&client);
    secrets.push("provider-secret".into());
    let record = capture_record(
        "ocg-test",
        "upstream",
        1,
        "https://example.com/v1/messages",
        &upstream,
        br#"{"messages":[{"content":"gateway-secret provider-secret"}]}"#,
        &secrets,
    );
    assert!(!record.to_string().contains("gateway-secret"));
    assert!(!record.to_string().contains("provider-secret"));
}

#[test]
fn schema_named_metadata_cannot_bypass_redaction() {
    let body = json!({"metadata":{"properties":{"api_key":"hidden1"},"definitions":{"password":"hidden2"},"$defs":{"token":"hidden3"}}});
    let record = capture_record(
        "ocg-test",
        "client",
        0,
        "/",
        &HeaderMap::new(),
        &serde_json::to_vec(&body).unwrap(),
        &[],
    );
    for secret in ["hidden1", "hidden2", "hidden3"] {
        assert!(!record.to_string().contains(secret));
    }
}
