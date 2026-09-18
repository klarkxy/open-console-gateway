//! Runtime catalog models must reach their selected upstream through the public gateway.
use axum::http::StatusCode;
use chrono::Utc;
use ocg_core::dashboard_v3::OfficialProtocolBaseline;
use ocg_core::provider::{OPENCODE_GO_BASE_URL, OPENCODE_PROVIDER_ID, UpstreamProtocolKind};
use ocg_core::provider_contracts::{
    CATALOG_SOURCE_OPENCODE_MODELS, ContractScope, ProtocolOverrideState,
};
use ocg_core::state::CoreStateInner;
use serde_json::{Value, json};
#[path = "fixtures/gateway_fallback.rs"]
mod fixture;
use fixture::*;

fn set_catalog(state: &CoreStateInner, model: &str, protocol: UpstreamProtocolKind, enabled: bool) {
    let scope = ContractScope::provider(OPENCODE_PROVIDER_ID);
    let now = Utc::now();
    let models = vec![model.to_string()];
    {
        let db = state.db.lock();
        db.set_contract_catalog(
            &scope,
            &models,
            Some(now),
            CATALOG_SOURCE_OPENCODE_MODELS,
            OPENCODE_GO_BASE_URL,
            now,
        )
        .unwrap();
        // This is a deterministic docs fixture, not a live upstream capability claim.
        db.apply_official_protocol_baseline(
            &scope,
            &models,
            &OfficialProtocolBaseline::mapped([(model, protocol)]),
            now,
        )
        .unwrap();
        db.set_model_protocol_overrides(
            &scope,
            &[(
                model.to_string(),
                protocol,
                if enabled {
                    ProtocolOverrideState::ForceOn
                } else {
                    ProtocolOverrideState::ForceOff
                },
            )],
            now,
        )
        .unwrap();
    }
    state.reload_provider_contracts().unwrap();
}

#[tokio::test]
async fn issue58_refreshed_go_models_reach_correct_upstream_and_preserve_client_name() {
    for (model, protocol, expected_path, response) in [
        (
            "muse-spark-1.3-contributor",
            UpstreamProtocolKind::Responses,
            "/v1/responses",
            ok_responses(),
        ),
        (
            "omen-alpha",
            UpstreamProtocolKind::ChatCompletions,
            "/v1/chat/completions",
            ok(),
        ),
        (
            "future-go-model",
            UpstreamProtocolKind::Messages,
            "/v1/messages",
            ok_messages(),
        ),
    ] {
        let p = PreparedFallback::go(
            &[(
                "key-1",
                &[
                    response.clone(),
                    response.clone(),
                    response.clone(),
                    response,
                ],
            )],
            &["key-1"],
        )
        .await;
        set_catalog(&p.state, model, protocol, true);
        let h = p.bind().await;
        for path in ["/v1/responses", "/v1/chat/completions", "/v1/messages"] {
            let (status, body) = h.protocol(path, model).await;
            assert_eq!(status, StatusCode::OK, "{model} {path}: {body}");
            assert_eq!(body["model"], model);
        }
        let (status, body) = gemini_call(h.port, model).await;
        assert_eq!(status, StatusCode::OK, "{model} Gemini: {body}");
        let calls = h.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        for call in calls.iter() {
            assert_eq!(call.path, expected_path);
            let body: Value = serde_json::from_str(&call.body).unwrap();
            assert_eq!(body["model"], model);
            assert_eq!(call.key, "key-1");
        }
    }
}

#[tokio::test]
async fn issue58_unlisted_disabled_and_removed_models_never_send_upstream() {
    let p = PreparedFallback::go(&[("key-1", &[ok()])], &["key-1"]).await;
    set_catalog(
        &p.state,
        "omen-alpha",
        UpstreamProtocolKind::ChatCompletions,
        false,
    );
    let h = p.bind().await;
    for model in ["omen-alpha", "model-not-in-catalog"] {
        let (status, _) = h.protocol("/v1/chat/completions", model).await;
        assert_ne!(status, StatusCode::OK);
    }
    assert!(h.calls.lock().unwrap().is_empty());
    set_catalog(
        &h.state,
        "omen-alpha",
        UpstreamProtocolKind::ChatCompletions,
        true,
    );
    assert_eq!(
        h.protocol("/v1/chat/completions", "omen-alpha").await.0,
        StatusCode::OK
    );
    set_catalog(
        &h.state,
        "replacement-model",
        UpstreamProtocolKind::ChatCompletions,
        true,
    );
    let (status, _) = h.protocol("/v1/chat/completions", "omen-alpha").await;
    assert_ne!(status, StatusCode::OK);
    assert_eq!(h.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn issue58_protocol_constraints_are_still_enforced_by_actual_candidate() {
    let p = PreparedFallback::go(&[("key-1", &[ok()])], &["key-1"]).await;
    set_catalog(
        &p.state,
        "omen-alpha",
        UpstreamProtocolKind::ChatCompletions,
        true,
    );
    let h = p.bind().await;
    let response = loopback_client()
        .post(format!("http://127.0.0.1:{}/v1/responses", h.port))
        .bearer_auth("gw-test")
        .json(&json!({"model":"omen-alpha","input":"hello","store":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(h.calls.lock().unwrap().is_empty());
}
