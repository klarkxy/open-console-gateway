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
        "ocg-v4-binding-receipt-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("binding-receipt"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn close(dir: std::path::PathBuf, state: CoreState) {
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn invalid_json_is_rejected_and_a_missing_binding_stays_rejected() {
    let (dir, state) = fresh();
    let error = patch(
        State(state.clone()),
        Path("binding-1".into()),
        Bytes::from_static(b"{"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_reason(), "invalidJson");
    let body = serde_json::json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
        "enabled": false
    });
    let error = patch(
        State(state.clone()),
        Path("binding-1".into()),
        Bytes::from(serde_json::to_vec(&body).unwrap()),
    )
    .await
    .unwrap_err();
    assert_eq!(error.operation_reason(), "notFound");
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| {
        row.action == "binding.update"
            && row.outcome == OperationOutcome::Rejected
            && row.subject_id.as_deref() == Some("binding-1")
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
