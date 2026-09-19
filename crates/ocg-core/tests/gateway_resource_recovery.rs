//! Public Gateway behavior for normalized rejection facts and demand-driven recovery.
use axum::http::StatusCode;
use ocg_core::crypto::StaticKeyCipher;
use ocg_core::db::Database;
use ocg_core::gateway::provider_adapter::{
    GoatLoopbackRouteGuard, install_goat_loopback_route_for_test,
};
use ocg_core::models::RoutingMode;
use ocg_core::provider::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS as MODEL, COMMAND_CODE_PROVIDER_ID,
};
use ocg_core::state::CoreStateInner;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const CREDITS: &str = r#"{"error":{"code":"BAD_REQUEST","message":"You have insufficient credits to make this request. Please purchase more credits to continue using the service.","type":"invalid_request_error"}}"#;
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
fn goats(p: &PreparedFallback, keys: &[&str]) -> (Vec<String>, Vec<GoatLoopbackRouteGuard>) {
    let ids: Vec<_> = keys
        .iter()
        .map(|_| format!("recovery-{}", uuid::Uuid::new_v4()))
        .collect();
    let guards = ids
        .iter()
        .zip(keys)
        .map(|(id, key)| {
            create_goat_account(&p.state, "acct-1", id, key);
            install_goat_loopback_route_for_test(id.clone(), p.base_url.clone()).unwrap()
        })
        .collect();
    reorder_first(&p.state, &ids);
    (ids, guards)
}
async fn succeeds(h: &FallbackHarness) {
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn waiting_skips_network_and_complete_probe_restores_sticky_resource() {
    let mut p = PreparedFallback::routing(
        &[("a", &[reply(400, CREDITS), ok()]), ("b", &[ok()])],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
    let clock = clocked(&mut p);
    let (ids, _routes) = goats(&p, &["a", "b"]);
    let h = p.bind().await;
    for _ in 0..3 {
        succeeds(&h).await;
    }
    assert_eq!(h.call_keys(), ["a", "b", "b", "b"]);
    clock.store(40, Ordering::SeqCst);
    succeeds(&h).await;
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "b", "b", "b", "a", "a"]);
    let account = h.account(&ids[0]);
    assert!(account.cooldown_until.is_none());
    assert!(account.auth_error.is_none());
    assert_eq!(
        h.logs()
            .iter()
            .filter(|r| r.error_stage.as_deref() == Some("resource_wait")
                && r.http_status.is_none()
                && r.cost.is_none())
            .count(),
        2
    );
}

#[tokio::test]
async fn malformed_success_does_not_release_a_probe() {
    let mut p = PreparedFallback::routing(
        &[
            ("a", &[reply(400, CREDITS), reply(200, "{}"), ok()]),
            ("b", &[ok()]),
        ],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
    let clock = clocked(&mut p);
    let (_ids, _routes) = goats(&p, &["a", "b"]);
    let h = p.bind().await;
    succeeds(&h).await;
    clock.store(40, Ordering::SeqCst);
    let _ = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(h.call_keys(), ["a", "b", "a"]);
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "b", "a", "b"]);
    clock.store(80, Ordering::SeqCst);
    succeeds(&h).await;
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "b", "a", "b", "a", "a"]);
}

#[tokio::test]
async fn credential_rotation_and_explicit_reset_allow_retry_without_waiting() {
    let p = PreparedFallback::routing(
        &[
            ("a", &[reply(400, CREDITS)]),
            ("new-a", &[reply(400, CREDITS), ok()]),
            ("b", &[ok()]),
        ],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
    let (ids, _routes) = goats(&p, &["a", "b"]);
    let h = p.bind().await;
    succeeds(&h).await;
    let key = h.state.encrypt_key("new-a").unwrap();
    h.state
        .db
        .lock()
        .rotate_account_credential(&ids[0], &key)
        .unwrap();
    succeeds(&h).await;
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "b", "new-a", "b", "b"]);
    let body = dashboard_cas(&h.state, serde_json::json!({}));
    let (status, result) = dashboard_json(
        h.port,
        reqwest::Method::POST,
        "v3",
        &format!("/accounts/{}/reset-cooldown", ids[0]),
        Some(&body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "b", "new-a", "b", "b", "new-a"]);
}

#[tokio::test]
async fn explicit_shared_pool_skips_sibling_but_not_independent_same_provider() {
    let p = PreparedFallback::routing(
        &[
            ("a", &[reply(400, CREDITS)]),
            ("sibling", &[ok()]),
            ("independent", &[ok()]),
        ],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
    let (ids, _routes) = goats(&p, &["a", "independent"]);
    let base = p.base_url.clone();
    let h = p.bind().await;
    let refs = identity_refs_for(&h.state, &ids[0]);
    let (status, connections) = v4_get(h.port, "/connections").await;
    assert_eq!(status, StatusCode::OK, "{connections}");
    let connection = connections["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["legacy"]["id"] == COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    let (status, result) = v4_mutate(h.port, &h.state, &format!("/identities/{}/credentials", refs.identity_id), serde_json::json!({
        "connectionId": connection["id"], "secretInput": "sibling", "quotaSharing": {"kind": "shared", "credentialId": refs.credential_id}
    })).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    let sibling = result["accountId"].as_str().unwrap().to_string();
    force_enable_unroutable_account_for_loopback_test(&h.state.data_dir, &sibling);
    let _sibling = install_goat_loopback_route_for_test(sibling.clone(), base).unwrap();
    reorder_first(&h.state, &[ids[0].clone(), sibling.clone(), ids[1].clone()]);
    assert_eq!(
        h.state
            .db
            .lock()
            .shared_pool_account_ids(&ids[0])
            .unwrap()
            .len(),
        2
    );
    succeeds(&h).await;
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "independent", "independent"]);
    assert!(h.logs().iter().any(|r| r.account_id == sibling
        && r.error_stage.as_deref() == Some("resource_wait")
        && r.http_status.is_none()));
    assert!(h.account(&ids[0]).cooldown_until.is_none());
    assert!(h.account(&sibling).cooldown_until.is_none());
}

#[tokio::test]
async fn mixed_transient_and_credit_failures_preserve_sticky_without_repeating_credit_send() {
    let p = PreparedFallback::routing(
        &[
            (
                "a",
                &[ok(), reply(429, TRANSIENT), reply(429, TRANSIENT), ok()],
            ),
            ("h", &[reply(400, CREDITS)]),
            ("c", &[ok()]),
        ],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
    let (ids, _routes) = goats(&p, &["a", "h", "c"]);
    let h = p.bind().await;
    for _ in 0..4 {
        succeeds(&h).await;
    }
    assert_eq!(h.call_keys(), ["a", "a", "h", "c", "a", "c", "a"]);
    for id in &ids[..2] {
        let a = h.account(id);
        assert!(a.cooldown_until.is_none());
        assert!(a.last_error.is_none());
        assert!(a.auth_error.is_none());
    }
    let logs = h.logs();
    assert_eq!(
        logs.iter().filter(|r| r.http_status == Some(429)).count(),
        2
    );
    assert_eq!(
        logs.iter().filter(|r| r.http_status == Some(400)).count(),
        1
    );
    assert_eq!(
        logs.iter()
            .filter(|r| r.error_stage.as_deref() == Some("resource_wait"))
            .count(),
        1
    );
    for row in logs
        .iter()
        .filter(|r| matches!(r.http_status, Some(400 | 429)))
    {
        assert_eq!(
            row.diagnostic.as_ref().unwrap()["retry_action"],
            "try_next_account"
        );
        assert!(row.cost.is_none());
    }
}

#[tokio::test]
async fn all_waiting_returns_without_resending_and_recovers_on_demand() {
    let mut p = PreparedFallback::routing(
        &[("a", &[reply(400, CREDITS), ok()])],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
    let clock = clocked(&mut p);
    let (_ids, _routes) = goats(&p, &["a"]);
    // The ordinary account is only a template for the GOAT fixture; it must
    // not remain a hidden, healthy fallback in an all-resources-waiting test.
    p.state.db.lock().delete_account("acct-1").unwrap();
    let h = p.bind().await;
    let (status, _) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_ne!(status, StatusCode::OK);
    let (status, _) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_ne!(status, StatusCode::OK);
    assert_eq!(h.call_keys(), ["a"]);
    clock.store(40, Ordering::SeqCst);
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "a"]);
}

#[tokio::test]
async fn one_request_never_exceeds_the_shared_attempt_budget() {
    let p = PreparedFallback::routing(&[], &["unused"], RoutingMode::StrictPriority, false).await;
    let entries: Vec<_> = (0..40)
        .map(|_| format!("budget-{}", uuid::Uuid::new_v4()))
        .collect();
    for id in &entries {
        create_goat_account(&p.state, "acct-1", id, "same-test-key");
    }
    let raw = format!("HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", TRANSIENT.len(), TRANSIENT).into_bytes();
    let (base, calls, stop) = start_raw_disconnect_upstream(raw).await;
    let _guards: Vec<_> = entries
        .iter()
        .map(|id| install_goat_loopback_route_for_test(id.clone(), base.clone()).unwrap())
        .collect();
    reorder_first(&p.state, &entries);
    let mut h = p.bind().await;
    h.push_stop(stop);
    let (status, _) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(calls.load(Ordering::SeqCst), 32);
    assert!(
        h.logs()
            .iter()
            .any(|r| r.error_stage.as_deref() == Some("request_budget"))
    );
}
