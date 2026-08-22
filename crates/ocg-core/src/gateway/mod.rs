pub mod classify;
pub mod diagnostics;
pub mod forwarder;
pub mod free_models;
pub mod handler;
pub mod limit;
pub mod materialize;
pub mod protocol;
pub mod protocol_stream;
pub mod provider_adapter;
pub mod routing;
pub mod selector;

use crate::state::{CoreState, GatewayHandle};
use anyhow::Result;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::HeaderName;
use axum::middleware;
use axum::routing::{get, post};
use std::net::SocketAddr;
use tokio::sync::oneshot;
use tower_http::cors::{Any, CorsLayer};

// 1M-token conversations exceed Axum's 2 MiB Bytes default; keep a bounded cap before auth.
const MAX_GATEWAY_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
const _: () = assert!(MAX_GATEWAY_REQUEST_BODY_BYTES > 2 * 1024 * 1024);

pub fn build_router(state: CoreState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        .expose_headers([HeaderName::from_static(diagnostics::REQUEST_ID_HEADER)]);

    let gateway_api = Router::new()
        .route("/v1/chat/completions", post(handler::chat_completions))
        .route("/v1/responses", post(handler::responses))
        .route("/v1/messages", post(handler::messages))
        .route("/v1/models", get(handler::models))
        .route(
            "/claude-desktop/v1/messages",
            post(handler::claude_desktop_messages),
        )
        .route(
            "/claude-desktop/v1/models",
            get(handler::claude_desktop_models),
        )
        .route(
            "/v1beta/models/{*model_action}",
            post(handler::gemini_model_action),
        )
        .route(
            "/v1/models/{*model_action}",
            post(handler::gemini_model_action),
        )
        .layer(cors)
        .layer(DefaultBodyLimit::max(MAX_GATEWAY_REQUEST_BODY_BYTES))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            handler::request_trace_middleware,
        ));

    Router::new()
        .merge(gateway_api)
        .nest(
            "/dashboard/api",
            crate::dashboard::api_router(state.clone()),
        )
        .route("/dashboard", get(crate::dashboard::serve_index))
        .route("/dashboard/", get(crate::dashboard::serve_index))
        .route(
            "/dashboard/assets/{*path}",
            get(crate::dashboard::serve_asset),
        )
        .with_state(state)
}

pub async fn start_gateway(state: CoreState, port: u16) -> Result<GatewayHandle> {
    start_gateway_on(state, SocketAddr::from(([127, 0, 0, 1], port))).await
}

pub async fn start_gateway_on(state: CoreState, addr: SocketAddr) -> Result<GatewayHandle> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    state.set_dashboard_local_mode(local_addr.ip().is_loopback());
    spawn_forward_log_backfill(state.clone());
    crate::usage_sync::spawn_usage_sync_loop(state.clone());
    let app = build_router(state);
    let port = local_addr.port();

    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let handle = tokio::spawn(async move {
        let server = axum::serve(listener, app);
        let server = server.with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        if let Err(e) = server.await {
            eprintln!("gateway server error: {}", e);
        }
    });

    Ok(GatewayHandle {
        port,
        shutdown: shutdown_tx,
        task: handle,
    })
}

/// Attributes pre-multi-key forward logs to the primary key in bounded
/// chunks. The runtime context (not `CoreStateInner` construction) owns this
/// so pure synchronous construction never starts it. Small tables finish
/// inline; a dedicated thread (holding only a weak state reference) continues
/// large ones chunk by chunk so request logging never waits behind more than
/// one short transaction. DB work is synchronous, so no lock is ever held
/// across an await point.
fn spawn_forward_log_backfill(state: CoreState) {
    // The primary key attributes under its fixed hardcoded id (see
    // `gateway_keys::PRIMARY_KEY_ID`); no config or db lock is needed.
    let (key_id, key_name) = (
        crate::gateway_keys::PRIMARY_KEY_ID.to_string(),
        crate::gateway_keys::PRIMARY_KEY_NAME.to_string(),
    );
    // Fast path: one bounded step inline. Fresh databases (and tests) have no
    // NULL rows, so this records the completion marker and never spawns.
    let more_chunks = {
        let db = state.db.lock();
        db.backfill_forward_logs_client_key_step(
            &key_id,
            &key_name,
            crate::db::FORWARD_LOG_BACKFILL_CHUNK_ROWS,
        )
    };
    match more_chunks {
        Ok(true) => {}
        Ok(false) => return,
        Err(error) => {
            eprintln!("warning: forward log key backfill unavailable: {error}");
            return;
        }
    }
    let weak = std::sync::Arc::downgrade(&state);
    std::thread::Builder::new()
        .name("ocg-forward-log-backfill".to_string())
        .spawn(move || {
            while let Some(state) = weak.upgrade() {
                let step = {
                    let db = state.db.lock();
                    db.backfill_forward_logs_client_key_step(
                        &key_id,
                        &key_name,
                        crate::db::FORWARD_LOG_BACKFILL_CHUNK_ROWS,
                    )
                };
                match step {
                    Ok(true) => std::thread::sleep(crate::db::FORWARD_LOG_BACKFILL_CHUNK_PAUSE),
                    Ok(false) => return,
                    Err(error) => {
                        eprintln!("warning: forward log key backfill paused: {error}");
                        return;
                    }
                }
            }
        })
        .ok();
}

pub fn stop_gateway(handle: GatewayHandle) {
    let _ = handle.shutdown.send(());
    // ponytail: don't block_on the JoinHandle — stop_gateway is called from
    // tokio runtime contexts and ExitRequested handlers.
    // The spawned task will exit when the graceful-shutdown future resolves.
    // If blocking is needed later, spawn the wait on a dedicated std::thread.
}

#[cfg(test)]
mod tests {
    use super::{MAX_GATEWAY_REQUEST_BODY_BYTES, start_gateway_on};
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::db::Database;
    use crate::state::CoreStateInner;
    use axum::http::StatusCode;
    use serde_json::json;
    use std::fs;
    use std::net::SocketAddr;
    use std::sync::Arc;

    #[tokio::test]
    async fn gateway_request_body_limit_accepts_16_mib_and_rejects_larger() {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be valid")
            .as_nanos();
        dir.push(format!("ocg-gateway-body-limit-{nanos}"));
        fs::create_dir_all(&dir).expect("test data directory should be created");
        let db = Database::open(dir.clone()).expect("test database should open");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let state =
            Arc::new(CoreStateInner::new(db, dir.clone(), cipher).expect("state should load"));
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

        let mut accepted_body = vec![b' '; MAX_GATEWAY_REQUEST_BODY_BYTES];
        accepted_body[MAX_GATEWAY_REQUEST_BODY_BYTES - 1] = b'x';
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
            .body(vec![b'x'; MAX_GATEWAY_REQUEST_BODY_BYTES + 1])
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
                let logs = db
                    .query_gateway_logs(10, Some(request_id))
                    .expect("request id query should work");
                assert_eq!(logs.len(), 1);
                assert_eq!(logs[0].request_id.as_deref(), Some(request_id.as_str()));
                assert_eq!(logs[0].error_source.as_deref(), Some("client"));
                assert_eq!(logs[0].error_stage.as_deref(), Some(stage));
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
        let state =
            Arc::new(CoreStateInner::new(db, dir.clone(), cipher).expect("state should load"));
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
                .get(format!("{root}/claude-desktop/v1/models"))
                .header("x-api-key", "wrong-key"),
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
                .post(format!("{root}/claude-desktop/v1/messages"))
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
            .body(vec![b'x'; MAX_GATEWAY_REQUEST_BODY_BYTES + 1])
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
        let logs = state
            .db
            .lock()
            .query_gateway_logs(10, Some(invalid_request_id))
            .expect("gateway logs should query");
        assert_eq!(logs.len(), 1, "real local failures should stay diagnosable");
        assert_eq!(logs[0].error_stage.as_deref(), Some("parse"));
        let diagnostic = logs[0]
            .diagnostic
            .as_ref()
            .expect("parse failure should keep bounded diagnostic detail");
        assert!(diagnostic["upstream_body_bytes"].is_null());
        assert_eq!(diagnostic["client_body_bytes"], 1);

        let _ = handle.shutdown.send(());
        handle.task.await.expect("test gateway should stop");
        drop(state);
        fs::remove_dir_all(dir).expect("test data directory should be removed");
    }

    #[tokio::test]
    async fn claude_desktop_routes_are_wired_and_protected() {
        let mut dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be valid")
            .as_nanos();
        dir.push(format!("ocg-claude-desktop-routes-{nanos}"));
        fs::create_dir_all(&dir).expect("test data directory should be created");
        let db = Database::open(dir.clone()).expect("test database should open");
        let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
        let state =
            Arc::new(CoreStateInner::new(db, dir.clone(), cipher).expect("state should load"));
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

        let unauthorized = client
            .get(format!("{root}/claude-desktop/v1/models"))
            .send()
            .await
            .expect("models request should complete");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let models = client
            .get(format!("{root}/claude-desktop/v1/models"))
            .header("x-api-key", "gateway-test-key")
            .send()
            .await
            .expect("authorized models request should complete");
        assert_eq!(models.status(), StatusCode::OK);
        let models: serde_json::Value =
            models.json().await.expect("models response should be JSON");
        assert_eq!(models["data"][0]["id"], "claude-sonnet-4-6");

        let messages = client
            .post(format!("{root}/claude-desktop/v1/messages"))
            .json(&json!({"model":"claude-sonnet-4-6","max_tokens":1,"messages":[]}))
            .send()
            .await
            .expect("messages request should complete");
        assert_eq!(messages.status(), StatusCode::UNAUTHORIZED);
        let unsupported = client
            .post(format!("{root}/claude-desktop/v1/messages"))
            .header("x-api-key", "gateway-test-key")
            .json(&json!({"model":"claude-unknown","max_tokens":1,"messages":[]}))
            .send()
            .await
            .expect("authorized messages request should complete");
        assert_eq!(unsupported.status(), StatusCode::BAD_REQUEST);

        let dashboard = client
            .get(format!("{root}/dashboard/api/claude-desktop/models"))
            .header("x-forwarded-for", "203.0.113.1")
            .send()
            .await
            .expect("dashboard request should complete");
        assert_eq!(dashboard.status(), StatusCode::UNAUTHORIZED);

        let invalid = client
            .put(format!("{root}/dashboard/api/claude-desktop/models"))
            .json(&json!({"sonnet":"","opus":"","haiku":""}))
            .send()
            .await
            .expect("dashboard update should complete");
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

        let updated = client
            .put(format!("{root}/dashboard/api/claude-desktop/models"))
            .json(&json!({"sonnet":"","opus":"glm-5.2","haiku":""}))
            .send()
            .await
            .expect("dashboard update should complete");
        assert_eq!(updated.status(), StatusCode::OK);
        let updated: serde_json::Value = updated.json().await.expect("update should return JSON");
        assert_eq!(
            updated,
            json!({"sonnet":"glm-5.2","opus":"glm-5.2","haiku":"glm-5.2"})
        );
        let fetched: serde_json::Value = client
            .get(format!("{root}/dashboard/api/claude-desktop/models"))
            .send()
            .await
            .expect("dashboard models request should complete")
            .json()
            .await
            .expect("dashboard models response should be JSON");
        assert_eq!(fetched, updated);

        let _ = handle.shutdown.send(());
        handle.task.await.expect("test gateway should stop");
        drop(state);
        fs::remove_dir_all(dir).expect("test data directory should be removed");
    }
}
