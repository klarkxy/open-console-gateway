//! Approved GOAT #81 gateway behavior, plus the supplier 429 that has no `error.code`.
//!
//! The injected wall is `2099-03-15T12:00:00Z`. Every declared reset is an offset from
//! that instant. These tests do not write `.artifacts/issue-verification`.
//!
//! `/accounts` and `/connections` filter with the process clock, so a 2099 deadline
//! stays listed there after this injected clock passes it. Router expiry is proven
//! by a chat request. `error.resets_at` is the soonest deadline; the Retry-After
//! header is computed from the process clock and is not asserted.
use axum::http::StatusCode;
use chrono::{DateTime, SecondsFormat, Utc};
use ocg_core::crypto::StaticKeyCipher;
use ocg_core::dashboard_v3::install_official_protocol_fetch_unavailable_for_tests;
use ocg_core::db::Database;
use ocg_core::gateway::provider_adapter::{
    GoatLoopbackRouteGuard, install_goat_loopback_route_for_test,
};
use ocg_core::goat::install_goat_catalog_origin_for_test;
use ocg_core::models::{Account, RoutingMode, UsageWindowKind};
use ocg_core::provider::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_ALIAS as MODEL, COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS,
    COMMAND_CODE_PROVIDER_ID,
};
use ocg_core::state::CoreStateInner;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "../fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const WALL: &str = "2099-03-15T12:00:00Z";
const SUPPLIER_TRANSIENT: &str = r#"{"error":{"message":"Upstream model provider is temporarily unavailable. Please try again in a moment.","type":"rate_limit_error"}}"#;

struct Clock {
    seconds: Arc<AtomicU64>,
    wall: DateTime<Utc>,
}

struct Scene {
    harness: FallbackHarness,
    clock: Clock,
    ids: Vec<String>,
    _routes: Vec<GoatLoopbackRouteGuard>,
}

struct ScriptedScene {
    scene: Scene,
    queue: Arc<Mutex<VecDeque<ScriptedReply>>>,
    journal: Arc<Mutex<Vec<String>>>,
}

struct ScriptedReply {
    arrived: Option<tokio::sync::oneshot::Sender<()>>,
    release: Option<tokio::sync::oneshot::Receiver<()>>,
    status: u16,
    body: String,
    retry_after: Option<String>,
}

fn fixed_wall() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(WALL)
        .unwrap()
        .with_timezone(&Utc)
}

fn clocked(prepared: &mut PreparedFallback) -> Clock {
    let clock = Clock {
        seconds: Arc::new(AtomicU64::new(0)),
        wall: fixed_wall(),
    };
    prepared.state = state_at(&prepared.dir, &clock, Instant::now());
    clock
}

fn state_at(dir: &Path, clock: &Clock, mono: Instant) -> Arc<CoreStateInner> {
    let wall = clock.wall;
    let wall_offset = clock.seconds.clone();
    let mono_offset = clock.seconds.clone();
    let state = Arc::new(
        CoreStateInner::new_with_test_gateway_clock(
            Database::open(dir.to_path_buf()).unwrap(),
            dir.to_path_buf(),
            Arc::new(StaticKeyCipher::new("test")),
            move || wall + chrono::Duration::seconds(wall_offset.load(Ordering::SeqCst) as i64),
            move || mono + Duration::from_secs(mono_offset.load(Ordering::SeqCst)),
        )
        .unwrap(),
    );
    state
        .usage_sync
        .set_reactive_refresh_enabled_for_test(false);
    state.usage_sync.set_fetch_for_test(|_, _| {
        Box::pin(async { Err(ocg_core::go_usage::GoUsageError::Network) })
    });
    state
}

fn advance(clock: &Clock, seconds: u64) {
    clock.seconds.store(seconds, Ordering::SeqCst);
}

fn parse_stamp(stamp: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(stamp)
        .unwrap()
        .with_timezone(&Utc)
}

fn stamp_of(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}

fn strict_stamp(window: &str, stamp: &str) -> String {
    format!(
        r#"{{"error":{{"code":"RATE_LIMITED","type":"rate_limit_error","message":"You've reached your {window} usage limit for your plan. Your limit resets at {stamp}. Please wait for the window to reset or upgrade your plan to continue."}}}}"#
    )
}

fn strict_at(window: &str, at: DateTime<Utc>) -> &'static str {
    leak(strict_stamp(window, &stamp_of(at)))
}

fn leak(body: String) -> &'static str {
    Box::leak(body.into_boxed_str())
}

fn assert_instant(value: &Value, expected: DateTime<Utc>, label: &str) {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{label} has no timestamp: {value}"));
    let parsed = DateTime::parse_from_rfc3339(text)
        .unwrap_or_else(|error| panic!("{label} {text}: {error}"))
        .with_timezone(&Utc);
    assert_eq!(parsed, expected, "{label}");
    let millis = expected.timestamp_subsec_millis();
    if millis != 0 {
        let marker = format!(".{millis:03}");
        assert!(
            text.contains(&marker),
            "{label} dropped milliseconds: {text}"
        );
    }
}

fn assert_plan_columns_clear(account: &Account, label: &str) {
    assert!(
        account.cooldown_5h_until.is_none(),
        "{label} five-hour column"
    );
    assert!(account.cooldown_week_until.is_none(), "{label} week column");
    assert!(
        account.cooldown_month_until.is_none(),
        "{label} month column"
    );
    assert!(account.cooldown_generic_until.is_none(), "{label} generic");
    assert!(account.cooldown_free_until.is_none(), "{label} free");
    assert!(account.cooldown_until.is_none(), "{label} derived until");
    assert!(account.auth_error.is_none(), "{label} auth");
    assert!(account.last_error.is_none(), "{label} last error");
    assert!(account.enabled, "{label} enabled");
}

fn facts_of(row: &ocg_core::models::ForwardLog) -> Value {
    row.diagnostic
        .as_ref()
        .and_then(|value| value.pointer("/restriction/facts").cloned())
        .unwrap_or(Value::Null)
}

async fn open_scene(label: &str, entries: &[(&str, &[MockReply])]) -> Scene {
    let mut prepared =
        PreparedFallback::routing(entries, &["unused"], RoutingMode::StrictPriority, false).await;
    let clock = clocked(&mut prepared);
    let (ids, routes) = install_goats(
        &prepared.state,
        &prepared.base_url,
        label,
        &entry_keys(entries),
    );
    let harness = prepared.bind().await;
    Scene {
        harness,
        clock,
        ids,
        _routes: routes,
    }
}

async fn open_scripted(label: &str, keys: &[&str]) -> ScriptedScene {
    let mut prepared =
        PreparedFallback::routing(&[], &["unused"], RoutingMode::StrictPriority, false).await;
    let clock = clocked(&mut prepared);
    let queue = Arc::new(Mutex::new(VecDeque::new()));
    let (base, journal, stop) = start_scripted_upstream(queue.clone()).await;
    let (ids, routes) = install_goats(&prepared.state, &base, label, keys);
    let mut harness = prepared.bind().await;
    harness.push_stop(stop);
    ScriptedScene {
        scene: Scene {
            harness,
            clock,
            ids,
            _routes: routes,
        },
        queue,
        journal,
    }
}

fn entry_keys<'a>(entries: &[(&'a str, &[MockReply])]) -> Vec<&'a str> {
    entries.iter().map(|(key, _)| *key).collect()
}

fn install_goats(
    state: &Arc<CoreStateInner>,
    origin: &str,
    label: &str,
    keys: &[&str],
) -> (Vec<String>, Vec<GoatLoopbackRouteGuard>) {
    let mut ids = Vec::new();
    let mut routes = Vec::new();
    for key in keys {
        let id = format!("goat-{label}-{key}-{}", uuid::Uuid::new_v4());
        create_goat_account(state, "acct-1", &id, key);
        routes.push(install_goat_loopback_route_for_test(id.clone(), origin).unwrap());
        ids.push(id);
    }
    set_account_enabled(state, "acct-1", false);

    reorder_first(state, &ids);
    (ids, routes)
}

async fn chat(scene: &Scene) -> (StatusCode, Value) {
    scene.harness.protocol("/v1/chat/completions", MODEL).await
}

fn assert_served(status: StatusCode, body: &Value, label: &str) {
    assert_eq!(status, StatusCode::OK, "{label}: {body}");
}

async fn listed(scene: &Scene, path: &str) -> Value {
    let (status, body) = v4_get(scene.harness.port, path).await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

fn destination_credential<'a>(body: &'a Value, credential_id: &str) -> &'a Value {
    body["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == credential_id)
        .unwrap_or_else(|| panic!("missing credential {credential_id}"))
}

fn account_credential<'a>(body: &'a Value, account_id: &str) -> &'a Value {
    body["identities"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|identity| identity["credentials"].as_array().into_iter().flatten())
        .find(|row| row["legacy"]["id"] == account_id)
        .unwrap_or_else(|| panic!("missing account credential {account_id}"))
}

fn goat_connection(body: &Value) -> &Value {
    body["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["legacy"]["kind"] == "builtin_provider"
                && row["legacy"]["id"] == COMMAND_CODE_PROVIDER_ID
        })
        .expect("command-code connection")
}

fn quota_window<'a>(credential: &'a Value, period: &str) -> Option<&'a Value> {
    credential["quotaWindows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["period"] == period)
}

fn assert_no_quota_recovery(row: &Value, label: &str) {
    assert!(
        row.get("quotaRecovery").is_none(),
        "{label} published quota recovery: {row}"
    );
}

fn rejected_facts(scene: &Scene, account_id: &str) -> Value {
    let row = scene
        .harness
        .logs()
        .into_iter()
        .find(|row| row.http_status == Some(429) && row.account_id == account_id)
        .unwrap_or_else(|| panic!("missing 429 log for {account_id}"));
    let facts = facts_of(&row);
    assert_eq!(row.error_stage.as_deref(), Some("upstream_http"));
    facts
}

fn response_reset(body: &Value) -> DateTime<Utc> {
    let text = body["error"]["resets_at"]
        .as_str()
        .unwrap_or_else(|| panic!("missing resets_at: {body}"));
    parse_stamp(text)
}

async fn reset_cooldown(scene: &Scene, account_id: &str) {
    let (status, body) = v4_mutate(
        scene.harness.port,
        &scene.harness.state,
        &format!("/accounts/{account_id}/reset-cooldown"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn rotate_key(scene: &Scene, account_id: &str, secret: &str) {
    let credential_id = identity_refs_for(&scene.harness.state, account_id).credential_id;
    let (status, body) = v4_mutate(
        scene.harness.port,
        &scene.harness.state,
        &format!("/credentials/{credential_id}/rotate"),
        json!({ "secretInput": secret }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn reopen_scene(scene: Scene, entries: &[(&str, &[MockReply])]) -> Scene {
    let Scene {
        mut harness,
        clock,
        ids,
        _routes,
    } = scene;
    let dir = harness.take_dir();
    drop(harness);
    drop(_routes);
    let state = reopened_state(&dir, &clock).await;
    let (base_url, calls, stop_mock) = start_fake_upstream(script(entries)).await;
    let mut routes = Vec::new();
    for id in &ids {
        routes.push(install_goat_loopback_route_for_test(id.clone(), base_url.clone()).unwrap());
    }
    let harness = FallbackHarness::from_parts(state, dir, calls, Some(stop_mock), None).await;
    Scene {
        harness,
        clock,
        ids,
        _routes: routes,
    }
}

async fn reopened_state(dir: &Path, clock: &Clock) -> Arc<CoreStateInner> {
    let mut last = String::new();
    for _ in 0..20 {
        match Database::open(dir.to_path_buf()) {
            Ok(db) => {
                let wall = clock.wall;
                let wall_offset = clock.seconds.clone();
                let mono_offset = clock.seconds.clone();
                let mono = Instant::now();
                let state = Arc::new(
                    CoreStateInner::new_with_test_gateway_clock(
                        db,
                        dir.to_path_buf(),
                        Arc::new(StaticKeyCipher::new("test")),
                        move || {
                            wall + chrono::Duration::seconds(
                                wall_offset.load(Ordering::SeqCst) as i64
                            )
                        },
                        move || mono + Duration::from_secs(mono_offset.load(Ordering::SeqCst)),
                    )
                    .unwrap(),
                );
                state
                    .usage_sync
                    .set_reactive_refresh_enabled_for_test(false);
                state.usage_sync.set_fetch_for_test(|_, _| {
                    Box::pin(async { Err(ocg_core::go_usage::GoUsageError::Network) })
                });
                return state;
            }
            Err(error) => {
                last = error.to_string();
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    panic!("gateway database did not reopen: {last}");
}

fn at_offset(seconds: i64, millis: i64) -> DateTime<Utc> {
    fixed_wall() + chrono::Duration::seconds(seconds) + chrono::Duration::milliseconds(millis)
}

fn credential_id(scene: &Scene, account_id: &str) -> String {
    identity_refs_for(&scene.harness.state, account_id).credential_id
}

fn assert_sent(scene: &Scene, expected: &[&str]) {
    assert_eq!(
        scene.harness.call_keys(),
        expected
            .iter()
            .copied()
            .map(str::to_string)
            .collect::<Vec<_>>(),
        "upstream keys"
    );
}

fn assert_journal(scene: &ScriptedScene, expected: &[&str]) {
    let keys = scene.journal.lock().unwrap().clone();
    assert_eq!(
        keys,
        expected
            .iter()
            .copied()
            .map(str::to_string)
            .collect::<Vec<_>>(),
        "scripted Authorization keys"
    );
}

fn assert_reset(body: &Value, expected: DateTime<Utc>, label: &str) {
    assert_eq!(response_reset(body), expected, "{label}: {body}");
}

fn assert_cooldown(row: &Value, field: &str, expected: DateTime<Utc>, label: &str) {
    assert_instant(
        &row["cooldowns"][field],
        expected,
        &format!("{label} {field}"),
    );
}

fn assert_cooldown_clear(row: &Value, field: &str, label: &str) {
    assert!(row["cooldowns"][field].is_null(), "{label} {field}: {row}");
}

fn assert_quota_window(
    credential: &Value,
    period: &str,
    credential_id: &str,
    reset: DateTime<Utc>,
    label: &str,
) {
    let window =
        quota_window(credential, period).unwrap_or_else(|| panic!("{label}: {credential}"));
    assert_eq!(window["subject"], "credential", "{label}");
    assert_eq!(window["subjectRef"], credential_id, "{label}");
    assert_instant(&window["blockedUntil"], reset, label);
}

fn assert_plan_facts(facts: &Value, window: &str, reset: DateTime<Utc>) {
    assert_eq!(facts["cause"], "quota_exhausted", "{facts}");
    assert_eq!(facts["scope"], "credential", "{facts}");
    assert_eq!(facts["window"], window, "{facts}");
    assert_eq!(facts["rule_id"], "goat.plan_window", "{facts}");
    assert_eq!(facts["rule_version"], 1, "{facts}");
    assert_instant(&facts["upstream_reset_at"], reset, "upstream_reset_at");
}

fn recorded_for_current(scene: &Scene, account_id: &str) -> bool {
    let row = scene
        .harness
        .logs()
        .into_iter()
        .find(|row| row.http_status == Some(429) && row.account_id == account_id)
        .unwrap_or_else(|| panic!("missing 429 log for {account_id}"));
    row.diagnostic
        .as_ref()
        .and_then(|value| value.pointer("/restriction/recorded_for_current_generation"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn resource_wait_until(scene: &Scene, account_id: &str) -> DateTime<Utc> {
    let row = scene
        .harness
        .logs()
        .into_iter()
        .find(|row| {
            row.account_id == account_id && row.error_stage.as_deref() == Some("resource_wait")
        })
        .unwrap_or_else(|| panic!("missing resource_wait for {account_id}"));
    let text = row
        .diagnostic
        .as_ref()
        .and_then(|value| value.pointer("/restriction/wait/upstream_not_before/at"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("missing wait deadline: {:?}", row.diagnostic));
    parse_stamp(text)
}

fn assert_eligibility(body: &Value, state: &str, reason: &str) {
    let connection = goat_connection(body);
    assert_eq!(connection["eligibility"]["state"], state, "{connection}");
    assert_eq!(connection["eligibility"]["reason"], reason, "{connection}");
}

fn assert_only_ordinary_week(account: &Account, until: DateTime<Utc>, label: &str) {
    assert_eq!(account.cooldown_week_until, Some(until), "{label} week");
    assert_eq!(account.cooldown_until, Some(until), "{label} derived");
    assert!(account.cooldown_5h_until.is_none(), "{label} five-hour");
    assert!(account.cooldown_month_until.is_none(), "{label} month");
    assert!(account.cooldown_generic_until.is_none(), "{label} generic");
    assert!(account.cooldown_free_until.is_none(), "{label} free");
    assert_eq!(
        account.last_error.as_deref(),
        Some("ordinary-week"),
        "{label}"
    );
    assert!(account.auth_error.is_none(), "{label} auth");
    assert!(account.enabled, "{label} enabled");
}

async fn chat_ok(scene: &Scene, label: &str) {
    let (status, body) = chat(scene).await;
    assert_served(status, &body, label);
}

async fn chat_limited(scene: &Scene, expected: DateTime<Utc>, label: &str) {
    let (status, body) = chat(scene).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{label}: {body}");
    assert_reset(&body, expected, label);
}

fn push_step(scene: &ScriptedScene, step: ScriptedReply) {
    scene.queue.lock().unwrap().push_back(step);
}

fn hold(
    status: u16,
    body: String,
    retry_after: Option<String>,
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

fn immediate(status: u16, body: impl Into<String>, retry_after: Option<String>) -> ScriptedReply {
    ScriptedReply {
        arrived: None,
        release: None,
        status,
        body: body.into(),
        retry_after,
    }
}

fn spawn_chat(scene: &Scene) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let port = scene.harness.port;
    tokio::spawn(async move { protocol_call(port, "/v1/chat/completions", MODEL).await })
}

async fn wait_arrived(arrived: tokio::sync::oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(8), arrived)
        .await
        .expect("held upstream did not accept the request")
        .expect("arrival notice dropped");
}

fn command_catalog_body() -> String {
    let mut data: Vec<Value> = COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS
        .iter()
        .map(|id| json!({ "id": id }))
        .collect();
    data.push(json!({ "id": "future-command-model" }));
    json!({ "object": "list", "data": data }).to_string()
}

async fn serve_models(mut socket: tokio::net::TcpStream, body: String) {
    let mut buf = vec![0_u8; 16 * 1024];
    if tokio::time::timeout(Duration::from_secs(8), socket.read(&mut buf))
        .await
        .ok()
        .and_then(Result::ok)
        .is_none()
    {
        return;
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = tokio::time::timeout(
        Duration::from_secs(8),
        socket.write_all(response.as_bytes()),
    )
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(8), socket.shutdown()).await;
}

async fn start_models_origin() -> (String, tokio::sync::oneshot::Sender<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
    let body = command_catalog_body();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                accepted = listener.accept() => {
                    let Ok((socket, _)) = accepted else { break };
                    let body = body.clone();
                    tokio::spawn(serve_models(socket, body));
                }
            }
        }
    });
    (format!("http://{address}"), stop_tx)
}

async fn refresh_command_catalog(scene: &Scene) {
    let path = format!("/provider-contracts/provider/{COMMAND_CODE_PROVIDER_ID}/catalog/refresh");
    let (status, body) =
        v4_mutate(scene.harness.port, &scene.harness.state, &path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let text = body.to_string();
    assert!(
        text.contains("future-command-model"),
        "refresh did not keep the added model: {body}"
    );
    assert!(
        text.contains("deepseek/deepseek-v4-flash"),
        "refresh dropped the routable model: {body}"
    );
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

async fn serve_scripted_connection(
    mut socket: tokio::net::TcpStream,
    queue: Arc<Mutex<VecDeque<ScriptedReply>>>,
    journal: Arc<Mutex<Vec<String>>>,
) {
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
            let _ = tokio::time::timeout(Duration::from_secs(30), release).await;
        }
    }
    let (status, body, retry_after) = match step.as_ref() {
        Some(step) => (step.status, step.body.clone(), step.retry_after.clone()),
        None => (500, "{}".to_string(), None),
    };
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
    queue: Arc<Mutex<VecDeque<ScriptedReply>>>,
) -> (
    String,
    Arc<Mutex<Vec<String>>>,
    tokio::sync::oneshot::Sender<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
    let journal = Arc::new(Mutex::new(Vec::new()));
    let recorded = journal.clone();
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
    (format!("http://{address}"), recorded, stop_tx)
}

#[tokio::test]
async fn weekly_receiving_key_stays_blocked_at_forty_seconds_with_published_deadline() {
    let reset = at_offset(7 * 86_400, 0);
    let scene = open_scene(
        "weekly",
        &[
            ("key-a", &[reply(429, strict_at("weekly", reset))]),
            ("key-b", &[ok()]),
        ],
    )
    .await;
    chat_ok(&scene, "first weekly fallback").await;
    assert_sent(&scene, &["key-a", "key-b"]);
    advance(&scene.clock, 40);
    chat_ok(&scene, "forty seconds still uses B").await;
    assert_sent(&scene, &["key-a", "key-b", "key-b"]);

    let credentials = listed(&scene, "/credentials").await;
    let id_a = credential_id(&scene, &scene.ids[0]);
    let id_b = credential_id(&scene, &scene.ids[1]);
    let row_a = destination_credential(&credentials, &id_a);
    let row_b = destination_credential(&credentials, &id_b);
    assert_cooldown(row_a, "weekUntil", reset, "A");
    assert_cooldown_clear(row_a, "fiveHourUntil", "A");
    assert_cooldown_clear(row_a, "monthUntil", "A");
    assert_no_quota_recovery(row_a, "A");
    assert_cooldown_clear(row_b, "weekUntil", "B");
    assert_cooldown_clear(row_b, "fiveHourUntil", "B");
    assert_cooldown_clear(row_b, "monthUntil", "B");
    assert_no_quota_recovery(row_b, "B");
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[0]), "A");
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[1]), "B");

    let accounts = listed(&scene, "/accounts").await;
    let summary_a = account_credential(&accounts, &scene.ids[0]);
    let summary_b = account_credential(&accounts, &scene.ids[1]);
    assert_quota_window(summary_a, "week", &id_a, reset, "A week window");
    assert!(quota_window(summary_b, "week").is_none(), "{summary_b}");
    assert_plan_facts(&rejected_facts(&scene, &scene.ids[0]), "week", reset);
    assert!(recorded_for_current(&scene, &scene.ids[0]));

    let connections = listed(&scene, "/connections").await;
    assert_eligibility(&connections, "eligible", "none");
    scene.harness.set_enabled(&scene.ids[1], false);
    let connections = listed(&scene, "/connections").await;
    assert_eligibility(&connections, "cooling", "cooling");
}

#[tokio::test]
async fn short_declared_reset_has_no_synthetic_thirty_second_floor() {
    let reset = at_offset(10, 0);
    let scene = open_scene(
        "short",
        &[
            ("key-a", &[reply(429, strict_at("weekly", reset)), ok()]),
            ("key-b", &[ok()]),
        ],
    )
    .await;
    chat_ok(&scene, "short reset falls through to B").await;
    advance(&scene.clock, 9);
    chat_ok(&scene, "nine seconds still uses B").await;
    assert_sent(&scene, &["key-a", "key-b", "key-b"]);
    scene.harness.set_enabled(&scene.ids[1], false);
    chat_limited(&scene, reset, "nine seconds is not a thirty-second floor").await;
    assert_sent(&scene, &["key-a", "key-b", "key-b"]);
    advance(&scene.clock, 11);
    chat_ok(&scene, "eleven seconds sends A again").await;
    assert_sent(&scene, &["key-a", "key-b", "key-b", "key-a"]);
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[0]), "A");
}

#[tokio::test]
async fn plan_fields_keep_exact_instants_and_routing_uses_the_later_deadline() {
    let five_stamp = "2099-03-15T20:00:00.241+08:00";
    let month_stamp = "2099-03-17T21:00:00.500+09:00";
    let five = parse_stamp(five_stamp);
    let month = parse_stamp(month_stamp);
    let week_9d = at_offset(9 * 86_400, 500);
    let ordinary = at_offset(15 * 86_400, 0);
    let scripted = open_scripted("exact", &["key-a", "key-b"]).await;
    let scene = &scripted.scene;
    let id_a = credential_id(scene, &scene.ids[0]);
    let id_b = credential_id(scene, &scene.ids[1]);

    // Authorize every send before any rejection. Later replies can still
    // contribute evidence while the receiving Key is already cooling.
    let responses = [
        strict_stamp("5-hour", five_stamp),
        strict_stamp("weekly", &stamp_of(at_offset(3 * 86_400, 0))),
        strict_stamp("weekly", &stamp_of(week_9d)),
        strict_stamp("weekly", &stamp_of(at_offset(86_400, 0))),
        strict_stamp("monthly", month_stamp),
    ];
    let mut pending = Vec::new();
    for body in responses {
        let (held, arrived, release) = hold(429, body, None);
        push_step(&scripted, held);
        let request = spawn_chat(scene);
        wait_arrived(arrived).await;
        pending.push((release, request));
    }
    for _ in 0..8 {
        push_step(&scripted, immediate(200, SUCCESS_BODY, None));
    }
    for (release, request) in pending {
        release.send(()).unwrap();
        let (status, body) = request.await.unwrap();
        assert_served(status, &body, "concurrent rejection falls through to B");
    }
    let credentials = listed(scene, "/credentials").await;
    let row_a = destination_credential(&credentials, &id_a);
    let row_b = destination_credential(&credentials, &id_b);
    assert_cooldown(row_a, "fiveHourUntil", five, "five-hour");
    assert_cooldown(
        row_a,
        "weekUntil",
        week_9d,
        "shorter reply preserves raised week",
    );
    assert_cooldown(row_a, "monthUntil", month, "month");
    assert_cooldown_clear(row_b, "weekUntil", "B");
    assert_no_quota_recovery(row_a, "A");
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[0]), "A map");
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[1]), "B map");
    let accounts = listed(scene, "/accounts").await;
    let summary_a = account_credential(&accounts, &scene.ids[0]);
    assert_quota_window(summary_a, "five_hours", &id_a, five, "five-hour window");
    assert_quota_window(summary_a, "week", &id_a, week_9d, "week window");
    assert_quota_window(summary_a, "month", &id_a, month, "month window");
    let mut expected = vec!["key-a"; 5];
    expected.extend(["key-b"; 5]);
    assert_journal(&scripted, &expected);

    advance(&scene.clock, 4 * 3_600);
    chat_ok(scene, "four hours still skips A").await;
    expected.push("key-b");
    assert_journal(&scripted, &expected);
    scene
        .harness
        .state
        .db
        .lock()
        .set_account_rate_limit(
            &scene.ids[0],
            ordinary,
            "ordinary-week",
            Some(UsageWindowKind::Week),
        )
        .unwrap();
    assert_only_ordinary_week(
        &scene.harness.account(&scene.ids[0]),
        ordinary,
        "A ordinary",
    );
    let credentials = listed(scene, "/credentials").await;
    let row_a = destination_credential(&credentials, &id_a);
    assert_cooldown(row_a, "weekUntil", ordinary, "effective week");
    assert_cooldown(row_a, "fiveHourUntil", five, "local five-hour remains");
    assert_cooldown(row_a, "monthUntil", month, "local month remains");
    assert!(
        scene
            .harness
            .account(&scene.ids[1])
            .cooldown_week_until
            .is_none()
    );
    advance(&scene.clock, 10 * 86_400);
    chat_ok(
        scene,
        "ordinary week still blocks after local windows expire",
    )
    .await;
    expected.push("key-b");
    assert_journal(&scripted, &expected);
    scene.harness.set_enabled(&scene.ids[1], false);
    advance(&scene.clock, 16 * 86_400);
    chat_ok(scene, "all blockers expired and A becomes eligible").await;
    expected.push("key-a");
    assert_journal(&scripted, &expected);
}

#[tokio::test]
async fn reopened_gateway_keeps_the_plan_window_until_it_expires() {
    let reset = at_offset(60, 0);
    let scene = open_scene(
        "reopen",
        &[
            ("key-a", &[reply(429, strict_at("weekly", reset))]),
            ("key-b", &[ok()]),
        ],
    )
    .await;
    let id_a = credential_id(&scene, &scene.ids[0]);
    chat_ok(&scene, "record the minute window").await;
    advance(&scene.clock, 10);
    chat_ok(&scene, "before reopen A is skipped").await;
    assert_sent(&scene, &["key-a", "key-b", "key-b"]);

    let scene = reopen_scene(scene, &[("key-a", &[ok()]), ("key-b", &[ok()])]).await;
    let credentials = listed(&scene, "/credentials").await;
    assert_cooldown(
        destination_credential(&credentials, &id_a),
        "weekUntil",
        reset,
        "reopened",
    );
    chat_ok(&scene, "reopened gateway still skips A").await;
    assert_sent(&scene, &["key-b"]);
    scene.harness.set_enabled(&scene.ids[1], false);
    chat_limited(&scene, reset, "reopened known deadline").await;
    assert_sent(&scene, &["key-b"]);
    advance(&scene.clock, 61);
    chat_ok(&scene, "natural expiry sends A").await;
    assert_sent(&scene, &["key-b", "key-a"]);
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[0]), "reopened A");
}

#[tokio::test]
async fn all_waiting_keys_report_the_soonest_deadline_without_another_send() {
    let soon = at_offset(3_600, 0);
    let later = at_offset(3 * 3_600, 0);
    let scene = open_scene(
        "soonest",
        &[
            ("key-a", &[reply(429, strict_at("weekly", soon))]),
            ("key-b", &[reply(429, strict_at("monthly", later))]),
        ],
    )
    .await;
    chat_limited(&scene, soon, "soonest of the two plan windows").await;
    assert_sent(&scene, &["key-a", "key-b"]);
    chat_limited(&scene, soon, "second request does not send").await;
    assert_sent(&scene, &["key-a", "key-b"]);
    let credentials = listed(&scene, "/credentials").await;
    assert_cooldown(
        destination_credential(&credentials, &credential_id(&scene, &scene.ids[0])),
        "weekUntil",
        soon,
        "A",
    );
    assert_cooldown(
        destination_credential(&credentials, &credential_id(&scene, &scene.ids[1])),
        "monthUntil",
        later,
        "B",
    );
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[0]), "A");
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[1]), "B");
}

#[tokio::test]
async fn delayed_reply_after_same_key_reset_does_not_repopulate_the_map() {
    let reset = at_offset(7 * 86_400, 0);
    let scene = open_scripted("reset-late", &["key-a"]).await;
    let (held, arrived, release) = hold(429, strict_stamp("weekly", &stamp_of(reset)), None);
    push_step(&scene, held);
    push_step(&scene, immediate(200, SUCCESS_BODY, None));
    let request = spawn_chat(&scene.scene);
    wait_arrived(arrived).await;
    reset_cooldown(&scene.scene, &scene.scene.ids[0]).await;
    release.send(()).unwrap();
    let (_status, _body) = request.await.unwrap();
    let id_a = credential_id(&scene.scene, &scene.scene.ids[0]);
    let credentials = listed(&scene.scene, "/credentials").await;
    assert_cooldown_clear(
        destination_credential(&credentials, &id_a),
        "weekUntil",
        "reset",
    );
    assert_plan_columns_clear(&scene.scene.harness.account(&scene.scene.ids[0]), "reset");
    chat_ok(&scene.scene, "A is eligible after the rejected late reply").await;
    assert_journal(&scene, &["key-a", "key-a"]);
}

#[tokio::test]
async fn delayed_reply_after_same_key_rotation_does_not_repopulate_the_map() {
    let reset = at_offset(7 * 86_400, 0);
    let scene = open_scripted("rotate-late", &["key-a"]).await;
    let (held, arrived, release) = hold(429, strict_stamp("weekly", &stamp_of(reset)), None);
    push_step(&scene, held);
    push_step(&scene, immediate(200, SUCCESS_BODY, None));
    let request = spawn_chat(&scene.scene);
    wait_arrived(arrived).await;
    rotate_key(&scene.scene, &scene.scene.ids[0], "rotated-a-key").await;
    release.send(()).unwrap();
    let (_status, _body) = request.await.unwrap();
    let id_a = credential_id(&scene.scene, &scene.scene.ids[0]);
    let credentials = listed(&scene.scene, "/credentials").await;
    assert_cooldown_clear(
        destination_credential(&credentials, &id_a),
        "weekUntil",
        "rotated",
    );
    chat_ok(&scene.scene, "rotated key is sent").await;
    assert_journal(&scene, &["key-a", "rotated-a-key"]);
    assert_cooldown_clear(
        destination_credential(&listed(&scene.scene, "/credentials").await, &id_a),
        "weekUntil",
        "rotated after send",
    );
}

#[tokio::test]
async fn sibling_reset_and_rotation_keep_the_other_keys_plan_window() {
    let reset = at_offset(7 * 86_400, 0);
    let scene = open_scene(
        "sibling",
        &[
            ("key-a", &[reply(429, strict_at("weekly", reset))]),
            ("key-b", &[ok()]),
        ],
    )
    .await;
    let id_a = credential_id(&scene, &scene.ids[0]);
    let id_b = credential_id(&scene, &scene.ids[1]);
    chat_ok(&scene, "record A only").await;
    reset_cooldown(&scene, &scene.ids[1]).await;
    let credentials = listed(&scene, "/credentials").await;
    assert_cooldown(
        destination_credential(&credentials, &id_a),
        "weekUntil",
        reset,
        "A after B reset",
    );
    assert_cooldown_clear(
        destination_credential(&credentials, &id_b),
        "weekUntil",
        "B after its reset",
    );
    chat_ok(&scene, "A stays blocked on the original B key").await;
    assert_sent(&scene, &["key-a", "key-b", "key-b"]);
    rotate_key(&scene, &scene.ids[1], "rotated-b-key").await;
    let credentials = listed(&scene, "/credentials").await;
    assert_cooldown(
        destination_credential(&credentials, &id_a),
        "weekUntil",
        reset,
        "A after B rotation",
    );
    assert_cooldown_clear(
        destination_credential(&credentials, &id_b),
        "weekUntil",
        "B after its rotation",
    );
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[0]), "A");
}

#[tokio::test]
async fn retry_after_outlives_short_plan_window_across_catalog_refresh() {
    let plan_reset = at_offset(10, 0);
    let retry_until = at_offset(300, 0);
    let mut scene = open_scripted("retry-after", &["key-a"]).await;
    let (origin, stop_models) = start_models_origin().await;
    scene.scene.harness.push_stop(stop_models);
    let _catalog = install_goat_catalog_origin_for_test(
        scene.scene.harness.state.process_generation(),
        origin,
    )
    .unwrap();
    let _protocols = install_official_protocol_fetch_unavailable_for_tests(
        scene.scene.harness.state.process_generation(),
    );
    let (held, arrived, release) = hold(
        429,
        strict_stamp("weekly", &stamp_of(plan_reset)),
        Some("300".to_string()),
    );
    push_step(&scene, held);
    push_step(&scene, immediate(200, SUCCESS_BODY, None));
    let request = spawn_chat(&scene.scene);
    wait_arrived(arrived).await;
    refresh_command_catalog(&scene.scene).await;
    release.send(()).unwrap();
    let (status, body) = request.await.unwrap();
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_reset(&body, retry_until, "first reply waits out Retry-After");
    assert_journal(&scene, &["key-a"]);

    let id_a = credential_id(&scene.scene, &scene.scene.ids[0]);
    let credentials = listed(&scene.scene, "/credentials").await;
    let row = destination_credential(&credentials, &id_a);
    assert_cooldown(
        row,
        "weekUntil",
        plan_reset,
        "plan window is not Retry-After",
    );
    assert_no_quota_recovery(row, "A");
    assert_plan_columns_clear(&scene.scene.harness.account(&scene.scene.ids[0]), "A");
    let facts = rejected_facts(&scene.scene, &scene.scene.ids[0]);
    assert_plan_facts(&facts, "week", plan_reset);
    assert_instant(&facts["retry_not_before"]["at"], retry_until, "retry");
    assert_eq!(facts["retry_not_before"]["kind"], "until");
    assert!(recorded_for_current(&scene.scene, &scene.scene.ids[0]));

    refresh_command_catalog(&scene.scene).await;
    advance(&scene.scene.clock, 11);
    chat_limited(
        &scene.scene,
        retry_until,
        "eleven seconds stays blocked until Retry-After",
    )
    .await;
    assert_journal(&scene, &["key-a"]);
    advance(&scene.scene.clock, 301);
    chat_ok(&scene.scene, "after Retry-After A is sent").await;
    assert_journal(&scene, &["key-a", "key-a"]);
}

#[tokio::test]
async fn supplier_429_without_error_code_is_the_same_temporary_wait_not_account_quota() {
    let scene = open_scene(
        "supplier",
        &[
            ("key-a", &[reply(429, SUPPLIER_TRANSIENT)]),
            ("key-b", &[ok()]),
        ],
    )
    .await;
    chat_ok(&scene, "supplier fallback").await;
    assert_sent(&scene, &["key-a", "key-b"]);
    let facts = rejected_facts(&scene, &scene.ids[0]);
    assert_eq!(facts["cause"], "transient", "{facts}");
    assert_eq!(facts["scope"], "unspecified", "{facts}");
    assert_eq!(facts["rule_id"], "http.429.temporary", "{facts}");
    assert!(facts["window"].is_null(), "{facts}");
    assert!(recorded_for_current(&scene, &scene.ids[0]));
    assert_plan_columns_clear(&scene.harness.account(&scene.ids[0]), "supplier");
    let credentials = listed(&scene, "/credentials").await;
    assert_cooldown_clear(
        destination_credential(&credentials, &credential_id(&scene, &scene.ids[0])),
        "weekUntil",
        "supplier",
    );

    advance(&scene.clock, 29);
    chat_ok(&scene, "twenty-nine seconds still skips A").await;
    assert_sent(&scene, &["key-a", "key-b", "key-b"]);
    assert_eq!(resource_wait_until(&scene, &scene.ids[0]), at_offset(30, 0));
    advance(&scene.clock, 40);
    chat_ok(&scene, "forty seconds sends A again").await;
    assert_sent(&scene, &["key-a", "key-b", "key-b", "key-a", "key-b"]);
}
