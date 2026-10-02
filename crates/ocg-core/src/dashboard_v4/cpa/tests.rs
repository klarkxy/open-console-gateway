use super::*;
use crate::crypto::StaticKeyCipher;
use crate::db::{CpaCatalogModel, Database};
use crate::state::CoreStateInner;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn cpa_selection_receipt_uses_the_write_revision_and_stale_cas_writes_nothing() {
    let dir = std::env::temp_dir().join(format!("ocg-cpa-select-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(dir.clone()).unwrap();
    let state = Arc::new(
        CoreStateInner::new(
            db,
            dir.clone(),
            Arc::new(StaticKeyCipher::new("cpa-select")),
        )
        .unwrap(),
    );
    state
        .db
        .lock()
        .replace_cpa_model_catalog(
            &[
                CpaCatalogModel {
                    id: "alpha".into(),
                    owned_by: None,
                    enabled: true,
                },
                CpaCatalogModel {
                    id: "beta".into(),
                    owned_by: None,
                    enabled: false,
                },
            ],
            "https://cpa.invalid/models",
            Utc::now(),
        )
        .unwrap();
    let before = state.settings_revision();
    let generation = state.process_generation();
    let updated = put_models(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": before,
                "processGeneration": generation,
                "enabledIds": ["beta"],
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(updated.revision.revision, before + 1);
    assert_eq!(updated.revision.revision, state.settings_revision());
    assert_eq!(updated.revision.process_generation, generation);
    let enabled: Vec<_> = updated
        .models
        .iter()
        .filter(|model| model.enabled)
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(enabled, vec!["beta"]);
    let stale = put_models(
        State(state.clone()),
        Bytes::from(
            serde_json::to_vec(&json!({
                "expectedRevision": before,
                "processGeneration": generation,
                "enabledIds": ["alpha"],
            }))
            .unwrap(),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(stale.into_response().status(), StatusCode::CONFLICT);
    let current = get_models(State(state.clone())).await.unwrap().0;
    let enabled: Vec<_> = current
        .models
        .iter()
        .filter(|model| model.enabled)
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(enabled, vec!["beta"]);
    assert_eq!(current.revision.revision, updated.revision.revision);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}
