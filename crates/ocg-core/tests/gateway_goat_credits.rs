//! Exact credit rejections create local admission waits, never account quota facts.
use axum::http::StatusCode;
use ocg_core::gateway::provider_adapter::install_goat_loopback_route_for_test;
use ocg_core::models::RoutingMode;
use ocg_core::provider::{COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS as MODEL, ZEN_FREE_ACCOUNT_ID};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const CREDIT_ERROR: &str = r#"{"error":{"code":"BAD_REQUEST","message":"You have insufficient credits to make this request. Please purchase more credits to continue using the service.","type":"invalid_request_error"}}"#;

#[tokio::test]
async fn goat_credit_400_waits_across_requests_without_publishing_quota_recovery() {
    let p = PreparedFallback::routing(
        &[
            ("a", &[reply(400, CREDIT_ERROR), reply(400, CREDIT_ERROR)]),
            ("b", &[ok(), ok()]),
        ],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
    let a = format!("goat-a-{}", uuid::Uuid::new_v4());
    let b = format!("goat-b-{}", uuid::Uuid::new_v4());
    create_goat_account(&p.state, "acct-1", &a, "a");
    create_goat_account(&p.state, "acct-1", &b, "b");
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
    assert_eq!(
        h.call_keys(),
        ["a", "b", "b"],
        "the second request must skip A locally without sending another rejected inference"
    );
    assert!(
        h.logs()
            .iter()
            .any(|row| row.error_stage.as_deref() == Some("resource_wait"))
    );
    let (status, policies) = v4_get(h.port, "/routing/temporary-policies").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(policies["waits"].as_array().unwrap().len(), 1);
    assert_eq!(policies["waits"][0]["status"], "waiting");
    assert_eq!(
        policies["waits"][0]["model"],
        h.calls.lock().unwrap()[0]
            .body
            .parse::<serde_json::Value>()
            .unwrap()["model"]
    );
    let credential = identity_refs_for(&h.state, &a).credential_id;
    let (status, view) = v4_get(h.port, "/credentials").await;
    assert_eq!(status, StatusCode::OK);
    let row = view["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == credential)
        .unwrap();
    assert!(
        row.get("quotaRecovery").is_none(),
        "an upstream 400 body must not publish a durable quota episode: {row}"
    );
    let after = h.account(&a);
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.auth_error, before.auth_error);

    let logs = h.logs();
    let failed: Vec<_> = logs
        .iter()
        .filter(|row| row.http_status == Some(400))
        .collect();
    assert_eq!(failed.len(), 1);
    for row in failed {
        assert_eq!(row.attempt, Some(1));
        let diagnostic = row.diagnostic.as_ref().unwrap();
        assert_eq!(diagnostic["retry_action"], "try_next_account");
        assert!(row.cost.is_none());
    }
    assert_eq!(
        logs.iter()
            .filter(|row| row.http_status == Some(200) && row.attempt == Some(2))
            .count(),
        2
    );
}

#[tokio::test]
async fn ordinary_goat_400_and_413_never_try_a_second_key() {
    for (status, body) in [
        (
            400,
            r#"{"error":{"message":"maximum context length exceeded"}}"#,
        ),
        (400, r#"{"error":{"message":"unknown model"}}"#),
        (413, CREDIT_ERROR),
    ] {
        let (h, id) = start_goat(
            &[("goat-key", &[reply(status, body)]), ("open-key", &[ok()])],
            &[],
            true,
            true,
        )
        .await;
        let (actual, _) = h.protocol("/v1/chat/completions", MODEL).await;
        assert_eq!(actual.as_u16(), status);
        assert_eq!(h.call_keys(), ["goat-key"]);
        assert!(h.account(&id).cooldown_until.is_none());
        assert!(h.account(&id).auth_error.is_none());
    }
}
