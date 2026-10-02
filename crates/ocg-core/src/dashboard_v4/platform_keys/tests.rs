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
        "ocg-v4-platform-receipt-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("platform-receipt"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn close(dir: std::path::PathBuf, state: CoreState) {
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn invalid_page_and_missing_account_stay_atomic_rejections() {
    let (dir, state) = fresh();
    let page = serde_json::json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
        "page": 0
    });
    let error = import_keys(
        State(state.clone()),
        Path("platform-1".into()),
        Bytes::from(serde_json::to_vec(&page).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_reason(), "invalidRequest");
    let missing = serde_json::json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
        "page": 1
    });
    let error = import_keys(
        State(state.clone()),
        Path("platform-1".into()),
        Bytes::from(serde_json::to_vec(&missing).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_reason(), "notFound");
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| {
        row.action == "platform.import"
            && row.outcome == OperationOutcome::Rejected
            && row.subject_id.as_deref() == Some("platform-1")
    }));
    assert!(
        rows.iter()
            .any(|row| row.reason_code.as_deref() == Some("not.found"))
    );
    assert!(
        rows.iter()
            .any(|row| row.reason_code.as_deref() == Some("invalid.request"))
    );
    close(dir, state);
}
