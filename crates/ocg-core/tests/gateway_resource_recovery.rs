//! Local admission waits remain separate from durable quota episodes.
use axum::http::StatusCode;
use ocg_core::gateway::provider_adapter::{
    GoatLoopbackRouteGuard, install_goat_loopback_route_for_test,
};
use ocg_core::models::RoutingMode;
use ocg_core::provider::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS as MODEL, COMMAND_CODE_PROVIDER_ID,
};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const CREDITS: &str = r#"{"error":{"code":"BAD_REQUEST","message":"You have insufficient credits to make this request. Please purchase more credits to continue using the service.","type":"invalid_request_error"}}"#;
const TRANSIENT: &str = r#"{"error":{"message":"Upstream model provider is temporarily unavailable. Please try again in a moment.","type":"rate_limit_error"}}"#;

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
async fn goat_credit_400_waits_across_strict_priority_requests_without_quota_recovery() {
    let p = PreparedFallback::routing(
        &[
            ("a", &[reply(400, CREDITS), reply(400, CREDITS)]),
            ("b", &[ok(), ok()]),
        ],
        &["unused"],
        RoutingMode::StrictPriority,
        false,
    )
    .await;
    let (ids, _routes) = goats(&p, &["a", "b"]);
    let h = p.bind().await;
    let before = h.account(&ids[0]);
    succeeds(&h).await;
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "b", "b"]);
    let after = h.account(&ids[0]);
    assert_eq!(after.cooldown_until, before.cooldown_until);
    assert_eq!(after.auth_error, before.auth_error);
    let credential_id = identity_refs_for(&h.state, &ids[0]).credential_id;
    let (status, credentials) = v4_get(h.port, "/credentials").await;
    assert_eq!(status, StatusCode::OK, "{credentials}");
    let credential = credentials["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == credential_id)
        .unwrap();
    assert!(
        credential.get("quotaRecovery").is_none(),
        "a 400 body must not become durable quota state: {credential}"
    );
}

#[tokio::test]
async fn temporary_429_is_per_key_and_does_not_fan_out_to_a_declared_pool() {
    let p = PreparedFallback::routing(
        &[
            ("a", &[reply(429, TRANSIENT)]),
            ("sibling", &[ok(), ok()]),
            ("independent", &[ok()]),
        ],
        &["unused"],
        RoutingMode::StrictPriority,
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
        .find(|connection| connection["legacy"]["id"] == COMMAND_CODE_PROVIDER_ID)
        .unwrap();
    let (status, created) = v4_mutate(
        h.port,
        &h.state,
        &format!("/identities/{}/credentials", refs.identity_id),
        serde_json::json!({
            "connectionId": connection["id"], "secretInput": "sibling",
            "quotaSharing": {"kind": "shared", "credentialId": refs.credential_id}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let sibling = created["accountId"].as_str().unwrap().to_string();
    force_enable_unroutable_account_for_loopback_test(&h.state.data_dir, &sibling);
    let _sibling = install_goat_loopback_route_for_test(sibling.clone(), base).unwrap();
    reorder_first(&h.state, &[ids[0].clone(), sibling.clone(), ids[1].clone()]);

    succeeds(&h).await;
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "sibling", "sibling"]);
    assert!(h.account(&ids[0]).cooldown_until.is_none());
    assert!(h.account(&sibling).cooldown_until.is_none());
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
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 32);
    assert!(
        h.logs()
            .iter()
            .any(|row| row.error_stage.as_deref() == Some("request_budget"))
    );
}
