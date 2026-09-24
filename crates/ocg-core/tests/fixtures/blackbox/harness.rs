//! HTTP-only helpers for the alias / multi-Plan black-box suite.
//!
//! Tests talk to Gateway and dashboard JSON (`/dashboard/api/v4`) using
//! camelCase field names. Named helpers may unwrap a V3 envelope
//! (`entries`, `accounts`, `account`); raw `*_json` methods return the HTTP
//! body unchanged. They do not construct private gateway types.
//! `CoreStateInner` is used only to boot an isolated data dir.

#![allow(dead_code)]

use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway;
use ocg_core::models::{AccountUpdate, ProxyMode};
use ocg_core::state::{CoreStateInner, GatewayHandle};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[path = "../fake_upstream.rs"]
mod fake_upstream;
#[path = "../refreshed_go_catalog.rs"]
mod refreshed_go_catalog;

pub(crate) use fake_upstream::FakeReply;
use fake_upstream::{FakeCall, FakeCalls, start_fake_upstream, start_raw_disconnect_upstream};

pub(crate) const GATEWAY_KEY: &str = "gw-v2-contract";
pub(crate) const GO_ACCOUNT_KEY: &str = "v2-secret-KEY-9f3a2c1b-go";
pub(crate) const GO_ACCOUNT_KEY_2: &str = "v2-secret-KEY-9f3a2c1b-go-2";
pub(crate) const GOAT_ACCOUNT_KEY: &str = "v2-secret-KEY-9f3a2c1b-goat";
pub(crate) const CUSTOM_ACCOUNT_KEY: &str = "v2-secret-KEY-9f3a2c1b-custom";

pub(crate) const OPENCODE_PROVIDER_ID: &str = "opencode";
pub(crate) const COMMAND_CODE_PROVIDER_ID: &str = "command-code";
pub(crate) const CUSTOM_PROVIDER_ID: &str = "custom";
pub(crate) const CUSTOM_UNROUTABLE_MODEL_ID: &str = "custom-unroutable-model";

pub(crate) const GO_ALIAS: &str = "deepseek-v4-flash";
pub(crate) const GOAT_UNIQUE_RAW_ID: &str = "deepseek/deepseek-v4-flash";
pub(crate) const FREE_MODEL: &str = "mimo-v2.5-free";
pub(crate) const AMBIGUOUS_ERROR_TYPE: &str = "ambiguous_model_id";
pub(crate) const CUSTOM_OVERLAP_RAW_ID: &str = "shared-raw-model";

pub(crate) const SUCCESS_CHAT_BODY: &str = r#"{"id":"ok","object":"chat.completion","model":"upstream-should-not-leak","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":0}}}"#;

const CHAT_STREAM_HEAD: &str = "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n";

pub(crate) fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("black-box test client should build")
}

pub(crate) struct BlackBoxHarness {
    pub state: Arc<CoreStateInner>,
    pub dir: PathBuf,
    pub handle: GatewayHandle,
    pub client: reqwest::Client,
    pub port: u16,
    pub upstream_base_url: String,
    fake_calls: Option<FakeCalls>,
    disconnect_calls: Option<Arc<std::sync::atomic::AtomicUsize>>,
    stop_fake: Option<tokio::sync::oneshot::Sender<()>>,
}

impl BlackBoxHarness {
    pub(crate) async fn start() -> Self {
        Self::start_with_upstream(None).await
    }

    pub(crate) async fn start_with_chat_success(account_keys: &[&str]) -> Self {
        let mut replies = HashMap::new();
        for key in account_keys {
            replies.insert(
                (*key).to_string(),
                VecDeque::from([FakeReply {
                    status: 200,
                    body: SUCCESS_CHAT_BODY,
                }]),
            );
        }
        replies.insert(
            String::new(),
            VecDeque::from([FakeReply {
                status: 200,
                body: SUCCESS_CHAT_BODY,
            }]),
        );
        Self::start_with_upstream(Some(replies)).await
    }

    pub(crate) async fn start_with_upstream(
        replies: Option<HashMap<String, VecDeque<FakeReply>>>,
    ) -> Self {
        let dir = temp_data_dir();
        let db = Database::open(dir.clone()).unwrap();
        let cipher: Arc<dyn KeyCipher + Send + Sync> =
            Arc::new(StaticKeyCipher::new("blackbox-tests"));
        let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

        let (upstream, fake_calls, stop_fake) = if let Some(replies) = replies {
            let (base, calls, stop) = start_fake_upstream(replies).await;
            (Some(base), Some(calls), Some(stop))
        } else {
            (None, None, None)
        };

        let mut config = state.config();
        config.gateway_key = GATEWAY_KEY.into();
        config.proxy_mode = ProxyMode::Direct;
        let upstream_base_url = if let Some(base) = upstream {
            // Go and Zen share this suffix; the fake server is path-agnostic.
            format!("{}/zen/go", base.trim_end_matches('/'))
        } else {
            // Isolated tests must never touch a real provider. A closed
            // loopback port fails closed without leaving the machine.
            "http://127.0.0.1:1".into()
        };
        config.upstream_base_url = upstream_base_url.clone();
        state.set_config(config).unwrap();
        refreshed_go_catalog::persist_refreshed_go_catalog(&state);

        let handle =
            gateway::start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
                .await
                .unwrap();
        let client = loopback_client();
        wait_ready(&client, handle.port).await;
        Self {
            state,
            dir,
            port: handle.port,
            handle,
            client,
            upstream_base_url,
            fake_calls,
            disconnect_calls: None,
            stop_fake,
        }
    }

    pub(crate) fn dashboard(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/dashboard/api/v4{path}", self.port)
    }

    pub(crate) fn mutation_body(&self, body: Value) -> Value {
        with_cas_tokens(
            body,
            self.state.settings_revision(),
            self.state.process_generation(),
        )
    }

    pub(crate) fn gateway(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    pub(crate) async fn get_json(&self, path: &str) -> (StatusCode, Value) {
        let response = self.client.get(self.dashboard(path)).send().await.unwrap();
        let status = response.status();
        let body = decode_json(response).await;
        (status, body)
    }

    pub(crate) async fn post_json(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        let response = self
            .client
            .post(self.dashboard(path))
            .json(&self.mutation_body(body.clone()))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = decode_json(response).await;
        (status, body)
    }

    pub(crate) async fn patch_json(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        let response = self
            .client
            .patch(self.dashboard(path))
            .json(&self.mutation_body(body.clone()))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = decode_json(response).await;
        (status, body)
    }

    pub(crate) async fn put_json(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        let response = self
            .client
            .put(self.dashboard(path))
            .json(&self.mutation_body(body.clone()))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = decode_json(response).await;
        (status, body)
    }

    pub(crate) async fn delete_json(&self, path: &str, body: &Value) -> (StatusCode, Value) {
        let response = self
            .client
            .delete(self.dashboard(path))
            .json(&self.mutation_body(body.clone()))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = decode_json(response).await;
        (status, body)
    }

    pub(crate) async fn catalog(&self) -> Value {
        let (status, body) = self.get_json("/providers").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "catalog must be readable on loopback: {body}"
        );
        body.get("entries")
            .cloned()
            .unwrap_or_else(|| panic!("GET /providers must return entries: {body}"))
    }

    pub(crate) async fn accounts(&self) -> Value {
        let (status, body) = self.get_json("/account-records").await;
        assert_eq!(status, StatusCode::OK, "account list: {body}");
        body.get("accounts")
            .cloned()
            .unwrap_or_else(|| panic!("GET /account-records must return accounts: {body}"))
    }

    pub(crate) async fn create_account(&self, payload: Value) -> (StatusCode, Value) {
        let (status, body) = self.post_json("/accounts", &payload).await;
        if status.is_success() {
            (status, mutation_account(body))
        } else {
            (status, body)
        }
    }

    pub(crate) async fn create_go_account(&self, name: &str, key: &str) -> Value {
        let revision = self.settings_revision().await;
        let (status, body) = self
            .create_account(json!({
                "providerId": OPENCODE_PROVIDER_ID,
                "name": name,
                "key": key,
                "expectedRevision": revision
            }))
            .await;
        assert_eq!(status, StatusCode::OK, "create Go account: {body}");
        let id = body["id"].as_str().expect("created Go account id");
        self.enable_stored_account(id);
        self.account_by_id(id).await
    }

    pub(crate) fn enable_stored_account(&self, id: &str) {
        self.state
            .db
            .lock()
            .update_account(
                id,
                &AccountUpdate {
                    enabled: Some(true),
                    ..AccountUpdate::default()
                },
                None,
                None,
            )
            .unwrap();
        self.state.reload_provider_contracts().unwrap();
    }

    pub(crate) async fn account_by_id(&self, id: &str) -> Value {
        self.accounts()
            .await
            .as_array()
            .into_iter()
            .flatten()
            .find(|account| account["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("account {id} missing from dashboard list"))
    }

    pub(crate) async fn settings_revision(&self) -> u64 {
        let (_, settings) = self.get_json("/settings").await;
        settings["revision"].as_u64().unwrap_or(0)
    }

    pub(crate) async fn chat(&self, model: &str) -> (StatusCode, Value) {
        let response = self
            .client
            .post(self.gateway("/v1/chat/completions"))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {GATEWAY_KEY}"),
            )
            .json(&json!({
                "model": model,
                "messages": [{"role": "user", "content": "ping"}],
                "max_tokens": 3,
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = decode_json(response).await;
        (status, body)
    }

    pub(crate) async fn list_client_models(&self) -> (StatusCode, Value) {
        let response = self
            .client
            .get(self.gateway("/v1/models"))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {GATEWAY_KEY}"),
            )
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = decode_json(response).await;
        (status, body)
    }

    pub(crate) async fn forward_logs(&self) -> Value {
        let (status, body) = self.get_json("/logs/forward?limit=50").await;
        assert_eq!(status, StatusCode::OK, "forward logs: {body}");
        body
    }

    pub(crate) async fn gateway_logs(&self) -> Value {
        let (status, body) = self.get_json("/logs/gateway?limit=100").await;
        assert_eq!(status, StatusCode::OK, "gateway logs: {body}");
        body
    }

    pub(crate) fn fake_calls(&self) -> Vec<FakeCall> {
        self.fake_calls
            .as_ref()
            .map(|calls| calls.lock().expect("fake call log").clone())
            .unwrap_or_default()
    }

    pub(crate) fn fake_call_keys(&self) -> Vec<String> {
        self.fake_calls().into_iter().map(|call| call.key).collect()
    }

    pub(crate) fn disconnect_call_count(&self) -> usize {
        self.disconnect_calls
            .as_ref()
            .map(|calls| calls.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub(crate) fn shutdown(mut self) {
        gateway::stop_gateway(self.handle);
        if let Some(stop) = self.stop_fake.take() {
            let _ = stop.send(());
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

async fn start_output_then_disconnect_upstream() -> (
    String,
    Arc<std::sync::atomic::AtomicUsize>,
    tokio::sync::oneshot::Sender<()>,
) {
    let payload = CHAT_STREAM_HEAD;
    let raw = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n{:X}\r\n{}\r\n",
        payload.len(),
        payload
    )
    .into_bytes();
    start_raw_disconnect_upstream(raw).await
}

pub(crate) async fn start_with_disconnect_upstream() -> BlackBoxHarness {
    let dir = temp_data_dir();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("blackbox-tests"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let (base, calls, stop) = start_output_then_disconnect_upstream().await;
    let mut config = state.config();
    config.gateway_key = GATEWAY_KEY.into();
    config.proxy_mode = ProxyMode::Direct;
    config.upstream_base_url = format!("{}/zen/go", base.trim_end_matches('/'));
    state.set_config(config).unwrap();
    refreshed_go_catalog::persist_refreshed_go_catalog(&state);
    let handle = gateway::start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let client = loopback_client();
    wait_ready(&client, handle.port).await;
    BlackBoxHarness {
        state,
        dir,
        port: handle.port,
        handle,
        client,
        upstream_base_url: format!("{}/zen/go", base.trim_end_matches('/')),
        fake_calls: None,
        disconnect_calls: Some(calls),
        stop_fake: Some(stop),
    }
}

pub(crate) fn catalog_entry<'a>(catalog: &'a Value, provider_id: &str) -> Option<&'a Value> {
    catalog
        .as_array()?
        .iter()
        .find(|entry| entry["providerId"] == provider_id)
}

pub(crate) fn custom_create_payload(
    name: &str,
    key: &str,
    revision: u64,
    base_url: &str,
    model_id: &str,
) -> Value {
    let endpoint_url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    json!({
        "providerId": CUSTOM_PROVIDER_ID,
        "name": name,
        "key": key,
        "expectedRevision": revision,
        "customConfig": {
            "endpointUrl": endpoint_url,
            "upstreamProtocol": "chat_completions"
        },
        "modelCapabilities": [{
            "modelId": model_id,
            "protocol": "chat_completions"
        }]
    })
}

pub(crate) fn error_type(body: &Value) -> Option<&str> {
    body.pointer("/error/type")
        .and_then(Value::as_str)
        .or_else(|| body.get("type").and_then(Value::as_str))
}

pub(crate) fn json_contains_secret(value: &Value, secret: &str) -> bool {
    if secret.is_empty() {
        return false;
    }
    match value {
        Value::String(text) => text.contains(secret),
        Value::Array(items) => items.iter().any(|item| json_contains_secret(item, secret)),
        Value::Object(map) => map.values().any(|item| json_contains_secret(item, secret)),
        _ => false,
    }
}

fn temp_data_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ocg-blackbox-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

async fn wait_ready(client: &reqwest::Client, port: u16) {
    let url = format!("http://127.0.0.1:{port}/dashboard/api/auth/status");
    for _ in 0..50 {
        if client.get(&url).send().await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("gateway on port {port} did not become ready");
}

async fn decode_json(response: reqwest::Response) -> Value {
    let text = response.text().await.unwrap_or_default();
    serde_json::from_str(&text).unwrap_or_else(|_| json!({ "raw": text }))
}

fn with_cas_tokens(mut body: Value, revision: u64, process_generation: u64) -> Value {
    let Some(object) = body.as_object_mut() else {
        return body;
    };
    object
        .entry("expectedRevision")
        .or_insert_with(|| json!(revision));
    object
        .entry("processGeneration")
        .or_insert_with(|| json!(process_generation));
    body
}

pub(crate) fn mutation_account(body: Value) -> Value {
    body.get("account")
        .filter(|account| !account.is_null())
        .cloned()
        .unwrap_or_else(|| panic!("V3 account mutation must wrap an account: {body}"))
}
