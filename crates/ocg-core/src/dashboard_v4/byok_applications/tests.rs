use super::*;
use crate::byok_application::{
    ByokClient, ByokError, ByokHostRequest, ByokInspection, ByokSecret, ByokStatus,
};
use crate::crypto::StaticKeyCipher;
use crate::dashboard_session::SESSION_COOKIE;
use crate::dashboard_v3::V3ApiError;
use crate::dashboard_v4::types::ByokConfigureRequest;
use crate::db::Database;
use crate::gateway_keys;
use crate::models::{
    Account, AccountCustomConfigInput, AccountModelCapabilityInput, AccountSetupStep, AccountType,
};
use crate::provider::{CUSTOM_PROVIDER_ID, CredentialKind, QuotaScope, UpstreamProtocolKind};
use crate::state::{CoreState, CoreStateInner};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const EXACT_MODEL: &str = "vendor/model.name";

fn open_state(label: &str) -> (PathBuf, CoreState) {
    let dir = std::env::temp_dir().join(format!("ocg-byok-api-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new(label)),
        )
        .unwrap(),
    );
    // A fresh catalog skips the background models.dev fetch inside publication.
    *state.modelsdev_catalog.write() = Arc::new(crate::modelsdev::ModelsDevCatalog {
        fetched_at: Some(Utc::now()),
        models: Default::default(),
    });
    (dir, state)
}

fn close_state(dir: PathBuf, state: CoreState) {
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

fn body_bytes(value: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(value).unwrap())
}

fn expectation(state: &CoreState) -> Value {
    json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
    })
}

fn configure_body(state: &CoreState) -> Value {
    let mut body = mutation_body(state);
    body["clientClosed"] = json!(true);
    body
}

fn mutation_body(state: &CoreState) -> Value {
    let mut body = expectation(state);
    body["expectedFingerprint"] = json!("fp-test");
    body["clientClosed"] = json!(false);
    body
}

async fn error_parts(error: V3ApiError) -> (StatusCode, Value) {
    let response = error.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn assert_secret_absent(value: &impl serde::Serialize, secret: &str) {
    assert!(
        secret.len() > 8,
        "test secret must be long enough to search"
    );
    let encoded = serde_json::to_string(value).unwrap();
    assert!(
        !encoded.contains(secret),
        "serialized BYOK reply contained a gateway secret"
    );
}

struct Observed {
    kind: &'static str,
    client: ByokClient,
    target_path: Option<String>,
    fingerprint: Option<String>,
    secret: Option<String>,
    gateway_v1_url: Option<String>,
    model_ids: Vec<String>,
    context_windows: Vec<Option<u64>>,
    max_output_tokens: Vec<Option<u64>>,
    tool_calling: Vec<Option<bool>>,
    default_model_id: Option<String>,
    client_closed: Option<bool>,
    debug: String,
}

fn observe(request: &ByokHostRequest) -> Observed {
    let debug = format!("{request:?}");
    match request {
        ByokHostRequest::Inspect {
            client,
            target_path,
        } => Observed {
            kind: "inspect",
            client: *client,
            target_path: target_path.clone(),
            fingerprint: None,
            secret: None,
            gateway_v1_url: None,
            model_ids: Vec::new(),
            context_windows: Vec::new(),
            max_output_tokens: Vec::new(),
            tool_calling: Vec::new(),
            default_model_id: None,
            client_closed: None,
            debug,
        },
        ByokHostRequest::Configure {
            client,
            target_path,
            expected_fingerprint,
            gateway_v1_url,
            secret,
            models,
            default_model_id,
            client_closed,
        } => Observed {
            kind: "configure",
            client: *client,
            target_path: target_path.clone(),
            fingerprint: Some(expected_fingerprint.clone()),
            secret: Some(secret.expose_to_host().to_string()),
            gateway_v1_url: Some(gateway_v1_url.clone()),
            model_ids: models.iter().map(|model| model.id.clone()).collect(),
            context_windows: models
                .iter()
                .map(|model| model.metadata.context_window)
                .collect(),
            max_output_tokens: models
                .iter()
                .map(|model| model.metadata.max_output_tokens)
                .collect(),
            tool_calling: models
                .iter()
                .map(|model| model.metadata.tool_calling)
                .collect(),
            default_model_id: default_model_id.clone(),
            client_closed: Some(*client_closed),
            debug,
        },
        ByokHostRequest::Remove {
            client,
            target_path,
            expected_fingerprint,
            client_closed,
        } => Observed {
            kind: "remove",
            client: *client,
            target_path: target_path.clone(),
            fingerprint: Some(expected_fingerprint.clone()),
            secret: None,
            gateway_v1_url: None,
            model_ids: Vec::new(),
            context_windows: Vec::new(),
            max_output_tokens: Vec::new(),
            tool_calling: Vec::new(),
            default_model_id: None,
            client_closed: Some(*client_closed),
            debug,
        },
        ByokHostRequest::Recover {
            client,
            target_path,
            expected_fingerprint,
            client_closed,
        } => Observed {
            kind: "recover",
            client: *client,
            target_path: target_path.clone(),
            fingerprint: Some(expected_fingerprint.clone()),
            secret: None,
            gateway_v1_url: None,
            model_ids: Vec::new(),
            context_windows: Vec::new(),
            max_output_tokens: Vec::new(),
            tool_calling: Vec::new(),
            default_model_id: None,
            client_closed: Some(*client_closed),
            debug,
        },
    }
}

struct Probe {
    calls: Mutex<Vec<Observed>>,
    replies: Mutex<VecDeque<Result<ByokInspection, ByokError>>>,
}

impl Probe {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(VecDeque::new()),
        })
    }

    fn install(self: &Arc<Self>, state: &CoreState) {
        let probe = Arc::clone(self);
        state.set_byok_application_host(Arc::new(move |request| {
            let observed = observe(&request);
            probe.calls.lock().unwrap().push(observed);
            probe
                .replies
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err(ByokError::internal("unexpected BYOK host call")))
        }));
    }

    fn push_ok(&self, inspection: ByokInspection) {
        self.replies.lock().unwrap().push_back(Ok(inspection));
    }

    fn push_err(&self, error: ByokError) {
        self.replies.lock().unwrap().push_back(Err(error));
    }

    fn calls(&self) -> Vec<Observed> {
        self.calls.lock().unwrap().drain(..).collect()
    }
}

fn inspection(client: ByokClient, status: ByokStatus) -> ByokInspection {
    ByokInspection {
        client,
        status,
        detected: status == ByokStatus::Configured || status == ByokStatus::Ready,
        config_path: "isolated-byok-target".into(),
        discovery_source: "explicit".into(),
        target_paths: vec!["isolated-byok-target".into()],
        configure_supported: status != ByokStatus::UnsupportedRuntime,
        remove_supported: status == ByokStatus::Configured,
        recovery_supported: status == ByokStatus::RecoveryRequired,
        requires_closed_client: client.requires_closed_client(),
        activation_required: status == ByokStatus::Configured,
        fingerprint: Some("fp-host".into()),
        configured_model_ids: vec![EXACT_MODEL.into()],
        default_model_id: Some(EXACT_MODEL.into()),
        backup_path: None,
        detail: Some("host-prepared".into()),
    }
}

fn publish_exact_model(state: &CoreState) {
    publish_models(state, &[EXACT_MODEL.to_owned()]);
}

fn publish_models(state: &CoreState, ids: &[String]) {
    let now = Utc::now();
    state
        .db
        .lock()
        .create_account_with_contract(
            &Account {
                id: "byok-custom".into(),
                provider_id: CUSTOM_PROVIDER_ID.into(),
                credential_kind: CredentialKind::ApiKey,
                quota_scope: QuotaScope::Key,
                name: "BYOK test".into(),
                username: None,
                password_cipher: None,
                key_cipher: state.encrypt_key("sk-ocg-byok-test-upstream").unwrap(),
                enabled: true,
                account_type: AccountType::Key,
                setup_step: AccountSetupStep::Ready,
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
            },
            Some(&AccountCustomConfigInput {
                endpoint_url: "https://example.test/v1/chat/completions".into(),
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &ids.iter()
                .map(|id| AccountModelCapabilityInput {
                    public_model: id.clone(),
                    upstream_model: id.clone(),
                    protocol: UpstreamProtocolKind::ChatCompletions,
                    source: None,
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
}

fn published_ids(state: &CoreState) -> Vec<String> {
    let _guard = state.settings_update.lock();
    available_models_locked(state)
        .unwrap()
        .into_iter()
        .map(|model| model.id)
        .collect()
}

#[test]
fn byok_secret_debug_redacts_the_value() {
    let secret = ByokSecret::new("ocg-byok-debug-secret".into());
    let rendered = format!(
        "{:?}",
        ByokHostRequest::Configure {
            client: ByokClient::Codex,
            target_path: None,
            expected_fingerprint: "fp".into(),
            gateway_v1_url: "http://127.0.0.1:9/v1".into(),
            secret,
            models: Vec::new(),
            default_model_id: None,
            client_closed: true,
        }
    );
    assert!(rendered.contains("ByokSecret([redacted])"));
    assert!(!rendered.contains("ocg-byok-debug-secret"));
}

#[test]
fn configure_has_no_secondary_key_or_model_selection() {
    let body = json!({
        "expectedRevision": 1, "processGeneration": 2,
        "expectedFingerprint": "fp", "clientClosed": true
    });
    let parsed: ByokConfigureRequest = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(parsed.target_path, None);
    for field in ["models", "keyId", "defaultModelId"] {
        let mut extra = body.clone();
        extra[field] = Value::Null;
        assert!(serde_json::from_value::<ByokConfigureRequest>(extra).is_err());
    }
}

#[tokio::test]
async fn inspect_is_lightweight_secret_free_and_does_not_create_a_key() {
    let (dir, state) = open_state("inspect-only");
    let probe = Probe::new();
    probe.install(&state);
    probe.push_ok(inspection(ByokClient::Minimax, ByokStatus::Ready));
    let revision = state.settings_revision();
    let Json(view) = inspect(
        State(state.clone()),
        Path("minimax".into()),
        Query(TargetQuery::default()),
    )
    .await
    .unwrap();
    assert!(serde_json::to_value(&view).unwrap().get("models").is_none());
    assert!(
        state
            .db
            .lock()
            .list_active_sub_gateway_keys()
            .unwrap()
            .is_empty()
    );
    assert_eq!(state.settings_revision(), revision);
    assert_secret_absent(&view, &state.config().gateway_key);
    close_state(dir, state);
}

#[tokio::test]
async fn all_harnesses_export_the_full_key_catalog_and_reuse_named_keys() {
    let (dir, state) = open_state("all-models");
    let mut ids = (0..125)
        .map(|i| format!("vendor/model.{i:03}"))
        .collect::<Vec<_>>();
    ids.push("模".repeat(100));
    publish_models(&state, &ids);
    let expected = published_ids(&state);
    assert_eq!(expected, ids);
    let probe = Probe::new();
    probe.install(&state);
    for client in ByokClient::ALL {
        for _ in 0..2 {
            probe.push_ok(inspection(client, ByokStatus::Configured));
            let Json(view) = configure(
                State(state.clone()),
                Path(client.id().into()),
                body_bytes(&configure_body(&state)),
            )
            .await
            .unwrap();
            let calls = probe.calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].model_ids, expected);
            assert_eq!(calls[0].client, client);
            assert!(calls[0].context_windows.iter().all(Option::is_none));
            assert!(calls[0].max_output_tokens.iter().all(Option::is_none));
            assert!(calls[0].tool_calling.iter().all(Option::is_none));
            assert_eq!(calls[0].target_path, None);
            assert_eq!(calls[0].fingerprint.as_deref(), Some("fp-test"));
            assert_eq!(calls[0].client_closed, Some(true));
            assert_eq!(
                calls[0].gateway_v1_url.as_deref(),
                Some(gateway_url(&state).as_str())
            );
            assert_eq!(calls[0].default_model_id, None);
            let keys = state.db.lock().list_active_sub_gateway_keys().unwrap();
            let matching = keys
                .iter()
                .filter(|key| key.name == client.key_name())
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1);
            assert_eq!(calls[0].secret.as_deref(), Some(matching[0].key.as_str()));
            assert_secret_absent(&view, &matching[0].key);
            assert!(!calls[0].debug.contains(&matching[0].key));
        }
    }
    close_state(dir, state);
}

#[tokio::test]
async fn stale_request_and_empty_catalog_have_no_key_or_host_side_effects() {
    let (dir, state) = open_state("preconditions");
    let probe = Probe::new();
    probe.install(&state);
    let mut stale = configure_body(&state);
    stale["expectedRevision"] = json!(state.settings_revision() + 1);
    let (status, _) = error_parts(
        configure(
            State(state.clone()),
            Path("codex".into()),
            body_bytes(&stale),
        )
        .await
        .unwrap_err(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = error_parts(
        configure(
            State(state.clone()),
            Path("codex".into()),
            body_bytes(&configure_body(&state)),
        )
        .await
        .unwrap_err(),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert!(probe.calls().is_empty());
    assert!(
        state
            .db
            .lock()
            .list_active_sub_gateway_keys()
            .unwrap()
            .is_empty()
    );
    close_state(dir, state);
}

#[tokio::test]
async fn failed_native_write_keeps_one_usable_key_and_reports_new_revision() {
    let (dir, state) = open_state("retry-key");
    publish_exact_model(&state);
    let probe = Probe::new();
    probe.install(&state);
    probe.push_err(ByokError::internal("fixture disk write failed"));
    let revision = state.settings_revision();
    let (status, error) = error_parts(
        configure(
            State(state.clone()),
            Path("kimi".into()),
            body_bytes(&configure_body(&state)),
        )
        .await
        .unwrap_err(),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["currentRevision"], revision + 1);
    assert_eq!(error["processGeneration"], state.process_generation());
    assert_eq!(state.settings_revision(), revision + 1);
    let key = state
        .db
        .lock()
        .list_active_sub_gateway_keys()
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(key.name, "kimi-code");
    assert!(key.authenticates());
    probe.push_ok(inspection(ByokClient::Kimi, ByokStatus::Configured));
    let _ = configure(
        State(state.clone()),
        Path("kimi".into()),
        body_bytes(&configure_body(&state)),
    )
    .await
    .unwrap();
    let keys = state.db.lock().list_active_sub_gateway_keys().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].id, key.id);
    close_state(dir, state);
}

#[tokio::test]
async fn disabled_named_key_is_not_reenabled_or_reused() {
    let (dir, state) = open_state("disabled-key");
    publish_exact_model(&state);
    let old = gateway_keys::create_sub_key(&state, "codex").unwrap();
    gateway_keys::set_sub_key_enabled(&state, &old.id, false).unwrap();
    let probe = Probe::new();
    probe.install(&state);
    probe.push_ok(inspection(ByokClient::Codex, ByokStatus::Configured));
    let _ = configure(
        State(state.clone()),
        Path("codex".into()),
        body_bytes(&configure_body(&state)),
    )
    .await
    .unwrap();
    assert!(
        !state
            .db
            .lock()
            .get_sub_gateway_key(&old.id)
            .unwrap()
            .unwrap()
            .enabled
    );
    let calls = probe.calls();
    assert_ne!(calls[0].secret.as_deref(), Some(old.key.as_str()));
    assert_eq!(
        state
            .db
            .lock()
            .list_active_sub_gateway_keys()
            .unwrap()
            .len(),
        2
    );
    close_state(dir, state);
}

#[tokio::test]
async fn removal_and_recovery_work_without_published_models_or_named_keys() {
    let (dir, state) = open_state("remove-recover");
    let probe = Probe::new();
    probe.install(&state);
    probe.push_ok(inspection(ByokClient::Zcode, ByokStatus::Ready));
    let _ = remove(
        State(state.clone()),
        Path("zcode".into()),
        body_bytes(&mutation_body(&state)),
    )
    .await
    .unwrap();
    probe.push_ok(inspection(ByokClient::Zcode, ByokStatus::Ready));
    let _ = recover(
        State(state.clone()),
        Path("zcode".into()),
        body_bytes(&mutation_body(&state)),
    )
    .await
    .unwrap();
    assert!(
        state
            .db
            .lock()
            .list_active_sub_gateway_keys()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        probe
            .calls()
            .iter()
            .map(|call| call.kind)
            .collect::<Vec<_>>(),
        vec!["remove", "recover"]
    );
    close_state(dir, state);
}

#[tokio::test]
async fn router_requires_a_session_or_loopback_authority_before_the_host() {
    let (dir, state) = open_state("router");
    let probe = Probe::new();
    probe.install(&state);
    probe.push_ok(inspection(ByokClient::Codex, ByokStatus::NotDetected));
    probe.push_ok(inspection(ByokClient::Codex, ByokStatus::NotDetected));
    let primary = state.config().gateway_key.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::dashboard_v4::api_router(state.clone()).with_state(state.clone());
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = stop_rx.await;
            })
            .await;
    });
    let client = reqwest::Client::new();
    let url = format!("http://{address}/applications/byok/codex");

    let anonymous = client.get(&url).send().await.unwrap();
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        anonymous.json::<Value>().await.unwrap()["code"],
        "unauthorized"
    );
    assert!(probe.calls().is_empty());

    let wrong = client
        .get(&url)
        .header("cookie", format!("{SESSION_COOKIE}=wrong-token"))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(probe.calls().is_empty());

    let token = state.dashboard_session_token.lock().clone();
    let authed = client
        .get(&url)
        .header("cookie", format!("{SESSION_COOKIE}={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(authed.status(), reqwest::StatusCode::OK);
    let authed_body: Value = authed.json().await.unwrap();
    assert_eq!(authed_body["client"], "codex");
    assert_secret_absent(&authed_body, &primary);
    assert_eq!(probe.calls().len(), 1);

    *state.dashboard_session_token.lock() = "rotated-token".into();
    let stale = client
        .get(&url)
        .header("cookie", format!("{SESSION_COOKIE}={token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(probe.calls().is_empty());

    state.set_dashboard_local_mode(true);
    let forwarded = client
        .get(&url)
        .header("x-forwarded-for", "203.0.113.8")
        .send()
        .await
        .unwrap();
    assert_eq!(forwarded.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(probe.calls().is_empty());

    let local = client.get(&url).send().await.unwrap();
    assert_eq!(local.status(), reqwest::StatusCode::OK);
    assert_eq!(probe.calls().len(), 1);
    let _ = stop.send(());

    close_state(dir, state);
}
