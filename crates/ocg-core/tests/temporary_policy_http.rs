//! V4 temporary-unavailability HTTP integration: router, session middleware,
//! CAS, and the real gateway forwarder. Decisions are observed from live
//! upstream replies, not handler calls or hand-built recovery permits.
//!
//! This file only adds tests. It does not change production seams.

use axum::Router;
use axum::body::Body;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use ocg_core::crypto::StaticKeyCipher;
use ocg_core::db::Database;
use ocg_core::models::RoutingMode;
use ocg_core::state::CoreStateInner;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

const POLICY: &str = "/routing/temporary-unavailability";
const RESTRICTIONS: &str = "/routing/temporary-unavailability/restrictions";
const MODEL_A: &str = "tp-a-primary";
const MODEL_A_OTHER: &str = "tp-a-other";
const MODEL_B: &str = "tp-b";
const UP_A: &str = "vendor/tp-a-primary";
const UP_A_OTHER: &str = "vendor/tp-a-other";
const UP_B: &str = "vendor/tp-b";
const KEY_A: &str = "dummy-tp-a";
const KEY_B: &str = "dummy-tp-b";
const KEY_SOLO: &str = "dummy-tp-solo";
const MODEL_SHARED: &str = "tp-shared";
const UP_SHARED: &str = "vendor/tp-shared";
const NOTIFY_BOUND: Duration = Duration::from_secs(8);
const BODY_400: &str = r#"{"error":{"code":"BAD_REQUEST","message":"temporary policy 400","type":"invalid_request_error"}}"#;
const BODY_400_ALPHA: &str = r#"{"error":{"code":"BAD_REQUEST","message":"alpha-only temporary","type":"invalid_request_error"}}"#;
const BODY_400_BETA: &str = r#"{"error":{"code":"BAD_REQUEST","message":"beta-only temporary","type":"invalid_request_error"}}"#;
const BODY_503: &str = r#"{"error":{"message":"upstream overloaded","type":"api_error"}}"#;

#[derive(Clone)]
struct ScriptedReply {
    status: u16,
    body: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    hold: Option<(Arc<Notify>, Arc<Notify>)>,
}

impl ScriptedReply {
    fn immediate(status: u16, body: &'static str) -> Self {
        Self {
            status,
            body,
            headers: Vec::new(),
            hold: None,
        }
    }

    fn held(status: u16, body: &'static str, arrived: Arc<Notify>, release: Arc<Notify>) -> Self {
        Self {
            status,
            body,
            headers: Vec::new(),
            hold: Some((arrived, release)),
        }
    }

    fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }
}

#[derive(Clone)]
struct ScriptedState {
    scripts: Arc<Mutex<HashMap<String, VecDeque<ScriptedReply>>>>,
    calls: Arc<AtomicUsize>,
    default: ScriptedReply,
}

struct ScriptedUpstream {
    url: String,
    calls: Arc<AtomicUsize>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for ScriptedUpstream {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn start_scripted_upstream(
    scripts: HashMap<String, VecDeque<ScriptedReply>>,
    default: ScriptedReply,
) -> ScriptedUpstream {
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .fallback(any(scripted_reply))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(ScriptedState {
            scripts: Arc::new(Mutex::new(scripts)),
            calls: calls.clone(),
            default,
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("scripted upstream should bind");
    let address = listener.local_addr().expect("scripted listener address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        let _ = server.await;
    });
    ScriptedUpstream {
        url: format!("http://{address}"),
        calls,
        stop: Some(shutdown_tx),
    }
}

async fn scripted_reply(
    State(state): State<ScriptedState>,
    uri: OriginalUri,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let _ = body;
    let authorization = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let key = authorization
        .strip_prefix("Bearer ")
        .unwrap_or(authorization)
        .to_string();
    let path = uri.path().to_owned();
    if method == Method::GET && path == "/" && key.is_empty() {
        return (StatusCode::NOT_FOUND, "{}").into_response();
    }
    state.calls.fetch_add(1, Ordering::SeqCst);
    let reply = {
        let mut scripts = state.scripts.lock().expect("script queue");
        scripts
            .get_mut(&key)
            .and_then(VecDeque::pop_front)
            .unwrap_or_else(|| state.default.clone())
    };
    if let Some((arrived, release)) = reply.hold {
        arrived.notify_one();
        let _ = tokio::time::timeout(NOTIFY_BOUND, release.notified()).await;
    }
    let mut builder = Response::builder().status(reply.status);
    builder = builder.header(axum::http::header::CONTENT_TYPE, "application/json");
    for (name, value) in reply.headers {
        builder = builder.header(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    builder
        .body(Body::from(reply.body))
        .expect("scripted response")
}

async fn spawn_held_protocol(
    port: u16,
    model: &'static str,
    arrived: &Notify,
) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    let waiting = arrived.notified();
    tokio::pin!(waiting);
    let pending =
        tokio::spawn(async move { protocol_call(port, "/v1/chat/completions", model).await });
    if tokio::time::timeout(NOTIFY_BOUND, waiting).await.is_err() {
        pending.abort();
        panic!("upstream must see the in-flight request without a sleep barrier");
    }
    pending
}

async fn join_protocol(
    pending: tokio::task::JoinHandle<(StatusCode, Value)>,
    what: &'static str,
) -> (StatusCode, Value) {
    match tokio::time::timeout(NOTIFY_BOUND, pending).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => panic!("{what}: client task panicked: {error}"),
        Err(_) => panic!("{what}: client join exceeded {NOTIFY_BOUND:?}"),
    }
}

async fn clocked_lab() -> (FallbackHarness, Arc<AtomicU64>) {
    let (state, dir) = build_state_with_routing(
        "http://127.0.0.1:1".into(),
        &[],
        RoutingMode::StrictPriority,
        false,
    );
    drop(state);
    let seconds = Arc::new(AtomicU64::new(0));
    let wall = chrono::Utc::now();
    let mono = Instant::now();
    let wall_ticks = seconds.clone();
    let mono_ticks = seconds.clone();
    let state = Arc::new(
        CoreStateInner::new_with_test_gateway_clock(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("test")),
            move || wall + chrono::Duration::seconds(wall_ticks.load(Ordering::SeqCst) as i64),
            move || mono + Duration::from_secs(mono_ticks.load(Ordering::SeqCst)),
        )
        .unwrap(),
    );
    state
        .usage_sync
        .set_reactive_refresh_enabled_for_test(false);
    state.usage_sync.set_fetch_for_test(|_, _| {
        Box::pin(async { Err(ocg_core::go_usage::GoUsageError::Network) })
    });
    (FallbackHarness::from_state(state, dir).await, seconds)
}

async fn v4_put(
    port: u16,
    state: &CoreStateInner,
    path: &str,
    patch: Value,
) -> (StatusCode, Value) {
    dashboard_json(
        port,
        reqwest::Method::PUT,
        "v4",
        path,
        Some(&dashboard_cas(state, patch)),
    )
    .await
}

async fn v4_post(
    port: u16,
    state: &CoreStateInner,
    path: &str,
    patch: Value,
) -> (StatusCode, Value) {
    dashboard_json(
        port,
        reqwest::Method::POST,
        "v4",
        path,
        Some(&dashboard_cas(state, patch)),
    )
    .await
}

fn custom_rule(
    id: &str,
    destination_id: Option<&str>,
    scope: &str,
    status: u16,
    initial: u64,
    max: u64,
) -> Value {
    json!({
        "kind": "custom",
        "id": id,
        "destinationId": destination_id,
        "enabled": true,
        "scope": scope,
        "match": { "statusCodes": [status] },
        "backoff": { "initialSeconds": initial, "maxSeconds": max }
    })
}

fn custom_message_rule(id: &str, needle: &str) -> Value {
    json!({
        "kind": "custom",
        "id": id,
        "destinationId": Value::Null,
        "enabled": true,
        "scope": "credential_model",
        "match": { "statusCodes": [400], "messageContains": [needle] },
        "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
    })
}

async fn put_rules(h: &FallbackHarness, rules: Value) -> (StatusCode, Value) {
    v4_put(h.port, &h.state, POLICY, json!({ "rules": rules })).await
}

async fn get_policy(h: &FallbackHarness) -> Value {
    let (status, body) = v4_get(h.port, POLICY).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

async fn get_restrictions(h: &FallbackHarness) -> Value {
    let (status, body) = v4_get(h.port, RESTRICTIONS).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn restriction_rows(body: &Value) -> &[Value] {
    body["restrictions"].as_array().expect("restrictions array")
}

fn rule_ids(body: &Value) -> Vec<&str> {
    restriction_rows(body)
        .iter()
        .filter_map(|row| row["ruleId"].as_str())
        .collect()
}

fn destination_id_named(body: &Value, name: &str) -> String {
    body["destinations"]
        .as_array()
        .expect("destinations")
        .iter()
        .find(|row| row["name"] == name)
        .and_then(|row| row["id"].as_str())
        .unwrap_or_else(|| panic!("destination `{name}` missing: {body}"))
        .to_string()
}

struct CreatedLab {
    dest_id: String,
    account_id: String,
}

fn prefer_account_order(state: &CoreStateInner, first: &[&str]) {
    let mut rest: Vec<String> = state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|account| account.id)
        .collect();
    let mut ordered = Vec::new();
    for id in first {
        if let Some(index) = rest.iter().position(|item| item == id) {
            ordered.push(rest.remove(index));
        }
    }
    ordered.extend(rest);
    state
        .db
        .lock()
        .reorder_accounts(&ordered)
        .expect("test account order must cover the live account set");
}

fn last_retry_action(h: &FallbackHarness) -> Option<String> {
    h.logs()
        .last()
        .and_then(|row| row.diagnostic.as_ref())
        .and_then(|diagnostic| diagnostic.get("retry_action"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

async fn create_http_lab(
    h: &FallbackHarness,
    name: &str,
    endpoint: &str,
    key: &str,
    models: &[(&str, &str)],
) -> CreatedLab {
    let (status, created) = v4_mutate(
        h.port,
        &h.state,
        "/providers",
        json!({
            "name": name,
            "endpointUrl": endpoint,
            "upstreamProtocol": "chat_completions",
            "authKind": "bearer",
            "key": key,
            "models": models.iter().map(|(public, upstream)| json!({
                "publicModel": public,
                "upstreamModel": upstream
            })).collect::<Vec<_>>()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created provider id missing: {created}"))
        .to_string();
    let account = h
        .state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == provider_id);
    let account_id = account
        .as_ref()
        .map(|account| account.id.clone())
        .unwrap_or_else(|| panic!("account for provider {provider_id} missing: {created}"));
    if account.is_some_and(|account| !account.enabled) {
        let (status, body) = v4_mutate(
            h.port,
            &h.state,
            &format!("/accounts/{account_id}/toggle"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, destinations) = v4_get(h.port, "/destinations").await;
    assert_eq!(status, StatusCode::OK, "{destinations}");
    CreatedLab {
        dest_id: destination_id_named(&destinations, name),
        account_id,
    }
}

fn assert_raw_destination_id(config: &Value, rule_id: &str, expected: &str) {
    let rules = config["rules"].as_array().expect("rules");
    let rule = rules
        .iter()
        .find(|rule| rule["id"] == rule_id)
        .unwrap_or_else(|| panic!("rule `{rule_id}` missing: {config}"));
    assert!(
        rule.get("destination_id").is_none(),
        "wire JSON must not emit snake_case destination_id: {rule}"
    );
    assert_eq!(
        rule["destinationId"], expected,
        "destinationId must stay on the one connection, not fall to global: {rule}"
    );
}

/// Group 1: raw JSON destinationId, isolation, reject-without-write, CAS, auth.
#[tokio::test]
async fn destination_scoped_put_get_and_isolation_go_through_v4_http() {
    let journal = SharedJournal::new();
    let lab_a = start_journaled_lab(
        &journal,
        "tp-a",
        KEY_A,
        &[reply(400, BODY_400), ok(), ok(), ok(), ok()],
    )
    .await;
    let lab_b = start_journaled_lab(
        &journal,
        "tp-b",
        KEY_B,
        &[ok(), reply(400, BODY_400), ok(), ok()],
    )
    .await;
    let (state, dir) = build_state_with_routing(
        "http://127.0.0.1:1".into(),
        &[],
        RoutingMode::StrictPriority,
        false,
    );
    let mut h = FallbackHarness::from_state(state, dir).await;
    let dest_a = create_http_lab(
        &h,
        "oneconnection-a",
        &lab_a.url,
        KEY_A,
        &[(MODEL_A, UP_A), (MODEL_A_OTHER, UP_A_OTHER)],
    )
    .await
    .dest_id;
    let dest_b = create_http_lab(&h, "oneconnection-b", &lab_b.url, KEY_B, &[(MODEL_B, UP_B)])
        .await
        .dest_id;
    h.push_stop(lab_a.stop);
    h.push_stop(lab_b.stop);
    assert_ne!(dest_a, dest_b);

    let before = get_policy(&h).await;
    let (status, rejected) = v4_put(
        h.port,
        &h.state,
        POLICY,
        json!({
            "rules": [{
                "kind": "custom",
                "id": "status-400",
                "destinationId": dest_a,
                "enabled": true,
                "scope": "credential_model",
                "match": { "statusCodes": [400] },
                "backoff": { "initialSeconds": 30, "maxSeconds": 300 },
                "unexpectedField": true
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(get_policy(&h).await["rules"], before["rules"]);

    let (status, rejected) = put_rules(
        &h,
        json!([{
            "kind": "custom",
            "id": "empty-set",
            "destinationId": dest_a,
            "enabled": true,
            "scope": "credential_model",
            "match": { "statusCodes": [] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(get_policy(&h).await["rules"], before["rules"]);

    let (status, stale) = dashboard_json(
        h.port,
        reqwest::Method::PUT,
        "v4",
        POLICY,
        Some(&json!({
            "expectedRevision": 0,
            "processGeneration": h.state.process_generation(),
            "rules": []
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");
    assert_eq!(stale["code"], "revisionConflict");
    assert_eq!(get_policy(&h).await["rules"], before["rules"]);

    let (status, committed) = put_rules(
        &h,
        json!([custom_rule(
            "status-400",
            Some(&dest_a),
            "credential_model",
            400,
            30,
            300
        )]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");
    assert_raw_destination_id(&committed, "status-400", &dest_a);
    let fetched = get_policy(&h).await;
    assert_raw_destination_id(&fetched, "status-400", &dest_a);
    assert_eq!(fetched["rules"].as_array().map(Vec::len), Some(1));

    let (status, body) = h.protocol("/v1/chat/completions", MODEL_A).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let listed = get_restrictions(&h).await;
    let rows = restriction_rows(&listed);
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["ruleId"], "status-400");
    assert_eq!(rows[0]["destinationId"], dest_a);
    assert_eq!(rows[0]["source"], "connection");
    assert_eq!(rows[0]["scope"], "credential_model");
    assert_eq!(rows[0]["upstreamModel"], UP_A);
    assert_eq!(rows[0]["state"], "waiting");

    let before_b = journal.keys();
    let (status, body) = h.protocol("/v1/chat/completions", MODEL_A_OTHER).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = h.protocol("/v1/chat/completions", MODEL_B).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        journal.keys().iter().any(|key| key == KEY_B),
        "destination B must remain independently usable: {before_b:?} -> {:?}",
        journal.keys()
    );

    let skipped_before = journal.keys().len();
    let (status, _) = h.protocol("/v1/chat/completions", MODEL_A).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "restricted model A must not succeed"
    );
    assert_eq!(
        journal.keys().len(),
        skipped_before,
        "a waiting dest-A model must not send again: {:?}",
        journal.keys()
    );
    assert!(
        h.logs()
            .iter()
            .any(|row| row.error_stage.as_deref() == Some("local_policy_skip")),
        "dest A skip must be local_policy_skip: {:?}",
        h.logs()
    );

    let (status, body) = h.protocol("/v1/chat/completions", MODEL_B).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let after_b_400 = get_restrictions(&h).await;
    assert!(
        restriction_rows(&after_b_400)
            .iter()
            .all(|row| row["destinationId"] == dest_a),
        "a dest-A-only rule must not record dest B after dest B's own 400: {after_b_400}"
    );
    let (status, body) = h.protocol("/v1/chat/completions", MODEL_B).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    h.state.set_dashboard_local_mode(false);
    for path in [POLICY, RESTRICTIONS] {
        let (status, body) = v4_get(h.port, path).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} {body}");
        assert_eq!(body["code"], "unauthorized", "{path} {body}");
    }
    let (status, body) = dashboard_json(
        h.port,
        reqwest::Method::PUT,
        "v4",
        POLICY,
        Some(&json!({
            "expectedRevision": 1,
            "processGeneration": 1,
            "rules": []
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "unauthorized");
    let (status, body) = dashboard_json(
        h.port,
        reqwest::Method::POST,
        "v4",
        &format!("{RESTRICTIONS}/tp-missing/clear"),
        Some(&json!({
            "expectedRevision": 1,
            "processGeneration": 1
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "unauthorized");
}

/// A supplied empty match set must reject even when another field is present.
/// Isolated so a product write-through here cannot skip dest/CAS/401 coverage.
#[tokio::test]
async fn supplied_empty_match_field_with_other_rejects_without_write() {
    let journal = SharedJournal::new();
    let lab = start_journaled_lab(&journal, "tp-empty", KEY_SOLO, &[ok()]).await;
    let (state, dir) = build_state_with_routing(
        "http://127.0.0.1:1".into(),
        &[],
        RoutingMode::StrictPriority,
        false,
    );
    let mut h = FallbackHarness::from_state(state, dir).await;
    let dest = create_http_lab(
        &h,
        "empty-with-other",
        &lab.url,
        KEY_SOLO,
        &[(MODEL_A, UP_A)],
    )
    .await
    .dest_id;
    h.push_stop(lab.stop);
    let before = get_policy(&h).await;
    let (status, rejected) = put_rules(
        &h,
        json!([{
            "kind": "custom",
            "id": "empty-with-other",
            "destinationId": dest,
            "enabled": true,
            "scope": "credential_model",
            "match": { "statusCodes": [], "errorCodes": ["X"] },
            "backoff": { "initialSeconds": 30, "maxSeconds": 300 }
        }]),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    assert_eq!(get_policy(&h).await["rules"], before["rules"]);
}

/// Group 2: delayed 400 with A+B captured; HTTP delete A; B records; no client retry.
#[tokio::test]
async fn delayed_400_records_surviving_rule_after_http_delete() {
    let arrived = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut scripts = HashMap::new();
    scripts.insert(
        KEY_SOLO.to_string(),
        VecDeque::from([ScriptedReply::held(
            400,
            BODY_400,
            arrived.clone(),
            release.clone(),
        )]),
    );
    let upstream = start_scripted_upstream(
        scripts,
        ScriptedReply::immediate(200, fixture::SUCCESS_BODY),
    )
    .await;
    let (state, dir) = build_state_with_routing(
        "http://127.0.0.1:1".into(),
        &[],
        RoutingMode::StrictPriority,
        false,
    );
    let h = FallbackHarness::from_state(state, dir).await;
    let _dest = create_http_lab(&h, "held-400", &upstream.url, KEY_SOLO, &[(MODEL_A, UP_A)]).await;
    let (status, committed) = put_rules(
        &h,
        json!([
            custom_rule("rule-a", None, "credential_model", 400, 30, 300),
            custom_rule("rule-b", None, "credential_model", 400, 30, 300)
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");

    let pending = spawn_held_protocol(h.port, MODEL_A, &arrived).await;
    assert_eq!(upstream.calls.load(Ordering::SeqCst), 1);

    let (status, after_delete) = put_rules(
        &h,
        json!([custom_rule(
            "rule-b",
            None,
            "credential_model",
            400,
            30,
            300
        )]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{after_delete}");
    assert!(
        after_delete["rules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|rule| rule["id"] != "rule-a"),
        "{after_delete}"
    );

    release.notify_one();
    let (status, body) = join_protocol(pending, "in-flight client").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        upstream.calls.load(Ordering::SeqCst),
        1,
        "the client's first 400 must not be auto-retried"
    );

    let listed = get_restrictions(&h).await;
    let ids = rule_ids(&listed);
    assert!(
        ids.contains(&"rule-b"),
        "surviving global rule B must record after A was deleted in flight: {listed}"
    );
    assert!(
        !ids.contains(&"rule-a"),
        "deleted rule A must not record: {listed}"
    );
}

/// Group 2b: same-id rebuild / generation fence on the delayed HTTP path.
#[tokio::test]
async fn delayed_400_does_not_record_rebuilt_same_id_generation() {
    let arrived = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut scripts = HashMap::new();
    scripts.insert(
        KEY_SOLO.to_string(),
        VecDeque::from([
            ScriptedReply::held(400, BODY_400, arrived.clone(), release.clone()),
            ScriptedReply::immediate(400, BODY_400),
        ]),
    );
    let upstream = start_scripted_upstream(
        scripts,
        ScriptedReply::immediate(200, fixture::SUCCESS_BODY),
    )
    .await;
    let (state, dir) = build_state_with_routing(
        "http://127.0.0.1:1".into(),
        &[],
        RoutingMode::StrictPriority,
        false,
    );
    let h = FallbackHarness::from_state(state, dir).await;
    let _dest = create_http_lab(
        &h,
        "rebuild-400",
        &upstream.url,
        KEY_SOLO,
        &[(MODEL_A, UP_A)],
    )
    .await;
    let (status, _) = put_rules(
        &h,
        json!([custom_rule(
            "rebuild-me",
            None,
            "credential_model",
            400,
            30,
            300
        )]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let pending = spawn_held_protocol(h.port, MODEL_A, &arrived).await;

    let (status, cleared) = put_rules(&h, json!([])).await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    let (status, rebuilt) = put_rules(
        &h,
        json!([custom_rule(
            "rebuild-me",
            None,
            "credential_model",
            400,
            45,
            300
        )]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rebuilt}");

    release.notify_one();
    let (status, body) = join_protocol(pending, "in-flight client").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let after_stale = get_restrictions(&h).await;
    assert!(
        restriction_rows(&after_stale).is_empty(),
        "an old in-flight 400 must not record the rebuilt same-id generation: {after_stale}"
    );

    let (status, body) = h.protocol("/v1/chat/completions", MODEL_A).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let after_live = get_restrictions(&h).await;
    assert!(
        rule_ids(&after_live).contains(&"rebuild-me"),
        "a later request may record the new generation: {after_live}"
    );
}

/// Group 3: custom 503 + Retry-After 600 stays after local clear / rule delete.
#[tokio::test]
async fn retry_after_survives_http_clear_and_rule_delete() {
    let mut scripts = HashMap::new();
    scripts.insert(
        KEY_A.to_string(),
        VecDeque::from([
            ScriptedReply::immediate(503, BODY_503).with_header("retry-after", "600"),
            ScriptedReply::immediate(200, fixture::SUCCESS_BODY),
            ScriptedReply::immediate(200, fixture::SUCCESS_BODY),
        ]),
    );
    let mut upstream = start_scripted_upstream(
        scripts,
        ScriptedReply::immediate(200, fixture::SUCCESS_BODY),
    )
    .await;
    let (mut h, clock) = clocked_lab().await;
    let dest = create_http_lab(
        &h,
        "retry-after-a",
        &upstream.url,
        KEY_A,
        &[(MODEL_A, UP_A), (MODEL_A_OTHER, UP_A_OTHER)],
    )
    .await
    .dest_id;
    h.push_stop(upstream.stop.take().expect("upstream stop"));

    let (status, committed) = put_rules(
        &h,
        json!([custom_rule(
            "custom-503",
            Some(&dest),
            "credential_model",
            503,
            30,
            30
        )]),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{committed}");

    let sends_after_503 = upstream.calls.load(Ordering::SeqCst);
    let (status, body) = h.protocol("/v1/chat/completions", MODEL_A).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let after_503 = upstream.calls.load(Ordering::SeqCst);
    assert!(
        after_503 > sends_after_503,
        "503 must actually reach upstream"
    );

    let listed = get_restrictions(&h).await;
    let row = restriction_rows(&listed)
        .iter()
        .find(|row| row["ruleId"] == "custom-503")
        .unwrap_or_else(|| panic!("local 30s restriction missing: {listed}"));
    assert_eq!(row["destinationId"], dest);
    assert_eq!(row["upstreamModel"], UP_A);
    assert_eq!(row["scope"], "credential_model");
    let local_id = row["id"].as_str().expect("restriction id").to_string();

    let (status, body) = h.protocol("/v1/chat/completions", MODEL_A_OTHER).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, cleared) = v4_post(
        h.port,
        &h.state,
        &format!("{RESTRICTIONS}/{local_id}/clear"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert!(
        restriction_rows(&cleared)
            .iter()
            .all(|row| row["id"] != local_id),
        "clear must drop the local restriction id: {cleared}"
    );

    let after_clear = upstream.calls.load(Ordering::SeqCst);
    let (status, _) = h.protocol("/v1/chat/completions", MODEL_A).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "Retry-After 600 must outlive local clear"
    );
    assert_eq!(
        upstream.calls.load(Ordering::SeqCst),
        after_clear,
        "same model must not be sent after local clear"
    );
    let (status, body) = h.protocol("/v1/chat/completions", MODEL_A_OTHER).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, deleted) = put_rules(&h, json!([])).await;
    assert_eq!(status, StatusCode::OK, "{deleted}");
    assert!(get_policy(&h).await["rules"].as_array().unwrap().is_empty());

    let after_delete = upstream.calls.load(Ordering::SeqCst);
    let (status, _) = h.protocol("/v1/chat/completions", MODEL_A).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "Retry-After 600 must outlive rule delete"
    );
    assert_eq!(
        upstream.calls.load(Ordering::SeqCst),
        after_delete,
        "same model must not be sent after HTTP rule delete"
    );

    clock.store(40, Ordering::SeqCst);
    let after_30 = upstream.calls.load(Ordering::SeqCst);
    let (status, _) = h.protocol("/v1/chat/completions", MODEL_A).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "elapsed local 30s must not shorten Retry-After 600"
    );
    assert_eq!(upstream.calls.load(Ordering::SeqCst), after_30);

    clock.store(600, Ordering::SeqCst);
    let (status, body) = h.protocol("/v1/chat/completions", MODEL_A).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        upstream.calls.load(Ordering::SeqCst) > after_30,
        "the same model may send only after the independent 600s wait"
    );
}

/// Optional: old probe success must not clear a B restriction recorded later.
#[tokio::test]
async fn probe_success_does_not_clear_later_rule_b_over_http() {
    let x_arrived = Arc::new(Notify::new());
    let x_release = Arc::new(Notify::new());
    let p_arrived = Arc::new(Notify::new());
    let p_release = Arc::new(Notify::new());
    let mut scripts = HashMap::new();
    scripts.insert(
        KEY_SOLO.to_string(),
        VecDeque::from([
            ScriptedReply::held(400, BODY_400_BETA, x_arrived.clone(), x_release.clone()),
            ScriptedReply::immediate(400, BODY_400_ALPHA),
            ScriptedReply::held(
                200,
                fixture::SUCCESS_BODY,
                p_arrived.clone(),
                p_release.clone(),
            ),
        ]),
    );
    let upstream = start_scripted_upstream(
        scripts,
        ScriptedReply::immediate(200, fixture::SUCCESS_BODY),
    )
    .await;
    let (h, clock) = clocked_lab().await;
    let _dest = create_http_lab(&h, "probe-ab", &upstream.url, KEY_SOLO, &[(MODEL_A, UP_A)]).await;
    let (status, _) = put_rules(
        &h,
        json!([
            custom_message_rule("rule-a", "alpha-only"),
            custom_message_rule("rule-b", "beta-only")
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let pending_x = spawn_held_protocol(h.port, MODEL_A, &x_arrived).await;

    let (status, body) = h.protocol("/v1/chat/completions", MODEL_A).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let after_y = get_restrictions(&h).await;
    assert!(
        rule_ids(&after_y).contains(&"rule-a"),
        "request Y must restrict A: {after_y}"
    );
    assert!(
        !rule_ids(&after_y).contains(&"rule-b"),
        "B is recorded later by X, not by Y: {after_y}"
    );

    clock.store(40, Ordering::SeqCst);
    let pending_p = spawn_held_protocol(h.port, MODEL_A, &p_arrived).await;

    x_release.notify_one();
    let (status, body) = join_protocol(pending_x, "held X").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let after_x = get_restrictions(&h).await;
    assert!(
        rule_ids(&after_x).contains(&"rule-b"),
        "X must record B before probe success: {after_x}"
    );

    p_release.notify_one();
    let (status, body) = join_protocol(pending_p, "held probe").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after_probe = get_restrictions(&h).await;
    assert!(
        rule_ids(&after_probe).contains(&"rule-b"),
        "probe success on A must not clear later B: {after_probe}"
    );
}

/// Two destinations share one public model. Dest A returns 400/503; dest B
/// would return 200. The first reply must stay Return. A matching temporary
/// rule must not promote TryNextAccount (the reverted policy_promotes_fallback
/// candidate).
async fn assert_shared_model_first_error_returns(
    label: &str,
    expected: StatusCode,
    body: &'static str,
    attach_matching_rule: bool,
) {
    let journal = SharedJournal::new();
    let key_a = format!("dummy-tp-ret-a-{label}");
    let key_b = format!("dummy-tp-ret-b-{label}");
    let lab_a = start_journaled_lab(
        &journal,
        &format!("ret-a-{label}"),
        &key_a,
        &[reply(expected.as_u16(), body)],
    )
    .await;
    let lab_b =
        start_journaled_lab(&journal, &format!("ret-b-{label}"), &key_b, &[ok(), ok()]).await;
    let (state, dir) = build_state_with_routing(
        "http://127.0.0.1:1".into(),
        &[],
        RoutingMode::StrictPriority,
        false,
    );
    let mut h = FallbackHarness::from_state(state, dir).await;
    let created_a = create_http_lab(
        &h,
        &format!("ret-a-{label}"),
        &lab_a.url,
        &key_a,
        &[(MODEL_SHARED, UP_SHARED)],
    )
    .await;
    let created_b = create_http_lab(
        &h,
        &format!("ret-b-{label}"),
        &lab_b.url,
        &key_b,
        &[(MODEL_SHARED, UP_SHARED)],
    )
    .await;
    prefer_account_order(&h.state, &[&created_a.account_id, &created_b.account_id]);
    h.push_stop(lab_a.stop);
    h.push_stop(lab_b.stop);

    if attach_matching_rule {
        let (status, committed) = put_rules(
            &h,
            json!([custom_rule(
                &format!("no-promote-{label}"),
                Some(&created_a.dest_id),
                "credential_model",
                expected.as_u16(),
                30,
                300
            )]),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{committed}");
    }

    let (status, response) = h.protocol("/v1/chat/completions", MODEL_SHARED).await;
    assert_eq!(status, expected, "{label} {response}");
    assert_eq!(
        journal.keys(),
        vec![key_a.clone()],
        "{label}: dest B must not receive a promoted fallback: {:?}",
        journal.keys()
    );
    assert!(
        !journal.keys().iter().any(|key| key == &key_b),
        "{label}: dest B key must stay unused: {:?}",
        journal.keys()
    );
    assert_eq!(
        last_retry_action(&h).as_deref(),
        Some("return"),
        "{label}: first {expected} must stay Return: {:?}",
        h.logs()
    );
}

#[tokio::test]
async fn unknown_and_matched_400_503_do_not_promote_fallback() {
    for (label, status, body, attach) in [
        ("unknown-400", StatusCode::BAD_REQUEST, BODY_400, false),
        ("matched-400", StatusCode::BAD_REQUEST, BODY_400, true),
        (
            "unknown-503",
            StatusCode::SERVICE_UNAVAILABLE,
            BODY_503,
            false,
        ),
        (
            "matched-503",
            StatusCode::SERVICE_UNAVAILABLE,
            BODY_503,
            true,
        ),
    ] {
        assert_shared_model_first_error_returns(label, status, body, attach).await;
    }
}
