use super::*;
use crate::crypto::StaticKeyCipher;
use crate::db::{CpaCatalogModel, Database};
use crate::state::CoreStateInner;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Utc;
use serde_json::json;
use std::sync::Arc;

fn stored_catalog_row(state: &CoreState) -> (String, String, String) {
    state
        .db
        .lock()
        .conn
        .query_row(
            "SELECT models_json, refreshed_at, source_url
             FROM provider_model_catalogs WHERE provider_id = 'cpa'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn table_count(state: &CoreState, table: &str) -> i64 {
    state
        .db
        .lock()
        .conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[tokio::test]
async fn retired_shared_catalog_get_is_nonroutable_and_put_preserves_bytes() {
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
    let empty = get_models(State(state.clone())).await.unwrap().0;
    assert!(empty.models.is_empty());
    assert!(empty.source_url.is_none());
    assert!(empty.refreshed_at.is_none());

    let refreshed_at = Utc::now();
    state
        .db
        .lock()
        .replace_cpa_model_catalog(
            &[
                CpaCatalogModel {
                    id: "alpha".into(),
                    owned_by: Some("pool".into()),
                    enabled: true,
                },
                CpaCatalogModel {
                    id: "beta".into(),
                    owned_by: None,
                    enabled: false,
                },
            ],
            "https://cpa.invalid/models",
            refreshed_at,
        )
        .unwrap();
    let stored = stored_catalog_row(&state);
    assert!(stored.0.contains("\"enabled\":true"), "{}", stored.0);
    let revision = state.settings_revision();
    let generation = state.process_generation();
    let credentials = table_count(&state, "credentials");
    let grants = table_count(&state, "credential_grants");
    let destination_models = table_count(&state, "destination_models");

    let current = get_models(State(state.clone())).await.unwrap().0;
    assert_eq!(current.revision.revision, revision);
    assert_eq!(current.revision.process_generation, generation);
    assert_eq!(
        current.source_url.as_deref(),
        Some("https://cpa.invalid/models")
    );
    let shown =
        chrono::DateTime::parse_from_rfc3339(current.refreshed_at.as_deref().unwrap()).unwrap();
    let raw = chrono::DateTime::parse_from_rfc3339(&stored.1).unwrap();
    assert_eq!(shown, raw);
    assert!(current.models.iter().all(|model| !model.enabled));
    assert_eq!(current.models[0].id, "alpha");
    assert_eq!(current.models[0].owned_by.as_deref(), Some("pool"));
    assert_eq!(current.models[1].id, "beta");
    assert!(current.models[1].owned_by.is_none());
    assert_eq!(stored_catalog_row(&state), stored);
    assert!(state.cpa_model_catalog().is_empty());

    for body in [
        json!({
            "expectedRevision": revision,
            "processGeneration": generation,
            "enabledIds": ["beta"],
        }),
        json!({
            "expectedRevision": revision.wrapping_sub(1),
            "processGeneration": generation,
            "enabledIds": ["alpha"],
        }),
        json!({
            "expectedRevision": revision,
            "processGeneration": generation,
            "enabledIds": [],
        }),
    ] {
        let error = put_models(
            State(state.clone()),
            Bytes::from(serde_json::to_vec(&body).unwrap()),
        )
        .await
        .unwrap_err();
        let text = format!("{error:?}");
        assert!(text.contains(crate::state::RETIRED_CPA_CATALOG), "{text}");
        assert_eq!(
            error.into_response().status(),
            StatusCode::PRECONDITION_FAILED
        );
    }

    let missing_revision = put_models(State(state.clone()), Bytes::from_static(b"{}"))
        .await
        .unwrap_err();
    let missing_text = format!("{missing_revision:?}");
    assert!(
        missing_text.contains("expectedRevision is required"),
        "{missing_text}"
    );
    assert_eq!(
        missing_revision.into_response().status(),
        StatusCode::BAD_REQUEST
    );
    let invalid = put_models(State(state.clone()), Bytes::from_static(b"not-json"))
        .await
        .unwrap_err();
    assert_eq!(invalid.into_response().status(), StatusCode::BAD_REQUEST);

    assert_eq!(state.settings_revision(), revision);
    assert_eq!(state.process_generation(), generation);
    assert_eq!(stored_catalog_row(&state), stored);
    assert_eq!(table_count(&state, "credentials"), credentials);
    assert_eq!(table_count(&state, "credential_grants"), grants);
    assert_eq!(
        table_count(&state, "destination_models"),
        destination_models
    );
    assert!(state.cpa_model_catalog().is_empty());
    let after = get_models(State(state.clone())).await.unwrap().0;
    assert!(after.models.iter().all(|model| !model.enabled));
    assert_eq!(after.models.len(), 2);
    drop(state);
    let _ = std::fs::remove_dir_all(dir);
}
