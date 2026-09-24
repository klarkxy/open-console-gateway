//! Shared request deadlines must preserve phase evidence and never restart on fallback.
use axum::http::StatusCode;
use std::time::Duration;
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

#[tokio::test]
async fn stalled_error_body_keeps_known_status_and_account_at_deadline() {
    let h = FallbackHarness::delayed_configured(
        StatusCode::SERVICE_UNAVAILABLE,
        "application/json",
        vec![vec![(
            Duration::from_secs(3),
            "{\"error\":\"unavailable\"}",
        )]],
        &["key-1", "key-2"],
        |config| {
            config.non_stream_timeout_secs = 1;
        },
    )
    .await;
    let (status, _) = tokio::time::timeout(
        Duration::from_secs(5),
        protocol_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("known error body must be bounded");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(h.delayed_count(), 1, "known 5xx must never be replayed");
    let logs = h.logs();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!(logs[0].account_id, "acct-1");
    assert_eq!(logs[0].http_status, Some(503));
    assert_eq!(logs[0].status, "error");
    assert_eq!(logs[0].cost_state, "not_applicable");
    assert_eq!(logs[0].diagnostic.as_ref().unwrap()["upstream_status"], 503);
}

#[tokio::test]
async fn fallback_does_not_restart_the_non_stream_deadline() {
    let h = FallbackHarness::delayed_configured(
        StatusCode::FORBIDDEN,
        "application/json",
        vec![vec![(Duration::from_millis(1200), "{\"error\":\"forbidden\"}")]; 3],
        &["key-1", "key-2", "key-3"],
        |config| {
            config.non_stream_timeout_secs = 2;
        },
    )
    .await;
    let (status, _) = tokio::time::timeout(
        Duration::from_secs(5),
        protocol_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("fallback must share one deadline");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        h.delayed_count(),
        2,
        "third account must not receive a fresh budget"
    );
    let logs = h.logs();
    let sent: Vec<_> = logs
        .iter()
        .filter(|log| log.http_status == Some(403))
        .collect();
    assert_eq!(
        sent.len(),
        2,
        "both received HTTP rejections must be preserved: {logs:?}"
    );
    assert!(sent.iter().all(|log| log.status != "outcome_unknown"));
    assert!(
        logs.iter()
            .any(|log| log.error_stage.as_deref() == Some("request_budget"))
    );
}

#[tokio::test]
async fn partial_stream_frames_do_not_extend_pre_output_deadline_or_leave_streaming_rows() {
    let mut chunks = vec![(Duration::ZERO, "data: {\"id\":\"")];
    chunks.extend(vec![(Duration::from_millis(400), "x"); 8]);
    let h = FallbackHarness::delayed_configured(
        StatusCode::OK,
        "text/event-stream",
        vec![chunks],
        &["key-1", "key-2"],
        |config| {
            config.non_stream_timeout_secs = 10;
            config.stream_idle_timeout_secs = 1;
        },
    )
    .await;
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(3),
        protocol_stream_call(h.port, "/v1/messages", "minimax-m2.7"),
    )
    .await
    .expect("partial frames must not indefinitely refresh the pre-output budget");
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("upstream_outcome_unknown"), "{body}");
    assert_eq!(h.delayed_count(), 1);
    let logs = h.logs();
    assert_eq!(
        logs.len(),
        1,
        "timeout must finalize the original row: {logs:?}"
    );
    assert_eq!(logs[0].account_id, "acct-1");
    assert_eq!(logs[0].status, "outcome_unknown");
    assert_eq!(logs[0].cost_state, "outcome_unknown");
    assert_eq!(logs[0].error_stage.as_deref(), Some("request_budget"));
    assert!(logs[0].cost.is_none());
}
