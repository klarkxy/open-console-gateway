//! Temporary upstream failures must not mutate account availability or stickiness.
use axum::http::StatusCode;
use chrono::{Duration, Utc};
use ocg_core::gateway::provider_adapter::install_goat_loopback_route_for_test;
use ocg_core::models::RoutingMode;
use ocg_core::provider::{COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS as MODEL, ZEN_FREE_ACCOUNT_ID};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const TRANSIENT: &str = r#"{"error":{"message":"Upstream model provider is temporarily unavailable. Please try again in a moment.","type":"rate_limit_error"}}"#;

#[tokio::test]
async fn goat_transient_falls_back_only_for_this_request_and_next_request_returns_to_a() {
    let p = PreparedFallback::routing(
        &[
            ("key-a", &[reply(429, TRANSIENT), ok()]),
            ("key-b", &[ok()]),
        ],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
    let a = format!("goat-a-{}", uuid::Uuid::new_v4());
    let b = format!("goat-b-{}", uuid::Uuid::new_v4());
    create_goat_account(&p.state, "acct-1", &a, "key-a");
    create_goat_account(&p.state, "acct-1", &b, "key-b");
    let _a = install_goat_loopback_route_for_test(a.clone(), p.base_url.clone()).unwrap();
    let _b = install_goat_loopback_route_for_test(b.clone(), p.base_url.clone()).unwrap();
    p.state
        .db
        .lock()
        .reorder_accounts(&[
            a.clone(),
            b.clone(),
            "acct-1".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .unwrap();
    let h = p.bind().await;
    let before = h.account(&a);
    for _ in 0..2 {
        let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(h.call_keys(), ["key-a", "key-b", "key-a"]);
    let after = h.account(&a);
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.cooldown_generic_until, before.cooldown_generic_until);
    assert_eq!(after.last_error, before.last_error);
    assert_eq!(after.updated_at, before.updated_at);
    assert!(h.account(&b).cooldown_until.is_none());
    let logs = h.logs();
    let failed = logs
        .iter()
        .find(|row| row.http_status == Some(429))
        .unwrap();
    assert_eq!(failed.account_id, a);
    let diagnostic = failed.diagnostic.as_ref().unwrap();
    assert_eq!(diagnostic["retry_action"], "try_next_account");
}

#[tokio::test]
async fn goat_retry_after_header_reaches_persisted_deadline() {
    let raw = format!("HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 90\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", TRANSIENT.len(), TRANSIENT).into_bytes();
    let (base, calls, stop) = start_raw_disconnect_upstream(raw).await;
    let (state, dir) = build_state(base.clone(), &["unused"]);
    let goat = prepare_goat(&state, "key-a", &[], true);
    let _route = install_goat_loopback_route_for_test(goat.clone(), base).unwrap();
    let h =
        FallbackHarness::from_parts(state, dir, Default::default(), Some(stop), Some(calls)).await;
    let before = Utc::now();
    let (status, _) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_ne!(status, StatusCode::OK);
    let until = h.account(&goat).cooldown_generic_until.unwrap();
    assert!(until >= before + Duration::seconds(90));
    assert!(until <= Utc::now() + Duration::seconds(90));
    assert!(h.account(&goat).cooldown_week_until.is_none());
}
