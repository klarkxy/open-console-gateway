//! Shared setup for `gateway_fallback` integration tests.
//!
//! The suite talks to public Gateway HTTP plus `CoreState` seams. Helpers here
//! only collapse copy-pasted process/upstream wiring; each test still owns its
//! assertions.

#![allow(dead_code)]

#[path = "refreshed_go_catalog.rs"]
mod refreshed_go_catalog;

use ocg_core::alias;
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::{Database, ForwardLogQueryOptions};
use ocg_core::gateway;
use ocg_core::gateway::provider_adapter::{
    GoatLoopbackRouteGuard, install_goat_loopback_route_for_test,
};
use ocg_core::models::{
    Account, AccountUpdate, AppConfig, ForwardLog, ProxyListDirection, ProxyMode, RoutingMode,
};
use ocg_core::provider::{
    COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID, OPENCODE_PROVIDER_ID, ZEN_FREE_ACCOUNT_ID,
};
use ocg_core::state::{CoreStateInner, GatewayHandle};
use ocg_domain::credential::ModelScope;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::net::TcpListener as StdTcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

#[path = "fake_upstream.rs"]
mod fake_upstream;

pub(crate) use fake_upstream::{
    DelayedChunks, FakeCall as MockCall, FakeCalls, FakeReply as MockReply, SharedJournal,
    start_delayed_fake_upstream, start_fake_upstream, start_fake_upstream_on_journal,
    start_raw_disconnect_upstream,
};

pub(crate) const LIMITED_BODY: &str = r#"{"type":"error","error":{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 3 days."}}"#;
pub(crate) const OPAQUE_ACCOUNT_KEY: &str = "opaque/account+key=42";
pub(crate) const ERROR_BODY_WITH_ECHOED_KEY: &str = r#"{"error":{"message":"provider rejected opaque/account+key=42","detail":"opaque/account+key=42"}}"#;
pub(crate) const SUCCESS_BODY: &str = r#"{"id":"ok","object":"chat.completion","model":"deepseek-v4-flash","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":0}}}"#;
pub(crate) const SUCCESS_BODY_WITHOUT_USAGE: &str = r#"{"id":"ok","object":"chat.completion","model":"deepseek-v4-flash","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
pub(crate) const SUCCESS_BODY_WITH_ECHOED_KEY: &str = r#"{"id":"ok","object":"chat.completion","model":"deepseek-v4-flash","choices":[{"index":0,"message":{"role":"assistant","content":"before opaque/account+key=42 after"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":0}}}"#;
pub(crate) const SUCCESS_BODY_WITH_COMMON_KEY: &str = r#"{"id":"ok","object":"chat.completion","model":"deepseek-v4-flash","choices":[{"index":0,"message":{"role":"assistant","content":"before text after"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":0}}}"#;
pub(crate) const SUCCESS_BODY_WITH_NESTED_ARGUMENT_KEY: &str = r#"{"id":"ok","object":"chat.completion","model":"deepseek-v4-flash","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"run","arguments":"{\"data\":\"safe\",\"token\":\"data\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":0}}}"#;
pub(crate) const RESPONSES_SUCCESS_BODY: &str = r#"{"id":"resp_ok","object":"response","status":"completed","model":"deepseek-v4-flash","output":[{"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"ok","annotations":[]}]}],"usage":{"input_tokens":10,"output_tokens":2,"input_tokens_details":{"cached_tokens":0}}}"#;
pub(crate) const MESSAGES_SUCCESS_BODY: &str = r#"{"id":"msg-ok","type":"message","role":"assistant","model":"minimax-m2.7","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":0}}"#;
pub(crate) const MESSAGES_SUCCESS_BODY_WITH_ECHOED_KEY_IN_THINKING: &str = r#"{"id":"msg-ok","type":"message","role":"assistant","model":"minimax-m2.7","content":[{"type":"thinking","thinking":"opaque/account+key=42","signature":"sig_123"},{"type":"text","text":"ok"}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":10,"output_tokens":2,"cache_read_input_tokens":0}}"#;
pub(crate) const CHAT_STREAM_BODY: &str = concat!(
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":0}}}\n\n",
    "data: [DONE]\n\n"
);
pub(crate) const CHAT_STREAM_WITHOUT_USAGE: &str = concat!(
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);
pub(crate) const CHAT_STREAM_WITH_UNTERMINATED_KEY_TAIL: &str = concat!(
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"opaque/account+key=42\"},\"finish_reason\":\"stop\"}]}"
);
pub(crate) const CHAT_STREAM_WITH_SPLIT_ECHOED_KEY: &str = concat!(
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"before opaque/account+\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"key=42 after\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":0}}}\n\n",
    "data: [DONE]\n\n"
);
pub(crate) const CHAT_STREAM_WITH_COMMON_KEY: &str = concat!(
    "data: {\"id\":\"chat-stream\",\"object\":\"chat.completion.chunk\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"before text after\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":0}}}\n\n",
    "data: [DONE]\n\n"
);
pub(crate) const MESSAGES_STREAM_BODY: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-stream\",\"model\":\"minimax-m2.7\",\"usage\":{\"input_tokens\":6,\"cache_read_input_tokens\":4}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
);
pub(crate) const MESSAGES_STREAM_HEAD: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg-stream\",\"model\":\"minimax-m2.7\",\"usage\":{\"input_tokens\":6,\"cache_read_input_tokens\":4}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n"
);
pub(crate) const MESSAGES_STREAM_TAIL: &str = concat!(
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
);

pub(crate) fn reply(status: u16, body: &'static str) -> MockReply {
    MockReply { status, body }
}

pub(crate) fn ok() -> MockReply {
    reply(200, SUCCESS_BODY)
}

pub(crate) fn ok_messages() -> MockReply {
    reply(200, MESSAGES_SUCCESS_BODY)
}

pub(crate) fn ok_responses() -> MockReply {
    reply(200, RESPONSES_SUCCESS_BODY)
}

pub(crate) fn limited() -> MockReply {
    reply(429, LIMITED_BODY)
}

pub(crate) fn script(entries: &[(&str, &[MockReply])]) -> HashMap<String, VecDeque<MockReply>> {
    entries
        .iter()
        .map(|(key, queued)| ((*key).to_string(), VecDeque::from(queued.to_vec())))
        .collect()
}

pub(crate) fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ocg-gateway-test-{}-{}",
        label,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub(crate) fn free_port() -> u16 {
    let listener = StdTcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

pub(crate) fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client should build")
}

pub(crate) fn build_state(base_url: String, keys: &[&str]) -> (Arc<CoreStateInner>, PathBuf) {
    build_state_with_routing(base_url, keys, RoutingMode::StrictPriority, false)
}

pub(crate) fn build_state_with_routing(
    base_url: String,
    keys: &[&str],
    routing_mode: RoutingMode,
    conversation_sticky: bool,
) -> (Arc<CoreStateInner>, PathBuf) {
    let dir = temp_data_dir("state");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("test"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let mut config = state.config();
    config.gateway_key = "gw-test".into();
    config.upstream_base_url = base_url;
    config.proxy_mode = ProxyMode::Direct;
    config.routing_mode = routing_mode;
    config.conversation_sticky = conversation_sticky;
    state.set_config(config).unwrap();

    let now = chrono::Utc::now();
    for (idx, key) in keys.iter().enumerate() {
        let account = Account {
            id: format!("acct-{}", idx + 1),
            provider_id: ocg_core::provider::default_provider_id(),

            credential_kind: ocg_core::provider::default_credential_kind(),
            quota_scope: ocg_core::provider::default_quota_scope(),
            name: format!("acct-{}", idx + 1),
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
            created_at: now + chrono::Duration::seconds(idx as i64),
            updated_at: now + chrono::Duration::seconds(idx as i64),
        };
        state.db.lock().create_account(&account).unwrap();
    }
    refreshed_go_catalog::persist_refreshed_go_catalog(&state);
    refreshed_go_catalog::persist_enabled_zen_catalog(&state);

    (state, dir)
}

pub(crate) async fn start_gateway(state: Arc<CoreStateInner>) -> (u16, GatewayHandle) {
    let port = free_port();
    let handle = gateway::start_gateway(state, port).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (port, handle)
}

pub(crate) struct PreparedFallback {
    pub state: Arc<CoreStateInner>,
    pub dir: PathBuf,
    pub calls: FakeCalls,
    pub base_url: String,
    stop_mock: Option<tokio::sync::oneshot::Sender<()>>,
}

impl PreparedFallback {
    pub async fn go(entries: &[(&str, &[MockReply])], keys: &[&str]) -> Self {
        Self::start(
            script(entries),
            keys,
            RoutingMode::StrictPriority,
            false,
            false,
        )
        .await
    }

    pub async fn zen_go(entries: &[(&str, &[MockReply])], keys: &[&str]) -> Self {
        Self::start(
            script(entries),
            keys,
            RoutingMode::StrictPriority,
            false,
            true,
        )
        .await
    }

    pub async fn routing(
        entries: &[(&str, &[MockReply])],
        keys: &[&str],
        routing_mode: RoutingMode,
        conversation_sticky: bool,
    ) -> Self {
        Self::start(
            script(entries),
            keys,
            routing_mode,
            conversation_sticky,
            false,
        )
        .await
    }

    pub async fn start(
        replies: HashMap<String, VecDeque<MockReply>>,
        keys: &[&str],
        routing_mode: RoutingMode,
        conversation_sticky: bool,
        zen_go: bool,
    ) -> Self {
        let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
        let upstream = if zen_go {
            format!("{base_url}/zen/go")
        } else {
            base_url.clone()
        };
        let (state, dir) =
            build_state_with_routing(upstream, keys, routing_mode, conversation_sticky);
        Self {
            state,
            dir,
            calls,
            base_url,
            stop_mock: Some(stop_mock),
        }
    }

    pub async fn bind(self) -> FallbackHarness {
        let (port, gateway_handle) = start_gateway(self.state.clone()).await;
        FallbackHarness {
            state: self.state,
            dir: Some(self.dir),
            port,
            gateway_handle: Some(gateway_handle),
            calls: self.calls,
            delayed_calls: None,
            stop_mock: self.stop_mock,
            extra_stops: Vec::new(),
            _goat_route: None,
        }
    }
}

pub(crate) struct FallbackHarness {
    pub state: Arc<CoreStateInner>,
    dir: Option<PathBuf>,
    pub port: u16,
    gateway_handle: Option<GatewayHandle>,
    pub calls: FakeCalls,
    pub delayed_calls: Option<Arc<AtomicUsize>>,
    stop_mock: Option<tokio::sync::oneshot::Sender<()>>,
    extra_stops: Vec<tokio::sync::oneshot::Sender<()>>,
    _goat_route: Option<GoatLoopbackRouteGuard>,
}

impl FallbackHarness {
    pub async fn go(entries: &[(&str, &[MockReply])], keys: &[&str]) -> Self {
        PreparedFallback::go(entries, keys).await.bind().await
    }

    pub async fn zen_go(entries: &[(&str, &[MockReply])], keys: &[&str]) -> Self {
        PreparedFallback::zen_go(entries, keys).await.bind().await
    }

    pub async fn routing(
        entries: &[(&str, &[MockReply])],
        keys: &[&str],
        routing_mode: RoutingMode,
        conversation_sticky: bool,
    ) -> Self {
        PreparedFallback::routing(entries, keys, routing_mode, conversation_sticky)
            .await
            .bind()
            .await
    }

    pub async fn delayed(
        status: axum::http::StatusCode,
        content_type: &'static str,
        chunks: DelayedChunks,
        keys: &[&str],
    ) -> Self {
        Self::delayed_seq(status, content_type, vec![chunks], keys).await
    }

    pub async fn delayed_seq(
        status: axum::http::StatusCode,
        content_type: &'static str,
        responses: Vec<DelayedChunks>,
        keys: &[&str],
    ) -> Self {
        let (base_url, delayed_calls, stop_mock) =
            start_delayed_fake_upstream(status, content_type, responses).await;
        let (state, dir) = build_state(base_url, keys);
        let (port, gateway_handle) = start_gateway(state.clone()).await;
        Self {
            state,
            dir: Some(dir),
            port,
            gateway_handle: Some(gateway_handle),
            calls: Arc::new(Mutex::new(Vec::new())),
            delayed_calls: Some(delayed_calls),
            stop_mock: Some(stop_mock),
            extra_stops: Vec::new(),
            _goat_route: None,
        }
    }

    pub async fn disconnect(raw_response: Vec<u8>, keys: &[&str]) -> Self {
        let (base_url, delayed_calls, stop_mock) =
            start_raw_disconnect_upstream(raw_response).await;
        let (state, dir) = build_state(base_url, keys);
        let (port, gateway_handle) = start_gateway(state.clone()).await;
        Self {
            state,
            dir: Some(dir),
            port,
            gateway_handle: Some(gateway_handle),
            calls: Arc::new(Mutex::new(Vec::new())),
            delayed_calls: Some(delayed_calls),
            stop_mock: Some(stop_mock),
            extra_stops: Vec::new(),
            _goat_route: None,
        }
    }

    pub async fn from_state(state: Arc<CoreStateInner>, dir: PathBuf) -> Self {
        Self::from_parts(state, dir, Arc::new(Mutex::new(Vec::new())), None, None).await
    }

    pub async fn from_parts(
        state: Arc<CoreStateInner>,
        dir: PathBuf,
        calls: FakeCalls,
        stop_mock: Option<tokio::sync::oneshot::Sender<()>>,
        delayed_calls: Option<Arc<AtomicUsize>>,
    ) -> Self {
        let (port, gateway_handle) = start_gateway(state.clone()).await;
        Self {
            state,
            dir: Some(dir),
            port,
            gateway_handle: Some(gateway_handle),
            calls,
            delayed_calls,
            stop_mock,
            extra_stops: Vec::new(),
            _goat_route: None,
        }
    }

    pub async fn delayed_configured(
        status: axum::http::StatusCode,
        content_type: &'static str,
        responses: Vec<DelayedChunks>,
        keys: &[&str],
        patch: impl FnOnce(&mut AppConfig),
    ) -> Self {
        let (base_url, delayed_calls, stop_mock) =
            start_delayed_fake_upstream(status, content_type, responses).await;
        let (state, dir) = build_state(base_url, keys);
        let mut config = state.config();
        patch(&mut config);
        state.set_config(config).unwrap();
        Self::from_parts(
            state,
            dir,
            Arc::new(Mutex::new(Vec::new())),
            Some(stop_mock),
            Some(delayed_calls),
        )
        .await
    }

    pub fn configure(&self, patch: impl FnOnce(&mut AppConfig)) {
        let mut config = self.state.config();
        patch(&mut config);
        self.state.set_config(config).unwrap();
    }

    pub fn set_enabled(&self, account_id: &str, enabled: bool) {
        set_account_enabled(&self.state, account_id, enabled);
    }

    pub fn account(&self, account_id: &str) -> Account {
        self.state
            .db
            .lock()
            .get_account(account_id)
            .unwrap()
            .unwrap()
    }

    pub fn logs(&self) -> Vec<ForwardLog> {
        self.state.db.lock().list_forward_logs(50).unwrap()
    }

    pub fn call_keys(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| call.key.clone())
            .collect()
    }

    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    pub fn delayed_count(&self) -> usize {
        self.delayed_calls
            .as_ref()
            .map(|calls| calls.load(std::sync::atomic::Ordering::Relaxed))
            .unwrap_or(0)
    }

    pub fn attach_goat_route(&mut self, goat_id: String, origin: String) {
        self._goat_route = Some(install_goat_loopback_route_for_test(goat_id, origin).unwrap());
    }

    pub fn take_dir(&mut self) -> PathBuf {
        self.dir.take().expect("harness data dir already taken")
    }

    pub fn push_stop(&mut self, stop: tokio::sync::oneshot::Sender<()>) {
        self.extra_stops.push(stop);
    }

    pub async fn chat(&self) -> (u16, String) {
        chat(self.port).await
    }

    pub async fn chat_with_conversation(
        &self,
        conversation_id: Option<&str>,
        user: &str,
    ) -> (u16, String) {
        chat_with_conversation(self.port, conversation_id, user).await
    }

    pub async fn protocol(
        &self,
        path: &str,
        model: &str,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        protocol_call(self.port, path, model).await
    }

    pub async fn stream(&self, path: &str, model: &str) -> (axum::http::StatusCode, String) {
        protocol_stream_call(self.port, path, model).await
    }

    pub async fn models(&self) -> (axum::http::StatusCode, String) {
        models(self.port).await
    }

    pub async fn application_models(&self) -> (axum::http::StatusCode, serde_json::Value) {
        get_application_models(self.port).await
    }

    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        if let Some(handle) = self.gateway_handle.take() {
            gateway::stop_gateway(handle);
        }
        if let Some(stop) = self.stop_mock.take() {
            let _ = stop.send(());
        }
        for stop in self.extra_stops.drain(..) {
            let _ = stop.send(());
        }
        if let Some(dir) = self.dir.take() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

impl Drop for FallbackHarness {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) async fn chat(port: u16) -> (u16, String) {
    chat_with_conversation(port, None, "ping").await
}

pub(crate) async fn chat_with_conversation(
    port: u16,
    conversation_id: Option<&str>,
    user: &str,
) -> (u16, String) {
    let request = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .json(&serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": user}],
            "max_tokens": 3,
            "stream": false
        }));
    let request = if let Some(conversation_id) = conversation_id {
        request.header("x-ocg-conversation-id", conversation_id)
    } else {
        request
    };
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let body = response.text().await.unwrap();
    (status, body)
}

pub(crate) fn set_account_enabled(state: &Arc<CoreStateInner>, account_id: &str, enabled: bool) {
    state
        .db
        .lock()
        .update_account(
            account_id,
            &AccountUpdate {
                name: None,
                username: None,
                password: None,
                key: None,
                enabled: Some(enabled),
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            None,
            None,
        )
        .unwrap();
}

pub(crate) fn create_goat_account(
    state: &Arc<CoreStateInner>,
    source_account_id: &str,
    account_id: &str,
    key: &str,
) {
    let mut account = state
        .db
        .lock()
        .get_account(source_account_id)
        .unwrap()
        .expect("source account");
    account.id = account_id.to_string();
    account.provider_id = COMMAND_CODE_PROVIDER_ID.to_string();
    account.name = account_id.to_string();
    account.key_cipher = state.encrypt_key(key).unwrap();
    account.cooldown_until = None;
    account.cooldown_generic_until = None;
    account.cooldown_5h_until = None;
    account.cooldown_week_until = None;
    account.cooldown_month_until = None;
    account.cooldown_free_until = None;
    account.auth_error = None;
    account.created_at = chrono::Utc::now();
    account.updated_at = account.created_at;
    account.enabled = false;
    state.db.lock().create_account(&account).unwrap();
    force_enable_unroutable_account_for_loopback_test(&state.data_dir, &account.id);
    persist_goat_verified_catalog(
        state,
        account_id,
        ocg_core::provider::COMMAND_CODE_GOAT_INCLUDED_MODEL_IDS,
    );
    assert!(
        state
            .db
            .lock()
            .get_account(&account.id)
            .unwrap()
            .unwrap()
            .enabled,
        "loopback GOAT fixture must be enabled in the already-open database"
    );
}

pub(crate) fn persist_goat_verified_catalog(
    state: &Arc<CoreStateInner>,
    _account_id: &str,
    models: &[&str],
) {
    let models: Vec<String> = models.iter().map(|model| (*model).to_string()).collect();
    let scope = ocg_core::provider_contracts::ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let now = chrono::Utc::now();
    let db = state.db.lock();
    db.set_contract_catalog(
        &scope,
        &models,
        Some(now),
        "test_command_code_catalog",
        "http://127.0.0.1/provider/v1/models",
        now,
    )
    .unwrap();
    let overrides = models
        .iter()
        .flat_map(|model| {
            [
                ocg_core::provider::UpstreamProtocolKind::ChatCompletions,
                ocg_core::provider::UpstreamProtocolKind::Messages,
            ]
            .into_iter()
            .map(move |protocol| {
                (
                    model.clone(),
                    protocol,
                    ocg_core::provider_contracts::ProtocolOverrideState::ForceOn,
                )
            })
        })
        .collect::<Vec<_>>();
    db.set_model_protocol_overrides(&scope, &overrides, now)
        .unwrap();
    drop(db);
    state.reload_provider_contracts().unwrap();
}

pub(crate) fn force_enable_unroutable_account_for_loopback_test(data_dir: &Path, account_id: &str) {
    let conn = rusqlite::Connection::open(data_dir.join("data.sqlite"))
        .expect("loopback test sqlite should open");
    conn.busy_timeout(StdDuration::from_millis(5_000))
        .expect("loopback test sqlite should set busy timeout");
    let changed = conn
        .execute(
            "UPDATE accounts SET enabled = 1 WHERE id = ?1",
            [account_id],
        )
        .expect("loopback test enable poke should execute");
    assert_eq!(changed, 1, "loopback test account {account_id} must exist");
    // This fixture inserts a disabled account, then enables the test route
    // directly. Enable its independent binding too; ordinary account toggles
    // must not override a binding the user explicitly disabled.
    let binding_changed = conn
        .execute(
            "UPDATE credential_bindings SET enabled = 1 WHERE account_id = ?1",
            [account_id],
        )
        .expect("loopback fixture binding should enable");
    assert_eq!(binding_changed, 1, "loopback fixture binding must exist");
}

pub(crate) fn prepare_goat(
    state: &Arc<CoreStateInner>,
    goat_key: &str,
    catalog: &[&str],
    prefer_goat: bool,
) -> String {
    let goat_id = format!("goat-{}", uuid::Uuid::new_v4());
    create_goat_account(state, "acct-1", &goat_id, goat_key);
    if !catalog.is_empty() {
        persist_goat_verified_catalog(state, &goat_id, catalog);
    }
    let order = if prefer_goat {
        vec![goat_id.clone(), "acct-1".into(), ZEN_FREE_ACCOUNT_ID.into()]
    } else {
        vec!["acct-1".into(), goat_id.clone(), ZEN_FREE_ACCOUNT_ID.into()]
    };
    state.db.lock().reorder_accounts(&order).unwrap();
    goat_id
}

pub(crate) async fn gemini_call(
    port: u16,
    model: &str,
) -> (axum::http::StatusCode, serde_json::Value) {
    let response = loopback_client()
        .post(format!(
            "http://127.0.0.1:{port}/v1beta/models/{model}:generateContent"
        ))
        .header("x-goog-api-key", "gw-test")
        .json(&serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "ping"}]}]
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap();
    (status, body)
}

pub(crate) async fn models(port: u16) -> (axum::http::StatusCode, String) {
    let response = loopback_client()
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    (status, body)
}

pub(crate) async fn protocol_call(
    port: u16,
    path: &str,
    model: &str,
) -> (axum::http::StatusCode, serde_json::Value) {
    let body = match path {
        "/v1/chat/completions" => serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": false
        }),
        "/v1/responses" => serde_json::json!({
            "model": model,
            "input": "ping",
            "store": false,
            "max_output_tokens": 3,
            "stream": false
        }),
        "/v1/messages" => serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": false
        }),
        _ => panic!("unsupported test path: {path}"),
    };
    let client = loopback_client();
    let request = client
        .post(format!("http://127.0.0.1:{port}{path}"))
        .json(&body);
    let request = if path == "/v1/messages" {
        request
            .header("x-api-key", "gw-test")
            .header("anthropic-version", "2023-06-01")
    } else {
        request.header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
    };
    let response = request.send().await.unwrap();
    let status = response.status();
    assert!(
        response
            .headers()
            .get("x-ocg-request-id")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("ocg-")),
        "{path} should return a request id"
    );
    let body = response.json().await.unwrap();
    (status, body)
}

pub(crate) async fn protocol_stream_call(
    port: u16,
    path: &str,
    model: &str,
) -> (axum::http::StatusCode, String) {
    let body = match path {
        "/v1/chat/completions" => serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": true
        }),
        "/v1/responses" => serde_json::json!({
            "model": model,
            "input": "ping",
            "store": false,
            "max_output_tokens": 3,
            "stream": true
        }),
        "/v1/messages" => serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": true
        }),
        _ => panic!("unsupported test path: {path}"),
    };
    let client = loopback_client();
    let request = client
        .post(format!("http://127.0.0.1:{port}{path}"))
        .json(&body);
    let request = if path == "/v1/messages" {
        request.header("x-api-key", "gw-test")
    } else {
        request.header(reqwest::header::AUTHORIZATION, "Bearer gw-test")
    };
    let response = request.send().await.unwrap();
    let status = response.status();
    assert!(
        response
            .headers()
            .get("x-ocg-request-id")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("ocg-")),
        "{path} should return a request id"
    );
    let body = response.text().await.unwrap();
    (status, body)
}

pub(crate) fn chat_stream_text(body: &str) -> String {
    body.split("\n\n")
        .filter_map(|frame| frame.lines().find_map(|line| line.strip_prefix("data: ")))
        .filter(|payload| *payload != "[DONE]")
        .filter_map(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())
        .filter_map(|value| {
            value
                .pointer("/choices/0/delta/content")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

pub(crate) fn alias_has_enabled_protocol(state: &Arc<CoreStateInner>, alias_name: &str) -> bool {
    let contracts = state.provider_contracts();
    match alias::resolve(alias_name) {
        Ok(alias::ResolvedModel::Alias { mappings, .. }) => mappings
            .iter()
            .any(|mapping| mapping.routeable && contracts.mapping_has_enabled_protocol(mapping)),
        Ok(alias::ResolvedModel::PinnedRaw { mapping, .. }) => {
            mapping.routeable && contracts.mapping_has_enabled_protocol(&mapping)
        }
        _ => false,
    }
}

pub(crate) fn assert_local_openai_alias_list(state: &Arc<CoreStateInner>, body: &str) {
    let payload: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(payload["object"], "list", "{body}");
    let expected = ocg_core::alias::published_routeable_aliases()
        .into_iter()
        .filter(|published| alias_has_enabled_protocol(state, &published.alias))
        .collect::<Vec<_>>();
    let data = payload["data"].as_array().expect("OpenAI list data");
    for published in &expected {
        let item = data
            .iter()
            .find(|item| item["id"] == published.alias)
            .unwrap_or_else(|| panic!("missing base Alias {} in {body}", published.alias));
        assert_eq!(item["object"], "model");
        assert_eq!(item["owned_by"], published.owned_by);
    }
    for item in data {
        let alias = item["id"].as_str().expect("model id");
        assert!(!alias.contains('/'));
    }
    assert!(!body.contains(ocg_core::provider::COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM));
}

pub(crate) fn expected_local_application_models(state: &Arc<CoreStateInner>) -> Vec<String> {
    let priced = state
        .pricing_snapshot()
        .models
        .iter()
        .map(|model| model.model_id.clone())
        .collect::<HashSet<_>>();
    alias::routeable_aliases_for(OPENCODE_PROVIDER_ID)
        .into_iter()
        .filter(|alias| alias_has_enabled_protocol(state, alias))
        .filter(|alias| {
            priced.contains(alias)
                || alias
                    .strip_suffix("-highspeed")
                    .is_some_and(|base| priced.contains(base))
        })
        .collect()
}

pub(crate) async fn get_application_models(
    port: u16,
) -> (axum::http::StatusCode, serde_json::Value) {
    let response = loopback_client()
        .get(format!(
            "http://127.0.0.1:{port}/dashboard/api/v3/application-models"
        ))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json::<serde_json::Value>().await.unwrap();
    if status == axum::http::StatusCode::OK {
        assert!(
            body.get("models")
                .and_then(serde_json::Value::as_array)
                .is_some(),
            "GET /application-models must emit a models array: {body}"
        );
        assert!(
            body.get("revision").is_some()
                && body.get("processGeneration").is_some()
                && body.get("pricingRevision").is_some(),
            "GET /application-models must emit V3 snapshot tokens: {body}"
        );
    }
    (status, body)
}

pub(crate) fn application_model_ids(body: &serde_json::Value) -> Vec<String> {
    body.get("models")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("GET /application-models must emit models: {body}"))
        .iter()
        .map(|item| {
            item.as_str()
                .unwrap_or_else(|| panic!("application-models entries must be strings: {body}"))
                .to_string()
        })
        .collect()
}

pub(crate) fn assert_no_application_model_side_effects(
    state: &Arc<CoreStateInner>,
    calls: &Arc<Mutex<Vec<MockCall>>>,
    before: Option<&Account>,
    routing_before: &str,
) {
    assert!(
        calls.lock().unwrap().is_empty(),
        "GET /application-models must not call upstream: {:?}",
        calls.lock().unwrap()
    );
    assert!(state.db.lock().list_forward_logs(10).unwrap().is_empty());
    assert_eq!(format!("{:?}", state.routing), routing_before);
    if let Some(before) = before {
        let after = state.db.lock().get_account(&before.id).unwrap().unwrap();
        assert_eq!(after.cooldown_until, before.cooldown_until);
        assert_eq!(after.last_error, before.last_error);
        assert_eq!(after.auth_error, before.auth_error);
        assert_eq!(after.updated_at, before.updated_at);
    }
}

pub(crate) fn apply_list_whitelist_config(
    state: &Arc<CoreStateInner>,
    upstream_base: String,
    proxy_base: &str,
    listed: &[&str],
) {
    let mut config = state.config();
    config.upstream_base_url = upstream_base;
    config.proxy_mode = ProxyMode::List;
    config.proxy_url = proxy_base.to_string();
    config.proxy_list_direction = ProxyListDirection::Whitelist;
    config.proxy_list_models = listed.iter().map(|model| model.to_string()).collect();
    state.set_config(config).unwrap();
}

pub(crate) fn disable_go_protocols(
    state: &Arc<CoreStateInner>,
    model_id: &str,
    chat: bool,
    responses: bool,
    messages: bool,
) {
    let now = chrono::Utc::now();
    let scope = ocg_core::provider_contracts::ContractScope::provider(OPENCODE_PROVIDER_ID);
    let db = state.db.lock();
    let mut rows = Vec::new();
    if !chat {
        rows.push((
            model_id.to_string(),
            ocg_core::provider::UpstreamProtocolKind::ChatCompletions,
            ocg_core::provider_contracts::ProtocolOverrideState::ForceOff,
        ));
    }
    if !responses {
        rows.push((
            model_id.to_string(),
            ocg_core::provider::UpstreamProtocolKind::Responses,
            ocg_core::provider_contracts::ProtocolOverrideState::ForceOff,
        ));
    }
    if !messages {
        rows.push((
            model_id.to_string(),
            ocg_core::provider::UpstreamProtocolKind::Messages,
            ocg_core::provider_contracts::ProtocolOverrideState::ForceOff,
        ));
    }
    if !rows.is_empty() {
        db.set_model_protocol_overrides(&scope, &rows, now).unwrap();
    }
    drop(db);
    state.reload_provider_contracts().unwrap();
}

pub(crate) fn disable_command_protocols(state: &Arc<CoreStateInner>, model_id: &str) {
    let scope = ocg_core::provider_contracts::ContractScope::provider(COMMAND_CODE_PROVIDER_ID);
    let rows = [
        ocg_core::provider::UpstreamProtocolKind::ChatCompletions,
        ocg_core::provider::UpstreamProtocolKind::Responses,
        ocg_core::provider::UpstreamProtocolKind::Messages,
    ]
    .into_iter()
    .map(|protocol| {
        (
            model_id.to_string(),
            protocol,
            ocg_core::provider_contracts::ProtocolOverrideState::ForceOff,
        )
    })
    .collect::<Vec<_>>();
    state
        .db
        .lock()
        .set_model_protocol_overrides(&scope, &rows, chrono::Utc::now())
        .unwrap();
    state.reload_provider_contracts().unwrap();
}

pub(crate) async fn dashboard_protocol_probe(
    port: u16,
    state: &Arc<CoreStateInner>,
    account_id: &str,
    model_id: &str,
    protocols: &[&str],
) -> (axum::http::StatusCode, serde_json::Value) {
    let response = loopback_client()
        .post(format!(
            "http://127.0.0.1:{port}/dashboard/api/v3/providers/{OPENCODE_PROVIDER_ID}/protocol-probes"
        ))
        .json(&serde_json::json!({
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
            "accountId": account_id,
            "modelId": model_id,
            "protocols": protocols }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap();
    (status, body)
}

pub(crate) fn corrupt_account_cipher(state: &Arc<CoreStateInner>, account_id: &str, cipher: &str) {
    state
        .db
        .lock()
        .update_account(
            account_id,
            &AccountUpdate {
                name: None,
                username: None,
                password: None,
                key: None,
                enabled: None,
                referral_code: None,
                purchase_date: None,
                notes: None,
            },
            Some(cipher),
            None,
        )
        .unwrap();
}

pub(crate) async fn start_goat(
    entries: &[(&str, &[MockReply])],
    catalog: &[&str],
    prefer_goat: bool,
    loopback: bool,
) -> (FallbackHarness, String) {
    let p = PreparedFallback::go(entries, &["open-key"]).await;
    let origin = p.base_url.clone();
    let goat_id = prepare_goat(&p.state, "goat-key", catalog, prefer_goat);
    let mut h = p.bind().await;
    if loopback {
        h.attach_goat_route(goat_id.clone(), origin);
    }
    (h, goat_id)
}

pub(crate) fn legacy_forward_log() -> ForwardLog {
    ForwardLog {
        id: 0,
        timestamp: chrono::Utc::now(),
        model: "legacy".into(),
        account_id: "acct".into(),
        account_name: "acct".into(),
        route_account_id: None,
        provider_id: None,

        credential_account_id: None,
        client_key_id: None,
        client_key_name: None,
        status: "success".into(),
        http_status: Some(200),
        route: String::new(),
        prompt_tokens: 0,
        completion_tokens: 0,
        cached_tokens: 0,
        cache_creation_tokens: 0,
        cost: Some(0.0),
        raw_cost_usd: None,
        quota_debit: None,
        effective_paid_cost_usd: None,
        pricing_revision_id: None,
        quota_multiplier: None,
        local_adjustment_multiplier: None,
        service_tier: None,
        cost_state: "legacy_estimate".into(),
        error_message: None,
        request_id: None,
        attempt: None,
        error_source: None,
        error_stage: None,
        duration_ms: None,
        diagnostic: None,
    }
}

pub(crate) fn empty_forward_query() -> ForwardLogQueryOptions<'static> {
    ForwardLogQueryOptions {
        limit: 10,
        offset: 0,
        status: None,
        account_id: None,
        provider_id: None,

        route_account_id: None,
        credential_account_id: None,
        model: None,
        key_id: None,
        request_id: None,
        start_time: None,
        end_time: None,
        sort_by: None,
        sort_order: None,
    }
}

pub(crate) fn forbidden() -> MockReply {
    reply(403, r#"{"error":{"message":"forbidden key"}}"#)
}

pub(crate) fn server_error() -> MockReply {
    reply(500, r#"{"error":"temporary"}"#)
}

pub(crate) fn dashboard_cas(state: &CoreStateInner, patch: serde_json::Value) -> serde_json::Value {
    let mut body = patch.as_object().cloned().unwrap_or_default();
    body.insert(
        "expectedRevision".into(),
        serde_json::json!(state.settings_revision()),
    );
    body.insert(
        "processGeneration".into(),
        serde_json::json!(state.process_generation()),
    );
    serde_json::Value::Object(body)
}

pub(crate) async fn dashboard_json(
    port: u16,
    method: reqwest::Method,
    api: &str,
    path: &str,
    body: Option<&serde_json::Value>,
) -> (axum::http::StatusCode, serde_json::Value) {
    let client = loopback_client();
    let url = format!("http://127.0.0.1:{port}/dashboard/api/{api}{path}");
    let request = client.request(method, url);
    let request = match body {
        Some(body) => request.json(body),
        None => request,
    };
    let response = request.send().await.unwrap();
    let status = response.status();
    let parsed = response.json().await.unwrap_or(serde_json::Value::Null);
    (status, parsed)
}

pub(crate) async fn v3_mutate(
    port: u16,
    state: &CoreStateInner,
    path: &str,
    patch: serde_json::Value,
) -> (axum::http::StatusCode, serde_json::Value) {
    dashboard_json(
        port,
        reqwest::Method::POST,
        "v3",
        path,
        Some(&dashboard_cas(state, patch)),
    )
    .await
}

pub(crate) async fn v4_mutate(
    port: u16,
    state: &CoreStateInner,
    path: &str,
    patch: serde_json::Value,
) -> (axum::http::StatusCode, serde_json::Value) {
    dashboard_json(
        port,
        reqwest::Method::POST,
        "v4",
        path,
        Some(&dashboard_cas(state, patch)),
    )
    .await
}

pub(crate) async fn v4_get(port: u16, path: &str) -> (axum::http::StatusCode, serde_json::Value) {
    dashboard_json(port, reqwest::Method::GET, "v4", path, None).await
}

pub(crate) struct LabProvider {
    pub provider_id: String,
    pub account_id: Option<String>,
}

// Keep each lab scene fact explicit; packing would only meet the 7-arg lint threshold.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_dynamic_lab(
    port: u16,
    state: &CoreStateInner,
    name: &str,
    endpoint: &str,
    key: Option<&str>,
    public_model: &str,
    upstream_model: &str,
    protocol: &str,
) -> LabProvider {
    let auth_kind = if protocol == "messages" {
        "x-api-key"
    } else {
        "bearer"
    };
    let mut patch = serde_json::json!({
        "name": name,
        "endpointUrl": endpoint,
        "upstreamProtocol": protocol,
        "authKind": auth_kind,
        "models": [{
            "publicModel": public_model,
            "upstreamModel": upstream_model
        }]
    });
    if let Some(key) = key {
        patch["key"] = serde_json::json!(key);
    }
    let (status, created) = v3_mutate(port, state, "/providers", patch).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{created}");
    let provider_id = created["provider"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("dynamic provider id missing: {created}"))
        .to_string();
    let account_id = state
        .db
        .lock()
        .list_accounts()
        .unwrap()
        .into_iter()
        .find(|account| account.provider_id == provider_id)
        .map(|account| account.id);
    if let Some(id) = &account_id {
        let (status, body) = v3_mutate(
            port,
            state,
            &format!("/accounts/{id}/toggle"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    }
    LabProvider {
        provider_id,
        account_id,
    }
}

pub(crate) async fn create_custom_lab(
    port: u16,
    state: &CoreStateInner,
    name: &str,
    endpoint: &str,
    key: &str,
    model_id: &str,
    protocol: &str,
) -> String {
    let (status, created) = v3_mutate(
        port,
        state,
        "/accounts",
        serde_json::json!({
            "name": name,
            "key": key,
            "providerId": CUSTOM_PROVIDER_ID,
            "customConfig": {
                "endpointUrl": endpoint,
                "upstreamProtocol": protocol
            },
            "modelCapabilities": [{
                "modelId": model_id,
                "protocol": protocol
            }]
        }),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{created}");
    let id = created["account"]["id"]
        .as_str()
        .or_else(|| created["id"].as_str())
        .unwrap_or_else(|| panic!("custom account id missing: {created}"))
        .to_string();
    let (status, body) = v3_mutate(
        port,
        state,
        &format!("/accounts/{id}/toggle"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    id
}

pub(crate) struct IdentityRefs {
    pub identity_id: String,
    pub credential_id: String,
    pub binding_id: String,
}

pub(crate) fn identity_refs_for(state: &CoreStateInner, account_id: &str) -> IdentityRefs {
    let record = state
        .db
        .lock()
        .list_identity_model()
        .unwrap()
        .accounts
        .into_iter()
        .find(|record| record.account.id == account_id)
        .unwrap_or_else(|| panic!("identity record missing for {account_id}"));
    IdentityRefs {
        identity_id: record.identity_id,
        credential_id: record.credential_id,
        binding_id: record.binding_id,
    }
}

pub(crate) fn set_binding_gate(
    state: &CoreStateInner,
    account_id: &str,
    enabled: Option<bool>,
    model_scope: Option<ModelScope>,
) {
    let binding_id = identity_refs_for(state, account_id).binding_id;
    state
        .db
        .lock()
        .update_credential_binding(&binding_id, model_scope.as_ref(), enabled, None, None)
        .unwrap();
}

pub(crate) fn reorder_first(state: &CoreStateInner, first: &[String]) {
    let mut order = first.to_vec();
    for account in state.db.lock().list_accounts().unwrap() {
        if !order.iter().any(|id| id == &account.id) {
            order.push(account.id);
        }
    }
    state.db.lock().reorder_accounts(&order).unwrap();
}

pub(crate) fn sorted_logs(state: &CoreStateInner) -> Vec<ForwardLog> {
    let mut logs = state.db.lock().list_forward_logs(50).unwrap();
    logs.sort_by_key(|log| (log.request_id.clone(), log.attempt.unwrap_or(0), log.id));
    logs
}

pub(crate) fn routing_evidence_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tmp/routing-acceptance")
}

pub(crate) fn write_routing_evidence(scenario: &str, payload: serde_json::Value) {
    let dir = routing_evidence_dir();
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{scenario}.json"));
    fs::write(&path, serde_json::to_vec_pretty(&payload).unwrap()).unwrap();
}

pub(crate) fn arrivals_json(journal: &SharedJournal) -> serde_json::Value {
    serde_json::json!(
        journal
            .snapshot()
            .into_iter()
            .map(|arrival| {
                serde_json::json!({
                    "seq": arrival.seq,
                    "listener": arrival.listener,
                    "elapsedMs": arrival.elapsed_ms,
                    "dummyKey": arrival.key,
                    "method": arrival.method.to_string(),
                    "path": arrival.path,
                    "authorization": arrival.authorization,
                    "xApiKey": arrival.x_api_key,
                    "anthropicVersion": arrival.anthropic_version,
                })
            })
            .collect::<Vec<_>>()
    )
}

pub(crate) fn logs_json(logs: &[ForwardLog]) -> serde_json::Value {
    serde_json::json!(
        logs.iter()
            .map(|log| {
                serde_json::json!({
                    "attempt": log.attempt,
                    "accountId": log.account_id,
                    "providerId": log.provider_id,
                    "status": log.status,
                    "httpStatus": log.http_status,
                    "requestId": log.request_id,
                    "model": log.model,
                })
            })
            .collect::<Vec<_>>()
    )
}

pub(crate) struct JournaledUpstream {
    pub label: String,
    pub url: String,
    pub stop: tokio::sync::oneshot::Sender<()>,
}

pub(crate) async fn start_journaled_lab(
    journal: &SharedJournal,
    label: &str,
    key: &str,
    replies: &[MockReply],
) -> JournaledUpstream {
    let (url, _calls, stop) =
        start_fake_upstream_on_journal(label, script(&[(key, replies)]), journal.clone()).await;
    JournaledUpstream {
        label: label.to_string(),
        url,
        stop,
    }
}
