//! Focused coverage for low-frequency runtime/control-plane observability.

use ocg_core::crypto::{KeyCipher, StaticKeyCipher};
use ocg_core::db::Database;
use ocg_core::gateway::{self, GatewayLifecycle};
use ocg_core::state::CoreStateInner;
use reqwest::StatusCode;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;

#[path = "fixtures/dashboard_v3/harness.rs"]
mod harness;

#[tokio::test]
async fn listener_lifecycle_does_not_write_mixed_history_or_user_operations() {
    let dir = harness::temp_data_dir("runtime-logging-listener");
    let db = Database::open(dir.clone()).unwrap();
    let cipher: Arc<dyn KeyCipher + Send + Sync> =
        Arc::new(StaticKeyCipher::new("runtime-logging"));
    let state = Arc::new(CoreStateInner::new(db, dir.clone(), cipher).unwrap());

    let handle = gateway::start_gateway_on(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let first_port = handle.port;
    *state.gateway.lock() = Some(handle);

    let second_port =
        GatewayLifecycle::rebind(state.clone(), SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
    assert_ne!(first_port, second_port);

    let active = state.gateway.lock().take().unwrap();
    gateway::stop_gateway_and_wait(active).await;

    assert!(state.db.lock().list_gateway_logs(20).unwrap().is_empty());
    assert_eq!(
        state
            .db
            .lock()
            .query_operation_logs(&Default::default())
            .unwrap()
            .total,
        0
    );

    drop(state);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn settings_operation_names_changed_fields_without_recording_values() {
    let harness = harness::start_loopback("runtime-logging-settings").await;
    let canary_url = "https://runtime-log-canary.invalid";
    let primary_key = harness.state.config().gateway_key;
    let response = harness
        .client
        .put(format!("{}/settings", harness.v3_base))
        .json(&json!({
            "expectedRevision": harness.state.settings_revision(),
            "processGeneration": harness.state.process_generation(),
            "clientRootUrl": canary_url,
            "connectTimeoutSecs": 17
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let page = harness
        .state
        .db
        .lock()
        .query_operation_logs(&Default::default())
        .unwrap();
    let settings = page
        .items
        .iter()
        .find(|row| row.action == "settings.update")
        .unwrap();
    assert_eq!(
        settings.outcome,
        ocg_core::log_types::OperationOutcome::Success
    );
    assert!(
        settings
            .metadata
            .changed_fields
            .iter()
            .any(|field| field == "client_root_url")
    );
    assert!(
        settings
            .metadata
            .changed_fields
            .iter()
            .any(|field| field == "connect_timeout_secs")
    );
    let encoded = serde_json::to_string(&page).unwrap();
    assert!(!encoded.contains(canary_url));
    assert!(!encoded.contains(&primary_key));
    assert!(
        harness
            .state
            .db
            .lock()
            .list_gateway_logs(20)
            .unwrap()
            .is_empty()
    );

    harness.stop();
}
