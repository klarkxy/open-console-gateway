//! Temporary upstream failures are Key-local and do not become durable account state.
use axum::http::StatusCode;
use ocg_core::crypto::StaticKeyCipher;
use ocg_core::db::Database;
use ocg_core::gateway::provider_adapter::install_goat_loopback_route_for_test;
use ocg_core::models::RoutingMode;
use ocg_core::provider::{COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS as MODEL, ZEN_FREE_ACCOUNT_ID};
use ocg_core::state::CoreStateInner;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const TRANSIENT: &str = r#"{"error":{"message":"Upstream model provider is temporarily unavailable. Please try again in a moment.","type":"rate_limit_error"}}"#;

fn clocked(p: &mut PreparedFallback) -> Arc<AtomicU64> {
    let seconds = Arc::new(AtomicU64::new(0));
    let wall = chrono::Utc::now();
    let mono = Instant::now();
    let w = seconds.clone();
    let m = seconds.clone();
    p.state = Arc::new(
        CoreStateInner::new_with_test_gateway_clock(
            Database::open(p.dir.clone()).unwrap(),
            p.dir.clone(),
            Arc::new(StaticKeyCipher::new("test")),
            move || wall + chrono::Duration::seconds(w.load(Ordering::SeqCst) as i64),
            move || mono + Duration::from_secs(m.load(Ordering::SeqCst)),
        )
        .unwrap(),
    );
    seconds
}

#[tokio::test]
async fn goat_429_holds_only_that_key_for_thirty_seconds() {
    let mut p = PreparedFallback::routing(
        &[
            ("key-a", &[reply(429, TRANSIENT), ok()]),
            ("key-b", &[ok(), ok(), ok()]),
        ],
        &["unused"],
        RoutingMode::StrictPriority,
        false,
    )
    .await;
    let clock = clocked(&mut p);
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
    assert_eq!(h.call_keys(), ["key-a", "key-b", "key-b"]);
    let after = h.account(&a);
    assert_eq!(after.cooldown_until, before.cooldown_until);
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

    clock.store(29, std::sync::atomic::Ordering::SeqCst);
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.call_keys(), ["key-a", "key-b", "key-b", "key-b"]);

    clock.store(31, std::sync::atomic::Ordering::SeqCst);
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.call_keys(), ["key-a", "key-b", "key-b", "key-b", "key-a"]);
}

#[tokio::test]
async fn goat_retry_after_is_key_local_and_not_a_persisted_account_reset() {
    let raw = format!("HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 90\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", TRANSIENT.len(), TRANSIENT).into_bytes();
    let (base, calls, stop) = start_raw_disconnect_upstream(raw).await;
    let (state, dir) = build_state(base.clone(), &["unused"]);
    let goat = prepare_goat(&state, "key-a", &[], true);
    let _route = install_goat_loopback_route_for_test(goat.clone(), base).unwrap();
    let h =
        FallbackHarness::from_parts(state, dir, Default::default(), Some(stop), Some(calls)).await;
    let before = h.account(&goat);
    let (status, _) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_ne!(status, StatusCode::OK);
    let after = h.account(&goat);
    assert_eq!(after.cooldown_generic_until, before.cooldown_generic_until);
    assert_eq!(after.cooldown_week_until, before.cooldown_week_until);
    assert_eq!(after.updated_at, before.updated_at);
    let count = h
        .delayed_calls
        .as_ref()
        .unwrap()
        .load(std::sync::atomic::Ordering::SeqCst);
    let (status, _) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_ne!(status, StatusCode::OK);
    assert_eq!(
        h.delayed_calls
            .as_ref()
            .unwrap()
            .load(std::sync::atomic::Ordering::SeqCst),
        count
    );
    assert!(h.logs().iter().any(
        |row| row.error_stage.as_deref() == Some("resource_wait") && row.http_status.is_none()
    ));
}
