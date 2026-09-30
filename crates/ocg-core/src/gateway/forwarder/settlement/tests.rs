use super::*;

use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::gateway::diagnostics::RequestTrace;
use crate::http_client::RouteLabel;
use crate::kernel::protocol::ApiFormat;
use crate::models::{Account, AccountSetupStep, AccountType};
use crate::state::CoreStateInner;
use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ocg-stream-guard-{}-{}",
        label,
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn test_state(label: &str) -> (PathBuf, CoreState) {
    let dir = temp_dir(label);
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("stream-guard"));
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn account(state: &CoreState) -> Account {
    let now = Utc::now();
    Account {
        id: "acct-1".into(),
        provider_id: crate::provider::default_provider_id(),

        credential_kind: crate::provider::default_credential_kind(),
        quota_scope: crate::provider::default_quota_scope(),
        name: "acct-1".into(),
        username: None,
        password_cipher: None,
        key_cipher: state.encrypt_key("sk-guard").unwrap(),
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
    }
}

fn attempt_context() -> ForwardAttemptContext {
    ForwardAttemptContext {
        trace: RequestTrace::new(),
        client_body_bytes: 0,
        upstream_body_bytes: 0,
        attempt: 1,
        client_format: ApiFormat::ChatCompletions,
        upstream_format: ApiFormat::ChatCompletions,
        model: "deepseek-v4-flash".into(),
        requested_model: "deepseek-v4-flash".into(),
        resolved_alias: Some("deepseek-v4-flash".into()),
        upstream_model: "deepseek-v4-flash".into(),
        stream: true,
        route: RouteLabel::Direct,
        known_secret: None,
        route_account_id: Some("acct-1".into()),
        provider_id: Some(crate::provider::default_provider_id()),

        credential_account_id: Some("acct-1".into()),
        client_key_id: None,
        client_key_name: None,
        platform_price: None,
        official_price: None,
        restriction_details: None,
        credit_attempt: None,
        credit_log_id: None,
        credit_token_pricing_supported: true,
    }
}

fn insert_streaming_row(
    state: &CoreState,
    account: &Account,
    context: &ForwardAttemptContext,
) -> i64 {
    let pricing = RequestPricingSnapshot::from(state.pricing_snapshot());
    DbAttemptSink::new(&state.db.lock())
        .insert(
            &(account).into(),
            "deepseek-v4-flash",
            "streaming",
            Some(200),
            metadata_metrics(&pricing, None, "not_applicable"),
            None,
            context,
            None,
        )
        .unwrap()
}

#[test]
fn drop_without_terminal_finalizes_outcome_unknown() {
    let (dir, state) = test_state("drop-unknown");
    let account = account(&state);
    state.db.lock().create_account(&account).unwrap();
    let context = attempt_context();
    let log_id = insert_streaming_row(&state, &account, &context);
    {
        let _guard = StreamOutcomeGuard::new(
            state.clone(),
            log_id,
            Arc::new(Mutex::new(StreamState::default())),
            "deepseek-v4-flash".into(),
            state.pricing_snapshot(),
            None,
            context,
            200,
            0,
            Arc::new(Mutex::new(None)),
        );
    }
    let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "outcome_unknown");
    assert_eq!(log.cost_state, "outcome_unknown");
    assert_eq!(
        log.diagnostic
            .as_ref()
            .and_then(|value| value.get("error_stage"))
            .and_then(serde_json::Value::as_str),
        Some("downstream_disconnect")
    );
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn drop_after_terminal_usage_prices_from_stream_state_not_the_converter() {
    let (dir, state) = test_state("drop-usage");
    let account = account(&state);
    state.db.lock().create_account(&account).unwrap();
    let context = attempt_context();
    let log_id = insert_streaming_row(&state, &account, &context);
    let stream = StreamState {
        terminal: true,
        has_usage: true,
        usage: crate::gateway::protocol::UsageCounts {
            input_tokens: 11,
            output_tokens: 4,
            cached_tokens: 1,
            cache_creation_tokens: 0,
        },
        ..StreamState::default()
    };
    {
        let _guard = StreamOutcomeGuard::new(
            state.clone(),
            log_id,
            Arc::new(Mutex::new(stream)),
            "deepseek-v4-flash".into(),
            state.pricing_snapshot(),
            None,
            context,
            200,
            0,
            Arc::new(Mutex::new(None)),
        );
    }
    let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_ne!(log.status, "streaming");
    assert_eq!(log.prompt_tokens, 11);
    assert_eq!(log.completion_tokens, 4);
    assert_eq!(log.cached_tokens, 1);
    assert!(
        log.status.starts_with("success"),
        "terminal usage captured on Drop must finalize independently of StreamConverter: {}",
        log.status
    );
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn disarm_leaves_the_streaming_row_untouched() {
    let (dir, state) = test_state("disarm");
    let account = account(&state);
    state.db.lock().create_account(&account).unwrap();
    let context = attempt_context();
    let log_id = insert_streaming_row(&state, &account, &context);
    {
        let mut guard = StreamOutcomeGuard::new(
            state.clone(),
            log_id,
            Arc::new(Mutex::new(StreamState::default())),
            "deepseek-v4-flash".into(),
            state.pricing_snapshot(),
            None,
            context,
            200,
            0,
            Arc::new(Mutex::new(None)),
        );
        guard.disarm();
    }
    let log = state.db.lock().list_forward_logs(1).unwrap().remove(0);
    assert_eq!(log.status, "streaming");
    drop(state);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn db_attempt_sink_inserts_once_and_finalizes_the_same_row() {
    let (dir, state) = test_state("sink-once");
    let account = account(&state);
    state.db.lock().create_account(&account).unwrap();
    let context = attempt_context();
    let pricing = RequestPricingSnapshot::from(state.pricing_snapshot());
    let id = {
        let db = state.db.lock();
        let sink = DbAttemptSink::new(&db);
        let id = sink
            .insert(
                &(&account).into(),
                "deepseek-v4-flash",
                "streaming",
                Some(200),
                metadata_metrics(&pricing, None, "not_applicable"),
                None,
                &context,
                None,
            )
            .unwrap();
        sink.finalize(
            id,
            "success",
            Some(200),
            metadata_metrics(&pricing, None, "not_applicable"),
            None,
            None,
            &context,
        )
        .unwrap();
        id
    };
    let logs = state.db.lock().list_forward_logs(10).unwrap();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!(logs[0].id, id);
    assert_ne!(logs[0].status, "streaming");
    assert!(
        logs[0].status.starts_with("success"),
        "same-row finalize must leave one completed attempt: {}",
        logs[0].status
    );
    drop(state);
    let _ = fs::remove_dir_all(dir);
}
