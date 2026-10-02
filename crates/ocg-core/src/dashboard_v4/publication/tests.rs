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
        "ocg-v4-publication-receipt-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("publication-receipt"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());
    (dir, state)
}

fn close(dir: std::path::PathBuf, state: CoreState) {
    drop(state);
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn get_writes_nothing_and_publish_records_the_field_without_the_name() {
    let (dir, state) = fresh();
    let listed = get_publication(State(state.clone())).await.unwrap();
    assert!(listed.0.unpublished.is_empty());
    assert!(super::super::applications::operation_receipts(&state).is_empty());
    let body = serde_json::json!({
        "expectedRevision": state.settings_revision(),
        "processGeneration": state.process_generation(),
        "publicModel": "Alias-One",
        "published": false
    });
    let updated = patch_publication(
        State(state.clone()),
        Bytes::from(serde_json::to_vec(&body).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(updated.0.unpublished, vec!["alias-one".to_string()]);
    let rows = super::super::applications::operation_receipts(&state);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "publication.update");
    assert_eq!(rows[0].outcome, OperationOutcome::Success);
    assert_eq!(rows[0].metadata.changed_fields, vec!["published"]);
    assert!(rows[0].subject_id.is_none());
    let encoded = serde_json::to_string(&rows[0]).unwrap();
    assert!(!encoded.contains("alias-one"));
    assert!(!encoded.contains("Alias-One"));
    close(dir, state);
}
