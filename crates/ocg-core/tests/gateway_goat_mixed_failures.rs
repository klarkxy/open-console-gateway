//! Mixed GOAT failures must not turn request-local fallback into sticky lock-in.
use axum::http::StatusCode;
use ocg_core::gateway::provider_adapter::install_goat_loopback_route_for_test;
use ocg_core::models::RoutingMode;
use ocg_core::provider::{COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS as MODEL, ZEN_FREE_ACCOUNT_ID};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const TRANSIENT: &str = r#"{"error":{"message":"Upstream model provider is temporarily unavailable. Please try again in a moment.","type":"rate_limit_error"}}"#;
const CREDIT_ERROR: &str = r#"{"error":{"code":"BAD_REQUEST","message":"You have insufficient credits to make this request. Please purchase more credits to continue using the service.","type":"invalid_request_error"}}"#;

#[tokio::test]
async fn goat_transient_then_credit_error_reaches_third_account_without_changing_sticky_target() {
    let p = PreparedFallback::routing(
        &[
            (
                "key-a",
                &[ok(), reply(429, TRANSIENT), reply(429, TRANSIENT), ok()],
            ),
            (
                "key-h",
                &[reply(400, CREDIT_ERROR), reply(400, CREDIT_ERROR)],
            ),
            ("key-c", &[ok(), ok()]),
        ],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
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

    // A's transient 429 must not stop at H's credit 400. Repeat the chain to
    // expose any accidental persistent cooldown or replacement of sticky A.
    for _ in 0..2 {
        let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        h.call_keys(),
        [
            "key-a", "key-a", "key-h", "key-c", "key-a", "key-h", "key-c", "key-a"
        ]
    );

    // Neither upstream supplied a recovery deadline. In particular, a credit
    // error remains distinct from both an authentication error and a quota reset.
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

    let logs = h.logs();
    assert_eq!(logs.len(), 8);
    for (code, account_id, attempt) in [(429, &a, 1), (400, &h_id, 2)] {
        let failed: Vec<_> = logs
            .iter()
            .filter(|row| row.http_status == Some(code))
            .collect();
        assert_eq!(failed.len(), 2);
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
        2
    );
}
