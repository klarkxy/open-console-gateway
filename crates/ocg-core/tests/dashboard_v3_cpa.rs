//! Dashboard V3 CPA model catalog snapshot.

use ocg_core::dashboard_v3::CpaModels;
use ocg_core::db::CpaCatalogModel;
use reqwest::StatusCode;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::start_loopback;

#[tokio::test]
async fn cpa_model_catalog_get_returns_the_persisted_snapshot() {
    let harness = start_loopback("cpa-models-get").await;
    let (status, body) = harness
        .get_json(&format!(
            "{}/external-integrations/cpa/models",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let empty: CpaModels = serde_json::from_value(body).unwrap();
    assert!(empty.models.is_empty());
    assert!(empty.source_url.is_none());

    harness
        .state
        .activate_cpa_model_catalog(
            vec![CpaCatalogModel {
                id: "gpt-5".into(),
                owned_by: Some("openai".into()),
                enabled: true,
            }],
            "http://127.0.0.1:8317",
            chrono::Utc::now(),
        )
        .unwrap();

    let (status, body) = harness
        .get_json(&format!(
            "{}/external-integrations/cpa/models",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let catalog: CpaModels = serde_json::from_value(body).unwrap();
    assert_eq!(catalog.models.len(), 1);
    assert_eq!(catalog.models[0].id, "gpt-5");
    assert_eq!(catalog.models[0].owned_by.as_deref(), Some("openai"));
    assert_eq!(catalog.source_url.as_deref(), Some("http://127.0.0.1:8317"));
    harness.stop();
}
