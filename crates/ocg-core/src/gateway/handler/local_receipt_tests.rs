use super::*;
use crate::{
    crypto::StaticKeyCipher, db::Database, log_types::RequestLogQuery, state::CoreStateInner,
};
use axum::{Router, extract::DefaultBodyLimit, routing::post};
use std::sync::Arc;

#[tokio::test]
async fn actual_authenticated_body_limit_records_no_upstream_attempt_or_mixed_event() {
    let dir = std::env::temp_dir().join(format!("ocg-local-receipt-{}", uuid::Uuid::new_v4()));
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("local-receipt")),
        )
        .unwrap(),
    );
    let app = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .layer(DefaultBodyLimit::max(8))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            request_trace_middleware,
        ))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .post(format!("http://{address}/v1/chat/completions"))
        .bearer_auth(&state.config().gateway_key)
        .header("content-type", "application/json")
        .body(r#"{"model":"fixture","messages":[]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    {
        let db = state.db.lock();
        let page = db.query_request_logs(&RequestLogQuery::default()).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].attempt_count, 0);
        assert_eq!(page.items[0].recorded_row_count, 1);
        assert_eq!(page.items[0].status, "client_error");
        assert_eq!(page.summary.total_attempts, 0);
        assert_eq!(
            db.conn
                .query_row("SELECT COUNT(*) FROM gateway_logs", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        // Existing schema-64 rejections used 1. The projection must preserve the
        // stored row and still report zero actual sends after migration.
        db.conn
            .execute("UPDATE forward_logs SET attempt = 1", [])
            .unwrap();
        let migrated = db.query_request_logs(&RequestLogQuery::default()).unwrap();
        assert_eq!(migrated.items[0].attempt_count, 0);
        assert_eq!(migrated.items[0].recorded_row_count, 1);
    }
    drop(response);
    drop(client);
    task.abort();
    let _ = task.await;
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}
