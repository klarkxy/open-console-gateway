use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::log_types::OperationOutcome;
use crate::state::CoreStateInner;
use axum::body::Bytes;
use axum::extract::{Path, State};
use std::sync::Arc;

fn fresh() -> (std::path::PathBuf, CoreState) {
    let dir = std::env::temp_dir().join(format!(
        "ocg-v4-official-receipt-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("official-receipt"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn close(dir: std::path::PathBuf, state: CoreState) {
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn get_writes_nothing_and_invalid_refresh_is_rejected() {
    let (dir, state) = fresh();
    let missing = get_status(State(state.clone()), Path("missing-account".into()))
        .await
        .unwrap_err();
    assert_eq!(missing.operation_reason(), "notFound");
    assert!(super::super::applications::operation_receipts(&state).is_empty());
    let error = refresh_balance(
        State(state.clone()),
        Path("missing-account".into()),
        Bytes::from_static(b"{"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_reason(), "invalidJson");
    let body = serde_json::json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation()
    });
    let error = refresh_balance(
        State(state.clone()),
        Path("missing-account".into()),
        Bytes::from(serde_json::to_vec(&body).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_reason(), "notFound");
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| {
        row.action == "balance.refresh"
            && row.outcome == OperationOutcome::Rejected
            && row.subject_id.as_deref() == Some("missing-account")
            && row.metadata.related_ids.is_empty()
    }));
    assert!(
        rows.iter()
            .any(|row| row.reason_code.as_deref() == Some("not.found"))
    );
    assert!(
        rows.iter()
            .any(|row| row.reason_code.as_deref() == Some("invalid.json"))
    );
    close(dir, state);
}
