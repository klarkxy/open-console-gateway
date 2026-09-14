//! Shared helpers for V3 runtime characterization tests.
//!
//! Tests talk to public Gateway/dashboard HTTP plus `CoreState` seams that
//! production already exposes (config, pricing, contracts, catalogs). They do
//! not construct private gateway types.

#![allow(dead_code)]

#[path = "../refreshed_go_catalog.rs"]
mod refreshed_go_catalog;
pub(crate) use refreshed_go_catalog::{persist_enabled_zen_catalog, persist_refreshed_go_catalog};

use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::any;
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway;
use ocg_core::models::{Account, AccountUpdate, ProxyMode, RoutingMode};
use ocg_core::state::{CoreStateInner, GatewayHandle};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[path = "../fake_upstream.rs"]
mod fake_upstream;

pub(crate) use fake_upstream::{FakeReply, start_fake_upstream};

pub(crate) const GATEWAY_KEY: &str = "gw-v3-char";
pub(crate) const SUCCESS_BODY: &str = r#"{"id":"ok","object":"chat.completion","model":"deepseek-v4-flash","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":0}}}"#;
pub(crate) const SUCCESS_BODY_WITHOUT_USAGE: &str = r#"{"id":"ok","object":"chat.completion","model":"deepseek-v4-flash","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}"#;
pub(crate) const CHAT_STREAM_WITHOUT_USAGE: &str = concat!(
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chat-stream\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);
pub(crate) const LIMITED_BODY: &str = r#"{"type":"error","error":{"type":"GoUsageLimitError","message":"Weekly usage limit reached. Resets in 3 days."}}"#;
pub(crate) const FORBIDDEN_BODY: &str = r#"{"error":{"message":"forbidden key"}}"#;

pub(crate) fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!("ocg-v3-char-{}-{}", label, uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub(crate) fn loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("v3 test client should build")
}

pub(crate) fn build_go_state(base_url: String, keys: &[&str]) -> (Arc<CoreStateInner>, PathBuf) {
    let dir = temp_data_dir("state");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("v3-tests"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let mut config = state.config();
    config.gateway_key = GATEWAY_KEY.into();
    config.upstream_base_url = base_url;
    config.proxy_mode = ProxyMode::Direct;
    config.routing_mode = RoutingMode::StrictPriority;
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
    persist_refreshed_go_catalog(&state);
    persist_enabled_zen_catalog(&state);
    (state, dir)
}

pub(crate) async fn start_gateway(state: Arc<CoreStateInner>) -> (u16, GatewayHandle) {
    let handle = gateway::start_gateway_on(state, std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    (handle.port, handle)
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

pub(crate) async fn chat(port: u16, model: &str) -> (reqwest::StatusCode, String) {
    let response = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    (status, body)
}

pub(crate) async fn chat_stream(port: u16, model: &str) -> (reqwest::StatusCode, String) {
    let response = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {GATEWAY_KEY}"),
        )
        .json(&serde_json::json!({
            "model": model,
            "messages": [{"role": "user", "content": "ping"}],
            "max_tokens": 3,
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    (status, body)
}

#[derive(Clone)]
pub(crate) struct ScriptedReply {
    pub status: u16,
    pub body: &'static str,
}

#[derive(Clone)]
struct ScriptedState {
    replies: Arc<Vec<ScriptedReply>>,
    calls: Arc<AtomicUsize>,
    on_call: Arc<dyn Fn(usize) + Send + Sync>,
}

/// Loopback upstream that invokes `on_call(n)` before serving the nth scripted
/// reply. Exhausted scripts repeat the last reply so a late retry stays offline.
pub(crate) async fn start_scripted_upstream(
    replies: Vec<ScriptedReply>,
    on_call: Arc<dyn Fn(usize) + Send + Sync>,
) -> (String, Arc<AtomicUsize>, tokio::sync::oneshot::Sender<()>) {
    assert!(
        !replies.is_empty(),
        "scripted upstream needs at least one reply"
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .fallback(any(scripted_reply))
        .with_state(ScriptedState {
            replies: Arc::new(replies),
            calls: calls.clone(),
            on_call,
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("v3 scripted upstream should bind");
    let address = listener
        .local_addr()
        .expect("scripted listener should have an address");
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        let _ = server.await;
    });
    (format!("http://{address}"), calls, shutdown_tx)
}

async fn scripted_reply(
    axum::extract::State(state): axum::extract::State<ScriptedState>,
) -> impl IntoResponse {
    let index = state.calls.fetch_add(1, Ordering::SeqCst);
    (state.on_call)(index);
    let reply = state
        .replies
        .get(index)
        .unwrap_or_else(|| state.replies.last().expect("non-empty script"));
    let content_type = if reply.body.starts_with("data:") || reply.body.starts_with("event:") {
        "text/event-stream"
    } else {
        "application/json"
    };
    (
        StatusCode::from_u16(reply.status).expect("valid scripted status"),
        [("content-type", content_type)],
        reply.body,
    )
}
