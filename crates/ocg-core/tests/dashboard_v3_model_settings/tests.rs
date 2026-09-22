#[path = "../fixtures/refreshed_go_catalog.rs"]
mod refreshed_go_catalog;

use super::harness::{V3Harness, start_loopback};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

async fn mutate(h: &V3Harness, method: Method, path: &str, mut body: Value) -> (StatusCode, Value) {
    body["expectedRevision"] = json!(h.state.settings_revision());
    body["processGeneration"] = json!(h.state.process_generation());
    let response = h
        .client
        .request(method, format!("{}{path}", h.v3_base))
        .json(&body)
        .send()
        .await
        .unwrap();
    (response.status(), response.json().await.unwrap())
}

fn minimax(body: &Value) -> &Value {
    body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["providerId"] == "minimax")
        .unwrap()["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["modelId"] == "MiniMax-M3")
        .unwrap()
}

#[tokio::test]
async fn cn_protocol_choice_survives_disable_reload_transfer_and_static_reset() {
    let h = start_loopback("cn-choice").await;
    refreshed_go_catalog::persist_provider_catalog(&h.state, "minimax", &["MiniMax-M3"]);
    refreshed_go_catalog::persist_provider_catalog(&h.state, "opencode", &["grok-4.5"]);
    let path = "/provider-contracts/provider/minimax/model-protocol-overrides";
    let (status, selected) = mutate(&h, Method::PUT, path, json!({"overrides":[
        {"modelId":"MiniMax-M3","protocol":"chat_completions","state":"force_on","preferred":true},
        {"modelId":"MiniMax-M3","protocol":"messages","state":"force_off"}
    ]})).await;
    assert_eq!(status, StatusCode::OK, "{selected}");
    assert_eq!(minimax(&selected)["preferredProtocol"], "chat_completions");
    let (status, disabled) = mutate(
        &h,
        Method::PUT,
        path,
        json!({"overrides":[
            {"modelId":"MiniMax-M3","protocol":"chat_completions","state":"force_off"},
            {"modelId":"MiniMax-M3","protocol":"messages","state":"force_off"},
            {"modelId":"MiniMax-M3","protocol":"responses","state":"force_off"}
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{disabled}");
    assert_eq!(minimax(&disabled)["preferredProtocol"], "chat_completions");
    assert_eq!(minimax(&disabled)["routable"], false);
    h.state.reload_provider_contracts().unwrap();
    assert_eq!(
        h.state.provider_contracts().providers["minimax"]
            .model("MiniMax-M3")
            .unwrap()
            .preferred_protocol
            .as_str(),
        "chat_completions"
    );

    let response = h
        .client
        .post(format!("{}/accounts/transfer/export", h.v3_base))
        .json(&json!({"bundlePassword":"model-choice-test-password"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let exported: Value = response.json().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{exported}");
    let target = start_loopback("cn-choice-import").await;
    let (status, imported) = mutate(
        &target,
        Method::POST,
        "/accounts/transfer/import",
        json!({
            "bundle":exported["bundle"], "password":"model-choice-test-password"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    let imported = target.state.provider_contracts();
    let model = imported.providers["minimax"].model("MiniMax-M3").unwrap();
    assert_eq!(model.preferred_protocol.as_str(), "chat_completions");
    assert!(!model.routable);
    let (status, enabled) = mutate(
        &target,
        Method::PUT,
        path,
        json!({"overrides":[
            {"modelId":"MiniMax-M3","protocol":"chat_completions","state":"force_on"}
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enabled}");
    assert_eq!(
        minimax(&enabled)["protocols"]["chat_completions"]["enabled"],
        true
    );
    assert_eq!(minimax(&enabled)["protocols"]["messages"]["enabled"], false);
    let (status, reset) = mutate(
        &target,
        Method::POST,
        "/provider-contracts/provider/minimax/model-protocols/reset-static",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reset}");
    assert_eq!(minimax(&reset)["preferredProtocol"], "messages");
    assert!(
        target
            .state
            .db
            .lock()
            .load_persisted_contracts()
            .unwrap()
            .preferences
            .is_empty()
    );
    target.stop();
    h.stop();
}

#[tokio::test]
async fn multiple_preferences_reject_the_batch_while_responses_is_valid() {
    let h = start_loopback("invalid-cn-choice").await;
    refreshed_go_catalog::persist_provider_catalog(&h.state, "minimax", &["MiniMax-M3"]);
    refreshed_go_catalog::persist_provider_catalog(&h.state, "opencode", &["grok-4.5"]);
    let before = h.state.settings_revision();
    let (status, result) = mutate(
        &h,
        Method::PUT,
        "/provider-contracts/provider/minimax/model-protocol-overrides",
        json!({"overrides":[
            {"modelId":"MiniMax-M3","protocol":"chat_completions","state":"force_off","preferred":true},
            {"modelId":"MiniMax-M3","protocol":"messages","state":"force_off","preferred":true}
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{result}");
    assert_eq!(h.state.settings_revision(), before);
    assert!(
        h.state
            .db
            .lock()
            .load_persisted_contracts()
            .unwrap()
            .preferences
            .is_empty()
    );

    let (status, result) = mutate(
        &h,
        Method::PUT,
        "/provider-contracts/provider/minimax/model-protocol-overrides",
        json!({"overrides":[
            {"modelId":"MiniMax-M3","protocol":"responses","state":"force_on","preferred":true}
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(minimax(&result)["preferredProtocol"], "responses");
    assert_ne!(h.state.settings_revision(), before);
    let (status, result) = mutate(
        &h,
        Method::PUT,
        "/provider-contracts/provider/opencode/model-protocol-overrides",
        json!({"overrides":[
            {"modelId":"grok-4.5","protocol":"responses","state":"force_on","preferred":true}
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_ne!(h.state.settings_revision(), before);
    h.stop();
}
