//! HTTP regressions for the v2 Alias runtime slice.

use axum::http::StatusCode;
use chrono::Utc;
use ocg_core::alias;
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway;
use ocg_core::models::{Account, ProxyMode, RoutingMode};
use ocg_core::state::{CoreStateInner, GatewayHandle};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::net::TcpListener as StdTcpListener;
use std::path::PathBuf;
use std::sync::Arc;

#[allow(dead_code)]
#[path = "fixtures/fake_upstream.rs"]
mod fake_upstream;
#[path = "fixtures/refreshed_go_catalog.rs"]
mod refreshed_go_catalog;

use fake_upstream::{FakeReply, start_fake_upstream};

fn temp_data_dir(label: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ocg-v2-alias-runtime-{}-{}",
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

fn build_state(base_url: String, keys: &[&str]) -> (Arc<CoreStateInner>, PathBuf) {
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

    let now = Utc::now();
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

    (state, dir)
}

async fn start_gateway(state: Arc<CoreStateInner>) -> (u16, GatewayHandle) {
    let listener = StdTcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let handle = gateway::start_gateway(state, port).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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

fn assert_local_openai_alias_list(body: &Value) {
    assert_eq!(body["object"], "list");
    let data = body["data"].as_array().expect("OpenAI list data");
    assert!(!data.is_empty(), "{body}");
    let ids = data
        .iter()
        .map(|item| item["id"].as_str().expect("model id"))
        .collect::<Vec<_>>();
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{body}");
    for item in data {
        let published = item["id"].as_str().expect("model id");
        assert_eq!(item["object"], "model");
        assert!(item["owned_by"].is_string());
        assert_eq!(item["created"], 0);
        assert!(!published.contains('/'));
        assert!(!published.contains('_'));
        assert!(!published.contains(' '));
    }
    let go = data
        .iter()
        .find(|item| item["id"] == "deepseek-v4-flash")
        .expect("Go alias");
    assert_eq!(go["owned_by"], ocg_core::provider::OPENCODE_PROVIDER_ID);
    assert!(
        data.iter()
            .all(|item| item["id"] != "deepseek-v4-flash-free"),
        "Zen raw -free IDs must not be published"
    );
    let shared = data
        .iter()
        .find(|item| item["id"] == "hy3")
        .expect("current shared Go and Zen alias");
    assert_eq!(shared["owned_by"], ocg_core::provider::OPENCODE_PROVIDER_ID);
    assert!(
        data.iter()
            .all(|item| item["id"]
                != ocg_core::provider::COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM)
    );
}

#[tokio::test]
async fn models_list_ignores_raw_only_and_empty_fake_upstream() {
    let replies = HashMap::from([
        (
            "key-1".to_string(),
            VecDeque::from([FakeReply {
                status: 200,
                body: r#"{"object":"list","data":[{"id":"deepseek/deepseek-v4-flash"},{"id":"vendor-raw-not-an-alias"}]}"#,
            }]),
        ),
        (
            "key-2".to_string(),
            VecDeque::from([FakeReply {
                status: 200,
                body: r#"{"object":"list","data":[]}"#,
            }]),
        ),
    ]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url, &["key-1", "key-2"]);
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let response = loopback_client()
        .get(format!("http://127.0.0.1:{port}/v1/models"))
        .bearer_auth("gw-test")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_local_openai_alias_list(&body);
    assert!(
        calls.lock().unwrap().is_empty(),
        "GET /v1/models must not probe upstream even when accounts exist: {:?}",
        calls.lock().unwrap()
    );
    assert!(state.db.lock().list_forward_logs(8).unwrap().is_empty());
    for account in state.db.lock().list_accounts().unwrap() {
        assert!(account.cooldown_until.is_none());
        assert!(account.last_error.is_none());
        assert!(account.auth_error.is_none());
    }

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn slash_form_chat_model_does_not_collapse_and_does_not_hit_upstream() {
    let replies = HashMap::from([(
        "key-1".to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: r#"{"id":"ok"}"#,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url, &["key-1"]);
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let response = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth("gw-test")
        .json(&serde_json::json!({
            "model": "glm/5.2",
            "messages": [{"role": "user", "content": "ping"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("glm/5.2"))
    );
    assert!(calls.lock().unwrap().is_empty());

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn ambiguous_model_id_is_structured_across_client_formats() {
    let error = ocg_core::gateway::protocol::ProtocolError {
        status: StatusCode::BAD_REQUEST,
        message: alias::ResolveError::Ambiguous {
            requested: "shared-raw".into(),
            mappings: vec![
                alias::ProviderMapping {
                    provider_id: ocg_core::provider::OPENCODE_PROVIDER_ID.to_string(),
                    upstream_model: "shared-raw".into(),
                    routeable: true,
                },
                alias::ProviderMapping {
                    provider_id: ocg_core::provider::OPENCODE_ZEN_FREE_PROVIDER_ID.to_string(),
                    upstream_model: "shared-raw".into(),
                    routeable: true,
                },
            ],
        }
        .message(),
        code: Some(alias::AMBIGUOUS_MODEL_ID),
    };
    assert_eq!(error.code, Some(alias::AMBIGUOUS_MODEL_ID));

    for format in [
        ocg_core::gateway::protocol::ApiFormat::ChatCompletions,
        ocg_core::gateway::protocol::ApiFormat::Messages,
        ocg_core::gateway::protocol::ApiFormat::Responses,
        ocg_core::gateway::protocol::ApiFormat::Gemini,
    ] {
        let body = ocg_core::gateway::protocol::format_protocol_error(format, &error, None);
        match format {
            ocg_core::gateway::protocol::ApiFormat::Gemini => {
                assert_eq!(body["error"]["reason"], alias::AMBIGUOUS_MODEL_ID);
                assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");
            }
            ocg_core::gateway::protocol::ApiFormat::ChatCompletions
            | ocg_core::gateway::protocol::ApiFormat::Messages
            | ocg_core::gateway::protocol::ApiFormat::Responses => {
                assert_eq!(body["error"]["type"], alias::AMBIGUOUS_MODEL_ID);
            }
        }
        assert!(
            body["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(alias::AMBIGUOUS_MODEL_ID))
        );
    }
}

const CHAT_SUCCESS_BODY: &str = r#"{"id":"ok","object":"chat.completion","model":"upstream-should-not-leak","choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":0}}}"#;
const CHAT_STREAM_BODY: &str = concat!(
    "data: {\"id\":\"chat-stream\",\"model\":\"upstream-should-not-leak\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chat-stream\",\"model\":\"upstream-should-not-leak\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":0}}}\n\n",
    "data: [DONE]\n\n"
);

fn latest_log_identity(
    state: &CoreStateInner,
) -> (String, ocg_core::models::ForwardLogNativeAttribution) {
    let logs = state.db.lock().list_forward_logs(8).unwrap();
    assert_eq!(logs.len(), 1, "expected one forward log, got {logs:?}");
    let log = logs.into_iter().next().unwrap();
    let attribution = state
        .db
        .lock()
        .forward_log_native_attribution(log.id)
        .unwrap()
        .expect("native attribution should exist");
    (log.status, attribution)
}

#[tokio::test]
async fn successful_alias_chat_persists_requested_alias_and_upstream() {
    let replies = HashMap::from([(
        "key-1".to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_SUCCESS_BODY,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url, &["key-1"]);
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let response = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth("gw-test")
        .json(&serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "ping"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["model"], "deepseek-v4-flash");
    assert_eq!(calls.lock().unwrap().len(), 1);

    let (status, attribution) = latest_log_identity(&state);
    assert_eq!(status, "success");
    assert_eq!(
        attribution.requested_model.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        attribution.resolved_alias.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        attribution.upstream_model.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert!(attribution.native_cost_value.is_some());
    assert_eq!(attribution.native_cost_unit.as_deref(), Some("usd"));

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn mixed_case_alias_chat_persists_canonical_alias() {
    let replies = HashMap::from([(
        "key-1".to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_SUCCESS_BODY,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url, &["key-1"]);
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let response = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth("gw-test")
        .json(&serde_json::json!({
            "model": "MINIMAX-M3",
            "messages": [{"role": "user", "content": "ping"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["model"], "MINIMAX-M3");
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].path, "/v1/chat/completions");
    drop(recorded);

    let (status, attribution) = latest_log_identity(&state);
    assert_eq!(status, "success");
    assert_eq!(attribution.requested_model.as_deref(), Some("MINIMAX-M3"));
    assert_eq!(attribution.resolved_alias.as_deref(), Some("minimax-m3"));
    assert_eq!(attribution.upstream_model.as_deref(), Some("MINIMAX-M3"));

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn successful_alias_chat_stream_preserves_identity_after_finalize() {
    let replies = HashMap::from([(
        "key-1".to_string(),
        VecDeque::from([FakeReply {
            status: 200,
            body: CHAT_STREAM_BODY,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url, &["key-1"]);
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let response = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth("gw-test")
        .json(&serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "ping"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert!(body.contains("data:"), "{body}");
    assert_eq!(calls.lock().unwrap().len(), 1);

    let (status, attribution) = latest_log_identity(&state);
    assert_eq!(status, "success");
    assert_eq!(
        attribution.requested_model.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        attribution.resolved_alias.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        attribution.upstream_model.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert!(attribution.native_cost_value.is_some());
    assert_eq!(attribution.native_cost_unit.as_deref(), Some("usd"));

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn alias_upstream_error_still_persists_identity() {
    let replies = HashMap::from([(
        "key-1".to_string(),
        VecDeque::from([FakeReply {
            status: 500,
            body: r#"{"error":{"message":"boom"}}"#,
        }]),
    )]);
    let (base_url, calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url, &["key-1"]);
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let response = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth("gw-test")
        .json(&serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "ping"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(calls.lock().unwrap().len(), 1);

    let (status, attribution) = latest_log_identity(&state);
    assert_eq!(status, "error");
    assert_eq!(
        attribution.requested_model.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        attribution.resolved_alias.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        attribution.upstream_model.as_deref(),
        Some("deepseek-v4-flash")
    );

    stop(state, dir, gateway_handle, stop_mock);
}

#[tokio::test]
async fn alias_client_error_still_persists_identity() {
    let replies = HashMap::from([(
        "key-1".to_string(),
        VecDeque::from([FakeReply {
            status: 400,
            body: r#"{"error":{"message":"bad request"}}"#,
        }]),
    )]);
    let (base_url, _calls, stop_mock) = start_fake_upstream(replies).await;
    let (state, dir) = build_state(base_url, &["key-1"]);
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let response = loopback_client()
        .post(format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .bearer_auth("gw-test")
        .json(&serde_json::json!({
            "model": "deepseek-v4-flash",
            "messages": [{"role": "user", "content": "ping"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let (status, attribution) = latest_log_identity(&state);
    assert_eq!(status, "client_error");
    assert_eq!(
        attribution.resolved_alias.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        attribution.requested_model.as_deref(),
        Some("deepseek-v4-flash")
    );
    assert_eq!(
        attribution.upstream_model.as_deref(),
        Some("deepseek-v4-flash")
    );

    stop(state, dir, gateway_handle, stop_mock);
}
