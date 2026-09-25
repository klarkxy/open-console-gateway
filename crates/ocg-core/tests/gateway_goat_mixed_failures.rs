//! Mixed GOAT failures preserve sticky routing across temporary Key waits.
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
const CREDIT_ERROR: &str = r#"{"error":{"code":"BAD_REQUEST","message":"You have insufficient credits to make this request. Please purchase more credits to continue using the service.","type":"invalid_request_error"}}"#;

#[tokio::test]
async fn goat_mixed_failures_respect_temporary_key_waits_without_changing_sticky_target() {
    let mut p = PreparedFallback::routing(
        &[
            (
                "key-a",
                &[ok(), reply(429, TRANSIENT), reply(429, TRANSIENT), ok()],
            ),
            (
                "key-h",
                &[
                    reply(400, CREDIT_ERROR),
                    reply(400, CREDIT_ERROR),
                    reply(400, CREDIT_ERROR),
                    reply(400, CREDIT_ERROR),
                ],
            ),
            ("key-c", &[ok(), ok(), ok(), ok()]),
        ],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
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
    // Rebuilding the state must preserve the fixture's inference-only isolation.
    p.state
        .usage_sync
        .set_reactive_refresh_enabled_for_test(false);
    p.state.usage_sync.set_fetch_for_test(|_, _| {
        Box::pin(async { Err(ocg_core::go_usage::GoUsageError::Network) })
    });
    let a = format!("goat-a-{}", uuid::Uuid::new_v4());
    let h_id = format!("goat-h-{}", uuid::Uuid::new_v4());
    let c = format!("goat-c-{}", uuid::Uuid::new_v4());
    create_goat_account(&p.state, "acct-1", &a, "key-a");
    create_goat_account(&p.state, "acct-1", &h_id, "key-h");
    create_goat_account(&p.state, "acct-1", &c, "key-c");
    let _a = install_goat_loopback_route_for_test(a.clone(), p.base_url.clone()).unwrap();
    let _h = install_goat_loopback_route_for_test(h_id.clone(), p.base_url.clone()).unwrap();
    let _c = install_goat_loopback_route_for_test(c.clone(), p.base_url.clone()).unwrap();
    p.state
        .db
        .lock()
        .reorder_accounts(&[
            a.clone(),
            h_id.clone(),
            c.clone(),
            "acct-1".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .unwrap();
    let h = p.bind().await;

    // Establish the same healthy global sticky target before the mixed failures.
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.call_keys(), ["key-a"]);
    let before_a = h.account(&a);
    let before_h = h.account(&h_id);

    // Make H higher priority without clearing routing state. Losing sticky A
    // would now send H first, including once A's temporary wait expires.
    h.state
        .db
        .lock()
        .reorder_accounts(&[
            h_id.clone(),
            a.clone(),
            c.clone(),
            "acct-1".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .unwrap();

    // A's two 429s hold only that Key for 30s each. H's first credit
    // rejection now waits across requests, including when A is locally skipped.
    let mut expected_calls = vec!["key-a"];
    for (at, additions) in [
        (0, vec!["key-a", "key-h", "key-c"]),
        (29, vec!["key-c"]),
        (30, vec!["key-a"]),
    ] {
        seconds.store(at, Ordering::SeqCst);
        if at == 30 {
            // H's local jitter may put its first probe at 30..33 seconds.
            // Use the diagnostic deadline instead of assuming a jitter value.
            let (status, view) = v4_get(h.port, "/routing/temporary-policies").await;
            assert_eq!(status, StatusCode::OK, "{view}");
            let remaining = view["waits"][0]["nextProbeInSeconds"].as_u64().unwrap();
            assert!(remaining <= 3);
            seconds.store(at + remaining, Ordering::SeqCst);
        }
        let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        expected_calls.extend(additions);
        if at == 30 {
            expected_calls.extend(["key-h", "key-c"]);
        }
        assert_eq!(h.call_keys(), expected_calls);
    }
    // Before the second 429 deadline, both failures are locally skipped.
    let second_start = seconds.load(Ordering::SeqCst);
    seconds.store(second_start + 29, Ordering::SeqCst);
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    expected_calls.push("key-c");
    assert_eq!(h.call_keys(), expected_calls);
    // At the second deadline, the original healthy sticky target is tried.
    seconds.store(second_start + 30, Ordering::SeqCst);
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    expected_calls.push("key-a");
    assert_eq!(h.call_keys(), expected_calls);

    // Temporary admission state never becomes persistent account or quota state.
    for (before, after) in [(&before_a, h.account(&a)), (&before_h, h.account(&h_id))] {
        assert_eq!(after.enabled, before.enabled);
        assert_eq!(after.cooldown_until, before.cooldown_until);
        assert_eq!(after.cooldown_generic_until, before.cooldown_generic_until);
        assert_eq!(after.cooldown_5h_until, before.cooldown_5h_until);
        assert_eq!(after.cooldown_week_until, before.cooldown_week_until);
        assert_eq!(after.cooldown_month_until, before.cooldown_month_until);
        assert_eq!(after.cooldown_free_until, before.cooldown_free_until);
        assert_eq!(after.auth_error, before.auth_error);
        assert_eq!(after.last_error, before.last_error);
        assert_eq!(after.updated_at, before.updated_at);
    }
    let (status, view) = v4_get(h.port, "/credentials").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    for account_id in [&a, &h_id] {
        let credential = identity_refs_for(&h.state, account_id).credential_id;
        let row = view["credentials"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == credential)
            .unwrap();
        assert!(
            row.get("quotaRecovery").is_none(),
            "an inference error must not publish durable quota recovery: {row}"
        );
    }

    let logs = h.logs();
    assert_eq!(logs.len(), 14);
    let waits: Vec<_> = logs
        .iter()
        .filter(|row| row.error_stage.as_deref() == Some("resource_wait"))
        .collect();
    assert_eq!(waits.len(), 4);
    for row in waits {
        assert!(row.account_id == a || row.account_id == h_id);
        assert_eq!(row.attempt, Some(if row.account_id == a { 1 } else { 2 }));
        assert!(row.http_status.is_none());
        assert!(row.cost.is_none());
        assert_eq!(
            row.diagnostic.as_ref().unwrap()["retry_action"],
            "try_next_account"
        );
    }
    for (code, account_id, attempt, count) in [(429, &a, 1, 2), (400, &h_id, 2, 2)] {
        let failed: Vec<_> = logs
            .iter()
            .filter(|row| row.http_status == Some(code))
            .collect();
        assert_eq!(failed.len(), count);
        for row in failed {
            assert_eq!(&row.account_id, account_id);
            assert_eq!(row.attempt, Some(attempt));
            assert_eq!(
                row.diagnostic.as_ref().unwrap()["retry_action"],
                "try_next_account"
            );
            assert!(row.cost.is_none());
        }
    }
    assert_eq!(
        logs.iter()
            .filter(|row| {
                row.account_id == c && row.http_status == Some(200) && row.attempt == Some(3)
            })
            .count(),
        4
    );
}
