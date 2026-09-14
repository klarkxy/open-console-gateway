//! Dashboard V3 CPA runtime: fail-closed Host and secret-free reads.

use ocg_core::dashboard_v3::{CpaIntegration, CpaRuntime, ERROR_INVALID_REQUEST};
use reqwest::StatusCode;
use serde_json::json;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

use harness::start_loopback;

#[tokio::test]
async fn cpa_runtime_status_is_unsupported_without_a_host() {
    let harness = start_loopback("cpa-runtime-unsupported").await;
    let (status, body) = harness
        .get_json(&format!(
            "{}/external-integrations/cpa/runtime",
            harness.v3_base
        ))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime: CpaRuntime = serde_json::from_value(body.clone()).unwrap();
    assert!(!runtime.supported);
    assert_eq!(
        runtime.unavailable_reason.as_deref(),
        Some(ocg_core::cpa_runtime::UNAVAILABLE_REASON)
    );
    assert!(!runtime.installed);
    assert!(!runtime.running);
    assert!(!runtime.owned);
    assert!(!serde_json::to_string(&body).unwrap().contains("secret"));

    let (status, integration) = harness
        .get_json(&format!("{}/external-integrations/cpa", harness.v3_base))
        .await;
    assert_eq!(status, StatusCode::OK, "{integration}");
    let integration: CpaIntegration = serde_json::from_value(integration).unwrap();
    assert!(!integration.runtime_supported);
    assert!(!integration.runtime_owned);
    assert!(!integration.runtime_running);
    assert!(integration.installed_version.is_none());
    assert!(integration.latest_version.is_none());
    assert!(!integration.update_available);
    assert!(integration.current_operation.is_none());
    harness.stop();
}

#[tokio::test]
async fn cpa_runtime_mutations_fail_closed_without_a_host() {
    let harness = start_loopback("cpa-runtime-mutate").await;
    let response = harness
        .client
        .post(format!(
            "{}/external-integrations/cpa/runtime/install",
            harness.v3_base
        ))
        .json(&json!({
            "expectedRevision": harness.state.settings_revision(),
            "processGeneration": harness.state.process_generation()
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], ERROR_INVALID_REQUEST);
    assert_eq!(
        body["message"].as_str(),
        Some(ocg_core::cpa_runtime::UNAVAILABLE_REASON)
    );
    harness.stop();
}

#[tokio::test]
async fn client_key_routes_use_the_public_non_runtime_paths_only() {
    let harness = start_loopback("cpa-runtime-key-routes").await;
    let response = harness
        .client
        .get(format!(
            "{}/external-integrations/cpa/client-keys",
            harness.v3_base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = harness
        .client
        .get(format!(
            "{}/external-integrations/cpa/runtime/keys",
            harness.v3_base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    harness.stop();
}
