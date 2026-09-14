//! Gateway integration regressions for the Ollama Cloud sealed family.
//!
//! Covers the spec scenarios that only the full request path can prove:
//! attempt-level wire normalization (request clamp/reasoning copy, response
//! and SSE reasoning backfill), mixed candidate-chain byte isolation, the
//! `upstream_body_bytes` diagnostic contract, Cookie-free inference egress,
//! fail-closed unknown-model 400s, and unpriced-vs-Go pricing attribution.

use axum::http::StatusCode;
use chrono::Utc;
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway;
use ocg_core::gateway::provider_adapter::install_ollama_cloud_loopback_route_for_test;
use ocg_core::models::{Account, ForwardLog, ProxyMode, RoutingMode};
use ocg_core::provider::{OLLAMA_PROVIDER_ID, OllamaBillingTier};
use ocg_core::provider_contracts::ContractScope;
use ocg_core::state::{CoreStateInner, GatewayHandle};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::net::TcpListener as StdTcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;

#[path = "fixtures/fake_upstream.rs"]
#[allow(dead_code)] // the shared fixture carries delayed/raw helpers this suite never exercises
mod fake_upstream;
#[path = "fixtures/refreshed_go_catalog.rs"]
mod refreshed_go_catalog;

use fake_upstream::{FakeReply, start_fake_upstream};

const OLLAMA_KEY: &str = "ollama-key-1";
const GO_KEY: &str = "go-key-1";

const OLLAMA_SUCCESS_BODY: &str = r#"{"id":"ollama-ok","object":"chat.completion","model":"gpt-oss:120b","choices":[{"index":0,"message":{"role":"assistant","content":"ok","reasoning":"why"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#;

const OLLAMA_THINKING_STREAM: &str = concat!(
    "data: {\"id\":\"ollama-stream\",\"model\":\"gpt-oss:120b\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"thinking\":\"chain-of-thought\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"ollama-stream\",\"model\":\"gpt-oss:120b\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\n\n",
    "data: [DONE]\n\n"
);

fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ocg-ollama-gateway-{}-{}",
        label,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client should build")
}

fn base_account(state: &CoreStateInner, id: &str, key: &str) -> Account {
    let now = Utc::now();
    Account {
        id: id.into(),
        provider_id: OLLAMA_PROVIDER_ID.into(),
        credential_kind: ocg_core::provider::default_credential_kind(),
        quota_scope: ocg_core::provider::default_quota_scope(),
        name: id.into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key(key).unwrap(),
        enabled: true,
        account_type: ocg_core::models::AccountType::Key,
        setup_step: ocg_core::models::AccountSetupStep::Ready,
        referral_code: None,
        purchase_date: String::new(),
        expires_on: String::new(),
        cooldown_until: None,
        cooldown_generic_until: None,
        cooldown_5h_until: None,
        cooldown_week_until: None,
        cooldown_month_until: None,
        cooldown_free_until: None,
        last_error: None,
        auth_error: None,
        notes: None,
        created_at: now,
        updated_at: now,
    }
}

fn build_state(base_url: String) -> (Arc<CoreStateInner>, PathBuf) {
    let dir = temp_data_dir("state");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let mut config = state.config();
    config.gateway_key = "gw-test".into();
    config.upstream_base_url = base_url;
    config.proxy_mode = ProxyMode::Direct;
    config.routing_mode = RoutingMode::StrictPriority;
    state.set_config(config).unwrap();
    (state, dir)
}

fn persist_ollama_catalog(state: &Arc<CoreStateInner>, models: &[&str]) {
    let now = Utc::now();
    let model_ids: Vec<String> = models.iter().map(|model| model.to_string()).collect();
    state
        .db
        .lock()
        .set_contract_catalog(
            &ContractScope::provider(OLLAMA_PROVIDER_ID),
            &model_ids,
            Some(now),
            "ollama_cloud_get_models",
            "https://ollama.com/v1/models",
            now,
        )
        .unwrap();
    state.reload_provider_contracts().unwrap();
}

async fn start_gateway(state: Arc<CoreStateInner>) -> (u16, GatewayHandle) {
    let listener = StdTcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let handle = gateway::start_gateway(state, port).await.unwrap();
    tokio::time::sleep(StdDuration::from_millis(50)).await;
    (port, handle)
}

fn stop(
    state: Arc<CoreStateInner>,
    dir: PathBuf,
    gateway: GatewayHandle,
    mock: tokio::sync::oneshot::Sender<()>,
) {
    gateway::stop_gateway(gateway);
    let _ = mock.send(());
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

fn quirk_request(model: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": 200_000,
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "prior", "reasoning_content": "prior-thought"},
            {"role": "user", "content": "go"}
        ]
    })
}

async fn chat_call(
    port: u16,
    body: &Value,
    stream: bool,
    cookie: bool,
) -> (StatusCode, Value, String) {
    let mut request = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth("gw-test");
    if stream {
        request = request.header("accept", "text/event-stream");
    }
    if cookie {
        request = request.header("cookie", "session=stolen-if-leaked");
    }
    let response = request.json(body).send().await.unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, parsed, text)
}

#[tokio::test]
async fn ollama_cloud_attempt_normalizes_wire_and_never_sends_cookies() {
    let replies = HashMap::from([(
        OLLAMA_KEY.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: OLLAMA_SUCCESS_BODY,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url.clone());
    persist_ollama_catalog(&state, &["deepseek-v4-flash:0731", "gpt-oss:120b"]);
    let account = base_account(&state, "ollama-normalize", OLLAMA_KEY);
    state.db.lock().create_account(&account).unwrap();
    let _route =
        install_ollama_cloud_loopback_route_for_test("ollama-normalize", base_url).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, body, _text) = chat_call(port, &quirk_request("gpt-oss:120b"), false, true).await;
    assert_eq!(status, StatusCode::OK);
    // Response direction: the client sees reasoning_content backfilled from
    // the upstream `reasoning` field, and the original field is preserved.
    assert_eq!(body["choices"][0]["message"]["reasoning_content"], "why");
    assert_eq!(body["choices"][0]["message"]["reasoning"], "why");

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.path, "/v1/chat/completions");
    assert_eq!(call.authorization.as_deref(), Some("Bearer ollama-key-1"));
    assert!(
        call.cookie.is_none(),
        "inference egress must never carry a Cookie header"
    );
    let sent: Value = serde_json::from_str(&call.body).unwrap();
    // Request direction: the clamp and the assistant reasoning copy applied.
    assert_eq!(sent["max_tokens"], 65_535);
    assert_eq!(
        sent["model"], "gpt-oss:120b",
        "exact catalog id forwards as-is"
    );
    assert_eq!(sent["messages"][1]["reasoning"], "prior-thought");
    assert_eq!(sent["messages"][1]["reasoning_content"], "prior-thought");

    let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.provider_id.as_deref(), Some(OLLAMA_PROVIDER_ID));
    assert_eq!(log.model, "gpt-oss:120b");
    assert_eq!(log.status, "success_unpriced");
    assert_eq!(log.cost_state, "unpriced", "the family has no price table");
    assert!(log.cost.is_none());
    assert!(log.pricing_revision_id.is_none());
    assert_eq!(log.route, "direct", "the attempt's route leg is recorded");

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn ollama_cloud_stream_backfills_reasoning_content_per_delta() {
    let replies = HashMap::from([(
        OLLAMA_KEY.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: OLLAMA_THINKING_STREAM,
        }]),
    )]);
    let (base_url, _calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url.clone());
    persist_ollama_catalog(&state, &["deepseek-v4-flash:0731", "gpt-oss:120b"]);
    let account = base_account(&state, "ollama-stream", OLLAMA_KEY);
    state.db.lock().create_account(&account).unwrap();
    let _route = install_ollama_cloud_loopback_route_for_test("ollama-stream", base_url).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, _body, text) = chat_call(port, &quirk_request("gpt-oss:120b"), true, false).await;
    assert_eq!(status, StatusCode::OK);
    let first_delta = text
        .lines()
        .find(|line| line.starts_with("data: {") && line.contains("thinking"))
        .expect("first thinking delta");
    let parsed: Value = serde_json::from_str(first_delta.trim_start_matches("data: ")).unwrap();
    assert_eq!(
        parsed["choices"][0]["delta"]["reasoning_content"], "chain-of-thought",
        "thinking is backfilled into reasoning_content"
    );
    assert_eq!(
        parsed["choices"][0]["delta"]["thinking"],
        "chain-of-thought"
    );
    assert!(
        text.contains("data: [DONE]"),
        "the [DONE] sentinel passes through untouched"
    );

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn ollama_cloud_failure_diagnostic_records_normalized_body_bytes() {
    let replies = HashMap::from([(
        OLLAMA_KEY.to_string(),
        VecDeque::from([FakeReply {
            status: 500,
            body: r#"{"error":"boom"}"#,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url.clone());
    persist_ollama_catalog(&state, &["deepseek-v4-flash:0731", "gpt-oss:120b"]);
    let account = base_account(&state, "ollama-diag", OLLAMA_KEY);
    state.db.lock().create_account(&account).unwrap();
    let _route = install_ollama_cloud_loopback_route_for_test("ollama-diag", base_url).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, _body, _text) =
        chat_call(port, &quirk_request("gpt-oss:120b"), false, false).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    let sent_body = calls.lock().unwrap()[0].body.clone();
    let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "error");
    let diagnostic = log.diagnostic.expect("failure rows carry a diagnostic");
    assert_eq!(
        diagnostic["upstream_body_bytes"].as_u64(),
        Some(sent_body.len() as u64),
        "the log must record the bytes actually sent after normalization"
    );
    let sent: Value = serde_json::from_str(&sent_body).unwrap();
    assert_eq!(sent["max_tokens"], 65_535);

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn mixed_candidate_chain_keeps_go_attempt_bytes_identical() {
    let replies = HashMap::from([
        (
            GO_KEY.to_string(),
            VecDeque::from([FakeReply {
                status: 200,
                body: r#"{"id":"go-ok","object":"chat.completion","model":"deepseek-v4-flash","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
            }]),
        ),
        (
            OLLAMA_KEY.to_string(),
            VecDeque::from([FakeReply {
                status: 200,
                body: OLLAMA_SUCCESS_BODY,
            }]),
        ),
    ]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url.clone());
    persist_ollama_catalog(&state, &["deepseek-v4-flash:0731", "gpt-oss:120b"]);

    refreshed_go_catalog::persist_refreshed_go_catalog(&state);
    // Go account sorts first: the shared alias is served by Go.
    let mut go = base_account(&state, "go-1", GO_KEY);
    go.provider_id = ocg_core::provider::OPENCODE_PROVIDER_ID.into();
    go.enabled = true;
    state.db.lock().create_account(&go).unwrap();
    let ollama = base_account(&state, "ollama-mixed", OLLAMA_KEY);
    state.db.lock().create_account(&ollama).unwrap();
    let _route = install_ollama_cloud_loopback_route_for_test("ollama-mixed", base_url).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, body, _text) =
        chat_call(port, &quirk_request("deepseek-v4-flash"), false, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["choices"][0]["message"]["content"], "ok");

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "the Go attempt succeeds without fallback");
    assert_eq!(calls[0].key, GO_KEY);
    let sent: Value = serde_json::from_str(&calls[0].body).unwrap();
    assert_eq!(
        sent["max_tokens"], 200_000,
        "non-Ollama attempts keep the client's exact output limit"
    );
    assert!(
        sent["messages"][1].get("reasoning").is_none(),
        "no reasoning copy leaks into other families' bytes"
    );

    // Attribution follows the family that actually served the request.
    let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(
        log.provider_id.as_deref(),
        Some(ocg_core::provider::OPENCODE_PROVIDER_ID)
    );
    assert_eq!(log.cost_state, "priced");
    assert!(log.pricing_revision_id.is_some());
    assert_eq!(
        log.model, "deepseek-v4-flash",
        "client-facing name is preserved"
    );

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn shared_alias_served_by_ollama_is_unpriced_and_uses_the_snapshot_id_upstream() {
    let replies = HashMap::from([(
        OLLAMA_KEY.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: OLLAMA_SUCCESS_BODY,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url.clone());
    persist_ollama_catalog(&state, &["deepseek-v4-flash:0731"]);
    let account = base_account(&state, "ollama-shared", OLLAMA_KEY);
    state.db.lock().create_account(&account).unwrap();
    let _route = install_ollama_cloud_loopback_route_for_test("ollama-shared", base_url).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, _body, _text) =
        chat_call(port, &quirk_request("deepseek-v4-flash"), false, false).await;
    assert_eq!(status, StatusCode::OK);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let sent: Value = serde_json::from_str(&calls[0].body).unwrap();
    assert_eq!(
        sent["model"], "deepseek-v4-flash:0731",
        "the upstream side receives the exact snapshot id from the stem binding"
    );

    let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.provider_id.as_deref(), Some(OLLAMA_PROVIDER_ID));
    assert_eq!(log.cost_state, "unpriced");
    assert!(log.cost.is_none());
    assert!(log.raw_cost_usd.is_none());
    assert!(log.pricing_revision_id.is_none());

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn unknown_ollama_model_fails_closed_before_any_upstream_call() {
    let replies = HashMap::from([(
        OLLAMA_KEY.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: OLLAMA_SUCCESS_BODY,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url.clone());
    persist_ollama_catalog(&state, &["deepseek-v4-flash:0731"]);
    let account = base_account(&state, "ollama-unknown", OLLAMA_KEY);
    state.db.lock().create_account(&account).unwrap();
    let _route = install_ollama_cloud_loopback_route_for_test("ollama-unknown", base_url).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    // Not in the saved catalog: no raw pin, no alias — a client 400 with zero
    // upstream traffic.
    let (status, body, _text) = chat_call(port, &quirk_request("gpt-oss:999b"), false, false).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("gpt-oss:999b"))
    );
    assert!(
        calls.lock().unwrap().is_empty(),
        "unknown models must fail before any upstream call"
    );

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn ollama_catalog_does_not_add_v1_models_entries() {
    let replies: HashMap<String, VecDeque<FakeReply>> = HashMap::new();
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url.clone());
    persist_ollama_catalog(&state, &["deepseek-v4-flash:0731", "gpt-oss:120b"]);
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let response = loopback_client()
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .bearer_auth("gw-test")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&"deepseek-v4-flash:0731") && !ids.contains(&"gpt-oss:120b"),
        "exact catalog ids must stay off GET /v1/models: {ids:?}"
    );
    assert_eq!(
        ids.iter().filter(|id| **id == "deepseek-v4-flash").count(),
        1,
        "the shared alias stays published exactly once"
    );
    let flash = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "deepseek-v4-flash")
        .unwrap();
    assert_eq!(flash["owned_by"], ocg_core::provider::OPENCODE_PROVIDER_ID);
    assert!(calls.lock().unwrap().is_empty());

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn ollama_soft_quota_overage_does_not_skip_selection() {
    let replies = HashMap::from([(
        OLLAMA_KEY.to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: OLLAMA_SUCCESS_BODY,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url.clone());
    persist_ollama_catalog(&state, &["gpt-oss:120b"]);
    let mut account = base_account(&state, "ollama-overage", OLLAMA_KEY);
    account.purchase_date = "2026-09-01".into();
    state.db.lock().create_account(&account).unwrap();
    state
        .db
        .lock()
        .set_ollama_cloud_billing_tier("ollama-overage", Some(OllamaBillingTier::Pro))
        .unwrap();
    state
        .db
        .lock()
        .log_forward(&ForwardLog {
            id: 0,
            timestamp: Utc::now(),
            model: "gpt-oss:120b".into(),
            account_id: "ollama-overage".into(),
            account_name: "ollama-overage".into(),
            route_account_id: Some("ollama-overage".into()),
            provider_id: Some(OLLAMA_PROVIDER_ID.into()),
            credential_account_id: Some("ollama-overage".into()),
            client_key_id: None,
            client_key_name: None,
            status: "success".into(),
            http_status: Some(200),
            route: "direct".into(),
            prompt_tokens: 10,
            completion_tokens: 2,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cost: Some(80.0),
            raw_cost_usd: Some(80.0),
            quota_debit: Some(80.0),
            effective_paid_cost_usd: None,
            pricing_revision_id: Some("ollama-test".into()),
            quota_multiplier: Some(1.0),
            local_adjustment_multiplier: Some(1.0),
            service_tier: None,
            cost_state: "priced".into(),
            error_message: None,
            request_id: Some("overage".into()),
            attempt: Some(1),
            error_source: None,
            error_stage: None,
            duration_ms: Some(5),
            diagnostic: None,
        })
        .unwrap();
    let _route = install_ollama_cloud_loopback_route_for_test("ollama-overage", base_url).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, _body, _text) =
        chat_call(port, &quirk_request("gpt-oss:120b"), false, false).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "meter fullness must not skip Ollama selection"
    );
    assert_eq!(calls.lock().unwrap().len(), 1);

    stop(state, dir, gateway_handle, stop_mock);
}
