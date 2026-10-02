use super::*;
use crate::crypto::StaticKeyCipher;
use crate::db::Database;
use crate::state::CoreStateInner;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn stale_publication_is_conflict_and_leaves_the_hidden_set() {
    let dir = std::env::temp_dir().join(format!("ocg-publication-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            db,
            dir.clone(),
            Arc::new(StaticKeyCipher::new("publication-http")),
        )
        .unwrap(),
    );
    let before = state.settings_revision();
    let hidden = patch_publication(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": before,
                "processGeneration": state.process_generation(),
                "publicModel": "Hidden-Model",
                "published": false,
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(hidden.unpublished, vec!["hidden-model".to_string()]);
    assert_eq!(hidden.revision.revision, before + 1);
    let stale = patch_publication(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": before,
                "processGeneration": state.process_generation(),
                "publicModel": "hidden-model",
                "published": true,
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.into_response().status(), StatusCode::CONFLICT);
    let current = get_publication(State(state.clone())).await.unwrap().0;
    assert_eq!(current.unpublished, vec!["hidden-model".to_string()]);
    assert_eq!(current.revision.revision, hidden.revision.revision);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}
