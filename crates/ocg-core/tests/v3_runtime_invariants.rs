//! V3 characterization: request-entry snapshots, per-fallback re-reads,
//! stream finalization, Go `success_no_usage`, Custom 401 rotation, and
//! deferred post-429 usage sync.
//!
//! Existing suites already freeze retry/SSE/redaction/model-list/OpenCode 401
//! passthrough and the proxy `ForwardRouteSet` snapshot. This file covers the
//! gaps a GatewayExecutor refactor can otherwise redefine.

use axum::http::StatusCode;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway;
use ocg_core::models::{Account, ProxyMode, RoutingMode, UsageWindowKind};
use ocg_core::provider::{
    COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM, COMMAND_CODE_PROVIDER_ID, CUSTOM_PROVIDER_ID,
    OPENCODE_PROVIDER_ID, UpstreamProtocolKind, ZEN_FREE_ACCOUNT_ID,
};
use ocg_core::provider_contracts::{ContractScope, ProtocolOverrideState};
use ocg_core::state::CoreStateInner;
use ocg_core::zen_models::{ZEN_MODELS_SOURCE_URL, ZenFreeModelCatalog};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[path = "fixtures/v3/harness.rs"]
mod harness;

use harness::*;

const GO_MODEL: &str = "deepseek-v4-flash";
const ZEN_ONLY_MODEL: &str = "nemotron-3-ultra-free";
const SHARED_ALIAS: &str = "mimo-v2.5";
const CUSTOM_MODEL: &str = "custom-v3-model";
const CUSTOM_KEY_A: &str = "v3-custom-key-a";
const CUSTOM_KEY_B: &str = "v3-custom-key-b";
const CUSTOM_KEY_C: &str = "v3-custom-key-c";

fn disable_all_go_protocols(state: &Arc<ocg_core::state::CoreStateInner>) {
    let now = Utc::now();
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    {
        let db = state.db.lock();
        db.set_model_protocol_overrides(
            &scope,
            &[
                (
                    GO_MODEL.into(),
                    UpstreamProtocolKind::ChatCompletions,
                    ProtocolOverrideState::ForceOff,
                ),
                (
                    GO_MODEL.into(),
                    UpstreamProtocolKind::Responses,
                    ProtocolOverrideState::ForceOff,
                ),
                (
                    GO_MODEL.into(),
                    UpstreamProtocolKind::Messages,
                    ProtocolOverrideState::ForceOff,
                ),
            ],
            now,
        )
        .unwrap();
    }
    state.reload_provider_contracts().unwrap();
}

fn inflate_active_pricing(state: &Arc<ocg_core::state::CoreStateInner>, model_id: &str) -> String {
    let mut snapshot = (*state.pricing_snapshot()).clone();
    let mut found = false;
    for model in &mut snapshot.models {
        if model.model_id == model_id {
            model.quota_multiplier *= 100.0;
            found = true;
        }
    }
    assert!(
        found,
        "priced model {model_id} must exist in the seed snapshot"
    );
    snapshot.revision = format!("v3-inflated-{}", uuid::Uuid::new_v4());
    snapshot.activated_at = Utc::now().to_rfc3339();
    let revision = snapshot.revision.clone();
    state.activate_pricing_snapshot(snapshot).unwrap();
    revision
}

fn go_state_with_keys(keys: &[&str]) -> (Arc<ocg_core::state::CoreStateInner>, std::path::PathBuf) {
    local_go_state("http://127.0.0.1:1".into(), keys)
}

fn local_go_state(
    base_url: String,
    keys: &[&str],
) -> (Arc<ocg_core::state::CoreStateInner>, std::path::PathBuf) {
    let (state, dir) = build_go_state(base_url, keys);
    install_local_usage_sync_seams(&state);
    (state, dir)
}

fn install_local_usage_sync_seams(state: &Arc<CoreStateInner>) {
    state
        .usage_sync
        .set_reactive_refresh_enabled_for_test(false);
    state.usage_sync.set_fetch_for_test(|_cfg, _key| {
        Box::pin(async { Err(ocg_core::go_usage::GoUsageError::Network) })
    });
}

fn go_state_with_keys_and_clock(
    keys: &[&str],
    wall: impl Fn() -> DateTime<Utc> + Send + Sync + 'static,
    mono: impl Fn() -> Instant + Send + Sync + 'static,
) -> (Arc<CoreStateInner>, std::path::PathBuf) {
    let dir = temp_data_dir("state");
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("v3-tests"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(
        CoreStateInner::new_with_test_gateway_clock(db, dir.clone(), cipher, wall, mono).unwrap(),
    );
    let mut config = state.config();
    config.gateway_key = GATEWAY_KEY.into();
    config.upstream_base_url = "http://127.0.0.1:1".into();
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
            created_at: now + ChronoDuration::seconds(idx as i64),
            updated_at: now + ChronoDuration::seconds(idx as i64),
        };
        state.db.lock().create_account(&account).unwrap();
    }
    persist_refreshed_go_catalog(&state);
    persist_enabled_zen_catalog(&state);
    install_local_usage_sync_seams(&state);
    (state, dir)
}

fn closed_upstream_url() -> String {
    for _ in 0..8 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_err() {
            return format!("http://{addr}");
        }
    }
    "http://127.0.0.1:1".into()
}

#[tokio::test]
async fn entry_pricing_snapshot_survives_midflight_activation() {
    let (state, dir) = go_state_with_keys(&["key-1", "key-2"]);
    let captured_revision = state.pricing_snapshot().revision.clone();
    let expected_cost = state.estimate_cost(GO_MODEL, 10, 2, 0, 0, None).quota_debit;

    let state_for_cb = state.clone();
    let captured_for_cb = captured_revision.clone();
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![
            ScriptedReply {
                status: 403,
                body: FORBIDDEN_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
        ],
        Arc::new(move |index| {
            if index == 0 {
                let inflated = inflate_active_pricing(&state_for_cb, GO_MODEL);
                assert_ne!(inflated, captured_for_cb);
            }
        }),
    )
    .await;
    let mut config = state.config();
    config.upstream_base_url = base_url;
    state.set_config(config).unwrap();

    let (port, gateway_handle) = start_gateway(state.clone()).await;
    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let logs = state.db.lock().list_forward_logs(10).unwrap();
    let success = logs
        .iter()
        .find(|log| log.status.starts_with("success"))
        .expect("fallback success row");
    assert_eq!(
        success.pricing_revision_id.as_deref(),
        Some(captured_revision.as_str()),
        "in-flight fallback must keep the entry pricing revision"
    );
    assert_eq!(success.quota_debit, expected_cost);
    assert_ne!(
        state.pricing_snapshot().revision,
        captured_revision,
        "live pricing must have flipped after the first attempt"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn entry_route_snapshot_stops_after_live_protocol_revocation() {
    let (state, dir) = go_state_with_keys(&["key-1", "key-2"]);
    let state_for_cb = state.clone();
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![
            ScriptedReply {
                status: 403,
                body: FORBIDDEN_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
        ],
        Arc::new(move |index| {
            if index == 0 {
                disable_all_go_protocols(&state_for_cb);
            }
        }),
    )
    .await;
    let mut config = state.config();
    config.upstream_base_url = base_url;
    state.set_config(config).unwrap();

    let (port, gateway_handle) = start_gateway(state.clone()).await;
    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the frozen route must not bypass a live protocol revocation: {body}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no fallback or remapped send is authorized"
    );
    let logs = state.db.lock().list_forward_logs(10).unwrap();
    let sent = logs
        .iter()
        .filter(|row| row.error_source.as_deref() == Some("upstream"))
        .collect::<Vec<_>>();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].account_id, "acct-1");
    assert_eq!(sent[0].http_status, Some(403));
    // Local selection failures also carry the downstream HTTP status; they
    // are not another upstream receipt.
    let rejected = logs
        .iter()
        .find(|row| {
            row.error_source.as_deref() == Some("gateway")
                && row.error_stage.as_deref() == Some("account_selection")
        })
        .expect("revocation must produce a local selection failure");
    assert_eq!(rejected.http_status, Some(503));
    assert!(rejected.diagnostic.as_ref().unwrap()["upstream_status"].is_null());

    let (status, body) = chat(port, GO_MODEL).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "a later request must observe the disabled protocols: {body}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the follow-up request must fail locally without another upstream call"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn entry_alias_snapshot_survives_midflight_zen_catalog_replace() {
    let (state, dir) = go_state_with_keys(&["key-1"]);
    state
        .db
        .lock()
        .reorder_accounts(&[ZEN_FREE_ACCOUNT_ID.into(), "acct-1".into()])
        .unwrap();

    let state_for_cb = state.clone();
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![ScriptedReply {
            status: 429,
            body: LIMITED_BODY,
        }],
        Arc::new(move |index| {
            if index == 0 {
                state_for_cb
                    .activate_zen_free_model_catalog(ZenFreeModelCatalog {
                        models: vec!["hy3-free".into()],
                        refreshed_at: Some(Utc::now()),
                        source_url: ZEN_MODELS_SOURCE_URL.into(),
                    })
                    .unwrap();
            }
        }),
    )
    .await;
    let mut config = state.config();
    config.upstream_base_url = format!("{base_url}/zen/go");
    state.set_config(config).unwrap();

    let (port, gateway_handle) = start_gateway(state.clone()).await;
    let (status, body) = chat(port, ZEN_ONLY_MODEL).await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "in-flight zen-only resolve must not become unknown_model: {body}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let free_account = state
        .db
        .lock()
        .get_account(ZEN_FREE_ACCOUNT_ID)
        .unwrap()
        .unwrap();
    assert!(
        free_account.cooldown_free_until.is_none(),
        "a Free 429 must not copy a multi-day reset from error prose: {free_account:?}"
    );
    assert!(
        state
            .db
            .lock()
            .free_channel_cooldown_until()
            .unwrap()
            .is_none(),
        "temporary Free waits stay process-local, not durable settings"
    );

    let (status, body) = chat(port, ZEN_ONLY_MODEL).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a later request must re-resolve against the replaced catalog: {body}"
    );
    assert!(
        body.contains("unknown model"),
        "replaced catalog must drop the zen-only alias: {body}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn fallback_rereads_accounts_and_skips_a_card_disabled_mid_request() {
    let (state, dir) = go_state_with_keys(&["key-1", "key-2", "key-3"]);
    let state_for_cb = state.clone();
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![
            ScriptedReply {
                status: 403,
                body: FORBIDDEN_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
        ],
        Arc::new(move |index| {
            if index == 0 {
                set_account_enabled(&state_for_cb, "acct-2", false);
            }
        }),
    )
    .await;
    let mut config = state.config();
    config.upstream_base_url = base_url;
    state.set_config(config).unwrap();

    let (port, gateway_handle) = start_gateway(state.clone()).await;
    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let mut logs = state.db.lock().list_forward_logs(10).unwrap();
    logs.sort_by_key(|log| log.attempt);
    assert_eq!(logs.len(), 2, "{logs:?}");
    assert_eq!(logs[0].account_id, "acct-1");
    assert_eq!(logs[0].attempt, Some(1));
    assert_eq!(logs[1].account_id, "acct-3");
    assert_eq!(logs[1].attempt, Some(2));
    assert!(
        logs.iter().all(|log| log.account_id != "acct-2"),
        "disabled card must be skipped after the per-fallback account re-read: {logs:?}"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn fallback_rereads_free_cooldown_and_skips_zen() {
    let (state, dir) = go_state_with_keys(&["key-1"]);
    state
        .db
        .lock()
        .reorder_accounts(&["acct-1".into(), ZEN_FREE_ACCOUNT_ID.into()])
        .unwrap();

    let state_for_cb = state.clone();
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![
            ScriptedReply {
                status: 403,
                body: FORBIDDEN_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
        ],
        Arc::new(move |index| {
            if index == 0 {
                let until = Utc::now() + ChronoDuration::minutes(30);
                state_for_cb
                    .db
                    .lock()
                    .set_account_rate_limit(
                        ZEN_FREE_ACCOUNT_ID,
                        until,
                        "free cooldown written mid-request",
                        Some(UsageWindowKind::Free),
                    )
                    .unwrap();
            }
        }),
    )
    .await;
    let mut config = state.config();
    config.upstream_base_url = format!("{base_url}/zen/go");
    state.set_config(config).unwrap();

    let (port, gateway_handle) = start_gateway(state.clone()).await;
    let (status, body) = chat(port, SHARED_ALIAS).await;
    assert_ne!(
        status,
        StatusCode::OK,
        "Zen must not be selected after a mid-request free cooldown re-read: {body}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "only the rejected Go attempt should have reached upstream"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn go_success_without_usage_is_success_no_usage_for_non_stream_and_stream() {
    let replies = HashMap::from([(
        "key-1".to_string(),
        VecDeque::from([
            FakeReply {
                status: 200,
                body: SUCCESS_BODY_WITHOUT_USAGE,
            },
            FakeReply {
                status: 200,
                body: CHAT_STREAM_WITHOUT_USAGE,
            },
        ]),
    )]);
    let (base_url, _calls, stop) = start_fake_upstream(replies).await;
    let (state, dir) = local_go_state(base_url, &["key-1"]);
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = chat_stream(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let mut logs = state.db.lock().list_forward_logs(10).unwrap();
    logs.sort_by_key(|log| log.id);
    assert_eq!(logs.len(), 2, "{logs:?}");
    for log in &logs {
        assert_eq!(log.status, "success_no_usage", "{log:?}");
        assert_eq!(log.cost_state, "usage_missing", "{log:?}");
        assert!(log.cost.is_none(), "{log:?}");
        assert!(log.quota_debit.is_none(), "{log:?}");
        assert_eq!((log.prompt_tokens, log.completion_tokens), (0, 0));
        assert_eq!(log.account_id, "acct-1");
        assert_eq!(log.attempt, Some(1));
    }

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn go_inference_429_returns_before_one_reactive_usage_fetch_completes() {
    let replies = HashMap::from([(
        "key-1".to_string(),
        VecDeque::from([FakeReply {
            status: 429,
            body: LIMITED_BODY,
        }]),
    )]);
    let (base_url, _calls, stop) = start_fake_upstream(replies).await;
    let (state, dir) = local_go_state(base_url, &["key-1"]);
    let now = chrono::DateTime::parse_from_rfc3339("2026-08-18T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    state.usage_sync.set_clock_for_test(move || now);
    let fetches = Arc::new(AtomicUsize::new(0));
    let fetches_cb = fetches.clone();
    let fetch_started = Arc::new(tokio::sync::Notify::new());
    let fetch_started_cb = fetch_started.clone();
    let release_fetch = Arc::new(tokio::sync::Notify::new());
    let release_fetch_cb = release_fetch.clone();
    let fetch_finished = Arc::new(tokio::sync::Notify::new());
    let fetch_finished_cb = fetch_finished.clone();
    state.usage_sync.set_reactive_refresh_enabled_for_test(true);
    state.usage_sync.set_fetch_for_test(move |_cfg, _key| {
        fetches_cb.fetch_add(1, Ordering::SeqCst);
        let fetch_started = fetch_started_cb.clone();
        let release_fetch = release_fetch_cb.clone();
        let fetch_finished = fetch_finished_cb.clone();
        Box::pin(async move {
            fetch_started.notify_one();
            release_fetch.notified().await;
            fetch_finished.notify_one();
            Err(ocg_core::go_usage::GoUsageError::Network)
        })
    });
    state
        .db
        .lock()
        .record_account_usage_sync_success(
            "acct-1",
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(20),
            false,
        )
        .unwrap();

    let (port, gateway_handle) = start_gateway(state.clone()).await;
    let (status, body) = tokio::time::timeout(Duration::from_secs(1), chat(port, GO_MODEL))
        .await
        .expect("the inference response must not wait for official usage");
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "a lone Go 429 still rotates then returns the soonest reset: {body}"
    );
    tokio::time::timeout(Duration::from_secs(1), fetch_started.notified())
        .await
        .expect("the reactive Go refresh must start immediately");
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
    ocg_core::usage_sync::spawn_reactive_usage_refresh(&state, "acct-1");
    tokio::task::yield_now().await;
    assert_eq!(
        fetches.load(Ordering::SeqCst),
        1,
        "the same Key must throttle a second reactive refresh"
    );
    release_fetch.notify_one();
    tokio::time::timeout(Duration::from_secs(1), fetch_finished.notified())
        .await
        .expect("the held reactive fetch must complete after release");

    state.usage_sync.clear_test_seams();
    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn custom_401_rotates_persists_auth_error_and_skips_a_runtime_disabled_mid_request() {
    let (state, dir) = go_state_with_keys(&[]);
    let disable_id: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let disable_id_cb = disable_id.clone();
    let state_for_cb = state.clone();
    let (origin, calls, stop) = start_scripted_upstream(
        vec![
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
            ScriptedReply {
                status: 401,
                body: r#"{"error":{"message":"expired custom key"}}"#,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
        ],
        Arc::new(move |index| {
            if index == 3
                && let Some(id) = disable_id_cb.lock().unwrap().clone()
            {
                set_account_enabled(&state_for_cb, &id, false);
            }
        }),
    )
    .await;
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let first = create_verified_custom(port, &state, "custom-a", CUSTOM_KEY_A, &origin).await;
    let second = create_verified_custom(port, &state, "custom-b", CUSTOM_KEY_B, &origin).await;
    let third = create_verified_custom(port, &state, "custom-c", CUSTOM_KEY_C, &origin).await;
    *disable_id.lock().unwrap() = Some(second.clone());

    let (status, body) = chat(port, CUSTOM_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        calls.load(Ordering::SeqCst) >= 5,
        "verify (3) + 401 + fallback must have happened: {}",
        calls.load(Ordering::SeqCst)
    );

    let after_first = state.db.lock().get_account(&first).unwrap().unwrap();
    assert!(
        after_first.auth_error.is_some(),
        "ordinary Custom 401 must persist auth_error: {after_first:?}"
    );
    let after_second = state.db.lock().get_account(&second).unwrap().unwrap();
    assert!(!after_second.enabled, "{after_second:?}");
    assert!(after_second.auth_error.is_none(), "{after_second:?}");

    let logs = state.db.lock().list_forward_logs(20).unwrap();
    assert!(
        logs.iter()
            .any(|log| log.account_id == first && log.http_status == Some(401)),
        "{logs:?}"
    );
    assert!(
        logs.iter()
            .any(|log| log.account_id == third && log.status.starts_with("success")),
        "third Custom runtime must be selected after the mid-request disable: {logs:?}"
    );
    assert!(
        logs.iter().all(|log| log.account_id != second),
        "disabled Custom runtime must not be attempted: {logs:?}"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn outer_fallback_resamples_injected_wall_for_cooldown() {
    let until = Utc::now() + ChronoDuration::hours(1);
    let wall = Arc::new(std::sync::Mutex::new(until - ChronoDuration::seconds(1)));
    let t0 = Instant::now();
    let (state, dir) = go_state_with_keys_and_clock(
        &["key-1", "key-2"],
        {
            let wall = wall.clone();
            move || *wall.lock().unwrap()
        },
        move || t0,
    );
    state
        .db
        .lock()
        .set_account_rate_limit("acct-2", until, "cooled under injected wall", None)
        .unwrap();

    let wall_for_cb = wall.clone();
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![
            ScriptedReply {
                status: 403,
                body: FORBIDDEN_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
        ],
        Arc::new(move |index| {
            if index == 0 {
                *wall_for_cb.lock().unwrap() = until + ChronoDuration::seconds(1);
            }
        }),
    )
    .await;
    let mut config = state.config();
    config.upstream_base_url = base_url;
    state.set_config(config).unwrap();

    let (port, gateway_handle) = start_gateway(state.clone()).await;
    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the next outer iteration must resample wall and select the recovered card: {body}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // Recovery admission/observation also samples the clock. Assert the
    // externally visible selection at the changed wall instant, not an internal
    // call count that cannot distinguish selection from recovery reads.
    let mut logs = state.db.lock().list_forward_logs(10).unwrap();
    logs.sort_by_key(|log| log.attempt);
    assert_eq!(logs.len(), 2, "{logs:?}");
    assert_eq!(logs[0].account_id, "acct-1");
    assert_eq!(logs[1].account_id, "acct-2");

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn same_account_retry_does_not_reselect_or_advance_round_robin() {
    let frozen = Utc::now();
    let t0 = Instant::now();
    let (state, dir) =
        go_state_with_keys_and_clock(&["key-1", "key-2"], move || frozen, move || t0);
    let mut config = state.config();
    config.upstream_base_url = closed_upstream_url();
    config.connect_timeout_secs = 1;
    config.routing_mode = RoutingMode::RoundRobin;
    state.set_config(config).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;
    for (expected_account, total) in [("acct-1", 2), ("acct-2", 4)] {
        let (status, _) = chat(port, GO_MODEL).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        let logs = state.db.lock().list_forward_logs(10).unwrap();
        assert_eq!(logs.len(), total, "{logs:?}");
        let mut current: Vec<_> = logs
            .iter()
            .filter(|log| log.account_id == expected_account)
            .collect();
        current.sort_by_key(|log| log.attempt);
        assert_eq!(
            current.len(),
            2,
            "safe retry must stay on the selected account: {logs:?}"
        );
        assert_eq!(current[0].attempt, Some(1));
        assert_eq!(current[1].attempt, Some(2));
        assert_eq!(current[0].request_id, current[1].request_id);
        assert_eq!(current[0].error_stage.as_deref(), Some("connect"));
        assert_eq!(current[1].error_stage.as_deref(), Some("connect"));
    }
    // The second logical request choosing acct-2 proves the first retry did
    // not advance round-robin selection, regardless of recovery clock reads.
    gateway::stop_gateway(gateway_handle);
    let _ = std::fs::remove_dir_all(dir);
}

async fn dashboard_json(
    port: u16,
    method: reqwest::Method,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = loopback_client()
        .request(
            method,
            format!("http://127.0.0.1:{port}/dashboard/api/v4{path}"),
        )
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap();
    (status, body)
}

async fn create_verified_custom(
    port: u16,
    state: &Arc<ocg_core::state::CoreStateInner>,
    name: &str,
    key: &str,
    origin: &str,
) -> String {
    let (status, draft) = dashboard_json(
        port,
        reqwest::Method::POST,
        "/accounts",
        json!({
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation(),
            "providerId": CUSTOM_PROVIDER_ID,
            "name": name,
            "key": key,
            "customConfig": {
                "endpointUrl": format!("{}/chat/completions", origin.trim_end_matches('/')),
                "upstreamProtocol": "chat_completions"
            },
            "modelCapabilities": [{
                "modelId": CUSTOM_MODEL,
                "protocol": "chat_completions"
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    let id = draft["account"]["id"].as_str().unwrap().to_string();
    let (status, verified) = dashboard_json(
        port,
        reqwest::Method::POST,
        &format!("/accounts/{id}/verify"),
        json!({
            "expectedRevision": state.settings_revision(),
            "processGeneration": state.process_generation()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{verified}");
    assert_eq!(
        verified["account"]["verificationStatus"].as_str(),
        Some("verified")
    );
    if verified["account"]["enabled"] != true {
        let (status, enabled) = dashboard_json(
            port,
            reqwest::Method::POST,
            &format!("/accounts/{id}/toggle"),
            json!({
                "expectedRevision": state.settings_revision(),
                "processGeneration": state.process_generation()
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{enabled}");
        assert_eq!(enabled["account"]["enabled"], true);
    }
    id
}

fn insert_disabled_offering(
    state: &Arc<CoreStateInner>,
    source_id: &str,
    account_id: &str,
    provider_id: &str,

    key: &str,
) {
    let mut account = state
        .db
        .lock()
        .get_account(source_id)
        .unwrap()
        .expect("source account");
    account.id = account_id.to_string();
    account.provider_id = provider_id.to_string();
    account.name = account_id.to_string();
    account.key_cipher = state.encrypt_key(key).unwrap();
    account.enabled = false;
    account.auth_error = None;
    account.cooldown_until = None;
    account.cooldown_generic_until = None;
    account.cooldown_5h_until = None;
    account.cooldown_week_until = None;
    account.cooldown_month_until = None;
    account.cooldown_free_until = None;
    account.created_at = Utc::now();
    account.updated_at = account.created_at;
    state.db.lock().create_account(&account).unwrap();
}

#[tokio::test]
async fn strict_priority_keeps_first_available_card_across_requests() {
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
        ],
        Arc::new(|_| {}),
    )
    .await;
    let (state, dir) = local_go_state(base_url, &["key-1", "key-2"]);
    let mut config = state.config();
    config.routing_mode = RoutingMode::StrictPriority;
    state.set_config(config).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let mut logs = state.db.lock().list_forward_logs(10).unwrap();
    logs.sort_by_key(|log| log.id);
    assert_eq!(logs.len(), 2, "{logs:?}");
    assert!(
        logs.iter().all(|log| log.account_id == "acct-1"),
        "strict priority must keep card 0: {logs:?}"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn round_robin_cycles_exact_card_order() {
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
        ],
        Arc::new(|_| {}),
    )
    .await;
    let (state, dir) = local_go_state(base_url, &["key-1", "key-2"]);
    let mut config = state.config();
    config.routing_mode = RoutingMode::RoundRobin;
    state.set_config(config).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let mut logs = state.db.lock().list_forward_logs(10).unwrap();
    logs.sort_by_key(|log| log.id);
    assert_eq!(
        logs.iter()
            .map(|log| log.account_id.as_str())
            .collect::<Vec<_>>(),
        ["acct-1", "acct-2"],
        "{logs:?}"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn sticky_global_transient_exclude_does_not_rewrite_next_request() {
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![
            ScriptedReply {
                status: 403,
                body: FORBIDDEN_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
            ScriptedReply {
                status: 200,
                body: SUCCESS_BODY,
            },
        ],
        Arc::new(|_| {}),
    )
    .await;
    let (state, dir) = local_go_state(base_url, &["key-1", "key-2"]);
    let mut config = state.config();
    config.routing_mode = RoutingMode::StickyGlobal;
    state.set_config(config).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let mut logs = state.db.lock().list_forward_logs(10).unwrap();
    logs.sort_by_key(|log| log.id);
    assert_eq!(
        logs.iter()
            .map(|log| log.account_id.as_str())
            .collect::<Vec<_>>(),
        ["acct-1", "acct-2", "acct-1"],
        "transient 403 must not rewrite global sticky: {logs:?}"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn free_gates_close_only_free_candidates_on_shared_alias() {
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![ScriptedReply {
            status: 200,
            body: SUCCESS_BODY,
        }],
        Arc::new(|_| {}),
    )
    .await;
    let (state, dir) = local_go_state(base_url.clone(), &["key-1"]);
    state
        .db
        .lock()
        .reorder_accounts(&[ZEN_FREE_ACCOUNT_ID.into(), "acct-1".into()])
        .unwrap();
    let until = Utc::now() + ChronoDuration::minutes(30);
    state
        .db
        .lock()
        .set_account_rate_limit(
            ZEN_FREE_ACCOUNT_ID,
            until,
            "durable free cooldown",
            Some(UsageWindowKind::Free),
        )
        .unwrap();
    let mut config = state.config();
    config.upstream_base_url = format!("{base_url}/zen/go");
    state.set_config(config).unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, body) = chat(port, SHARED_ALIAS).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let logs = state.db.lock().list_forward_logs(10).unwrap();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!(logs[0].account_id, "acct-1");
    assert!(
        logs.iter().all(|log| log.account_id != ZEN_FREE_ACCOUNT_ID),
        "Free gates must close Zen without closing Go: {logs:?}"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn disabled_goat_is_skipped_and_unavailable_raw_has_no_route() {
    let (base_url, calls, stop) = start_scripted_upstream(
        vec![ScriptedReply {
            status: 200,
            body: SUCCESS_BODY,
        }],
        Arc::new(|_| {}),
    )
    .await;
    let (state, dir) = local_go_state(base_url, &["key-1"]);
    insert_disabled_offering(
        &state,
        "acct-1",
        "goat-disabled",
        COMMAND_CODE_PROVIDER_ID,
        "goat-key",
    );
    state
        .db
        .lock()
        .reorder_accounts(&[
            "goat-disabled".into(),
            "acct-1".into(),
            ZEN_FREE_ACCOUNT_ID.into(),
        ])
        .unwrap();
    let (port, gateway_handle) = start_gateway(state.clone()).await;

    let (status, body) = chat(port, GO_MODEL).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let logs = state.db.lock().list_forward_logs(10).unwrap();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!(logs[0].account_id, "acct-1");
    assert!(
        logs.iter().all(|log| log.account_id != "goat-disabled"),
        "disabled GOAT must stay off the production route set: {logs:?}"
    );

    let (status, body) = chat(port, COMMAND_CODE_GOAT_DEEPSEEK_V4_FLASH_UPSTREAM).await;
    assert_ne!(status, StatusCode::OK, "{body}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "unroutable GOAT must not reach upstream"
    );

    gateway::stop_gateway(gateway_handle);
    let _ = stop.send(());
    let _ = std::fs::remove_dir_all(dir);
}
