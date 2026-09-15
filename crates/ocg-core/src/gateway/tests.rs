use super::{DEFAULT_GATEWAY_REQUEST_BODY_BYTES, request_body_limit, start_gateway_on};
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::gateway_keys::{PRIMARY_KEY_ID, PRIMARY_KEY_NAME};
use crate::state::CoreStateInner;
use axum::http::StatusCode;
use serde_json::json;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

#[test]
fn gateway_request_body_limit_configuration() {
    assert_eq!(request_body_limit(None), 64 * 1024 * 1024);
    assert_eq!(request_body_limit(Some("134217728")), 128 * 1024 * 1024);
    assert_eq!(request_body_limit(Some(" 1024 ")), 1024);
    for invalid in ["", "0", "-1", "64MiB", "1.5", "999999999999999999999999"] {
        assert_eq!(request_body_limit(Some(invalid)), 64 * 1024 * 1024);
    }
}

#[tokio::test]
async fn gateway_request_body_limit_override_covers_all_inference_routes() {
    let dir = std::env::temp_dir().join(format!("ocg-body-limit-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let router =
        super::inference_router_with_body_limit(state.clone(), request_body_limit(Some("1024")))
            .with_state(state);
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
    for path in [
        "/v1/chat/completions",
        "/v1/responses",
        "/v1/messages",
        "/v1beta/models/test:generateContent",
        "/v1/models/test:streamGenerateContent",
    ] {
        for (size, expected) in [
            (1024, StatusCode::UNAUTHORIZED),
            (1025, StatusCode::PAYLOAD_TOO_LARGE),
        ] {
            let response = client
                .post(format!("http://{addr}{path}"))
                .body(vec![b' '; size])
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "{path}, {size} bytes");
        }
    }
    stop.send(()).unwrap();
    server.await.unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn gateway_request_body_limit_accepts_64_mib_and_rejects_larger() {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be valid")
        .as_nanos();
    dir.push(format!("ocg-gateway-body-limit-{nanos}"));
    fs::create_dir_all(&dir).expect("test data directory should be created");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).expect("state should load"));
    let mut config = state.config();
    // Pin the primary key value for the test requests.
    config.gateway_key = "gateway-test-key".to_string();
    state.set_config(config).expect("test config should save");
    let handle = start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("test gateway should start");
    let root = format!("http://127.0.0.1:{}", handle.port);
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client should build");

    let mut accepted_body = vec![b' '; DEFAULT_GATEWAY_REQUEST_BODY_BYTES];
    accepted_body[DEFAULT_GATEWAY_REQUEST_BODY_BYTES - 1] = b'x';
    let accepted = client
        .post(format!("{root}/v1/chat/completions"))
        .bearer_auth("gateway-test-key")
        .header("origin", "https://example.test")
        .body(accepted_body)
        .send()
        .await
        .expect("request at the body limit should complete");
    assert_eq!(accepted.status(), StatusCode::BAD_REQUEST);
    let accepted_request_id = accepted
        .headers()
        .get("x-ocg-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("parse failure should return a request id")
        .to_string();
    assert!(accepted_request_id.starts_with("ocg-"));
    assert!(
        accepted
            .headers()
            .get("access-control-expose-headers")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("x-ocg-request-id"))
    );
    let accepted_error: serde_json::Value = accepted
        .json()
        .await
        .expect("accepted request should reach protocol JSON parsing");
    assert!(
        accepted_error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("invalid JSON request"))
    );

    let rejected = client
        .post(format!("{root}/v1/chat/completions"))
        .bearer_auth("gateway-test-key")
        .header("origin", "https://example.test")
        .body(vec![b'x'; DEFAULT_GATEWAY_REQUEST_BODY_BYTES + 1])
        .send()
        .await
        .expect("request above the body limit should complete");
    assert_eq!(rejected.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let rejected_request_id = rejected
        .headers()
        .get("x-ocg-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("body limit failure should return a request id")
        .to_string();
    assert!(rejected_request_id.starts_with("ocg-"));
    assert_ne!(accepted_request_id, rejected_request_id);

    {
        let db = state.db.lock();
        for (request_id, stage) in [
            (&accepted_request_id, "parse"),
            (&rejected_request_id, "body_limit"),
        ] {
            let logs: Vec<_> = db
                .list_forward_logs(100)
                .expect("request logs should query")
                .into_iter()
                .filter(|row| row.request_id.as_deref() == Some(request_id.as_str()))
                .collect();
            assert_eq!(logs.len(), 1);
            assert_eq!(logs[0].request_id.as_deref(), Some(request_id.as_str()));
            assert_eq!(logs[0].error_source.as_deref(), Some("client"));
            assert_eq!(logs[0].error_stage.as_deref(), Some(stage));
            assert_eq!(logs[0].client_key_id.as_deref(), Some(PRIMARY_KEY_ID));
            assert_eq!(logs[0].client_key_name.as_deref(), Some(PRIMARY_KEY_NAME));
            assert!(logs[0].diagnostic.is_some());
        }
    }

    let _ = handle.shutdown.send(());
    handle.task.await.expect("test gateway should stop");
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[tokio::test]
async fn unauthorized_and_expected_fallback_requests_are_not_persisted() {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be valid")
        .as_nanos();
    dir.push(format!("ocg-gateway-unlogged-control-flow-{nanos}"));
    fs::create_dir_all(&dir).expect("test data directory should be created");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).expect("state should load"));
    let mut config = state.config();
    // Pin the primary key value for the test requests.
    config.gateway_key = "gateway-test-key".to_string();
    state.set_config(config).expect("test config should save");
    let handle = start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("test gateway should start");
    let root = format!("http://127.0.0.1:{}", handle.port);
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client should build");
    let chat_body = json!({
        "model": "deepseek-v4-flash",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 1
    });
    let responses_body = json!({
        "model": "deepseek-v4-flash",
        "input": "hello",
        "store": false,
        "max_output_tokens": 1
    });
    let messages_body = json!({
        "model": "minimax-m3",
        "messages": [{"role": "user", "content": "hello"}],
        "max_tokens": 1
    });
    let gemini_body = json!({
        "contents": [{"role": "user", "parts": [{"text": "hello"}]}]
    });

    let unauthorized_requests = [
        client
            .get(format!("{root}/v1/models"))
            .bearer_auth("wrong-key"),
        client
            .post(format!("{root}/v1/chat/completions"))
            .bearer_auth("wrong-key")
            .json(&chat_body),
        client
            .post(format!("{root}/v1/responses"))
            .bearer_auth("wrong-key")
            .json(&responses_body),
        client
            .post(format!("{root}/v1/messages"))
            .header("x-api-key", "wrong-key")
            .json(&messages_body),
        client
            .post(format!("{root}/v1beta/models/minimax-m3:generateContent"))
            .header("x-goog-api-key", "wrong-key")
            .json(&gemini_body),
        client
            .post(format!("{root}/v1beta/models/minimax-m3:countTokens"))
            .header("x-goog-api-key", "wrong-key")
            .json(&gemini_body),
        client
            .post(format!("{root}/v1beta/models/minimax-m3:embedContent"))
            .header("x-goog-api-key", "wrong-key")
            .json(&gemini_body),
    ];
    for request in unauthorized_requests {
        let response = request
            .send()
            .await
            .expect("unauthorized request should complete");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let request_id = response
            .headers()
            .get("x-ocg-request-id")
            .and_then(|value| value.to_str().ok())
            .expect("unauthorized response should keep correlation id");
        assert!(
            state
                .db
                .lock()
                .query_gateway_logs(10, Some(request_id))
                .expect("gateway logs should query")
                .is_empty(),
            "unauthorized request {request_id} must not be persisted"
        );
    }

    let oversized = client
        .post(format!("{root}/v1/chat/completions"))
        .bearer_auth("wrong-key")
        .header("content-type", "application/json")
        .body(vec![b'x'; DEFAULT_GATEWAY_REQUEST_BODY_BYTES + 1])
        .send()
        .await
        .expect("oversized unauthorized request should complete");
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let oversized_request_id = oversized
        .headers()
        .get("x-ocg-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("oversized unauthorized response should keep correlation id");
    assert!(
        state
            .db
            .lock()
            .query_gateway_logs(10, Some(oversized_request_id))
            .expect("gateway logs should query")
            .is_empty(),
        "oversized unauthorized request must not be persisted"
    );
    assert!(
        state
            .db
            .lock()
            .list_forward_logs(100)
            .expect("forward logs should query")
            .is_empty()
    );

    let count_tokens = client
        .post(format!("{root}/v1beta/models/minimax-m3:countTokens"))
        .header("x-goog-api-key", "gateway-test-key")
        .json(&gemini_body)
        .send()
        .await
        .expect("countTokens fallback should complete");
    assert_eq!(count_tokens.status(), StatusCode::NOT_IMPLEMENTED);
    let count_tokens_request_id = count_tokens
        .headers()
        .get("x-ocg-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("countTokens fallback should keep correlation id");
    assert!(
        state
            .db
            .lock()
            .query_gateway_logs(10, Some(count_tokens_request_id))
            .expect("gateway logs should query")
            .is_empty(),
        "expected countTokens fallback must not be persisted as a failure"
    );

    let invalid_json = client
        .post(format!("{root}/v1/chat/completions"))
        .bearer_auth("gateway-test-key")
        .header("content-type", "application/json")
        .body("{")
        .send()
        .await
        .expect("invalid JSON request should complete");
    assert_eq!(invalid_json.status(), StatusCode::BAD_REQUEST);
    let invalid_request_id = invalid_json
        .headers()
        .get("x-ocg-request-id")
        .and_then(|value| value.to_str().ok())
        .expect("validation failure should keep correlation id");
    {
        let db = state.db.lock();
        let logs: Vec<_> = db
            .list_forward_logs(100)
            .expect("request logs should query")
            .into_iter()
            .filter(|row| row.request_id.as_deref() == Some(invalid_request_id))
            .collect();
        assert_eq!(logs.len(), 1, "real local failures should stay diagnosable");
        assert_eq!(logs[0].error_stage.as_deref(), Some("parse"));
        assert_eq!(logs[0].client_key_id.as_deref(), Some(PRIMARY_KEY_ID));
        assert_eq!(logs[0].client_key_name.as_deref(), Some(PRIMARY_KEY_NAME));
        let diagnostic = logs[0]
            .diagnostic
            .as_ref()
            .expect("parse failure should keep bounded diagnostic detail");
        assert!(diagnostic["upstream_body_bytes"].is_null());
        assert_eq!(diagnostic["client_body_bytes"], 1);
        assert!(
            db.query_gateway_logs(10, Some(invalid_request_id))
                .expect("runtime logs should query")
                .is_empty(),
            "request failures must not enter runtime logs"
        );
    }

    let _ = handle.shutdown.send(());
    handle.task.await.expect("test gateway should stop");
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}

#[tokio::test]
async fn gemini_and_messages_routes_stay_wired() {
    let mut dir = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be valid")
        .as_nanos();
    dir.push(format!("ocg-client-routes-{nanos}"));
    fs::create_dir_all(&dir).expect("test data directory should be created");
    let db = Database::open(dir.clone()).expect("test database should open");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).expect("state should load"));
    let mut config = state.config();
    // Exercise wired routes with a saved catalog, independently of account availability.
    state
        .db
        .lock()
        .set_contract_catalog(
            &crate::provider_contracts::ContractScope::provider(
                crate::provider::OPENCODE_PROVIDER_ID,
            ),
            &crate::kernel::protocol::supported_model_ids()
                .map(str::to_string)
                .collect::<Vec<_>>(),
            Some(chrono::Utc::now()),
            "test_refreshed_catalog",
            "https://example.test/models",
            chrono::Utc::now(),
        )
        .unwrap();
    state.reload_provider_contracts().unwrap();
    // Pin the primary key value for the test requests.
    config.gateway_key = "gateway-test-key".to_string();
    state.set_config(config).expect("test config should save");
    let handle = start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("test gateway should start");
    let root = format!("http://127.0.0.1:{}", handle.port);
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client should build");

    let gemini_body = json!({
        "contents":[{"role":"user","parts":[{"text":"hello"}]}]
    });
    let gemini_unauthorized = client
        .post(format!("{root}/v1beta/models/minimax-m3:generateContent"))
        .json(&gemini_body)
        .send()
        .await
        .expect("Gemini unauthorized request should complete");
    assert_eq!(gemini_unauthorized.status(), StatusCode::UNAUTHORIZED);

    for path in [
        "/v1beta/models/minimax-m3:generateContent",
        "/v1/models/minimax-m3:streamGenerateContent?alt=sse",
    ] {
        let response = client
            .post(format!("{root}{path}"))
            .header("x-goog-api-key", "gateway-test-key")
            .json(&gemini_body)
            .send()
            .await
            .expect("authorized Gemini generation route should complete");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    let safety_response = client
        .post(format!("{root}/v1beta/models/minimax-m3:generateContent"))
        .header("x-goog-api-key", "gateway-test-key")
        .json(&json!({
            "contents":[{"role":"user","parts":[{"text":"hello"}]}],
            "safetySettings":[{
                "category":"HARM_CATEGORY_HATE_SPEECH",
                "threshold":"BLOCK_LOW_AND_ABOVE"
            }]
        }))
        .send()
        .await
        .expect("unsupported Gemini safety policy should complete");
    assert_eq!(safety_response.status(), StatusCode::BAD_REQUEST);
    let safety_error: serde_json::Value = safety_response
        .json()
        .await
        .expect("Gemini safety error should be Google JSON");
    assert_eq!(safety_error["error"]["status"], "INVALID_ARGUMENT");
    assert!(
        safety_error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("cannot be preserved"))
    );
    for path in [
        "/v1beta/models/minimax-m3:countTokens",
        "/v1/models/minimax-m3:embedContent",
    ] {
        let response = client
            .post(format!("{root}{path}"))
            .header("x-goog-api-key", "gateway-test-key")
            .json(&gemini_body)
            .send()
            .await
            .expect("unsupported Gemini route should complete");
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body: serde_json::Value = response.json().await.expect("Google error JSON");
        assert_eq!(body["error"]["status"], "UNIMPLEMENTED");
    }
    let unknown_action = client
        .post(format!("{root}/v1beta/models/minimax-m3:unknownAction"))
        .header("x-goog-api-key", "gateway-test-key")
        .json(&gemini_body)
        .send()
        .await
        .expect("unknown Gemini action should complete");
    assert_eq!(unknown_action.status(), StatusCode::NOT_FOUND);

    let ordinary_messages = client
        .post(format!("{root}/v1/messages"))
        .json(&json!({"model":"minimax-m3","max_tokens":1,"messages":[]}))
        .send()
        .await
        .expect("ordinary messages route should complete");
    assert_eq!(ordinary_messages.status(), StatusCode::UNAUTHORIZED);

    let _ = handle.shutdown.send(());
    handle.task.await.expect("test gateway should stop");
    drop(state);
    fs::remove_dir_all(dir).expect("test data directory should be removed");
}
