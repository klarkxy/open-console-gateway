#[test]
fn selector_invariant_maps_to_internal_error() {
    for (label, failure, expected) in [
        (
            "duplicate",
            super::SelectorInvariant::Duplicate(
                ocg_gateway::selector::SelectionError::DuplicateAccountId {
                    first: 0,
                    duplicate: 2,
                },
            ),
            "routing selector invariant: duplicate account id at candidate index 2 (first seen at 0)",
        ),
        (
            "index-out-of-range",
            super::SelectorInvariant::CandidateIndexOutOfRange { selected_index: 9 },
            "routing selector invariant: candidate index 9 is out of range",
        ),
    ] {
        let (status, message) = super::routing_selector_invariant(failure);
        assert_eq!(
            status,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "{label}"
        );
        assert_eq!(message, expected, "{label}");
    }
}

#[tokio::test]
async fn exhausted_candidate_reports_persistent_quota_retry_after() {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::models::{
        Account, AccountCustomConfigInput, AccountModelCapabilityInput, ProxyMode,
    };
    use crate::provider::UpstreamProtocolKind;
    use crate::state::CoreStateInner;
    use axum::{
        Json,
        extract::{Extension, State},
        http::{HeaderMap, StatusCode},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = hits.clone();
    let app = axum::Router::new().fallback(axum::routing::post(move || {
        let counted = counted.clone();
        async move {
            counted.fetch_add(1, Ordering::SeqCst);
            (
                StatusCode::TOO_MANY_REQUESTS,
                [("retry-after", "300")],
                Json(serde_json::json!({"error":{"code":"insufficient_quota","message":"quota exhausted"}})),
            )
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let dir = std::env::temp_dir().join(format!("ocg-live-cooldown-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = crate::db::Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cooldown-test"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let mut config = state.config();
    config.gateway_key = "gateway-cooldown-test".into();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let now = chrono::Utc::now();
    let account: Account = serde_json::from_value(serde_json::json!({
        "id":"cooldown-key", "provider_id":crate::provider::CUSTOM_PROVIDER_ID, "name":"Only Key",
        "key_cipher":state.encrypt_key("test-key").unwrap(), "enabled":true,
        "purchase_date":"", "created_at":now, "updated_at":now
    }))
    .unwrap();
    state
        .db
        .lock()
        .create_account_with_contract(
            &account,
            Some(&AccountCustomConfigInput {
                endpoint_url: endpoint,
                upstream_protocol: UpstreamProtocolKind::ChatCompletions,
            }),
            &[AccountModelCapabilityInput {
                public_model: "only-model".into(),
                upstream_model: "only-model".into(),
                protocol: UpstreamProtocolKind::ChatCompletions,
                source: None,
            }],
        )
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        "Bearer gateway-cooldown-test".parse().unwrap(),
    );
    let response = crate::gateway::handler::chat_completions(
        State(state.clone()),
        Extension(crate::gateway::diagnostics::RequestTrace::new()),
        headers,
        axum::body::Bytes::from_static(
            br#"{"model":"only-model","messages":[{"role":"user","content":"hello"}]}"#,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after: u64 = response
        .headers()
        .get("retry-after")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((850..=900).contains(&retry_after), "{retry_after}");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(
        state
            .db
            .lock()
            .get_account(&account.id)
            .unwrap()
            .unwrap()
            .cooldown_generic_until
            .is_none()
    );
    stop.send(()).unwrap();
    server.await.unwrap();
    drop(state);
    assert!(dir.starts_with(std::env::temp_dir()));
    std::fs::remove_dir_all(dir).unwrap();
}

fn persist_recovery(
    state: &crate::state::CoreState,
    account_id: &str,
    next_retry_at: chrono::DateTime<chrono::Utc>,
) -> crate::quota_recovery::QuotaEpisode {
    use crate::quota_recovery::{PersistedQuotaRecovery, QuotaEpisode};
    use ocg_gateway::quota::{QuotaEvidence, QuotaReason, QuotaWindowKind};
    let observed = next_retry_at - chrono::Duration::minutes(15);
    let mut recovery = PersistedQuotaRecovery::from_evidence(
        None,
        &QuotaEvidence {
            reason: QuotaReason::QuotaExhausted,
            window: QuotaWindowKind::Unknown,
            resets_at_rfc3339: None,
            resets_in_text: None,
        },
        observed,
        None,
    );
    recovery.next_retry_at = next_retry_at;
    let (id, version, key_cipher, _) =
        crate::db::quota_recovery::load_for_legacy_on(&state.db.lock().conn, account_id)
            .unwrap()
            .unwrap();
    let episode = QuotaEpisode {
        credential_id: id,
        account_id: account_id.into(),
        credential_version: version,
        epoch: recovery.epoch,
        key_cipher,
    };
    crate::db::quota_recovery::save_on(&state.db.lock().conn, &episode, &recovery).unwrap();
    episode
}

async fn chat(
    state: crate::state::CoreState,
    model: &str,
) -> axum::http::Response<axum::body::Body> {
    use axum::{extract::State, http::HeaderMap};
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        "Bearer gateway-quota-test".parse().unwrap(),
    );
    let body = format!(r#"{{"model":"{model}","messages":[{{"role":"user","content":"hello"}}]}}"#);
    crate::gateway::handler::chat_completions(
        State(state),
        axum::extract::Extension(crate::gateway::diagnostics::RequestTrace::new()),
        headers,
        axum::body::Bytes::from(body),
    )
    .await
}

fn custom_http_state(
    tag: &str,
    accounts: &[&str],
    endpoint: &str,
) -> (std::path::PathBuf, crate::state::CoreState) {
    use crate::crypto::{KeyCipher, StaticKeyCipher};
    use crate::models::{
        Account, AccountCustomConfigInput, AccountModelCapabilityInput, ProxyMode,
    };
    use crate::provider::UpstreamProtocolKind;
    use crate::state::CoreStateInner;
    use std::sync::Arc;
    let dir = std::env::temp_dir().join(format!("ocg-quota-exec-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = crate::db::Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new(tag));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    let mut config = state.config();
    config.gateway_key = "gateway-quota-test".into();
    config.proxy_mode = ProxyMode::Direct;
    state.set_config(config).unwrap();
    let now = chrono::Utc::now();
    for id in accounts {
        let account: Account = serde_json::from_value(serde_json::json!({
            "id": id, "provider_id": crate::provider::CUSTOM_PROVIDER_ID, "name": id,
            "key_cipher": state.encrypt_key("test-key").unwrap(), "enabled": true,
            "purchase_date": "", "created_at": now, "updated_at": now
        }))
        .unwrap();
        state
            .db
            .lock()
            .create_account_with_contract(
                &account,
                Some(&AccountCustomConfigInput {
                    endpoint_url: endpoint.into(),
                    upstream_protocol: UpstreamProtocolKind::ChatCompletions,
                }),
                &[AccountModelCapabilityInput {
                    public_model: "quota-model".into(),
                    upstream_model: "quota-model".into(),
                    protocol: UpstreamProtocolKind::ChatCompletions,
                    source: None,
                }],
            )
            .unwrap();
    }
    (dir, state)
}

#[tokio::test]
async fn all_waiting_quota_returns_earliest_deadline_429() {
    let (dir, state) = custom_http_state("wait", &["wait-a", "wait-b"], "http://127.0.0.1:1/v1");
    let early = chrono::Utc::now() + chrono::Duration::hours(1);
    let late = chrono::Utc::now() + chrono::Duration::hours(6);
    persist_recovery(&state, "wait-a", late);
    persist_recovery(&state, "wait-b", early);
    let response = chat(state.clone(), "quota-model").await;
    assert_eq!(response.status(), axum::http::StatusCode::TOO_MANY_REQUESTS);
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["error"]["resets_at"], early.to_rfc3339());
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn matching_probe_only_returns_503_stale_probe_does_not() {
    let (dir, state) = custom_http_state("probe", &["probe-a"], "http://127.0.0.1:1/v1");
    let due = chrono::Utc::now() - chrono::Duration::minutes(1);
    let episode = persist_recovery(&state, "probe-a", due);
    state
        .quota_probes
        .lock()
        .insert(episode.credential_id.clone(), episode.clone());
    let probing = chat(state.clone(), "quota-model").await;
    assert_eq!(
        probing.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );

    let mut stale = episode.clone();
    stale.credential_version = episode.credential_version.saturating_add(1);
    state
        .quota_probes
        .lock()
        .insert(episode.credential_id.clone(), stale);
    let unblocked = chat(state.clone(), "quota-model").await;
    assert_ne!(
        unblocked.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn rotation_during_probe_does_not_block_replacement_key() {
    use axum::{Json, http::StatusCode};
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = hits.clone();
    let app = axum::Router::new().fallback(axum::routing::post(move || {
        let counted = counted.clone();
        async move {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "id": "cmpl",
                    "object": "chat.completion",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "ok"},
                        "finish_reason": "stop"
                    }]
                })),
            )
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
    });
    let (dir, state) = custom_http_state("rotate-probe", &["rotate-a"], &endpoint);
    let due = chrono::Utc::now() - chrono::Duration::minutes(1);
    let episode = persist_recovery(&state, "rotate-a", due);
    state
        .quota_probes
        .lock()
        .insert(episode.credential_id.clone(), episode.clone());
    let blocked = chat(state.clone(), "quota-model").await;
    assert_eq!(blocked.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);

    let rotated = state.encrypt_key("replacement-key").unwrap();
    state
        .db
        .lock()
        .rotate_account_credential("rotate-a", &rotated)
        .unwrap();
    let sent = chat(state.clone(), "quota-model").await;
    assert_ne!(sent.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    stop.send(()).unwrap();
    server.await.unwrap();
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn request_budgets_keep_stream_and_non_stream_settings_independent() {
    let config = crate::models::AppConfig {
        non_stream_timeout_secs: 1,
        stream_idle_timeout_secs: 5,
        ..Default::default()
    };
    assert_eq!(super::request_budget_duration(&config, false).as_secs(), 1);
    assert_eq!(super::request_budget_duration(&config, true).as_secs(), 5);
}
