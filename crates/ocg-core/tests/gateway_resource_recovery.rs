//! New upstream errors stay request-local; only seeded legacy quota episodes persist.
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
    p.state
        .usage_sync
        .set_reactive_refresh_enabled_for_test(false);
    p.state.usage_sync.set_fetch_for_test(|_, _| {
        Box::pin(async { Err(ocg_core::go_usage::GoUsageError::Network) })
    });
    seconds
}

async fn succeeds(h: &FallbackHarness) {
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn goat_credit_400_is_request_local_and_never_publishes_quota_recovery() {
    let mut p = PreparedFallback::routing(
        &[
            ("a", &[reply(400, CREDITS), ok()]),
            ("b", &[ok(), ok(), ok()]),
        ],
        &["unused"],
        RoutingMode::StickyGlobal,
        false,
    )
    .await;
    let clock = clocked(&mut p);
    let (ids, _routes) = goats(&p, &["a", "b"]);
    let h = p.bind().await;
    let before = h.account(&ids[0]);
    succeeds(&h).await;
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "b", "b"]);
    // Initial wait is 30s plus bounded jitter; 40s is due without sleeping.
    clock.store(40, Ordering::SeqCst);
    succeeds(&h).await;
    assert_eq!(h.call_keys(), ["a", "b", "b", "a"]);
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
    let logs = h.logs();
    assert_eq!(
        logs.iter()
            .filter(|row| row.http_status == Some(400))
            .count(),
        1
    );
    assert!(
        logs.iter()
            .any(|row| row.error_stage.as_deref() == Some("local_policy_skip"))
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

#[tokio::test]
async fn goat_credit_wait_does_not_fan_out_to_a_declared_pool() {
    let p = PreparedFallback::routing(
        &[
            ("a", &[reply(400, CREDITS)]),
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
    let (status, view) = v4_get(h.port, "/credentials").await;
    assert_eq!(status, StatusCode::OK, "{view}");
    for account_id in [&ids[0], &sibling] {
        let credential = identity_refs_for(&h.state, account_id).credential_id;
        let row = view["credentials"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == credential)
            .unwrap();
        assert!(row.get("quotaRecovery").is_none(), "{row}");
    }
}

#[tokio::test]
async fn local_policy_all_waiting_is_503_with_zero_further_sends() {
    let p = PreparedFallback::routing(
        &[("a", &[reply(400, CREDITS)]), ("b", &[reply(400, CREDITS)])],
        &["unused"],
        RoutingMode::StrictPriority,
        false,
    )
    .await;
    let (_ids, _routes) = goats(&p, &["a", "b"]);
    set_account_enabled(&p.state, "acct-1", false);
    let h = p.bind().await;
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(h.call_keys(), ["a", "b"]);
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.to_string().contains("local_policy"), "{body}");
    assert_eq!(h.call_keys(), ["a", "b"]);
    assert!(
        h.logs()
            .iter()
            .any(|row| row.error_stage.as_deref() == Some("local_policy_skip"))
    );
}

#[tokio::test]
async fn more_than_thirty_two_local_policy_waits_still_reach_a_healthy_candidate() {
    let restricted: Vec<String> = (0..32).map(|i| format!("k{i}")).collect();
    let restricted_replies: Vec<Vec<_>> = restricted
        .iter()
        .map(|_| vec![reply(400, CREDITS)])
        .collect();
    let healthy_replies = vec![ok()];
    let mut entries: Vec<(&str, &[MockReply])> = restricted
        .iter()
        .zip(restricted_replies.iter())
        .map(|(key, replies)| (key.as_str(), replies.as_slice()))
        .collect();
    entries.push(("healthy", healthy_replies.as_slice()));
    let p =
        PreparedFallback::routing(&entries, &["unused"], RoutingMode::StrictPriority, false).await;
    let mut account_ids = Vec::new();
    let mut guards = Vec::new();
    for key in restricted
        .iter()
        .chain(std::iter::once(&"healthy".to_string()))
    {
        let id = format!("recovery-{}", uuid::Uuid::new_v4());
        create_goat_account(&p.state, "acct-1", &id, key);
        guards.push(install_goat_loopback_route_for_test(id.clone(), p.base_url.clone()).unwrap());
        account_ids.push(id);
    }
    reorder_first(&p.state, &account_ids);
    let h = p.bind().await;
    let (status, _) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(h.call_keys().len(), 32);
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.call_keys().last().map(String::as_str), Some("healthy"));
    assert_eq!(
        h.call_keys().iter().filter(|key| *key == "healthy").count(),
        1
    );
    assert!(
        h.logs()
            .iter()
            .filter(|row| row.error_stage.as_deref() == Some("local_policy_skip"))
            .count()
            >= 32
    );
    drop(guards);
}

struct ScriptedReply {
    arrived: Option<tokio::sync::oneshot::Sender<()>>,
    release: Option<tokio::sync::oneshot::Receiver<()>>,
    status: u16,
    body: &'static str,
    retry_after: Option<&'static str>,
}

fn hold_reply(
    status: u16,
    body: &'static str,
    retry_after: Option<&'static str>,
) -> (
    ScriptedReply,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let (arrived_tx, arrived_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    (
        ScriptedReply {
            arrived: Some(arrived_tx),
            release: Some(release_rx),
            status,
            body,
            retry_after,
        },
        arrived_rx,
        release_tx,
    )
}

fn immediate_reply(
    status: u16,
    body: &'static str,
    retry_after: Option<&'static str>,
) -> ScriptedReply {
    ScriptedReply {
        arrived: None,
        release: None,
        status,
        body,
        retry_after,
    }
}

fn authorization_key(head: &str) -> String {
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.eq_ignore_ascii_case("authorization") {
            continue;
        }
        let value = value.trim();
        return value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
            .unwrap_or(value)
            .trim()
            .to_string();
    }
    String::new()
}

fn recorded_keys(journal: &Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
    journal.lock().unwrap().clone()
}

fn push_ok(queue: &Arc<std::sync::Mutex<std::collections::VecDeque<ScriptedReply>>>, n: usize) {
    let mut steps = queue.lock().unwrap();
    for _ in 0..n {
        steps.push_back(immediate_reply(200, SUCCESS_BODY, None));
    }
}

async fn serve_scripted_connection(
    mut socket: tokio::net::TcpStream,
    queue: Arc<std::sync::Mutex<std::collections::VecDeque<ScriptedReply>>>,
    journal: Arc<std::sync::Mutex<Vec<String>>>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0_u8; 16 * 1024];
    let n = match tokio::time::timeout(Duration::from_secs(8), socket.read(&mut buf)).await {
        Ok(Ok(n)) => n,
        _ => return,
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    let line = head.lines().next().unwrap_or_default();
    if line.starts_with("GET / ") {
        let _ = socket
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        let _ = socket.shutdown().await;
        return;
    }
    journal.lock().unwrap().push(authorization_key(&head));
    let mut step = queue.lock().unwrap().pop_front();
    if let Some(step) = step.as_mut() {
        if let Some(arrived) = step.arrived.take() {
            let _ = arrived.send(());
        }
        if let Some(release) = step.release.take() {
            let _ = tokio::time::timeout(Duration::from_secs(8), release).await;
        }
    }
    let (status, body, retry_after) = step
        .as_ref()
        .map(|step| (step.status, step.body, step.retry_after))
        .unwrap_or((500, "{}", None));
    let retry = retry_after
        .map(|value| format!("Retry-After: {value}\r\n"))
        .unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{retry}Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tokio::time::timeout(
        Duration::from_secs(8),
        socket.write_all(response.as_bytes()),
    )
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(8), socket.shutdown()).await;
}

async fn start_scripted_upstream(
    queue: Arc<std::sync::Mutex<std::collections::VecDeque<ScriptedReply>>>,
) -> (
    String,
    Arc<std::sync::Mutex<Vec<String>>>,
    tokio::sync::oneshot::Sender<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
    let journal = Arc::new(std::sync::Mutex::new(Vec::new()));
    let keys = journal.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                accepted = listener.accept() => {
                    let Ok((socket, _)) = accepted else { break };
                    let queue = queue.clone();
                    let journal = journal.clone();
                    tokio::spawn(serve_scripted_connection(socket, queue, journal));
                }
            }
        }
    });
    (format!("http://{address}"), keys, stop_tx)
}

async fn put_policy(
    port: u16,
    state: &CoreStateInner,
    rules: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    dashboard_json(
        port,
        reqwest::Method::PUT,
        "v4",
        "/routing/temporary-unavailability",
        Some(&dashboard_cas(state, serde_json::json!({ "rules": rules }))),
    )
    .await
}

async fn listed_restrictions(port: u16) -> Vec<serde_json::Value> {
    let (status, body) = v4_get(port, "/routing/temporary-unavailability/restrictions").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["restrictions"].as_array().cloned().unwrap_or_default()
}

const SHARED_400: &str = r#"{"error":{"code":"SHARED","message":"shared four hundred"}}"#;
const CODE_A: &str = r#"{"error":{"code":"A_ONLY","message":"rule a"}}"#;
const CODE_B: &str = r#"{"error":{"code":"B_ONLY","message":"rule b"}}"#;

fn two_custom_status_rules() -> serde_json::Value {
    serde_json::json!([
        {
            "kind": "custom",
            "id": "rule-a",
            "destinationId": null,
            "enabled": true,
            "scope": "credential_model",
            "match": { "statusCodes": [400] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        },
        {
            "kind": "custom",
            "id": "rule-b",
            "destinationId": null,
            "enabled": true,
            "scope": "credential_model",
            "match": { "statusCodes": [400] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }
    ])
}

fn code_rules() -> serde_json::Value {
    serde_json::json!([
        {
            "kind": "custom",
            "id": "rule-a",
            "destinationId": null,
            "enabled": true,
            "scope": "credential_model",
            "match": { "errorCodes": ["A_ONLY"] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        },
        {
            "kind": "custom",
            "id": "rule-b",
            "destinationId": null,
            "enabled": true,
            "scope": "credential_model",
            "match": { "errorCodes": ["B_ONLY"] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }
    ])
}

#[tokio::test]
async fn delayed_400_still_records_b_after_a_is_disabled() {
    let mut p =
        PreparedFallback::routing(&[], &["unused"], RoutingMode::StrictPriority, false).await;
    let _clock = clocked(&mut p);
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let (hold, arrived, release) = hold_reply(400, SHARED_400, None);
    queue.lock().unwrap().push_back(hold);
    let (base, _journal, stop) = start_scripted_upstream(queue).await;
    let (ids, _guards) = goats(&p, &["held-key"]);
    let _route = install_goat_loopback_route_for_test(ids[0].clone(), base).unwrap();
    let mut h = p.bind().await;
    h.push_stop(stop);
    let (status, body) = put_policy(h.port, &h.state, two_custom_status_rules()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let port = h.port;
    let pending =
        tokio::spawn(async move { protocol_call(port, "/v1/chat/completions", MODEL).await });
    tokio::time::timeout(Duration::from_secs(3), arrived)
        .await
        .expect("upstream pause")
        .unwrap();
    let (status, body) = put_policy(
        h.port,
        &h.state,
        serde_json::json!([{
            "kind": "custom",
            "id": "rule-b",
            "destinationId": null,
            "enabled": true,
            "scope": "credential_model",
            "match": { "statusCodes": [400] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let _ = release.send(());
    let (status, _) = pending.await.unwrap();
    assert_eq!(status.as_u16(), 400);
    let rows = listed_restrictions(h.port).await;
    assert!(rows.iter().any(|row| row["ruleId"] == "rule-b"), "{rows:?}");
    assert!(rows.iter().all(|row| row["ruleId"] != "rule-a"), "{rows:?}");
}

#[tokio::test]
async fn delayed_400_does_not_record_after_same_id_recreate() {
    let mut p =
        PreparedFallback::routing(&[], &["unused"], RoutingMode::StrictPriority, false).await;
    let _clock = clocked(&mut p);
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let (hold, arrived, release) = hold_reply(400, SHARED_400, None);
    queue.lock().unwrap().push_back(hold);
    let (base, _journal, stop) = start_scripted_upstream(queue).await;
    let (ids, _guards) = goats(&p, &["held-key"]);
    let _route = install_goat_loopback_route_for_test(ids[0].clone(), base).unwrap();
    let mut h = p.bind().await;
    h.push_stop(stop);
    let (status, body) = put_policy(
        h.port,
        &h.state,
        serde_json::json!([{
            "kind": "custom",
            "id": "rule-a",
            "destinationId": null,
            "enabled": true,
            "scope": "credential_model",
            "match": { "statusCodes": [400] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let port = h.port;
    let pending =
        tokio::spawn(async move { protocol_call(port, "/v1/chat/completions", MODEL).await });
    tokio::time::timeout(Duration::from_secs(3), arrived)
        .await
        .expect("upstream pause")
        .unwrap();
    let (status, body) = put_policy(h.port, &h.state, serde_json::json!([])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = put_policy(
        h.port,
        &h.state,
        serde_json::json!([{
            "kind": "custom",
            "id": "rule-a",
            "destinationId": null,
            "enabled": true,
            "scope": "credential_model",
            "match": { "statusCodes": [400] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let _ = release.send(());
    let _ = pending.await.unwrap();
    let rows = listed_restrictions(h.port).await;
    assert!(
        rows.iter().all(|row| row["ruleId"] != "rule-a"),
        "recreated generation must ignore the in-flight response: {rows:?}"
    );
}

#[tokio::test]
async fn delayed_400_does_not_record_after_connection_override() {
    let mut p =
        PreparedFallback::routing(&[], &["unused"], RoutingMode::StrictPriority, false).await;
    let _clock = clocked(&mut p);
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let (hold, arrived, release) = hold_reply(400, SHARED_400, None);
    queue.lock().unwrap().push_back(hold);
    let (base, _journal, stop) = start_scripted_upstream(queue).await;
    let (ids, _guards) = goats(&p, &["held-key"]);
    let dest = ocg_domain::destination::destination_id_for_builtin(COMMAND_CODE_PROVIDER_ID);
    let _route = install_goat_loopback_route_for_test(ids[0].clone(), base).unwrap();
    let mut h = p.bind().await;
    h.push_stop(stop);
    let (status, body) = put_policy(
        h.port,
        &h.state,
        serde_json::json!([{
            "kind": "custom",
            "id": "rule-a",
            "destinationId": null,
            "enabled": true,
            "scope": "credential_model",
            "match": { "statusCodes": [400] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let port = h.port;
    let pending =
        tokio::spawn(async move { protocol_call(port, "/v1/chat/completions", MODEL).await });
    tokio::time::timeout(Duration::from_secs(3), arrived)
        .await
        .expect("upstream pause")
        .unwrap();
    let (status, body) = put_policy(
        h.port,
        &h.state,
        serde_json::json!([
            {
                "kind": "custom",
                "id": "rule-a",
                "destinationId": null,
                "enabled": true,
                "scope": "credential_model",
                "match": { "statusCodes": [400] },
                "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
            },
            {
                "kind": "custom",
                "id": "rule-a",
                "destinationId": dest,
                "enabled": false,
                "scope": "credential_model",
                "match": { "statusCodes": [400] },
                "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
            }
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let _ = release.send(());
    let _ = pending.await.unwrap();
    let rows = listed_restrictions(h.port).await;
    assert!(
        rows.iter().all(|row| row["ruleId"] != "rule-a"),
        "connection override must fence the in-flight dest-A response: {rows:?}"
    );
}

#[tokio::test]
async fn in_flight_success_does_not_clear_b_recorded_after_lease() {
    let mut p =
        PreparedFallback::routing(&[], &["unused"], RoutingMode::StrictPriority, false).await;
    let clock = clocked(&mut p);
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    let (hold_x, arrived_x, release_x) = hold_reply(400, CODE_B, None);
    let (hold_p, arrived_p, release_p) = hold_reply(200, SUCCESS_BODY, None);
    queue.lock().unwrap().push_back(hold_x);
    queue
        .lock()
        .unwrap()
        .push_back(immediate_reply(400, CODE_A, None));
    queue.lock().unwrap().push_back(hold_p);
    let (base, _journal, stop) = start_scripted_upstream(queue).await;
    let (ids, _guards) = goats(&p, &["held-key"]);
    let _route = install_goat_loopback_route_for_test(ids[0].clone(), base).unwrap();
    let mut h = p.bind().await;
    h.push_stop(stop);
    let (status, body) = put_policy(h.port, &h.state, code_rules()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let port = h.port;
    let pending_x =
        tokio::spawn(async move { protocol_call(port, "/v1/chat/completions", MODEL).await });
    tokio::time::timeout(Duration::from_secs(3), arrived_x)
        .await
        .expect("x pause")
        .unwrap();
    let (status, _) = protocol_call(h.port, "/v1/chat/completions", MODEL).await;
    assert_eq!(status.as_u16(), 400);
    clock.store(40, Ordering::SeqCst);
    let port = h.port;
    let pending_p =
        tokio::spawn(async move { protocol_call(port, "/v1/chat/completions", MODEL).await });
    tokio::time::timeout(Duration::from_secs(3), arrived_p)
        .await
        .expect("p pause")
        .unwrap();
    let _ = release_x.send(());
    let _ = pending_x.await.unwrap();
    let _ = release_p.send(());
    let (status, _) = pending_p.await.unwrap();
    assert_eq!(status, StatusCode::OK);
    let rows = listed_restrictions(h.port).await;
    assert!(
        rows.iter().any(|row| row["ruleId"] == "rule-b"),
        "probe success must not clear B recorded after lease: {rows:?}"
    );
}

#[tokio::test]
async fn goat_400_retry_after_survives_local_clear() {
    let mut p = PreparedFallback::routing(&[], &["unused"], RoutingMode::StickyGlobal, false).await;
    let clock = clocked(&mut p);
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    queue
        .lock()
        .unwrap()
        .push_back(immediate_reply(400, CREDITS, Some("600")));
    push_ok(&queue, 4);
    let (base, journal, stop) = start_scripted_upstream(queue).await;
    let (ids, _guards) = goats(&p, &["a", "b"]);
    let _a = install_goat_loopback_route_for_test(ids[0].clone(), base.clone()).unwrap();
    let _b = install_goat_loopback_route_for_test(ids[1].clone(), base).unwrap();
    let mut h = p.bind().await;
    h.push_stop(stop);
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(recorded_keys(&journal), ["a", "b"]);
    let rows = listed_restrictions(h.port).await;
    let id = rows
        .iter()
        .find(|row| row["ruleId"] == "builtin.goat.credits_rejection")
        .expect("goat restriction")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) = dashboard_json(
        h.port,
        reqwest::Method::POST,
        "v4",
        &format!("/routing/temporary-unavailability/restrictions/{id}/clear"),
        Some(&dashboard_cas(&h.state, serde_json::json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    clock.store(40, Ordering::SeqCst);
    succeeds(&h).await;
    assert_eq!(
        recorded_keys(&journal),
        ["a", "b", "b"],
        "Retry-After 600 must keep A skipped after local clear"
    );
    clock.store(600, Ordering::SeqCst);
    succeeds(&h).await;
    assert_eq!(recorded_keys(&journal), ["a", "b", "b", "a"]);
}

#[tokio::test]
async fn custom_503_retry_after_survives_rule_delete() {
    let mut p = PreparedFallback::routing(&[], &["unused"], RoutingMode::StickyGlobal, false).await;
    let clock = clocked(&mut p);
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    queue
        .lock()
        .unwrap()
        .push_back(immediate_reply(503, SHARED_400, Some("600")));
    push_ok(&queue, 3);
    let (base, journal, stop) = start_scripted_upstream(queue).await;
    let (ids, _guards) = goats(&p, &["a", "b"]);
    let _a = install_goat_loopback_route_for_test(ids[0].clone(), base.clone()).unwrap();
    let _b = install_goat_loopback_route_for_test(ids[1].clone(), base).unwrap();
    let mut h = p.bind().await;
    h.push_stop(stop);
    let (status, body) = put_policy(
        h.port,
        &h.state,
        serde_json::json!([{
            "kind": "custom",
            "id": "rule-503",
            "destinationId": null,
            "enabled": true,
            "scope": "credential_model",
            "match": { "statusCodes": [503] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h.protocol("/v1/chat/completions", MODEL).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(recorded_keys(&journal), ["a"]);
    let (status, body) = put_policy(h.port, &h.state, serde_json::json!([])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    clock.store(40, Ordering::SeqCst);
    succeeds(&h).await;
    assert_eq!(
        recorded_keys(&journal),
        ["a", "b"],
        "deleting the custom rule must not shorten Retry-After"
    );
    clock.store(600, Ordering::SeqCst);
    succeeds(&h).await;
    assert_eq!(recorded_keys(&journal), ["a", "b", "a"]);
}

#[tokio::test]
async fn expired_retry_after_admits_concurrent_success() {
    let mut p = PreparedFallback::routing(&[], &["unused"], RoutingMode::StickyGlobal, false).await;
    let clock = clocked(&mut p);
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
    queue
        .lock()
        .unwrap()
        .push_back(immediate_reply(400, CREDITS, Some("5")));
    push_ok(&queue, 1);
    let (hold, arrived, release) = hold_reply(200, SUCCESS_BODY, None);
    queue.lock().unwrap().push_back(hold);
    push_ok(&queue, 2);
    let (base, journal, stop) = start_scripted_upstream(queue).await;
    let (ids, _guards) = goats(&p, &["a", "b"]);
    let _a = install_goat_loopback_route_for_test(ids[0].clone(), base.clone()).unwrap();
    let _b = install_goat_loopback_route_for_test(ids[1].clone(), base).unwrap();
    let mut h = p.bind().await;
    h.push_stop(stop);
    succeeds(&h).await;
    assert_eq!(recorded_keys(&journal), ["a", "b"]);
    let rows = listed_restrictions(h.port).await;
    let id = rows
        .iter()
        .find(|row| row["ruleId"] == "builtin.goat.credits_rejection")
        .expect("goat restriction")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, body) = dashboard_json(
        h.port,
        reqwest::Method::POST,
        "v4",
        &format!("/routing/temporary-unavailability/restrictions/{id}/clear"),
        Some(&dashboard_cas(&h.state, serde_json::json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    clock.store(5, Ordering::SeqCst);
    let port = h.port;
    let pending =
        tokio::spawn(async move { protocol_call(port, "/v1/chat/completions", MODEL).await });
    tokio::time::timeout(Duration::from_secs(3), arrived)
        .await
        .expect("first concurrent request must reach upstream")
        .unwrap();
    let (status, body) = tokio::time::timeout(
        Duration::from_secs(3),
        protocol_call(h.port, "/v1/chat/completions", MODEL),
    )
    .await
    .expect("second request must be accepted while the first is held");
    assert_eq!(status, StatusCode::OK, "{body}");
    let _ = release.send(());
    let (status, body) = pending.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(recorded_keys(&journal), ["a", "b", "a", "a"]);
    let rows = listed_restrictions(h.port).await;
    assert!(rows.is_empty(), "{rows:?}");
}
