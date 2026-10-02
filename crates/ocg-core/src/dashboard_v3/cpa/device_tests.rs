use super::*;
use crate::crypto::StaticKeyCipher;
use crate::db::Database;
use crate::state::{CoreState, CoreStateInner};
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn device_rejects_other_providers_and_stale_requests_without_side_effects() {
    let dir = std::env::temp_dir().join(format!("ocg-device-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("device-test")),
        )
        .unwrap(),
    );
    let revision = state.settings_revision();
    let request = |provider: &str, revision: u64| {
        Bytes::from(
            serde_json::to_vec(&json!({
                "provider": provider, "method": "device", "expectedRevision": revision,
                "processGeneration": state.process_generation(),
            }))
            .unwrap(),
        )
    };
    assert!(
        start_oauth(State(state.clone()), request("anthropic", revision))
            .await
            .is_err()
    );
    let stale = start_oauth(State(state.clone()), request("codex", revision + 1))
        .await
        .unwrap_err();
    assert_eq!(stale.body.code, super::super::ERROR_REVISION_CONFLICT);
    // No managed Host/installation: do not dispatch an arbitrary external login.
    assert!(
        start_oauth(State(state.clone()), request("codex", revision))
            .await
            .is_err()
    );
    assert_eq!(state.settings_revision(), revision);
    // Unknown local session must be rejected locally, without needing saved CPA secrets.
    assert!(
        oauth_status(
            State(state.clone()),
            Query(OAuthStatusQuery {
                state: "ocg-device-unknown".into()
            })
        )
        .await
        .is_err()
    );
    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn browser_oauth_terminal_and_cancel_finish_the_same_operation_id() {
    let dir = std::env::temp_dir().join(format!("ocg-browser-oauth-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            Database::open(dir.clone()).unwrap(),
            dir.clone(),
            Arc::new(StaticKeyCipher::new("browser-oauth")),
        )
        .unwrap(),
    );
    let raw_state = "browser-oauth-state-must-stay-out-of-metadata";
    let mut operation = crate::user_operation::UserOperation::dashboard(
        &state,
        "cpa.oauth.start",
        "cpa",
        Some("codex".into()),
    );
    let id = operation.operation_id().to_string();
    let metadata = crate::log_types::OperationMetadata {
        related_ids: vec![oauth_flow_related_id(raw_state)],
        requested_count: Some(1),
        ..crate::log_types::OperationMetadata::default()
    };
    operation.accepted(metadata);

    finish_browser_oauth_operation(&state, raw_state, "wait");
    finish_browser_oauth_operation(&state, raw_state, "unknown");
    let (outcome, reason, stored) = receipt(&state, &id);
    assert_eq!(outcome, "pending");
    assert!(reason.is_none());
    assert!(stored.contains("oauth:"));
    assert!(!stored.contains(raw_state));

    finish_browser_oauth_operation(&state, raw_state, "ok");
    finish_browser_oauth_operation(&state, raw_state, "error");
    let (outcome, reason, stored) = receipt(&state, &id);
    assert_eq!(outcome, "success");
    assert!(reason.is_none());
    assert!(stored.contains("oauth:"));
    assert!(!stored.contains(raw_state));
    assert_eq!(receipt_count(&state, "cpa.oauth.start"), 1);

    let raw_cancel = "second-browser-state-also-unstored";
    let mut operation = crate::user_operation::UserOperation::dashboard(
        &state,
        "cpa.oauth.start",
        "cpa",
        Some("anthropic".into()),
    );
    let cancel_id = operation.operation_id().to_string();
    let metadata = crate::log_types::OperationMetadata {
        related_ids: vec![oauth_flow_related_id(raw_cancel)],
        ..crate::log_types::OperationMetadata::default()
    };
    operation.accepted(metadata);
    finish_browser_oauth_operation(&state, raw_cancel, "cancelled");
    let (outcome, reason, stored) = receipt(&state, &cancel_id);
    assert_eq!(outcome, "rejected");
    assert_eq!(reason.as_deref(), Some("cancelled"));
    assert!(stored.contains(&oauth_flow_related_id(raw_cancel)));
    assert!(!stored.contains(raw_cancel));

    let raw_error = "third-browser-state-error";
    let mut operation = crate::user_operation::UserOperation::dashboard(
        &state,
        "cpa.oauth.start",
        "cpa",
        Some("kimi".into()),
    );
    let error_id = operation.operation_id().to_string();
    let metadata = crate::log_types::OperationMetadata {
        related_ids: vec![oauth_flow_related_id(raw_error)],
        ..crate::log_types::OperationMetadata::default()
    };
    operation.accepted(metadata);
    finish_browser_oauth_operation(&state, raw_error, "error");
    let (outcome, reason, stored) = receipt(&state, &error_id);
    assert_eq!(outcome, "failed");
    assert_eq!(reason.as_deref(), Some("outboundFailed"));
    assert!(!stored.contains(raw_error));
    assert!(!stored.contains("upstream"));
    assert_eq!(receipt_count(&state, "cpa.oauth.start"), 3);

    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

fn receipt(state: &CoreState, id: &str) -> (String, Option<String>, String) {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT outcome, reason_code, metadata_json FROM operation_logs WHERE operation_id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn receipt_count(state: &CoreState, action: &str) -> i64 {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT COUNT(*) FROM operation_logs WHERE action = ?1",
            [action],
            |row| row.get(0),
        )
        .unwrap()
}
