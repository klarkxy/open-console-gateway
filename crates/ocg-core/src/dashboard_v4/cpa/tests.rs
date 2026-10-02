use super::*;
use crate::crypto::{KeyCipher, StaticKeyCipher};
use crate::db::Database;
use crate::log_types::OperationOutcome;
use crate::state::CoreStateInner;
use axum::body::Bytes;
use axum::extract::State;
use std::sync::Arc;

fn fresh() -> (std::path::PathBuf, CoreState) {
    let dir = std::env::temp_dir().join(format!(
        "ocg-v4-cpa-receipt-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> = Arc::new(StaticKeyCipher::new("cpa-receipt"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn close(dir: std::path::PathBuf, state: CoreState) {
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn get_writes_nothing_and_missing_catalog_is_rejected() {
    let (dir, state) = fresh();
    let listed = get_models(State(state.clone())).await.unwrap();
    assert!(listed.0.models.is_empty());
    assert!(super::super::applications::operation_receipts(&state).is_empty());
    let body = serde_json::json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
        "enabledIds": []
    });
    let error = put_models(
        State(state.clone()),
        Bytes::from(serde_json::to_vec(&body).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_reason(), "preconditionFailed");
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "cpa.replace");
    assert_eq!(rows[0].outcome, OperationOutcome::Rejected);
    assert_eq!(rows[0].reason_code.as_deref(), Some("precondition.failed"));
    close(dir, state);
}
